//! Consistent, verified SQLite snapshots, guarded restore and read-only
//! recovery diagnostics for the Host metadata database.
//!
//! ## Atomicity
//!
//! Backups use SQLite's [online backup API](https://www.sqlite.org/backup.html)
//! rather than `VACUUM INTO`:
//!
//! * SQLite holds a read transaction on the source, so the copy is a
//!   consistent snapshot even while the Host keeps writing in WAL mode.
//! * The copy is written to a fresh file in the destination directory with
//!   mode `0600` (Unix), verified with `PRAGMA integrity_check`, checked
//!   against the supported `user_version` and required tables, forced into a
//!   single-file `DELETE` journal, fsynced, and only then atomically renamed
//!   over the destination. A failure never leaves a partial destination.
//!
//! Restore validates a source snapshot, materializes it through the same
//! verified path, automatically backs up the current database, and atomically
//! replaces it. The previous database's `-wal`/`-shm`/`-journal` sidecars are
//! removed after the rename so they cannot be replayed against the new file.
//! Any failure after the replacement rolls back from the automatic safety
//! backup; failures before it leave the original file untouched.
//!
//! The module never selects or prints workspace rows. Diagnostics report only
//! paths, versions, `PRAGMA` results, table presence and sidecar sizes.

use super::{Result, Store, StoreError};
use rusqlite::{
    backup::{Backup, StepResult},
    params, Connection, OpenFlags,
};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Current ukk workspace schema. Kept here so this module does not need to
/// edit `schema.rs`; `schema_version_matches_migrated_store` guards drift.
pub const CURRENT_SCHEMA_VERSION: i64 = 2;
/// Oldest schema that can be backed up or restored. The Host migrates it on
/// the next normal open.
pub const MIN_SUPPORTED_SCHEMA_VERSION: i64 = 1;

const PRIVATE_FILE_MODE: u32 = 0o600;
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
const COPY_STEP_PAGES: i32 = 256;
const COPY_PAUSE: Duration = Duration::from_millis(25);
const COPY_TIMEOUT: Duration = Duration::from_secs(120);
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Result of a verified backup.
#[derive(Debug, Clone)]
pub struct BackupReport {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub bytes: u64,
    pub schema_version: i64,
    /// Whether an existing destination was replaced because the caller asked
    /// for it explicitly.
    pub overwritten: bool,
}

/// Result of a restore, including the automatic safety copy of the database
/// that was replaced.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    pub source: PathBuf,
    pub database: PathBuf,
    pub safety_backup: Option<PathBuf>,
    /// `true` when the safety copy is a verified snapshot; `false` when it is
    /// a raw copy of a corrupt/unreadable database.
    pub safety_backup_verified: bool,
    pub schema_version: i64,
    pub bytes: u64,
}

/// Read-only health report used by `db-doctor`. Contains no workspace rows.
#[derive(Debug, Clone, Default)]
pub struct DatabaseHealth {
    pub path: PathBuf,
    pub exists: bool,
    pub is_regular: bool,
    pub is_symlink: bool,
    pub bytes: u64,
    pub unix_mode: Option<u32>,
    pub schema_version: Option<i64>,
    pub supported_schema: bool,
    pub has_core_tables: bool,
    pub integrity: Option<String>,
    pub journal_mode: Option<String>,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub journal_bytes: u64,
    pub open_error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct DatabaseInfo {
    schema_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafetyKind {
    Verified,
    RawCopy,
}

impl Store {
    /// Back up through the connection that is already open. The connection is
    /// not closed, migrated or checkpointed by this operation.
    pub fn backup_to(
        &self,
        source: &Path,
        destination: &Path,
        overwrite: bool,
    ) -> Result<BackupReport> {
        backup_connection(&self.conn, source, destination, overwrite)
    }
}

/// Back up `source` to `destination` using a fresh connection.
///
/// The source must already exist and be a regular file. The destination is
/// never overwritten unless `overwrite` is true; a symlink or non-regular
/// destination is always rejected. On success the destination is a verified,
/// self-contained SQLite file with mode `0600` on Unix.
pub fn backup_database(source: &Path, destination: &Path, overwrite: bool) -> Result<BackupReport> {
    ensure_regular_file(source, "backup source")?;
    let connection = open_for_backup(source)?;
    backup_connection(&connection, source, destination, overwrite)
}

/// Restore `database` from the verified snapshot at `source`.
///
/// The source is validated read-only before anything is changed. If the
/// current database exists it is copied to an automatic `.pre-restore-*`
/// sibling first (a verified snapshot, or a raw copy when the file is corrupt).
/// The replacement is atomic and re-verified; a failure after the rename rolls
/// back from that safety copy.
pub fn restore_database(database: &Path, source: &Path) -> Result<RestoreReport> {
    ensure_regular_file(source, "restore source")?;
    if same_file(database, source) {
        return Err(StoreError::UnsafePath(
            "restore source and database are the same file".into(),
        ));
    }

    let source_connection = open_read_only(source, "restore source")?;
    let source_info = validate_database(&source_connection, "restore source")?;

    let parent = database_parent(database);
    create_database_parent(parent)?;
    let replacement = snapshot_to_temp(&source_connection, parent, "restore")?;
    drop(source_connection);

    let database_exists = match fs::symlink_metadata(database) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(StoreError::UnsafePath(format!(
                "database {} is a symlink; refusing to replace it",
                database.display()
            )))
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(StoreError::UnsafePath(format!(
                "database {} is not a regular file",
                database.display()
            )))
        }
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };

