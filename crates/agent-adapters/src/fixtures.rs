//! Versioned fixture corpus.
//!
//! Fixtures are checked into the crate so parse/merge tests run offline. They
//! are hand-authored from documented wire fields; `manifest.json` records the
//! source URLs and explicitly states that they are not live captures.

pub const CODEX_V2_TURN: &str = include_str!("../fixtures/codex/app_server_v2/turn.jsonl");
pub const CODEX_V2_APPROVAL: &str = include_str!("../fixtures/codex/app_server_v2/approval.jsonl");
pub const CODEX_V2_RESUME: &str = include_str!("../fixtures/codex/app_server_v2/resume.jsonl");
pub const CODEX_V2_UNSUPPORTED: &str =
    include_str!("../fixtures/codex/app_server_v2/unsupported.jsonl");
pub const CODEX_V2_MANIFEST: &str = include_str!("../fixtures/codex/app_server_v2/manifest.json");

pub const CLAUDE_STREAM_JSON_TURN: &str =
    include_str!("../fixtures/claude/stream_json_2_1/turn.jsonl");
pub const CLAUDE_STREAM_JSON_PARTIAL: &str =
    include_str!("../fixtures/claude/stream_json_2_1/partial.jsonl");
pub const CLAUDE_STREAM_JSON_PROGRESS: &str =
    include_str!("../fixtures/claude/stream_json_2_1/progress.jsonl");
pub const CLAUDE_STREAM_JSON_UNSUPPORTED: &str =
    include_str!("../fixtures/claude/stream_json_2_1/unsupported.jsonl");
pub const CLAUDE_STREAM_JSON_MANIFEST: &str =
    include_str!("../fixtures/claude/stream_json_2_1/manifest.json");

/// Non-empty JSONL lines, in file order.
pub fn lines(jsonl: &str) -> impl Iterator<Item = &str> {
    jsonl.lines().filter(|line| !line.trim().is_empty())
}

/// Parsed JSON values, in file order.
pub fn values(jsonl: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    lines(jsonl)
        .map(|line| serde_json::from_str(line).expect("checked-in fixtures must be valid JSON"))
}
