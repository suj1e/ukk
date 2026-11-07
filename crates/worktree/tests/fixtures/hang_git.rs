//! Test fixture: a fake `git` that never exits.
//!
//! Contract tests point `Worktree` at this executable to prove the timeout
//! path kills the child instead of blocking the caller. It is never installed
//! or used by product code.

fn main() {
    std::thread::sleep(std::time::Duration::from_secs(600));
}
