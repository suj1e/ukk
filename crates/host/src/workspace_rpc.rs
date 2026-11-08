//! Local workspace metadata mutations. Agent tool execution, approval,
//! terminal input, kill and screen control are intentionally NOT exposed.
//!
//! Requests are validated and use durable SQLite compare-and-swap. The caller
//! must first pass the local mutation capability at the transport boundary.

#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use ukk_domain::{PaneContent, SplitAxis};
use ukk_store::{Store, StoreError};

#[derive(Debug)]
pub(crate) struct RpcError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl RpcError {
    fn invalid(field: &str) -> Self {
        Self {
            code: -32602,
            message: format!("missing or invalid {field}"),
        }
    }
}

impl From<StoreError> for RpcError {
    fn from(error: StoreError) -> Self {
        let code = match error {
            StoreError::Conflict => -32031,
            StoreError::Invalid(_) | StoreError::Layout(_) => -32602,
            StoreError::NotFound(_) => -32034,
            _ => -32032,
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

fn str_arg<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RpcError::invalid(key))
}

/// A duplicate `request_id` with different Space inputs is a genuine conflict,
/// not a layout revision mismatch, so it must not be reported with the layout
/// reload wording (the Desktop relies on that wording for pane retries).
fn space_mutation_error(error: StoreError) -> RpcError {
    match error {
        StoreError::Conflict => RpcError {
            code: -32031,
            message: "request_id was already used with different Space details".into(),
        },
        other => other.into(),
    }
}

fn revision_arg(params: &Value) -> Result<i64, RpcError> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|v| *v >= 0)
        .ok_or_else(|| RpcError::invalid("expected_revision"))
}

fn split_axis(params: &Value) -> Result<SplitAxis, RpcError> {
    let raw = str_arg(params, "axis")?;
    match raw {
        "horizontal" => Ok(SplitAxis::Horizontal),
        "vertical" => Ok(SplitAxis::Vertical),
        _ => Err(RpcError::invalid("axis")),
    }
}

pub(crate) fn dispatch(method: &str, params: &Value) -> Result<Value, RpcError> {
    let mut store = Store::open_default()?;
    dispatch_with(&mut store, method, params)
}

