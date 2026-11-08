//! Unix-only filesystem safety for the private Host endpoint.
//!
//! Invariants:
//! * The socket lives in a directory owned by the Host user with mode 0700.
//! * The socket itself is mode 0600 and owned by the Host user.
//! * A stale socket is removed only after a connect probe proves no live Host
//!   is listening, and only while a lifecycle `flock` excludes concurrent
//!   Host startup. A live socket, symlink or unrelated file is never unlinked.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Exclusive lifetime lock held for as long as this Host is serving.
pub(super) struct HostLock {
    _file: File,
}

impl HostLock {
    /// Acquire the per-user lifecycle lock. `flock` is released by the kernel
    /// when the process exits, including after SIGTERM or a crash, so a stale
    /// lock file never blocks recovery.
    pub(super) fn acquire(directory: &Path) -> Result<Self, String> {
        let path = directory.join("agent-host.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            // Never follow a planted symlink at the lock path.
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        // SAFETY: valid owned file descriptor; LOCK_EX|LOCK_NB is documented.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let detail = io::Error::last_os_error();
            return Err(format!(
                "another ukk Host process holds the lifecycle lock at {} ({detail}); \
                 refusing to start a second Host or to unlink its socket",
                path.display()
            ));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not protect {}: {error}", path.display()))?;
        Ok(Self { _file: file })
    }
}

/// Create (when needed) and verify the private socket directory.
///
/// The default `~/.ukk/run` directory is forced to 0700. For an explicit
/// `UKK_SOCKET` override the directory is verified but never loosened or
/// tightened silently: an override must already be user-owned and private.
pub(super) fn secure_socket_directory(
    directory: &Path,
    default_location: bool,
) -> Result<(), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    // Validate the existing object *before* changing permissions: never chmod
    // a directory that is a symlink or owned by a different user.
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "socket directory {} must be a real directory, not a symlink or file",
            directory.display()
        ));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let expected = unsafe { libc::geteuid() };
    if metadata.uid() != expected {
        return Err(format!(
            "refusing socket directory {}: owned by uid {}, but the Host runs as uid {expected} \
             (permissions were not changed)",
            directory.display(),
            metadata.uid()
        ));
    }
    if default_location {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not protect {}: {error}", directory.display()))?;
    }
    // Re-read after the chmod so the verified mode is the mode used for bind.
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "socket directory {} changed while being verified; refusing to use it",
            directory.display()
        ));
    }
    verify_owner_and_private(directory, &metadata)?;
    Ok(())
}

fn verify_owner_and_private(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let expected = unsafe { libc::geteuid() };
    if metadata.uid() != expected {
        return Err(format!(
            "refusing socket directory {}: owned by uid {}, but the Host runs as uid {expected}",
            path.display(),
            metadata.uid()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "refusing insecure socket directory {}: mode {mode:04o} grants group/other access; \
             expected 0700 (group/other bits must all be clear)",
            path.display()
        ));
    }
    Ok(())
}