    let mut safety = None;
    if database_exists {
        ensure_no_active_writer(database)?;
        let safety_path = safety_backup_path(database);
        let kind = create_safety_backup(database, &safety_path)?;
        safety = Some((safety_path, kind));
    }

    let safety_path = safety.as_ref().map(|(path, _)| path.as_path());
    install_verified_snapshot(database, replacement.path(), safety_path)?;
    replacement.persist();

    let bytes = fs::metadata(database)?.len();
    let (safety_backup, safety_backup_verified) = match safety {
        Some((path, kind)) => (Some(path), kind == SafetyKind::Verified),
        None => (None, false),
    };
    Ok(RestoreReport {
        source: source.to_owned(),
        database: database.to_owned(),
        safety_backup,
        safety_backup_verified,
        schema_version: source_info.schema_version,
        bytes,
    })
}

/// Inspect the database without writing to it or reading workspace rows.
///
/// A missing path and a corrupt/foreign file are normal reports, not errors:
/// `db-doctor` prints the state and recovery guidance.
pub fn inspect_database(path: &Path) -> DatabaseHealth {
    let mut health = DatabaseHealth {
        path: path.to_owned(),
        ..DatabaseHealth::default()
    };

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return health,
        Err(error) => {
            health.open_error = Some(error.to_string());
            return health;
        }
    };

    health.exists = true;
    health.is_symlink = metadata.file_type().is_symlink();
    health.is_regular = metadata.is_file();
    health.bytes = metadata.len();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        health.unix_mode = Some(metadata.permissions().mode() & 0o777);
    }
    health.wal_bytes = sidecar_len(path, "-wal");
    health.shm_bytes = sidecar_len(path, "-shm");
    health.journal_bytes = sidecar_len(path, "-journal");

    if health.is_symlink {
        health.open_error = Some("path is a symlink; refusing to inspect through it".into());
        return health;
    }
    if !health.is_regular {
        health.open_error = Some("path is not a regular file".into());
        return health;
    }

    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(error) => {
            health.open_error = Some(format!("open: {error}"));
            return health;
        }
    };
    let _ = connection.busy_timeout(Duration::from_secs(2));

    match schema_version(&connection) {
        Ok(version) => {
            health.schema_version = Some(version);
            health.supported_schema =
                (MIN_SUPPORTED_SCHEMA_VERSION..=CURRENT_SCHEMA_VERSION).contains(&version);
        }
        Err(error) => health.open_error = Some(format!("user_version: {error}")),
    }

    match connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) => health.integrity = Some(result),
        Err(error) => health.integrity = Some(format!("unavailable: {error}")),
    }

    match connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
        Ok(mode) => health.journal_mode = Some(mode),
        Err(error) => {
            if health.open_error.is_none() {
                health.open_error = Some(format!("journal_mode: {error}"));
            }
        }
    }

    if let Some(version) = health.schema_version {
        match has_required_tables(&connection, version) {
            Ok(present) => health.has_core_tables = present,
            Err(error) => {
                if health.open_error.is_none() {
                    health.open_error = Some(format!("table check: {error}"));
                }
            }
        }
        health.supported_schema &= health.has_core_tables;
    }

    health
}

// ------------------------------------------------------------ backup core ---