fn dispatch_with(store: &mut Store, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "workspace.space.create" => {
            let space = store
                .create_space_with_id(
                    str_arg(params, "request_id")?,
                    str_arg(params, "title")?,
                    str_arg(params, "project_root")?,
                )
                .map_err(space_mutation_error)?;
            serde_json::to_value(space).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.space.rename" => {
            let space = store
                .rename_space(str_arg(params, "space_id")?, str_arg(params, "title")?)
                .map_err(space_mutation_error)?;
            serde_json::to_value(space).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.session.create" => {
            let session = store.create_session_with_id(
                str_arg(params, "space_id")?,
                str_arg(params, "title")?,
                str_arg(params, "request_id")?,
            )?;
            serde_json::to_value(session).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.pane.split" => {
            let session_id = str_arg(params, "session_id")?;
            let target = str_arg(params, "pane_id")?;
            let axis = split_axis(params)?;
            let expected = revision_arg(params)?;
            let content: PaneContent = serde_json::from_value(
                params
                    .get("content")
                    .cloned()
                    .ok_or_else(|| RpcError::invalid("content"))?,
            )
            .map_err(|_| RpcError::invalid("content"))?;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.split_leaf(target, axis, 0.5, content)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.pane.close" => {
            let session_id = str_arg(params, "session_id")?;
            let target = str_arg(params, "pane_id")?;
            let expected = revision_arg(params)?;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.close_leaf(target)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.pane.resize" => {
            let session_id = str_arg(params, "session_id")?;
            let target = str_arg(params, "split_id")?;
            let expected = revision_arg(params)?;
            let ratio = params
                .get("ratio")
                .and_then(Value::as_f64)
                .filter(|r| r.is_finite() && (0.15..=0.85).contains(r))
                .ok_or_else(|| RpcError::invalid("ratio"))? as f32;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.resize_split(target, ratio)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|e| RpcError {
                code: -32032,
                message: e.to_string(),
            })
        }
        "workspace.tab.add" => {
            let session_id = str_arg(params, "session_id")?;
            let pane_id = str_arg(params, "pane_id")?;
            let title = str_arg(params, "title")?.to_owned();
            let expected = revision_arg(params)?;
            let content: PaneContent = serde_json::from_value(
                params
                    .get("content")
                    .cloned()
                    .ok_or_else(|| RpcError::invalid("content"))?,
            )
            .map_err(|_| RpcError::invalid("content"))?;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.add_tab(pane_id, title, content)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|error| RpcError {
                code: -32032,
                message: error.to_string(),
            })
        }
        "workspace.tab.close" => {
            let session_id = str_arg(params, "session_id")?;
            let group_id = str_arg(params, "group_id")?;
            let tab_id = str_arg(params, "tab_id")?;
            let expected = revision_arg(params)?;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.close_tab(group_id, tab_id)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|error| RpcError {
                code: -32032,
                message: error.to_string(),
            })
        }
        "workspace.tab.move" => {
            let session_id = str_arg(params, "session_id")?;
            let source_group_id = str_arg(params, "source_group_id")?;
            let tab_id = str_arg(params, "tab_id")?;
            let destination_pane_id = str_arg(params, "destination_pane_id")?;
            let expected = revision_arg(params)?;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current
                .root
                .move_tab(source_group_id, tab_id, destination_pane_id)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|error| RpcError {
                code: -32032,
                message: error.to_string(),
            })
        }
        "workspace.tab.reorder" => {
            let session_id = str_arg(params, "session_id")?;
            let group_id = str_arg(params, "group_id")?;
            let tab_id = str_arg(params, "tab_id")?;
            let expected = revision_arg(params)?;
            let direction = params
                .get("direction")
                .and_then(Value::as_i64)
                .filter(|direction| matches!(direction, -1 | 1))
                .ok_or_else(|| RpcError::invalid("direction"))? as i32;
            let current = store.get_layout(session_id)?;
            if current.revision != expected {
                return Err(StoreError::Conflict.into());
            }
            let next = current.root.reorder_tab(group_id, tab_id, direction)?;
            let layout = store.set_layout(session_id, expected, next)?;
            serde_json::to_value(layout).map_err(|error| RpcError {
                code: -32032,
                message: error.to_string(),
            })
        }
        _ => Err(RpcError {
            code: -32601,
            message: "workspace method not found".into(),
        }),
    }
}

