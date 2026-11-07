//! `plugin.control` capability: an explicit, default-off local token for
//! plugin enable/disable/invoke.
//!
//! This is deliberately **separate** from both the rotated workspace metadata
//! token (`local_auth.rs`) and the agent-control token (`runtime_auth.rs`).
//! Mutating plugin RPC accepts only the value of this file, in a distinct
//! `control` field; the workspace `auth` field is ignored. A missing file means
//! plugin control is disabled, not a startup error.
//!
//! Local-only threat model: this is same-user, same-host defense in depth. It
//! is not remote authentication and does not protect against a process that can
//! already read the user's profile. Removing the file revokes plugin control
//! immediately (the Host reloads it on every mutating request) and requires no
//! Host restart.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use ukk_domain::new_id;
#[cfg(unix)]
use ukk_host_client::HostClient;

/// Same floor as the other local capabilities: long, ASCII, one line.
const MIN_SECRET_LEN: usize = 60;
const MAX_TOKEN_BYTES: u64 = 4096;

/// A loaded (or deliberately absent) plugin-control capability.
pub(crate) struct PluginControlAuth {
    path: PathBuf,
    secret: Option<String>,
    problem: Option<String>,
}

impl std::fmt::Debug for PluginControlAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginControlAuth")
            .field("path", &self.path)
            .field("enabled", &self.secret.is_some())
            .field("problem", &self.problem)
            .finish()
    }
}

impl PluginControlAuth {
    /// `UKK_PLUGIN_CONTROL_TOKEN_FILE` overrides the platform default. The
    /// override may point anywhere the Host user can read, but the Host still
    /// requires a regular 0600 file and rejects symlinks.
    pub fn default_path() -> PathBuf {
        std::env::var_os("UKK_PLUGIN_CONTROL_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(default_token_path)
    }

    pub fn from_env() -> Self {
        Self::load(Self::default_path())
    }

    /// Missing file means "disabled". Every insecure or malformed file also
    /// leaves control disabled and records a non-secret diagnostic.
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
        if !metadata.file_type().is_file() {
            return Self {
                path,
                secret: None,
                problem: Some("capability path must be a regular file".into()),
            };
        }
        if metadata.len() > MAX_TOKEN_BYTES {
            return Self {
                path,
                secret: None,
                problem: Some("capability file is unexpectedly large".into()),
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
        let read = (|| -> io::Result<String> {
            let file = fs::File::open(&path)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("capability path is not a regular file"));
            }
            let mut secret = String::new();
            file.take(MAX_TOKEN_BYTES).read_to_string(&mut secret)?;
            Ok(secret)
        })();
        match read {
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

    pub fn is_enabled(&self) -> bool {
        self.secret.is_some()
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Constant-time comparison; length is not secret.
    pub fn authorized(&self, candidate: Option<&str>) -> bool {
        let (Some(secret), Some(candidate)) = (self.secret.as_deref(), candidate) else {
            return false;
        };
        let left = secret.as_bytes();
        let right = candidate.as_bytes();
        let mut difference = left.len() ^ right.len();
        for (&a, &b) in left.iter().zip(right.iter()) {
            difference |= (a ^ b) as usize;
        }
        difference == 0
    }

    /// Explicit operator action. Refuses to overwrite an existing token so a
    /// running Host's capability is never rotated behind its back.
    pub fn provision(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid plugin-control token path"))?;
        let _created_parent = !parent.exists();
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if _created_parent {
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

    #[cfg(test)]
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }
}

#[cfg(unix)]
fn default_token_path() -> PathBuf {
    HostClient::from_env()
        .socket_path()
        .with_extension("plugin-control")
}

#[cfg(windows)]
fn default_token_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\nonexistent\\ukk"))
        .join("ukk")
        .join("run")
        .join("agent-host.plugin-control")
}

#[cfg(not(any(unix, windows)))]
fn default_token_path() -> PathBuf {
    PathBuf::from("ukk-plugin-control-token-unsupported-platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ukk-plugin-control-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_file_is_disabled_not_an_error() {
        let path = scratch("missing").join("token");
        let auth = PluginControlAuth::load(path);
        assert!(!auth.is_enabled());
        assert!(auth.problem().is_none());
        assert!(!auth.authorized(Some("anything")));
    }

    #[test]
    fn provisioned_token_authorizes_only_itself() {
        let path = scratch("token").join("token");
        let provisioned = PluginControlAuth::provision(&path).unwrap();
        let auth = PluginControlAuth::load(&provisioned);
        assert!(auth.is_enabled());
        let secret = auth.secret().unwrap().to_owned();
        assert!(auth.authorized(Some(&secret)));
        assert!(!auth.authorized(Some("wrong")));
        assert!(!auth.authorized(None));
        assert!(!auth.authorized(Some(&secret[..secret.len() - 1])));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&provisioned).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        PluginControlAuth::remove(&provisioned).unwrap();
    }

    #[test]
    fn provisioning_refuses_to_overwrite() {
        let path = scratch("overwrite").join("token");
        PluginControlAuth::provision(&path).unwrap();
        assert_eq!(
            PluginControlAuth::provision(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_loose_permissions_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("insecure");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, format!("{}:{}", "a".repeat(40), "b".repeat(40))).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let auth = PluginControlAuth::load(&link);
        assert!(!auth.is_enabled());
        assert!(auth.problem().unwrap().contains("symlink"));

        let loose = root.join("loose");
        fs::write(&loose, format!("{}:{}", "a".repeat(40), "b".repeat(40))).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
        let auth = PluginControlAuth::load(&loose);
        assert!(!auth.is_enabled());
        assert!(auth.problem().unwrap().contains("0600"));
    }
}
