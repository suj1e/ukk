//! A short-lived, same-user capability for **workspace metadata only**.
//! It is rotated each Host startup and stored beside the Unix socket or in
//! the user's LOCALAPPDATA profile on Windows.
//!
//! This is *not* remote device authentication and not a defense against a
//! malicious process with full access to the same user profile. Before any
//! Agent/tool/system mutations, require a stronger audited trust boundary.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use ukk_domain::new_id;
use ukk_host_client::HostClient;

pub(crate) struct LocalAuth {
    secret: String,
}

impl LocalAuth {
    pub fn install() -> io::Result<Self> {
        let path = HostClient::from_env().mutation_token_path();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid token file path"))?;
        fs::create_dir_all(parent)?;
        let secret = format!("{}:{}", new_id(), new_id());
        let tmp = unique_temp(&path);
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(secret.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            #[cfg(windows)]
            if path.exists() {
                // The Host created the first Named Pipe instance before this
                // is called; no second Host should rotate credentials.
                fs::remove_file(&path)?;
            }
            fs::rename(&tmp, &path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            }
            Ok::<(), io::Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        Ok(Self { secret })
    }

    fn constant_time_matches(&self, candidate: &str) -> bool {
        // Secret length is constant and not sensitive. Compare content without
        // returning early based on a matching prefix.
        let left = self.secret.as_bytes();
        let right = candidate.as_bytes();
        let mut difference = left.len() ^ right.len();
        for (&a, &b) in left.iter().zip(right.iter()) {
            difference |= (a ^ b) as usize;
        }
        difference == 0
    }

    pub(crate) fn authorized(&self, candidate: Option<&str>) -> bool {
        candidate.is_some_and(|s| self.constant_time_matches(s))
    }

    #[cfg(test)]
    pub(crate) fn test() -> Self {
        Self {
            secret: "test-capability-do-not-use-as-real-credential".to_owned(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_token(&self) -> &str {
        &self.secret
    }
}

fn unique_temp(target: &Path) -> PathBuf {
    // Keep arbitrary non-UTF8 Unix filenames and Unicode Windows profiles.
    // Sibling filename guarantees same-filesystem rename.
    let mut name = target.as_os_str().to_os_string();
    name.push(format!(".{}", new_id()));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_capability_is_accepted() {
        let auth = LocalAuth::test();
        assert!(auth.authorized(Some(auth.test_token())));
        assert!(!auth.authorized(Some("test-capability-do-not-use-as-real-credentia")));
        assert!(!auth.authorized(None));
    }
}