impl From<ukk_domain::LayoutError> for RpcError {
    fn from(error: ukk_domain::LayoutError) -> Self {
        StoreError::from(error).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::{new_id, PaneNode};

    #[test]
    fn space_create_is_idempotent_validated_and_conflict_typed() {
        let mut store = Store::open_memory().unwrap();
        let request_id = new_id();
        let create = |store: &mut Store, id: &str, title: &str, root: &str| {
            dispatch_with(
                store,
                "workspace.space.create",
                &json!({"request_id":id,"title":title,"project_root":root}),
            )
        };
        let created = create(&mut store, &request_id, "ukk", "/work").unwrap();
        assert_eq!(created["id"], request_id);
        assert_eq!(created["title"], "ukk");
        assert_eq!(created["project_root"], "/work");
        let repeated = create(&mut store, &request_id, "ukk", "/work").unwrap();
        assert_eq!(repeated, created);
        assert_eq!(store.snapshot().unwrap().spaces.len(), 1);

        let conflict = create(&mut store, &request_id, "Renamed", "/work").unwrap_err();
        assert_eq!(conflict.code, -32031);
        assert!(conflict.message.contains("request_id"));

        for params in [
            json!({"request_id":new_id(),"title":"   ","project_root":"/work"}),
            json!({"request_id":new_id(),"title":"x".repeat(201),"project_root":"/work"}),
            json!({"request_id":new_id(),"title":"ukk","project_root":""}),
            json!({"request_id":"not-a-uuid","title":"ukk","project_root":"/work"}),
        ] {
            let error = dispatch_with(&mut store, "workspace.space.create", &params).unwrap_err();
            assert_eq!(error.code, -32602, "{error:?}");
        }
        assert_eq!(store.snapshot().unwrap().spaces.len(), 1);
    }

    #[test]
    fn space_rename_updates_only_space_metadata_and_rejects_missing_space() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("Before", "/work").unwrap();
        let renamed = dispatch_with(
            &mut store,
            "workspace.space.rename",
            &json!({"space_id":space.id,"title":" After "}),
        )
        .unwrap();
        assert_eq!(renamed["id"], space.id);
        assert_eq!(renamed["title"], "After");
        assert_eq!(renamed["project_root"], "/work");
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.spaces[0].title, "After");
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.agent_runs.is_empty());

        let missing = dispatch_with(
            &mut store,
            "workspace.space.rename",
            &json!({"space_id":new_id(),"title":"After"}),
        )
        .unwrap_err();
        assert_eq!(missing.code, -32034);
        let invalid = dispatch_with(
            &mut store,
            "workspace.space.rename",
            &json!({"space_id":space.id,"title":"x".repeat(201)}),
        )
        .unwrap_err();
        assert_eq!(invalid.code, -32602);
    }

    #[test]
    fn local_workflow_creates_then_splits_resizes_and_closes() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("ukk", "/work").unwrap();
        let id = new_id();
        let created = dispatch_with(
            &mut store,
            "workspace.session.create",
            &json!({
                "request_id":id,"space_id":space.id,"title":"New Session"
            }),
        )
        .unwrap();
        let session_id = created["id"].as_str().unwrap();
        let first = store.get_layout(session_id).unwrap();
        let original = first.root.id().to_owned();

        let split = dispatch_with(
            &mut store,
            "workspace.pane.split",
            &json!({
                "session_id":session_id,"pane_id":original,
                "axis":"horizontal","content":{"type":"diff"},"expected_revision":0
            }),
        )
        .unwrap();
        assert_eq!(split["revision"], 1);
        let split_id = split["root"]["id"].as_str().unwrap();

        let resized = dispatch_with(
            &mut store,
            "workspace.pane.resize",
            &json!({
                "session_id":session_id,"split_id":split_id,
                "ratio":0.7,"expected_revision":1
            }),
        )
        .unwrap();
        assert_eq!(resized["revision"], 2);

        let restored: PaneNode = serde_json::from_value(resized["root"].clone()).unwrap();
        let second = restored.leaves()[1].0.to_owned();
        let closed = dispatch_with(
            &mut store,
            "workspace.pane.close",
            &json!({
                "session_id":session_id,"pane_id":second,"expected_revision":2
            }),
        )
        .unwrap();
        assert_eq!(closed["revision"], 3);
        assert_eq!(store.get_layout(session_id).unwrap().root.leaves().len(), 1);
    }

    #[test]
    fn stale_revision_is_explicit_conflict_not_silent_overwrite() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("ukk", "/work").unwrap();
        let session = store.create_session(&space.id, "Test").unwrap();
        let leaf = store.get_layout(&session.id).unwrap().root.id().to_owned();
        let action = json!({
            "session_id":session.id,"pane_id":leaf,"axis":"vertical",
            "expected_revision":0,"content":{"type":"files"}
        });
        dispatch_with(&mut store, "workspace.pane.split", &action).unwrap();
        let error = dispatch_with(&mut store, "workspace.pane.split", &action).unwrap_err();
        assert_eq!(error.code, -32031);
    }

    #[test]
    fn cannot_use_rpc_to_run_terminal_or_agent_mutations() {
        let mut store = Store::open_memory().unwrap();
        let error = dispatch_with(&mut store, "terminal.kill", &json!({})).unwrap_err();
        assert_eq!(error.code, -32601);
    }

    #[test]
    fn tab_mutations_are_revisioned_and_never_touch_agent_runtime() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("ukk", "/repo").unwrap();
        let session = store.create_session(&space.id, "Tabs").unwrap();
        let original = store.get_layout(&session.id).unwrap();
        let leaf = original.root.id().to_owned();

        let added = dispatch_with(
            &mut store,
            "workspace.tab.add",
            &json!({
                "session_id":session.id, "pane_id":leaf,
                "title":"Files", "content":{"type":"files"}, "expected_revision":0
            }),
        )
        .unwrap();
        assert_eq!(added["revision"], 1);
        let group = added["root"]["id"].as_str().unwrap().to_owned();
        let last_tab = added["root"]["tabs"][1]["id"].as_str().unwrap().to_owned();

        let stale = dispatch_with(
            &mut store,
            "workspace.tab.close",
            &json!({
                "session_id":session.id,"group_id":group,"tab_id":last_tab,
                "expected_revision":0
            }),
        )
        .unwrap_err();
        assert_eq!(stale.code, -32031);

        let reordered = dispatch_with(
            &mut store,
            "workspace.tab.reorder",
            &json!({
                "session_id":session.id,"group_id":group,"tab_id":last_tab,
                "direction":-1,"expected_revision":1
            }),
        )
        .unwrap();
        assert_eq!(reordered["revision"], 2);
        assert_eq!(reordered["root"]["tabs"][0]["id"], last_tab);

        let closed = dispatch_with(
            &mut store,
            "workspace.tab.close",
            &json!({
                "session_id":session.id,"group_id":group,"tab_id":last_tab,
                "expected_revision":2
            }),
        )
        .unwrap();
        assert_eq!(closed["revision"], 3);
        assert_eq!(closed["root"]["kind"], "leaf");
        assert!(store.snapshot().unwrap().agent_runs.is_empty());
    }

    #[test]
    fn cross_pane_move_requires_fresh_revision_and_preserves_agent_binding() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("ukk", "/repo").unwrap();
        let session = store.create_session(&space.id, "tabs").unwrap();
        let agent = store
            .bind_agent(&session.id, "core", "t1", "codex", None)
            .unwrap();
        let layout = store.get_layout(&session.id).unwrap();
        let source = layout
            .root
            .add_tab(
                layout.root.id(),
                "Agent".into(),
                PaneContent::Agent {
                    agent_run_id: agent.id.clone(),
                },
            )
            .unwrap();
        let source_id = source.id().to_owned();
        let tab_id = source.leaves()[1].0.to_owned();
        let tree = source
            .split_leaf(source.id(), SplitAxis::Horizontal, 0.5, PaneContent::Diff)
            .unwrap();
        let destination_pane_id = tree.leaves()[2].0.to_owned();
        store.set_layout(&session.id, 0, tree).unwrap();
        let request = json!({
            "session_id":session.id,
            "source_group_id":source_id,
            "destination_pane_id":destination_pane_id,
            "tab_id":tab_id,
            "expected_revision":1
        });
        let saved = dispatch_with(&mut store, "workspace.tab.move", &request).unwrap();
        assert_eq!(saved["revision"], 2);
        let restored: PaneNode = serde_json::from_value(saved["root"].clone()).unwrap();
        assert!(matches!(
            restored.leaves()[2].1,
            PaneContent::Agent { agent_run_id } if agent_run_id == &agent.id
        ));
        let old = dispatch_with(&mut store, "workspace.tab.move", &request).unwrap_err();
        assert_eq!(old.code, -32031);
        assert_eq!(store.snapshot().unwrap().agent_runs[0].id, agent.id);
    }

    #[test]
    fn invalid_or_out_of_range_ratio_is_rejected() {
        let mut store = Store::open_memory().unwrap();
        let err = dispatch_with(
            &mut store,
            "workspace.pane.resize",
            &json!({
                "split_id":"s","session_id":"x","expected_revision":0,"ratio":0.95
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
    }
}