fn backup_connection(
    source_connection: &Connection,
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<BackupReport> {
    let source_info = validate_schema(source_connection, "backup source")?;
    if same_file(source, destination) {
        return Err(StoreError::UnsafePath(
            "backup destination is the same file as the source".into(),
        ));
    }

    let overwritten = ensure_backup_destination(destination, overwrite)?;
    let parent = destination_parent(destination)?;
    let temporary = TempFileGuard::new(unique_temp_path(parent, "backup"));
    create_private_file(temporary.path())?;

    {
        let mut destination_connection = Connection::open(temporary.path())?;
        destination_connection.busy_timeout(BUSY_TIMEOUT)?;
        copy_snapshot(source_connection, &mut destination_connection)?;
        let destination_info =
            normalize_and_validate(&mut destination_connection, "backup snapshot")?;
        if destination_info.schema_version != source_info.schema_version {
            return Err(StoreError::Invalid(format!(
                "backup schema v{} does not match source v{}",
                destination_info.schema_version, source_info.schema_version
            )));
        }
    }

    set_private_permissions(temporary.path())?;
    sync_file(temporary.path())?;
    replace_file(temporary.path(), destination)?;
    temporary.persist();
    // A forced overwrite may have left the previous file's sidecars. The new
    // snapshot is self-contained, so those must not be replayed against it.
    remove_sidecars(destination)?;
    sync_directory(parent);

    let bytes = fs::metadata(destination)?.len();
    Ok(BackupReport {
        source: source.to_owned(),
        destination: destination.to_owned(),
        bytes,
        schema_version: source_info.schema_version,
        overwritten,
    })
}

fn open_for_backup(path: &Path) -> Result<Connection> {
    // Prefer a read-only source: backup must not checkpoint or create WAL
    // sidecars. Fall back to read-write only when SQLite needs write access to
    // recover a WAL database whose -shm file is absent.
    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(_) => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(
            |error| {
                StoreError::Invalid(format!(
                    "could not open backup source {}: {error}",
                    path.display()
                ))
            },
        )?,
    };
    connection.busy_timeout(BUSY_TIMEOUT)?;
    Ok(connection)
}

fn open_read_only(path: &Path, role: &str) -> Result<Connection> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            StoreError::Invalid(format!(
                "could not open {role} {} read-only: {error}; restore expects a self-contained \
                 SQLite file, for example one produced by `ukk-host backup`",
                path.display()
            ))
        })?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    Ok(connection)
}

/// Copy a consistent snapshot and bound the total time so a permanently busy
/// writer cannot hang a CLI invocation.
fn copy_snapshot(source: &Connection, destination: &mut Connection) -> Result<()> {
    let deadline = Instant::now() + COPY_TIMEOUT;
    {
        let backup = Backup::new(source, destination)?;
        loop {
            match backup.step(COPY_STEP_PAGES)? {
                StepResult::Done => break,
                // More, Busy, Locked and any future transient state: yield and
                // retry under the overall deadline.
                _ => {
                    if Instant::now() >= deadline {
                        return Err(StoreError::Busy(format!(
                            "SQLite online backup did not finish within {}s; retry when the database is less busy",
                            COPY_TIMEOUT.as_secs()
                        )));
                    }
                    thread::sleep(COPY_PAUSE);
                }
            }
        }
    }
    Ok(())
}

/// Force a single-file result. A WAL-mode source copies its header, so without
/// this step the snapshot could depend on a `-wal` sidecar that we do not ship.
fn normalize_and_validate(connection: &mut Connection, role: &str) -> Result<DatabaseInfo> {
    let _mode: String = connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    validate_database(connection, role)
}

fn snapshot_to_temp(source: &Connection, parent: &Path, prefix: &str) -> Result<TempFileGuard> {
    let temporary = TempFileGuard::new(unique_temp_path(parent, prefix));
    create_private_file(temporary.path())?;
    {
        let mut destination = Connection::open(temporary.path())?;
        destination.busy_timeout(BUSY_TIMEOUT)?;
        copy_snapshot(source, &mut destination)?;
        normalize_and_validate(&mut destination, "restore snapshot")?;
    }
    set_private_permissions(temporary.path())?;
    sync_file(temporary.path())?;
    Ok(temporary)
}

// ----------------------------------------------------------- validation ---

