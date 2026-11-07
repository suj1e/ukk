//! Agent-control capability and audit log.
//!
//! This capability is deliberately **separate** from the workspace metadata
//! token in `local_auth.rs`. The workspace token can edit Session/Pane
//! metadata; it must never be accepted for terminal input, interrupt, stop or
//! runtime archive. The control capability is disabled unless its own file
//! exists, so upgrading ukk does not silently grant runtime control.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use ukk_domain::new_id;
use ukk_host_client::HostClient;

const MIN_SECRET_LEN: usize = 60;

/// A loaded (or deliberately absent) agent-control capability.
pub struct AgentControlAuth {
    path: PathBuf,
    secret: Option<String>,
    problem: Option<String>,
}

impl std::fmt::Debug for AgentControlAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentControlAuth")
            .field("path", &self.path)
            .field("enabled", &self.secret.is_some())
            .field("problem", &self.problem)
            .finish()
    }
}

impl AgentControlAuth {
    pub fn default_path() -> PathBuf {
        std::env::var_os("UKK_AGENT_CONTROL_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                HostClient::from_env()
                    .socket_path()
                    .with_extension("agent-control")
            })
    }

    pub fn audit_path() -> PathBuf {
        std::env::var_os("UKK_AGENT_CONTROL_AUDIT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let mut name = AgentControlAuth::default_path().into_os_string();
                name.push(".audit");
                PathBuf::from(name)
            })
    }

    pub fn from_env() -> Self {
        Self::load(Self::default_path())
    }

    /// Missing file means "disabled", not an error. Any malformed or
    /// insecurely permissioned file also leaves control disabled and records
    /// the reason for diagnostics.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Self {
                    path,
                    secret: None,
                    problem: None,
                }
            }
            Err(error) => {
                return Self {
                    path,
                    secret: None,
                    problem: Some(format!("cannot inspect capability file: {error}")),
                }
            }
        };
        if metadata.file_type().is_symlink() {
            return Self {
                path,
                secret: None,
                problem: Some("capability file must not be a symlink".into()),
            };
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Self {
                    path,
                    secret: None,
                    problem: Some("capability file permissions must be 0600".into()),
                };
            }
        }
        match fs::read_to_string(&path) {
            Ok(secret) => {
                let secret = secret.trim().to_owned();
                if secret.len() < MIN_SECRET_LEN || !secret.is_ascii() {
                    return Self {
                        path,
                        secret: None,
                        problem: Some("capability file is too short or not ASCII".into()),
                    };
                }
                Self {
                    path,
                    secret: Some(secret),
                    problem: None,
                }
            }
            Err(error) => Self {
                path,
                secret: None,
                problem: Some(format!("cannot read capability file: {error}")),
            },
        }
    }

    #[cfg(test)]
    pub fn test(secret: &str) -> Self {
        Self {
            path: PathBuf::from("/test/agent-control"),
            secret: Some(secret.to_owned()),
            problem: None,
        }
    }

    #[cfg(test)]
    pub fn disabled_test() -> Self {
        Self {
            path: PathBuf::from("/test/agent-control"),
            secret: None,
            problem: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.secret.is_some()
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    pub fn authorized(&self, candidate: Option<&str>) -> bool {
        let (Some(secret), Some(candidate)) = (self.secret.as_deref(), candidate) else {
            return false;
        };
        constant_time_matches(secret.as_bytes(), candidate.as_bytes())
    }

    /// Explicit operator action. Refuses to overwrite an existing token so a
    /// running Host's capability is never rotated behind its back.
    pub fn provision(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid agent-control token path"))?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let secret = format!("{}:{}", new_id(), new_id());
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(secret.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(path.to_path_buf())
    }

    pub fn remove(path: impl AsRef<Path>) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn constant_time_matches(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (&a, &b) in left.iter().zip(right.iter()) {
        difference |= (a ^ b) as usize;
    }
    difference == 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub at_ms: i64,
    pub request_id: String,
    pub action: String,
    pub core_id: String,
    pub terminal: String,
    pub intent: String,
    /// `ok`, `error:<code>`, or `denied:<code>`. Tokens never appear here.
    pub outcome: String,
}

pub enum AuditSink {
    #[cfg(test)]
    Memory(Mutex<Vec<AuditEntry>>),
    File {
        path: PathBuf,
        write: Mutex<()>,
    },
}

pub struct AuditLog {
    sink: AuditSink,
}

impl AuditLog {
    #[cfg(test)]
    pub fn memory() -> Self {
        Self {
            sink: AuditSink::Memory(Mutex::new(Vec::new())),
        }
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            sink: AuditSink::File {
                path: path.into(),
                write: Mutex::new(()),
            },
        }
    }

    pub fn record(&self, entry: AuditEntry) -> io::Result<()> {
        match &self.sink {
            #[cfg(test)]
            AuditSink::Memory(entries) => {
                entries
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(entry);
                Ok(())
            }
            AuditSink::File { path, write } => {
                let _guard = write.lock().unwrap_or_else(|error| error.into_inner());
                let parent = path
                    .parent()
                    .ok_or_else(|| io::Error::other("invalid audit path"))?;
                fs::create_dir_all(parent)?;
                let mut options = fs::OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(path)?;
                let value = serde_json::json!({
                    "at_ms": entry.at_ms,
                    "request_id": entry.request_id,
                    "action": entry.action,
                    "core_id": entry.core_id,
                    "terminal": entry.terminal,
                    "intent": entry.intent,
                    "outcome": entry.outcome,
                });
                writeln!(file, "{value}")?;
                file.flush()
            }
        }
    }

    /// Test-only view for the in-memory sink.
    #[cfg(test)]
    pub fn entries(&self) -> Vec<AuditEntry> {
        match &self.sink {
            AuditSink::Memory(entries) => entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            AuditSink::File { .. } => Vec::new(),
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ukk-agent-control-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_file_is_disabled_not_an_error() {
        let path = scratch("missing").join("token");
        let auth = AgentControlAuth::load(path);
        assert!(!auth.is_enabled());
        assert!(auth.problem().is_none());
        assert!(!auth.authorized(Some("anything")));
    }

    #[test]
    fn provisioned_token_authorizes_only_itself() {
        let path = scratch("token").join("token");
        let provisioned = AgentControlAuth::provision(&path).unwrap();
        let auth = AgentControlAuth::load(&provisioned);
        assert!(auth.is_enabled());
        let secret = auth.secret().unwrap().to_owned();
        assert!(auth.authorized(Some(&secret)));
        assert!(!auth.authorized(Some("wrong")));
        assert!(!auth.authorized(None));
        assert!(!auth.authorized(Some(&secret[..secret.len() - 1])));
        AgentControlAuth::remove(&provisioned).unwrap();
        let _ = fs::remove_dir_all(provisioned.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_token_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let directory = scratch("perms");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("token");
        fs::write(&path, format!("{}:{}", "a".repeat(36), "b".repeat(36))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let auth = AgentControlAuth::load(&path);
        assert!(!auth.is_enabled());
        assert!(auth.problem().unwrap().contains("0600"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn audit_entries_never_contain_the_token() {
        let log = AuditLog::memory();
        log.record(AuditEntry {
            at_ms: 1,
            request_id: "r1".into(),
            action: "runtime.stop".into(),
            core_id: "core".into(),
            terminal: "t7".into(),
            intent: "user".into(),
            outcome: "ok".into(),
        })
        .unwrap();
        let entries = log.entries();
        assert_eq!(entries.len(), 1);
        assert!(!format!("{:?}", entries[0]).contains("secret"));
    }
}
