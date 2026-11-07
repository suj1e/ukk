//! Compile-time build identity embedded by `build.rs`.

pub const NAME: &str = "ukk-desktop";
pub const VERSION: &str = env!("UKK_BUILD_VERSION");
pub const COMMIT: &str = env!("UKK_BUILD_COMMIT");

/// One-line, greppable identity used by `--version` and release smoke checks.
pub fn version_line() -> String {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    format!(
        "{NAME} version={VERSION} commit={COMMIT} profile={profile} target={}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_reports_name_version_and_commit() {
        let line = super::version_line();
        assert!(line.starts_with("ukk-desktop version="));
        assert!(line.contains(super::VERSION));
        assert!(line.contains("commit="));
        assert!(line.contains("profile="));
        assert!(line.contains("target="));
    }
}