fn schema_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn has_required_tables(connection: &Connection, version: i64) -> rusqlite::Result<bool> {
    let required: &[&str] = if version >= 2 {
        &[
            "spaces",
            "sessions",
            "agent_runs",
            "session_layouts",
            "workspace_revision",
        ]
    } else {
        &["spaces", "sessions", "agent_runs", "session_layouts"]
    };
    for table in required {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_schema(connection: &Connection, role: &str) -> Result<DatabaseInfo> {
    let version = schema_version(connection).map_err(|error| map_not_a_database(error, role))?;
    if version == 0 {
        return Err(StoreError::NotADatabase(format!(
            "{role} has schema version 0; it is not an initialized ukk workspace"
        )));
    }
    if !(MIN_SUPPORTED_SCHEMA_VERSION..=CURRENT_SCHEMA_VERSION).contains(&version) {
        return Err(StoreError::UnsupportedVersion(version));
    }
    if !has_required_tables(connection, version).map_err(|error| map_not_a_database(error, role))? {
        return Err(StoreError::NotADatabase(format!(
            "{role} does not contain the ukk workspace tables"
        )));
    }
    Ok(DatabaseInfo {
        schema_version: version,
    })
}

fn validate_database(connection: &Connection, role: &str) -> Result<DatabaseInfo> {
    let info = validate_schema(connection, role)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| map_not_a_database(error, role))?;
    if integrity != "ok" {
        return Err(StoreError::IntegrityCheckFailed(integrity));
    }
    Ok(info)
}

fn map_not_a_database(error: rusqlite::Error, role: &str) -> StoreError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error {
        if code.code == rusqlite::ErrorCode::NotADatabase {
            return StoreError::NotADatabase(format!("{role} is not a SQLite database"));
        }
    }
    StoreError::Sql(error)
}

// -------------------------------------------------------------- restore ---