/// Verify the bound socket is a user-owned 0600 socket, not a symlink or file.
pub(super) fn verify_socket_permissions(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect socket {}: {error}", path.display()))?;
    if !metadata.file_type().is_socket() {
        return Err(format!("{} is not a Unix socket", path.display()));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let expected = unsafe { libc::geteuid() };
    if metadata.uid() != expected {
        return Err(format!(
            "refusing socket {}: owned by uid {}, expected {expected}",
            path.display(),
            metadata.uid()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "refusing socket {}: mode {mode:04o}; expected 0600",
            path.display()
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SocketState {
    /// Nothing exists at the path.
    Missing,
    /// A Host accepted the liveness probe.
    Live,
    /// A socket file exists but no process accepted the probe.
    Stale,
    /// Something exists that is not a Unix socket (including a symlink).
    Foreign,
}

/// Probe the endpoint without mutating it. A successful connect proves a live
/// listener; a failed connect to a socket file is treated as stale only once
/// the caller also holds the lifecycle lock.
pub(super) fn socket_state(path: &Path) -> io::Result<SocketState> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SocketState::Missing),
        Err(error) => Err(error),
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(SocketState::Foreign),
        Ok(metadata) if metadata.file_type().is_socket() => match UnixStream::connect(path) {
            Ok(_) => Ok(SocketState::Live),
            // Only a refused connection proves that a socket exists with no
            // listener. EACCES, EMFILE, EAGAIN and other failures are
            // ambiguous and must never authorize an unlink.
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                Ok(SocketState::Stale)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SocketState::Missing),
            Err(error) => Err(error),
        },
        Ok(_) => Ok(SocketState::Foreign),
    }
}

/// Remove a stale socket, but only after the lifecycle lock is held and the
/// path is still a socket with no live listener. Never removes anything else.
/// Returns whether a file was actually removed.
pub(super) fn remove_stale_socket(path: &Path) -> Result<bool, String> {
    match socket_state(path) {
        Ok(SocketState::Missing) => Ok(false),
        Ok(SocketState::Live) => Err(format!(
            "another ukk Host is already listening on {}; refusing to replace it",
            path.display()
        )),
        Ok(SocketState::Foreign) => Err(format!(
            "{} exists but is not a Unix socket; refusing to remove it",
            path.display()
        )),
        Ok(SocketState::Stale) => {
            // Re-check immediately before unlinking so a path swap cannot
            // redirect the unlink to a file or symlink.
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_socket() => {
                    // Fail closed unless the object we are about to unlink is
                    // still our own 0600 socket, not a swapped file/symlink.
                    verify_socket_permissions(path)?;
                    fs::remove_file(path).map_err(|error| {
                        format!("could not remove stale socket {}: {error}", path.display())
                    })?;
                    Ok(true)
                }
                Ok(_) => Err(format!(
                    "{} changed while recovering the stale socket; refusing to remove it",
                    path.display()
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(format!(
                    "could not re-check stale socket {}: {error}",
                    path.display()
                )),
            }
        }
        Err(error) => Err(format!(
            "could not inspect socket {}: {error}",
            path.display()
        )),
    }
}

/// Explicit operator recovery: prove no live Host, then remove a stale socket.
pub(super) fn cleanup_stale_socket(path: &Path) -> Result<String, String> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("invalid socket path {}", path.display()))?;
    if !directory.exists() {
        return Ok(format!(
            "endpoint directory {} does not exist; nothing to clean up",
            directory.display()
        ));
    }
    // Cleanup must never unlink inside an insecure or foreign-owned directory.
    let default_location = env::var_os("UKK_SOCKET").is_none();
    secure_socket_directory(directory, default_location)?;
    let _lock = HostLock::acquire(directory)?;
    match socket_state(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?
    {
        SocketState::Missing => Ok(format!(
            "no socket at {}; nothing to clean up",
            path.display()
        )),
        SocketState::Live => Err(format!(
            "a live ukk Host is listening on {}; refusing to remove its socket",
            path.display()
        )),
        SocketState::Foreign => Err(format!(
            "{} exists but is not a Unix socket; refusing to remove it",
            path.display()
        )),
        SocketState::Stale => {
            remove_stale_socket(path)?;
            Ok(format!(
                "removed stale socket {} after confirming no live Host; \
                 herdr-core Agents were not signalled",
                path.display()
            ))
        }
    }
}

/// Human-readable, non-secret diagnostics for `ukk-host status`.
pub(super) fn describe_endpoint(path: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let directory = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => lines.push(format!(
            "  directory: {} (mode {:04o}, uid {})",
            directory.display(),
            metadata.permissions().mode() & 0o777,
            metadata.uid()
        )),
        Err(error) => lines.push(format!("  directory: {} ({error})", directory.display())),
    }
    match socket_state(path) {
        Ok(SocketState::Missing) => lines.push(format!(
            "  socket: {} (missing; Host is not running)",
            path.display()
        )),
        Ok(SocketState::Live) => lines.push(format!(
            "  socket: {} (live Host accepted a same-user connect probe)",
            path.display()
        )),
        Ok(SocketState::Stale) => lines.push(format!(
            "  socket: {} (stale: no live Host; `ukk-host serve` or `ukk-host cleanup` recovers it)",
            path.display()
        )),
        Ok(SocketState::Foreign) => lines.push(format!(
            "  socket: {} (not a Unix socket; recovery refuses to touch it)",
            path.display()
        )),
        Err(error) => lines.push(format!("  socket: {} ({error})", path.display())),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    /// Another test spawning a child can briefly inherit this listener's fd
    /// between `fork` and `exec`, making a dropped socket look live for a few
    /// milliseconds. `remove_stale_socket` correctly refuses to unlink in that
    /// window, so the test retries until the kernel reports refused instead of
    /// asserting on a scheduling race.
    fn wait_until_stale(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = socket_state(path).unwrap();
            if state == SocketState::Stale {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "socket did not become stale: {state:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn private_temp_dir(_tag: &str) -> PathBuf {
        // Keep the total Unix socket path below SUN_LEN; macOS temp dirs are
        // already long.
        let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!("ukkfs-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn rejects_group_or_other_accessible_socket_directory() {
        let directory = private_temp_dir("loose");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let error = secure_socket_directory(&directory, false).unwrap_err();
        assert!(error.contains("0755"), "{error}");
        assert!(error.contains("0700"), "{error}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn accepts_private_socket_directory() {
        let directory = private_temp_dir("private");
        secure_socket_directory(&directory, false).unwrap();
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_socket_with_loosened_permissions() {
        let directory = private_temp_dir("socket-mode");
        let path = directory.join("agent-host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        verify_socket_permissions(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let error = verify_socket_permissions(&path).unwrap_err();
        assert!(error.contains("0666"), "{error}");
        drop(listener);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn stale_socket_is_replaced_only_without_a_live_listener() {
        let directory = private_temp_dir("stale");
        let path = directory.join("agent-host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(socket_state(&path).unwrap(), SocketState::Live);
        let live_error = remove_stale_socket(&path).unwrap_err();
        assert!(live_error.contains("already listening"), "{live_error}");
        assert!(path.exists(), "a live socket must never be unlinked");
        drop(listener);
        wait_until_stale(&path);
        assert!(remove_stale_socket(&path).unwrap());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn stale_socket_with_loose_permissions_is_not_removed() {
        let directory = private_temp_dir("stale-mode");
        let path = directory.join("agent-host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        drop(listener);
        wait_until_stale(&path);
        assert_eq!(socket_state(&path).unwrap(), SocketState::Stale);
        let error = remove_stale_socket(&path).unwrap_err();
        assert!(error.contains("0600"), "{error}");
        assert!(
            path.exists(),
            "a non-private stale socket must not be unlinked"
        );
        let cleanup_error = cleanup_stale_socket(&path).unwrap_err();
        assert!(cleanup_error.contains("0600"), "{cleanup_error}");
        assert!(path.exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn foreign_file_is_never_removed() {
        let directory = private_temp_dir("foreign");
        let path = directory.join("agent-host.sock");
        fs::write(&path, b"do not delete").unwrap();
        assert_eq!(socket_state(&path).unwrap(), SocketState::Foreign);
        let error = remove_stale_socket(&path).unwrap_err();
        assert!(error.contains("not a Unix socket"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"do not delete");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn cleanup_refuses_a_live_host() {
        let directory = private_temp_dir("cleanup-live");
        let path = directory.join("agent-host.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let error = cleanup_stale_socket(&path).unwrap_err();
        assert!(error.contains("live ukk Host"), "{error}");
        assert!(path.exists());
        let _ = fs::remove_dir_all(&directory);
    }
}
