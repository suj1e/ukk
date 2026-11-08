//! Explicit CLI commands, not a public remote API.
use super::plugin_control::PluginControlAuth;
use super::runtime_auth::AgentControlAuth;
use super::*;
use std::path::Path;
use ukk_store::backup::{backup_database, inspect_database, restore_database, DatabaseHealth};

pub(super) fn execute() -> AppResult<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "doctor" => {
            let response = HcClient::default().call("terminal.list", json!({}))?;
            let count = response
                .get("terminals")
                .and_then(Value::as_array)
                .map(Vec::len)
                .ok_or("hc returned no terminals array")?;
            println!("Connected to herdr-core. {count} visible terminals.");
            Ok(())
        }
        "list" => {
            let result = HcClient::default().call("terminal.list", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "schema" => {
            let result = HcClient::default().call("rpc.discover", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "watch" => HcClient::default()
            .watch(|event| {
                println!("{event}");
                std::io::stdout().flush()?;
                Ok(())
            })
            .map_err(Into::into),
        "agent-control-enable" => {
            let path = AgentControlAuth::default_path();
            match AgentControlAuth::provision(&path) {
                Ok(path) => {
                    println!("Agent control capability created at {}", path.display());
                    println!("Mutating runtime RPC now requires this token; view close never stops an Agent.");
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                    "agent-control capability already exists at {}; remove it deliberately with agent-control-disable",
                    path.display()
                )
                .into()),
                Err(error) => Err(error.into()),
            }
        }
        "agent-control-disable" => {
            AgentControlAuth::remove(AgentControlAuth::default_path())?;
            println!("Agent control capability removed; runtime mutations are disabled.");
            Ok(())
        }
        "agent-control-status" => {
            let auth = AgentControlAuth::from_env();
            println!("enabled: {}", auth.is_enabled());
            println!("path: {}", auth.path().display());
            if let Some(problem) = auth.problem() {
                println!("problem: {problem}");
            }
            Ok(())
        }
        "plugin-control-enable" => {
            let path = PluginControlAuth::default_path();
            match PluginControlAuth::provision(&path) {
                Ok(path) => {
                    println!("Plugin control capability created at {}", path.display());
                    println!(
                        "plugin.enable/disable/invoke now require this separate token; \
                         plugin.snapshot stays read-only."
                    );
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                    "plugin-control capability already exists at {}; remove it deliberately with plugin-control-disable",
                    path.display()
                )
                .into()),
                Err(error) => Err(error.into()),
            }
        }
        "plugin-control-disable" => {
            PluginControlAuth::remove(PluginControlAuth::default_path())?;
            println!("Plugin control capability removed; plugin mutations are disabled.");
            Ok(())
        }
        "plugin-control-status" => {
            let auth = PluginControlAuth::from_env();
            println!("enabled: {}", auth.is_enabled());
            println!("path: {}", auth.path().display());
            if let Some(problem) = auth.problem() {
                println!("problem: {problem}");
            }
            Ok(())
        }
        "workspace-dump" => {
            let snapshot = Store::open_default()?.snapshot()?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        "backup" => {
            let mut destination = None;
            let mut force = false;
            for argument in env::args().skip(2) {
                match argument.as_str() {
                    "--force" => force = true,
                    other if other.starts_with('-') => {
                        return Err(format!("unknown backup option: {other}").into())
                    }
                    other => {
                        if destination.is_some() {
                            return Err("backup takes exactly one destination path".into());
                        }
                        destination = Some(other.to_owned());
                    }
                }
            }
            let destination = destination.ok_or("backup <path> [--force]")?;
            let database = ukk_store::default_database_path()?;
            let report = backup_database(&database, Path::new(&destination), force)?;
            println!("Backup written: {}", report.destination.display());
            println!("  source: {}", report.source.display());
            println!("  schema: v{}", report.schema_version);
            println!("  bytes: {}", report.bytes);
            if report.overwritten {
                println!("  replaced an existing destination (--force)");
            }
            println!("Snapshot verified with PRAGMA integrity_check; mode 0600 on Unix.");
            Ok(())
        }
        "restore" => {
            let mut args = env::args().skip(2);
            let source = args.next().ok_or("restore <path>")?;
            if args.next().is_some() {
                return Err("restore takes exactly one path".into());
            }
            let database = ukk_store::default_database_path()?;
            // Hold the Host lifecycle lock across validation and replacement so
            // a Host cannot start halfway through a restore. On Windows the
            // guard is a liveness probe with no value to bind.
            #[cfg(unix)]
            let _guard = acquire_restore_guard()?;
            #[cfg(not(unix))]
            acquire_restore_guard()?;
            let report = restore_database(&database, Path::new(&source))?;
            println!("Restored: {}", report.database.display());
            println!("  source: {}", report.source.display());
            println!("  schema: v{}", report.schema_version);
            println!("  bytes: {}", report.bytes);
            match &report.safety_backup {
                Some(path) => {
                    println!("  previous database backup: {}", path.display());
                    println!(
                        "  previous backup verified: {}",
                        if report.safety_backup_verified {
                            "yes"
                        } else {
                            "raw copy"
                        }
                    );
                }
                None => println!("  previous database: none (new database installed)"),
            }
            println!("The Host migrates an older schema on its next open.");
            Ok(())
        }
        "db-doctor" => {
            let database = ukk_store::default_database_path()?;
            let health = inspect_database(&database);
            print_database_health(&health);
            Ok(())
        }
        "space-create" => {
            let mut args = env::args().skip(2);
            let title = args.next().ok_or("space-create <title> <project_root>")?;
            let root = args.next().ok_or("space-create <title> <project_root>")?;
            let space = Store::open_default()?.create_space(&title, &root)?;
            println!("{}", serde_json::to_string_pretty(&space)?);
            Ok(())
        }
        "session-create" => {
            let mut args = env::args().skip(2);
            let space_id = args.next().ok_or("session-create <space_id> <title>")?;
            let title = args.next().ok_or("session-create <space_id> <title>")?;
            let session = Store::open_default()?.create_session(&space_id, &title)?;
            println!("{}", serde_json::to_string_pretty(&session)?);
            Ok(())
        }
        "session-archive" => {
            let id = env::args().nth(2).ok_or("session-archive <session_id>")?;
            Store::open_default()?.archive_session(&id)?;
            println!("Session archived; all herdr-core agents continue running.");
            Ok(())
        }
        "agent-bind" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or(
                "agent-bind <session_id> <core_id> <terminal_address> <adapter> [parent_run_id]",
            )?;
            let core_id = args.next().ok_or("missing herdr-core identity")?;
            let address = args.next().ok_or("missing hc terminal address")?;
            let adapter = args.next().ok_or("missing agent adapter")?;
            let parent_id = args.next();
            let run = Store::open_default()?.bind_agent(
                &session_id,
                &core_id,
                &address,
                &adapter,
                parent_id.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&run)?);
            Ok(())
        }
        "pane-split" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-split <session_id> <pane_id> <horizontal|vertical> <activity|files|diff|agent|terminal> [agent_run_id]")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let axis = match args.next().as_deref() {
                Some("horizontal") => SplitAxis::Horizontal,
                Some("vertical") => SplitAxis::Vertical,
                _ => return Err("axis must be horizontal or vertical".into()),
            };
            let content = match args.next().as_deref() {
                Some("activity") => PaneContent::Activity,
                Some("files") => PaneContent::Files,
                Some("diff") => PaneContent::Diff,
                Some("agent") => PaneContent::Agent {
                    agent_run_id: args.next().ok_or("agent pane requires agent_run_id")?,
                },
                Some("terminal") => PaneContent::Terminal {
                    agent_run_id: args.next().ok_or("terminal pane requires agent_run_id")?,
                },
                _ => return Err("unknown pane surface".into()),
            };
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.split_leaf(&pane_id, axis, 0.5, content)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        "pane-close" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-close <session_id> <pane_id>")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.close_leaf(&pane_id)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        "install" => lifecycle::install(),
        "uninstall" => lifecycle::uninstall(),
        "restart" => lifecycle::restart(),
        "status" => lifecycle::status(),
        "cleanup" => lifecycle::cleanup(),
        #[cfg(unix)]
        "serve" => serve(),
        #[cfg(windows)]
        "serve" => serve_windows(),
        #[cfg(windows)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request_windows(&method)
        }
        #[cfg(unix)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request(&method)
        }
        "version" | "--version" | "-V" => {
            println!("{}", build_info::version_line());
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}").into()),
    }
}

