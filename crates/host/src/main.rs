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
mod plugin_audit;
mod plugin_control;
mod plugin_service;
#[cfg(unix)]
mod protocol_bridge;
#[cfg(unix)]
mod protocol_endpoint;
#[cfg_attr(not(unix), allow(dead_code))]
mod protocol_service;
mod rpc;
mod runtime;
mod runtime_api;
mod runtime_auth;
mod transport;
mod workspace_rpc;
mod workspace_watch;

use broker::Broker;
use local_auth::LocalAuth;
use plugin_service::PluginService;
use rpc::handle_request_with_plugins;
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
