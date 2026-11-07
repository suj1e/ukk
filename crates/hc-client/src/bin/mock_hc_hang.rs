//! A deliberately non-responsive `hc connect` fixture.
//!
//! One-shot Host probes must bound their wait. The client writes a request,
//! this fixture reads it and then blocks until the client kills it; the test
//! asserts a typed timeout instead of an indefinitely pinned worker.

use std::io::Read;

fn main() {
    if std::env::args().nth(1).as_deref() != Some("connect") {
        std::process::exit(2);
    }
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
