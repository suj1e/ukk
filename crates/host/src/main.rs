//! ukk Agent Host entrypoint. OS transports, RPC authorization, workspace
//! mutations and developer CLI are separate modules.
mod broker;
mod build_info;
mod cli;
mod lifecycle;
mod local_auth;
#[cfg(unix)]
mod local_fs;
#[cfg(unix)]
mod peer;
mod rpc;
mod transport;
mod workspace_rpc;
mod workspace_watch;

use broker::Broker;
use local_auth::LocalAuth;
use rpc::handle_request;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::{
    env,
    error::Error,
    io::{Read, Write},
    sync::Arc,
};
#[cfg(unix)]
use transport::{request, serve};
#[cfg(windows)]
use transport::{request_windows, serve_windows};
use ukk_domain::{PaneContent, SplitAxis};
use ukk_hc_client::HcClient;
use ukk_store::Store;

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn main() {
    if let Err(error) = cli::execute() {
        eprintln!("ukk-host: {error}");
        std::process::exit(1);
    }
}