fn ensure_no_active_writer(database: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| {
            StoreError::Invalid(format!(
                "could not open {} for an exclusive check: {error}",
                database.display()
            ))
        })?;
    connection.busy_timeout(Duration::from_millis(0))?;
    match connection.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;") {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            Err(StoreError::Busy(
                "another process is writing to the database; stop the Host and retry restore"
                    .into(),
            ))
        }
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            ) =>
        {
            // A corrupt file cannot be actively serving valid writers. The
            // raw-copy safety backup below preserves it for recovery.
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn create_safety_backup(database: &Path, safety: &Path) -> Result<SafetyKind> {
    match backup_database(database, safety, false) {
        Ok(_) => Ok(SafetyKind::Verified),
        Err(StoreError::Busy(_)) => Err(StoreError::Busy(
            "another process is writing to the database; stop the Host and retry restore".into(),
        )),
        Err(error @ (StoreError::DestinationExists(_) | StoreError::UnsafePath(_))) => Err(error),
        Err(_) => {
            // The consistent snapshot could not be produced (corrupt file,
            // unsupported schema or failed WAL recovery). Preserve the current
            // bytes and sidecars instead of refusing to recover.
            let _ = fs::remove_file(safety);
            for suffix in SIDECARS {
                let _ = fs::remove_file(with_suffix(safety, suffix));
            }
            raw_copy_database(database, safety)?;
            Ok(SafetyKind::RawCopy)
        }
    }
}

/// Preserve a corrupt or foreign database byte-for-byte, including any
/// sidecars, when a consistent SQLite snapshot is impossible.
fn raw_copy_database(database: &Path, safety: &Path) -> Result<()> {
    create_private_file(safety)?;
    fs::copy(database, safety)?;
    set_private_permissions(safety)?;
    for suffix in SIDECARS {
        let source = with_suffix(database, suffix);
        match fs::metadata(&source) {
            Ok(metadata) if metadata.is_file() => {
                let destination = with_suffix(safety, suffix);
                fs::copy(&source, &destination)?;
                set_private_permissions(&destination)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    sync_file(safety)?;
    Ok(())
}

fn safety_backup_path(database: &Path) -> PathBuf {
    let parent = database_parent(database);
    let name = database
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace.sqlite3".to_owned());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    for attempt in 0..16 {
        let candidate = parent.join(format!(
            "{name}.pre-restore-{}-{}-{attempt}.sqlite3",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!(
        "{name}.pre-restore-{}-{}.sqlite3",
        std::process::id(),
        stamp.as_nanos()
    ))
}

/// Install an already-verified single-file snapshot over `database`.
///
/// The production caller validates `replacement` before this point; the
/// post-install check is a final defense and triggers the rollback path if the
/// file was corrupted or replaced between validation and rename.
fn install_verified_snapshot(
    database: &Path,
    replacement: &Path,
    safety: Option<&Path>,
) -> Result<()> {
    replace_file(replacement, database)?;
    if let Err(error) = remove_sidecars(database) {
        let rollback = rollback_from_safety(database, safety);
        return Err(StoreError::Invalid(format!(
            "could not remove stale SQLite sidecars after restore: {error}; {}",
            describe_rollback(rollback)
        )));
    }
    sync_directory(database_parent(database));

    match verify_installed_database(database) {
        Ok(()) => Ok(()),
        Err(error) => {
            let rollback = rollback_from_safety(database, safety);
            Err(StoreError::Invalid(format!(
                "restored database failed verification: {error}; {}",
                describe_rollback(rollback)
            )))
        }
    }
}

fn verify_installed_database(database: &Path) -> Result<()> {
    let connection = open_read_only(database, "restored database")?;
    validate_database(&connection, "restored database")?;
    Ok(())
}

fn rollback_from_safety(database: &Path, safety: Option<&Path>) -> std::result::Result<(), String> {
    let Some(safety) = safety else {
        // No previous database existed: remove the failed replacement so the
        // next Host start creates a clean file rather than inheriting garbage.
        let _ = fs::remove_file(database);
        remove_sidecars(database).map_err(|error| error.to_string())?;
        return Ok(());
    };

    if let Err(error) = replace_file(safety, database) {
        return Err(format!(
            "automatic rollback failed: {error}; the previous database remains at {}",
            safety.display()
        ));
    }
    for suffix in SIDECARS {
        let source = with_suffix(safety, suffix);
        let destination = with_suffix(database, suffix);
        if source.exists() {
            if let Err(error) = replace_file(&source, &destination) {
                return Err(format!(
                    "automatic rollback could not restore the {suffix} sidecar: {error}"
                ));
            }
        } else {
            let _ = fs::remove_file(&destination);
        }
    }
    sync_directory(database_parent(database));
    Ok(())
}

fn describe_rollback(result: std::result::Result<(), String>) -> String {
    match result {
        Ok(()) => "the previous database was restored automatically".to_owned(),
        Err(error) => error,
    }
}

// ------------------------------------------------------- path and file ---

fn ensure_regular_file(path: &Path, role: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(StoreError::NotFound(format!("{role} {}", path.display())))
        }
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::UnsafePath(format!(
            "{role} {} is a symlink; refusing to follow it",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_file() => Err(StoreError::UnsafePath(format!(
            "{role} {} is not a regular file",
            path.display()
        ))),
        Ok(_) => Ok(()),
    }
}

fn ensure_backup_destination(path: &Path, overwrite: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::UnsafePath(format!(
            "backup destination {} is a symlink; refusing to follow it",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_file() => Err(StoreError::UnsafePath(format!(
            "backup destination {} is not a regular file",
            path.display()
        ))),
        Ok(_) if !overwrite => Err(StoreError::DestinationExists(path.to_owned())),
        Ok(_) => Ok(true),
    }
}

fn destination_parent(path: &Path) -> Result<&Path> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::metadata(parent).map_err(|error| {
        StoreError::Invalid(format!(
            "destination directory {} is not accessible: {error}",
            parent.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(StoreError::UnsafePath(format!(
            "destination parent {} is not a directory",
            parent.display()
        )));
    }
    Ok(parent)
}

fn database_parent(database: &Path) -> &Path {
    database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn create_database_parent(parent: &Path) -> Result<bool> {
    match fs::metadata(parent) {
        Ok(metadata) if metadata.is_dir() => Ok(false),
        Ok(_) => Err(StoreError::UnsafePath(format!(
            "database parent {} is not a directory",
            parent.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}

fn unique_temp_path(parent: &Path, prefix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    parent.join(format!(
        ".ukk-{prefix}-{}-{}-{}.tmp",
        std::process::id(),
        stamp.as_secs(),
        stamp.subsec_nanos()
    ))
}

fn create_private_file(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
    }
    options.open(path)?;
    Ok(())
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        // MoveFileExW(MOVEFILE_WRITE_THROUGH) flushes the rename on Windows.
        let _ = path;
    }
    Ok(())
}

fn sync_directory(path: &Path) {
    #[cfg(unix)]
    {
        if let Ok(directory) = File::open(path) {
            let _ = directory.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let from_wide: Vec<u16> = from
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let to_wide: Vec<u16> = to
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: both buffers are NUL-terminated and live for the call.
        let moved = unsafe {
            MoveFileExW(
                from_wide.as_ptr(),
                to_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)?;
        Ok(())
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_sidecars(path: &Path) -> Result<()> {
    for suffix in SIDECARS {
        let sidecar = with_suffix(path, suffix);
        match fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn sidecar_len(path: &Path, suffix: &str) -> u64 {
    fs::metadata(with_suffix(path, suffix))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

struct TempFileGuard {
    path: PathBuf,
    keep: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn persist(mut self) {
        self.keep = true;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = fs::remove_file(&self.path);
        for suffix in SIDECARS {
            let _ = fs::remove_file(with_suffix(&self.path, suffix));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::new_id;

    fn temp_dir(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("ukk-backup-{label}-{}", new_id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn create_database(path: &Path, title: &str) -> (String, String) {
        let mut store = Store::open(path).unwrap();
        let space = store.create_space(title, "/repo").unwrap();
        let session = store.create_session(&space.id, "session").unwrap();
        (space.id, session.id)
    }

    #[test]
    fn schema_version_matches_migrated_store() {
        let store = Store::open_memory().unwrap();
        assert_eq!(schema_version(&store.conn).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn backup_while_database_is_open_is_consistent() {
        let directory = temp_dir("open");
        let database = directory.join("workspace.sqlite3");
        let backup = directory.join("snapshot.sqlite3");

        let mut store = Store::open(&database).unwrap();
        let space = store.create_space("ukk", "/repo").unwrap();
        let session = store.create_session(&space.id, "live").unwrap();

        // The live connection and its WAL stay open for the whole copy.
        let report = store.backup_to(&database, &backup, false).unwrap();
        assert_eq!(report.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(report.bytes, fs::metadata(&backup).unwrap().len());
        assert!(!report.overwritten);
        assert!(!with_suffix(&backup, "-wal").exists());
        assert!(!with_suffix(&backup, "-shm").exists());

        let restored = Store::open(&backup).unwrap();
        let snapshot = restored.snapshot().unwrap();
        assert_eq!(snapshot.spaces[0].id, space.id);
        assert_eq!(snapshot.sessions[0].id, session.id);

        drop(store);
        drop(restored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_refuses_overwrite_and_unsafe_destinations() {
        let directory = temp_dir("dest");
        let database = directory.join("workspace.sqlite3");
        let destination = directory.join("snapshot.sqlite3");
        create_database(&database, "source");

        backup_database(&database, &destination, false).unwrap();
        assert!(matches!(
            backup_database(&database, &destination, false),
            Err(StoreError::DestinationExists(_))
        ));
        backup_database(&database, &destination, true).unwrap();

        let directory_destination = directory.join("not-a-file");
        fs::create_dir(&directory_destination).unwrap();
        assert!(matches!(
            backup_database(&database, &directory_destination, true),
            Err(StoreError::UnsafePath(_))
        ));
        assert!(matches!(
            backup_database(&database, &database, true),
            Err(StoreError::UnsafePath(_))
        ));

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_and_restore_refuse_symlink_paths() {
        use std::os::unix::fs::symlink;

        let directory = temp_dir("symlink");
        let database = directory.join("workspace.sqlite3");
        create_database(&database, "source");
        let link = directory.join("link.sqlite3");
        symlink(&database, &link).unwrap();

        assert!(matches!(
            backup_database(&database, &link, true),
            Err(StoreError::UnsafePath(_))
        ));
        assert!(matches!(
            restore_database(&database, &link),
            Err(StoreError::UnsafePath(_))
        ));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_rejects_corrupt_foreign_and_unsupported_sources() {
        let directory = temp_dir("reject");
        let destination = directory.join("snapshot.sqlite3");

        let corrupt = directory.join("corrupt.sqlite3");
        fs::write(&corrupt, b"this is not a database").unwrap();
        assert!(matches!(
            backup_database(&corrupt, &destination, false),
            Err(StoreError::NotADatabase(_))
        ));
        assert!(!destination.exists());

        let empty = directory.join("empty.sqlite3");
        Connection::open(&empty).unwrap();
        assert!(matches!(
            backup_database(&empty, &destination, false),
            Err(StoreError::NotADatabase(_))
        ));
        assert!(!destination.exists());

        let foreign = directory.join("foreign.sqlite3");
        {
            let connection = Connection::open(&foreign).unwrap();
            connection
                .execute_batch("PRAGMA user_version=2; CREATE TABLE notes(id INTEGER);")
                .unwrap();
        }
        assert!(matches!(
            backup_database(&foreign, &destination, false),
            Err(StoreError::NotADatabase(_))
        ));
        assert!(!destination.exists());

        let newer = directory.join("newer.sqlite3");
        {
            let connection = Connection::open(&newer).unwrap();
            connection.execute_batch("PRAGMA user_version=99;").unwrap();
        }
        assert!(matches!(
            backup_database(&newer, &destination, false),
            Err(StoreError::UnsupportedVersion(99))
        ));
        assert!(!destination.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_is_verified_and_private() {
        let directory = temp_dir("verify");
        let database = directory.join("workspace.sqlite3");
        let destination = directory.join("snapshot.sqlite3");
        create_database(&database, "source");

        let report = backup_database(&database, &destination, false).unwrap();
        let connection =
            Connection::open_with_flags(&destination, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(schema_version(&connection).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_required_tables(&connection, CURRENT_SCHEMA_VERSION).unwrap());
        assert_eq!(report.schema_version, CURRENT_SCHEMA_VERSION);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_replaces_and_reopens_with_a_safety_backup() {
        let directory = temp_dir("restore");
        let database = directory.join("workspace.sqlite3");
        let source = directory.join("source.sqlite3");
        let (source_space, _) = create_database(&source, "source");
        let (current_space, _) = create_database(&database, "current");

        let report = restore_database(&database, &source).unwrap();
        assert_eq!(report.schema_version, CURRENT_SCHEMA_VERSION);
        let safety = report
            .safety_backup
            .expect("the previous database should be backed up");
        assert!(report.safety_backup_verified);
        assert!(safety.exists());

        let restored = Store::open(&database).unwrap();
        let snapshot = restored.snapshot().unwrap();
        assert_eq!(snapshot.spaces.len(), 1);
        assert_eq!(snapshot.spaces[0].id, source_space);
        assert!(snapshot
            .spaces
            .iter()
            .all(|space| space.id != current_space));
        drop(restored);

        // Reopen after restore.
        let reopened = Store::open(&database).unwrap();
        assert_eq!(reopened.snapshot().unwrap().spaces[0].id, source_space);
        drop(reopened);

        // The automatic backup still holds the database that was replaced.
        let previous = Store::open(&safety).unwrap();
        assert_eq!(previous.snapshot().unwrap().spaces[0].id, current_space);

        drop(previous);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_creates_a_missing_database() {
        let directory = temp_dir("missing");
        let database = directory.join("workspace.sqlite3");
        let source = directory.join("source.sqlite3");
        let (source_space, _) = create_database(&source, "source");

        let report = restore_database(&database, &source).unwrap();
        assert!(report.safety_backup.is_none());

        let restored = Store::open(&database).unwrap();
        assert_eq!(restored.snapshot().unwrap().spaces[0].id, source_space);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&database).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        drop(restored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_rejects_corrupt_or_foreign_sources_without_touching_the_database() {
        let directory = temp_dir("restore-reject");
        let database = directory.join("workspace.sqlite3");
        let (current_space, _) = create_database(&database, "current");

        let corrupt = directory.join("corrupt.sqlite3");
        fs::write(&corrupt, b"not sqlite").unwrap();
        assert!(restore_database(&database, &corrupt).is_err());

        let foreign = directory.join("foreign.sqlite3");
        {
            let connection = Connection::open(&foreign).unwrap();
            connection
                .execute_batch("PRAGMA user_version=1; CREATE TABLE notes(id INTEGER);")
                .unwrap();
        }
        assert!(restore_database(&database, &foreign).is_err());

        let store = Store::open(&database).unwrap();
        assert_eq!(store.snapshot().unwrap().spaces[0].id, current_space);
        drop(store);

        let pre_restore: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".pre-restore-")
            })
            .collect();
        assert!(pre_restore.is_empty());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_refuses_a_database_held_by_an_active_writer() {
        let directory = temp_dir("busy");
        let database = directory.join("workspace.sqlite3");
        let source = directory.join("source.sqlite3");
        create_database(&source, "source");
        create_database(&database, "current");

        let writer = Connection::open(&database).unwrap();
        writer.busy_timeout(Duration::from_millis(0)).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();

        let error = restore_database(&database, &source).unwrap_err();
        assert!(
            matches!(error, StoreError::Busy(_)),
            "unexpected: {error:?}"
        );

        writer.execute_batch("ROLLBACK;").unwrap();
        drop(writer);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_rolls_back_when_installed_snapshot_fails_verification() {
        let directory = temp_dir("rollback");
        let database = directory.join("workspace.sqlite3");
        let (current_space, _) = create_database(&database, "current");
        let safety = directory.join("safety.sqlite3");
        backup_database(&database, &safety, false).unwrap();

        // Bypass the pre-validation deliberately: this exercises the
        // post-replace verification and rollback path.
        let corrupt = directory.join("corrupt.sqlite3");
        fs::write(&corrupt, b"this will not verify").unwrap();
        let error = install_verified_snapshot(&database, &corrupt, Some(&safety)).unwrap_err();
        assert!(
            error.to_string().contains("restored"),
            "unexpected error: {error}"
        );

        let restored = Store::open(&database).unwrap();
        assert_eq!(restored.snapshot().unwrap().spaces[0].id, current_space);

        drop(restored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_recovers_a_corrupt_database_with_a_raw_safety_copy() {
        let directory = temp_dir("corrupt-current");
        let database = directory.join("workspace.sqlite3");
        let source = directory.join("source.sqlite3");
        let (source_space, _) = create_database(&source, "source");
        fs::write(&database, b"not a database").unwrap();

        let report = restore_database(&database, &source).unwrap();
        let safety = report
            .safety_backup
            .expect("the corrupt file should be preserved");
        assert!(!report.safety_backup_verified);
        assert_eq!(fs::read(&safety).unwrap(), b"not a database");

        let restored = Store::open(&database).unwrap();
        assert_eq!(restored.snapshot().unwrap().spaces[0].id, source_space);

        drop(restored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_accepts_v1_and_the_host_migrates_it_on_open() {
        let directory = temp_dir("v1");
        let source = directory.join("v1.sqlite3");
        let database = directory.join("workspace.sqlite3");
        let (space_id, _) = create_database(&source, "legacy");

        {
            let connection = Connection::open(&source).unwrap();
            for table in ["spaces", "sessions", "agent_runs", "session_layouts"] {
                for suffix in ["insert", "update", "delete"] {
                    connection
                        .execute_batch(&format!("DROP TRIGGER ukk_rev_{table}_{suffix};"))
                        .unwrap();
                }
            }
            connection
                .execute_batch("DROP TABLE workspace_revision; PRAGMA user_version=1;")
                .unwrap();
        }

        let report = restore_database(&database, &source).unwrap();
        assert_eq!(report.schema_version, 1);

        // Restore itself does not migrate; the next normal Host open does.
        let restored = Store::open(&database).unwrap();
        assert_eq!(
            schema_version(&restored.conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(restored.snapshot().unwrap().spaces[0].id, space_id);

        drop(restored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn db_doctor_reports_state_without_dumping_rows() {
        let directory = temp_dir("doctor");
        let database = directory.join("workspace.sqlite3");
        create_database(&database, "doctor");

        let health = inspect_database(&database);
        assert!(health.exists);
        assert!(health.is_regular);
        assert_eq!(health.schema_version, Some(CURRENT_SCHEMA_VERSION));
        assert!(health.supported_schema);
        assert!(health.has_core_tables);
        assert_eq!(health.integrity.as_deref(), Some("ok"));
        assert_eq!(health.journal_mode.as_deref(), Some("wal"));

        let missing = inspect_database(&directory.join("missing.sqlite3"));
        assert!(!missing.exists);

        let corrupt = directory.join("corrupt.sqlite3");
        fs::write(&corrupt, b"not a database").unwrap();
        let corrupt_health = inspect_database(&corrupt);
        assert!(corrupt_health.exists);
        assert!(
            corrupt_health
                .integrity
                .as_deref()
                .map(|value| value != "ok")
                .unwrap_or(true)
                || corrupt_health.open_error.is_some()
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_source_must_exist() {
        let directory = temp_dir("missing-source");
        let missing = directory.join("missing.sqlite3");
        let destination = directory.join("snapshot.sqlite3");
        assert!(matches!(
            backup_database(&missing, &destination, false),
            Err(StoreError::NotFound(_))
        ));
        assert!(!destination.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_source_and_database_must_differ() {
        let directory = temp_dir("same");
        let database = directory.join("workspace.sqlite3");
        create_database(&database, "same");
        assert!(matches!(
            restore_database(&database, &database),
            Err(StoreError::UnsafePath(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
