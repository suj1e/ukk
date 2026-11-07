//! Test fixture: a fake `git` that records its pid and never exits.
//!
//! Contract tests point `Worktree` at this executable to prove the timeout
//! path kills the child instead of blocking the caller. The pid file is
//! written inside the `-C` directory from the arguments Git would receive.
//! It is never installed or used by product code.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "-C" {
            if let Some(directory) = args.next() {
                let directory = PathBuf::from(directory);
                let _ = std::fs::write(
                    directory.join(".ukk-hang-git.pid"),
                    std::process::id().to_string(),
                );
            }
            break;
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(600));
}
