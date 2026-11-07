//! Explicit CLI commands, not a public remote API.
use super::*;

pub(super) fn execute() -> AppResult<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "doctor" => {
            let response = HcClient::default().call("terminal.list", json!({}))?;
            let count = response
                .get("terminals")
                .and_then(Value::as_array)
                .map(Vec::len)
                .ok_or("hc returned no terminals array")?;
            println!("Connected to herdr-core. {count} visible terminals.");
            Ok(())
        }
        "list" => {
            let result = HcClient::default().call("terminal.list", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "schema" => {
            let result = HcClient::default().call("rpc.discover", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "watch" => HcClient::default()
            .watch(|event| {
                println!("{event}");
                std::io::stdout().flush()?;
                Ok(())
            })
            .map_err(Into::into),
        "workspace-dump" => {
            let snapshot = Store::open_default()?.snapshot()?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        "space-create" => {
            let mut args = env::args().skip(2);
            let title = args.next().ok_or("space-create <title> <project_root>")?;
            let root = args.next().ok_or("space-create <title> <project_root>")?;
            let space = Store::open_default()?.create_space(&title, &root)?;
            println!("{}", serde_json::to_string_pretty(&space)?);
            Ok(())
        }
        "session-create" => {
            let mut args = env::args().skip(2);
            let space_id = args.next().ok_or("session-create <space_id> <title>")?;
            let title = args.next().ok_or("session-create <space_id> <title>")?;
            let session = Store::open_default()?.create_session(&space_id, &title)?;
            println!("{}", serde_json::to_string_pretty(&session)?);
            Ok(())
        }
        "session-archive" => {
            let id = env::args().nth(2).ok_or("session-archive <session_id>")?;
            Store::open_default()?.archive_session(&id)?;
            println!("Session archived; all herdr-core agents continue running.");
            Ok(())
        }
        "agent-bind" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or(
                "agent-bind <session_id> <core_id> <terminal_address> <adapter> [parent_run_id]",
            )?;
            let core_id = args.next().ok_or("missing herdr-core identity")?;
            let address = args.next().ok_or("missing hc terminal address")?;
            let adapter = args.next().ok_or("missing agent adapter")?;
            let parent_id = args.next();
            let run = Store::open_default()?.bind_agent(
                &session_id,
                &core_id,
                &address,
                &adapter,
                parent_id.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&run)?);
            Ok(())
        }
        "pane-split" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-split <session_id> <pane_id> <horizontal|vertical> <activity|files|diff|agent|terminal> [agent_run_id]")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let axis = match args.next().as_deref() {
                Some("horizontal") => SplitAxis::Horizontal,
                Some("vertical") => SplitAxis::Vertical,
                _ => return Err("axis must be horizontal or vertical".into()),
            };
            let content = match args.next().as_deref() {
                Some("activity") => PaneContent::Activity,
                Some("files") => PaneContent::Files,
                Some("diff") => PaneContent::Diff,
                Some("agent") => PaneContent::Agent {
                    agent_run_id: args.next().ok_or("agent pane requires agent_run_id")?,
                },
                Some("terminal") => PaneContent::Terminal {
                    agent_run_id: args.next().ok_or("terminal pane requires agent_run_id")?,
                },
                _ => return Err("unknown pane surface".into()),
            };
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.split_leaf(&pane_id, axis, 0.5, content)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        "pane-close" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-close <session_id> <pane_id>")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.close_leaf(&pane_id)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        #[cfg(unix)]
        "serve" => serve(),
        #[cfg(windows)]
        "serve" => serve_windows(),
        #[cfg(windows)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request_windows(&method)
        }
        #[cfg(unix)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request(&method)
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}").into()),
    }
}

fn print_help() {
    println!(
        r#"ukk-host — Agent runtime is read-only via this local RPC
Commands:
  doctor               Test hc connect + terminal.list
  list                 Print hc terminal snapshot
  schema               Print hc OpenRPC schema
  watch                Watch hc event notifications
  workspace-dump       Dump persisted Space/Session/Pane JSON
  space-create         <title> <project_root>
  session-create       <space_id> <title>
  session-archive      <session_id> (never stops Agent)
  agent-bind           <session_id> <core_id> <address> <adapter> [parent_id]
  pane-split           <session_id> <pane_id> <horizontal|vertical> <surface> [agent_run_id]
  pane-close           <session_id> <pane_id> (never stops Agent)
  serve                Start private local Unix socket / Named Pipe
  request [method]     Read local Host RPC (default host.ping)
Local-only RPC metadata editing:
  workspace.session.create; workspace.pane.split/close/resize
  Requires rotated per-Host capability; never enables Agent commands.
Environment:
  UKK_HC_BIN, UKK_DB_PATH; UKK_SOCKET on macOS/Linux only"#
    );
}