#[cfg(unix)]
fn acquire_restore_guard() -> AppResult<Option<local_fs::HostLock>> {
    let socket = transport::socket_path()?;
    let directory = socket.parent().ok_or("invalid Host socket path")?;
    let default_location = env::var_os("UKK_SOCKET").is_none();
    if !directory.exists() && !default_location {
        // A missing explicit socket directory cannot contain a live Host.
        return Ok(None);
    }
    local_fs::secure_socket_directory(directory, default_location).map_err(
        |error| -> Box<dyn Error + Send + Sync> {
            format!("Host endpoint check failed: {error}").into()
        },
    )?;
    // The Host holds this exclusive lock for its whole serving lifetime. If we
    // acquire it, no Host can start until the restore has finished.
    let lock = local_fs::HostLock::acquire(directory).map_err(
        |error| -> Box<dyn Error + Send + Sync> {
            format!("another ukk Host appears to be running; stop it before restore ({error})")
                .into()
        },
    )?;
    Ok(Some(lock))
}

#[cfg(windows)]
fn acquire_restore_guard() -> AppResult<()> {
    match ukk_host_client::windows_pipe::open_client() {
        Ok(_) => Err("another ukk Host is running; stop it before restore".into()),
        Err(_) => Ok(()),
    }
}

#[cfg(not(any(unix, windows)))]
fn acquire_restore_guard() -> AppResult<()> {
    Ok(())
}

