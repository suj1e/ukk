//! Stamps the Host build with release identity supplied by the packaging job.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=UKK_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=UKK_BUILD_COMMIT");

    let version = build_value("UKK_BUILD_VERSION")
        .or_else(|| env::var("CARGO_PKG_VERSION").ok())
        .unwrap_or_else(|| "0.0.0".to_owned());
    let commit = build_value("UKK_BUILD_COMMIT").unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=UKK_BUILD_VERSION={version}");
    println!("cargo:rustc-env=UKK_BUILD_COMMIT={commit}");
}

fn build_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