fn print_database_health(health: &DatabaseHealth) {
    println!("ukk workspace database");
    println!("  path: {}", health.path.display());
    if !health.exists {
        println!("  state: missing (the Host creates it on next start)");
        println!("  guidance: after creating workspace data, run `ukk-host backup <path>`.");
        return;
    }
    println!(
        "  state: {}",
        if health.is_symlink {
            "symlink (refused)"
        } else if health.is_regular {
            "regular file"
        } else {
            "not a regular file (refused)"
        }
    );
    println!("  bytes: {}", health.bytes);
    #[cfg(unix)]
    if let Some(mode) = health.unix_mode {
        println!("  mode: {mode:04o}");
    }
    match health.schema_version {
        Some(version) => println!(
            "  schema: v{version} ({})",
            if health.supported_schema {
                "supported"
            } else {
                "unsupported"
            }
        ),
        None => println!("  schema: unknown"),
    }
    match &health.integrity {
        Some(result) => println!("  integrity: {result}"),
        None => println!("  integrity: unavailable"),
    }
    match &health.journal_mode {
        Some(mode) => println!("  journal mode: {mode}"),
        None => println!("  journal mode: unknown"),
    }
    println!(
        "  sidecars: wal={} bytes, shm={} bytes, journal={} bytes",
        health.wal_bytes, health.shm_bytes, health.journal_bytes
    );
    if let Some(error) = &health.open_error {
        println!("  problem: {error}");
    }
    if health.open_error.is_some() || health.integrity.as_deref() != Some("ok") {
        println!("  recovery: restore a verified backup with `ukk-host restore <path>`.");
        println!(
            "            Restore backs up the current file automatically before replacing it."
        );
    } else if !health.supported_schema {
        if health
            .schema_version
            .map(|version| version > ukk_store::backup::CURRENT_SCHEMA_VERSION)
            .unwrap_or(false)
        {
            println!(
                "  recovery: the schema is newer than this Host; upgrade the Host, do not downgrade."
            );
        } else {
            println!(
                "  recovery: not a supported ukk workspace database; restore a verified backup or start the Host to create a new one."
            );
        }
    } else {
        println!("  backup: `ukk-host backup <path>` is safe while the Host is running.");
    }
    if health.wal_bytes > 0 {
        println!(
            "  note: WAL contains committed frames not yet checkpointed; a backup includes them."
        );
    }
    if health.journal_bytes > 0 {
        println!("  note: a rollback journal is present; an interrupted write may be pending.");
    }
}

fn print_help() {
    println!(
        r#"ukk-host — Agent runtime is read-only via this local RPC
Commands:
  version              Print build version, commit, profile and target
  doctor               Test hc connect + terminal.list
  list                 Print hc terminal snapshot
  schema               Print hc OpenRPC schema
  watch                Watch hc event notifications
  workspace-dump       Dump persisted Space/Session/Pane JSON
Database safety (Host metadata only; never prints rows):
  backup <path> [--force]  Consistent verified snapshot; refuses overwrite unless forced
  restore <path>       Stop the Host first; auto-backs up current DB; atomic replace
  db-doctor            Path, schema, integrity, WAL/journal state and backup guidance
  space-create         <title> <project_root>
  session-create       <space_id> <title>
  session-archive      <session_id> (never stops Agent)
  agent-bind           <session_id> <core_id> <address> <adapter> [parent_id]
  pane-split           <session_id> <pane_id> <horizontal|vertical> <surface> [agent_run_id]
  pane-close           <session_id> <pane_id> (never stops Agent)
  agent-control-enable Create the separate agent-control token (mutations only)
  agent-control-disable Remove that token; runtime mutations become disabled
  agent-control-status Show whether agent control is enabled
  plugin-control-enable Create the separate plugin-control token (plugin mutations only)
  plugin-control-disable Remove that token; plugin mutations become disabled
  plugin-control-status Show whether plugin control is enabled
  serve                Start private local Unix socket / Named Pipe
  request [method]     Read local Host RPC (default host.ping)
Lifecycle (explicit; `serve` never installs anything):
  install              Register the current-user Host service (launchd/systemd --user/logon task)
  uninstall            Remove it (herdr-core Agents keep running)
  restart              Restart the Host only (Agents keep running)
  status               Service, endpoint, permission and stale-socket diagnostics
  cleanup              Remove a stale Unix socket only after proving no live Host
Local-only RPC metadata editing:
  workspace.session.create; workspace.pane.split/close/resize
  Requires rotated per-Host capability; never enables Agent commands.
Runtime control (separate capability file, disabled until created):
  runtime.observe/attach/send/interrupt/stop/detach/archive-finished
  Mutations require request_id, intent=user and the agent-control token;
  closing a view or detaching never stops a herdr-core process.
Environment:
  UKK_HC_BIN, UKK_DB_PATH; UKK_SOCKET on macOS/Linux only"#
    );
}
