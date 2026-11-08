//! Explicit terminal observation and one-line input for the Desktop.
//!
//! The Desktop opens `hc connect` and `terminal.attach {observe:true}` directly
//! from a background worker. This is a deliberate read boundary: ukk's Host
//! remains the owner of Agent control and terminal lifecycle, while the view
//! only consumes an observer stream. The observe path has no input, resize,
//! interrupt, stop, kill or archive call. Dropping the attachment sends CLOSE
//! for this stream and dropping the connection reaps the local `hc connect`
//! helper; the terminal program is never signalled.
//!
//! A bound Session terminal may additionally offer one explicit line of
//! input. That path does not write to the attach stream: it calls the Host's
//! `runtime.send` through [`HostClient::runtime_send`], which requires the
//! separate agent-control capability and the exact `core_id` +
//! `terminal_address` binding. There is no keystroke stream, automatic input,
//! resize, interrupt, stop, kill or detach path in the composer, and a failed
//! send is never retried automatically.
//!
//! Attach happens only after an explicit pane/detail action. Raw terminal text
//! is displayed as raw terminal text and is never parsed into Agent messages,
//! tool calls, approvals or status.

use super::terminal_buffer::{
    TerminalBuffer, TerminalSanitizer, TerminalStatus, TERMINAL_BUFFER_BYTES, TERMINAL_VIEW_LINES,
};
use super::*;
use gpui::KeyDownEvent;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use ukk_hc_client::{AttachRequest, FramedConnection, HcError, StreamEvent, TerminalAddress};
use ukk_host_client::{RuntimeCapabilityStatus, RuntimeSendOutcome};

/// Initial geometry only. An observer never resizes after attaching.
const ATTACH_COLS: u16 = 120;
const ATTACH_ROWS: u16 = 40;
/// The worker wakes this often to notice a local detach even when the stream
/// is quiet. It performs no I/O on the GPUI thread.
const ATTACH_POLL: Duration = Duration::from_millis(250);
/// How often the GPUI foreground task checks for worker progress.
const UI_POLL: Duration = Duration::from_millis(60);

trait ReadOnlyEventSource {
    fn next_event(&self, timeout: Duration) -> std::result::Result<StreamEvent, Box<HcError>>;
}

impl ReadOnlyEventSource for ukk_hc_client::Attachment {
    fn next_event(&self, timeout: Duration) -> std::result::Result<StreamEvent, Box<HcError>> {
        ukk_hc_client::Attachment::next_event(self, timeout).map_err(Box::new)
    }
}

struct SharedFeed {
    buffer: Mutex<TerminalBuffer>,
    status: Mutex<TerminalStatus>,
    received: AtomicU64,
    generation: AtomicU64,
}

impl Default for SharedFeed {
    fn default() -> Self {
        Self {
            buffer: Mutex::new(TerminalBuffer::default()),
            status: Mutex::new(TerminalStatus::Connecting),
            received: AtomicU64::new(0),
            generation: AtomicU64::new(0),
        }
    }
}

impl SharedFeed {
    fn set_status(&self, status: TerminalStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = status;
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    fn status(&self) -> TerminalStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn append(&self, text: &str, received: u64) {
        if text.is_empty() && received == 0 {
            return;
        }
        if !text.is_empty() {
            self.buffer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(text);
        }
        if received > 0 {
            self.received.fetch_add(received, Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    fn finish_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(text);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn text(&self) -> String {
        self.buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .text()
    }

    fn tail(&self, max_lines: usize) -> String {
        self.buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tail(max_lines)
    }

    fn counts(&self) -> (usize, usize, u64) {
        let buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (
            buffer.byte_len(),
            buffer.line_count(),
            buffer.dropped_lines(),
        )
    }

    fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

/// One explicit attachment, keyed by its owning view.
pub(super) struct TerminalFeed {
    address: String,
    core_id: Option<String>,
    pane_id: Option<String>,
    shared: Arc<SharedFeed>,
    cancel: Arc<AtomicBool>,
    seen_generation: u64,
}

impl TerminalFeed {
    fn status(&self) -> TerminalStatus {
        self.shared.status()
    }

    fn request_detach(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn is_running(&self) -> bool {
        matches!(
            self.status(),
            TerminalStatus::Connecting | TerminalStatus::Attached
        )
    }

    fn snapshot(&self) -> FeedSnapshot {
        let (bytes, lines, dropped) = self.shared.counts();
        FeedSnapshot {
            address: self.address.clone(),
            core_id: self.core_id.clone(),
            status: self.shared.status(),
            received: self.shared.received(),
            text: self.shared.tail(TERMINAL_VIEW_LINES),
            bytes,
            lines,
            dropped,
        }
    }
}

struct FeedSnapshot {
    address: String,
    core_id: Option<String>,
    status: TerminalStatus,
    received: u64,
    text: String,
    bytes: usize,
    lines: usize,
    dropped: u64,
}

impl FeedSnapshot {
    fn diagnostics(&self, key: &str) -> String {
        format!(
            "ukk Desktop terminal observation\nview: {key}\nterminal: {}\ncore: {}\nstatus: {}\nreceived: {} bytes\nbuffered: {} bytes / {} lines ({} dropped)\nring budget: {} bytes\nattach: observe=true (no input, no resize, no stop)\nraw terminal text is not an Agent event\n",
            self.address,
            self.core_id.as_deref().unwrap_or("unknown"),
            self.status.with_progress(self.received),
            self.received,
            self.bytes,
            self.lines,
            self.dropped,
            TERMINAL_BUFFER_BYTES,
        )
    }

    fn ended(&self) -> bool {
        self.status.has_ended()
    }
}

#[derive(Default)]
pub(super) struct TerminalFeeds {
    feeds: HashMap<String, TerminalFeed>,
}

impl TerminalFeeds {
    pub(super) fn get(&self, key: &str) -> Option<&TerminalFeed> {
        self.feeds.get(key)
    }

    /// Start one explicit observe-only attach. A running feed for the same
    /// address is reused; a different or ended feed is replaced.
    pub(super) fn attach(
        &mut self,
        key: String,
        address: String,
        core_id: Option<String>,
        pane_id: Option<String>,
    ) {
        if let Some(existing) = self.feeds.get(&key) {
            if existing.address == address && existing.is_running() {
                return;
            }
            existing.request_detach();
            self.feeds.remove(&key);
        }

        let shared = Arc::new(SharedFeed::default());
        let cancel = Arc::new(AtomicBool::new(false));
        match TerminalAddress::parse(address.clone()) {
            Ok(terminal) => {
                let worker_shared = Arc::clone(&shared);
                let worker_cancel = Arc::clone(&cancel);
                let worker_core = core_id.clone();
                let thread_name = format!("ukk-terminal-{}", key.replace(':', "-"));
                let spawned = thread::Builder::new()
                    .name(thread_name)
                    .spawn(move || run_worker(terminal, worker_core, worker_shared, worker_cancel));
                if let Err(error) = spawned {
                    shared.set_status(TerminalStatus::Failed(format!(
                        "could not start terminal observer: {error}"
                    )));
                }
            }
            Err(error) => shared.set_status(TerminalStatus::Failed(format!(
                "invalid terminal address {address:?}: {error}"
            ))),
        }
        self.feeds.insert(
            key,
            TerminalFeed {
                address,
                core_id,
                pane_id,
                shared,
                cancel,
                seen_generation: 0,
            },
        );
    }

    pub(super) fn detach(&mut self, key: &str) {
        if let Some(feed) = self.feeds.remove(key) {
            feed.request_detach();
        }
    }

    /// The detail card observes one selected Host terminal at a time. Opening
    /// a new detail observer only drops the previous view's stream.
    fn detach_detail_except(&mut self, keep: &str) {
        let stale: Vec<String> = self
            .feeds
            .iter()
            .filter(|(key, feed)| feed.pane_id.is_none() && key.as_str() != keep)
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.detach(&key);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.feeds.is_empty()
    }

    /// Return true when any worker published a new status or byte batch.
    pub(super) fn poll_changed(&mut self) -> bool {
        let mut changed = false;
        for feed in self.feeds.values_mut() {
            let generation = feed.shared.generation();
            if generation != feed.seen_generation {
                feed.seen_generation = generation;
                changed = true;
            }
        }
        changed
    }

    /// Pane/Tab/Session removal only ends the view's stream. The terminal
    /// program and its herdr-core record are untouched.
    pub(super) fn retain_layouts(&mut self, layouts: &[SessionLayout]) {
        let mut terminal_panes = HashSet::new();
        for layout in layouts {
            for (id, content) in layout.root.leaves() {
                if matches!(content, PaneContent::Terminal { .. }) {
                    terminal_panes.insert(id.to_owned());
                }
            }
        }
        self.feeds.retain(|_, feed| match feed.pane_id.as_deref() {
            Some(pane) if !terminal_panes.contains(pane) => {
                feed.request_detach();
                false
            }
            _ => true,
        });
    }

    /// A selected Host terminal that disappeared from the snapshot loses its
    /// detail observation. A finished record is kept so its final output and
    /// close reason remain visible; Pane-bound feeds keep their own lifecycle.
    pub(super) fn retain_known_addresses(&mut self, snapshot: &HostSnapshot) {
        let known: HashSet<&str> = snapshot
            .terminals
            .iter()
            .map(|terminal| terminal.address.as_str())
            .collect();
        self.feeds.retain(|_, feed| {
            if feed.pane_id.is_none() && !known.contains(feed.address.as_str()) {
                feed.request_detach();
                false
            } else {
                true
            }
        });
    }
}

impl Drop for TerminalFeeds {
    fn drop(&mut self) {
        for feed in self.feeds.values() {
            feed.request_detach();
        }
    }
}

/// One explicit line per user action. This is not a keystroke stream: the
/// composer accumulates printable characters locally and submits them only on
/// Send or Enter.
const TERMINAL_INPUT_MAX_BYTES: usize = ukk_host_client::MAX_RUNTIME_SEND_BYTES;

/// Truthful projection of the Host's separate agent-control capability.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum TerminalControlCapability {
    /// `runtime.status` has not answered yet.
    #[default]
    Unknown,
    Enabled,
    Disabled {
        problem: Option<String>,
    },
    /// The local Host could not be reached or answered invalidly.
    Offline {
        error: String,
    },
}

impl TerminalControlCapability {
    pub(super) fn from_status(status: &RuntimeCapabilityStatus) -> Self {
        if status.enabled {
            Self::Enabled
        } else {
            Self::Disabled {
                problem: status.problem.clone(),
            }
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    fn label(&self) -> String {
        match self {
            Self::Unknown => "Checking the separate agent-control capability…".to_owned(),
            Self::Enabled => "agent-control enabled · one explicit line at a time".to_owned(),
            Self::Disabled {
                problem: Some(problem),
            } => format!(
                "Send unavailable: the separate agent-control capability is disabled ({})",
                terminal_notice_text(problem)
            ),
            Self::Disabled { problem: None } => {
                "Send unavailable: the separate agent-control capability is disabled".to_owned()
            }
            Self::Offline { error } => format!(
                "Send unavailable: the local Host is unavailable ({})",
                terminal_notice_text(error)
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalNoticeKind {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalInputNotice {
    key: String,
    kind: TerminalNoticeKind,
    text: String,
}

impl TerminalInputNotice {
    pub(super) fn kind(&self) -> TerminalNoticeKind {
        self.kind
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

/// What one key event asks the view to do. Only Enter can produce `Send`;
/// ordinary typing is local and never reaches the Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalKeyAction {
    None,
    Send,
}

/// Local composer state for one bound Session terminal.
///
/// The text is owned by one terminal key. Switching to another terminal drops
/// the previous draft instead of risking input to the wrong process. The field
/// is cleared only after the Host confirms the send; failures keep the draft
/// and are shown verbatim. Nothing here retries.
#[derive(Default)]
pub(super) struct TerminalInputState {
    owner: Option<String>,
    text: String,
    busy: bool,
    /// Terminal key whose draft is currently submitted. Only that draft is
    /// frozen; another pane can still be drafted locally.
    busy_key: Option<String>,
    notice: Option<TerminalInputNotice>,
    capability: TerminalControlCapability,
}

impl TerminalInputState {
    pub(super) fn capability(&self) -> &TerminalControlCapability {
        &self.capability
    }

    pub(super) fn set_capability(&mut self, capability: TerminalControlCapability) {
        self.capability = capability;
    }

    pub(super) fn busy(&self) -> bool {
        self.busy
    }

    pub(super) fn busy_for(&self, key: &str) -> bool {
        self.busy && self.busy_key.as_deref() == Some(key)
    }

    pub(super) fn text_for(&self, key: &str) -> &str {
        if self.owner.as_deref() == Some(key) {
            &self.text
        } else {
            ""
        }
    }

    pub(super) fn notice_for(&self, key: &str) -> Option<&TerminalInputNotice> {
        self.notice.as_ref().filter(|notice| notice.key == key)
    }

    fn ensure_owner(&mut self, key: &str) {
        if self.owner.as_deref() != Some(key) {
            self.owner = Some(key.to_owned());
            self.text.clear();
            self.notice = None;
        }
    }

    fn can_send(&self) -> bool {
        self.capability.is_enabled() && !self.busy && valid_terminal_line(&self.text)
    }

    /// Applies one key event. `Send` is returned only for Enter with a valid,
    /// non-empty draft and an enabled capability.
    pub(super) fn handle_key(&mut self, key: &str, event: &KeyDownEvent) -> TerminalKeyAction {
        if self.busy && self.busy_key.as_deref() == Some(key) {
            // The submitted draft must not drift while its Host reply is
            // pending. Other panes are not blocked.
            return TerminalKeyAction::None;
        }
        self.ensure_owner(key);
        match event.keystroke.key.as_str() {
            "enter" => {
                if self.can_send() {
                    TerminalKeyAction::Send
                } else {
                    TerminalKeyAction::None
                }
            }
            "backspace" => {
                self.text.pop();
                TerminalKeyAction::None
            }
            "escape" => {
                self.text.clear();
                TerminalKeyAction::None
            }
            _ => {
                let modifiers = event.keystroke.modifiers;
                if modifiers.control || modifiers.platform || modifiers.alt {
                    return TerminalKeyAction::None;
                }
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    if text
                        .chars()
                        .any(|character| character.is_control() || is_bidi_format(character))
                    {
                        return TerminalKeyAction::None;
                    }
                    if self.text.len() + text.len() <= TERMINAL_INPUT_MAX_BYTES {
                        self.text.push_str(text);
                    }
                }
                TerminalKeyAction::None
            }
        }
    }

    /// Reserve the current draft for one Host call. The draft stays visible
    /// until the call is verified, so a failure cannot look like a success.
    pub(super) fn begin_send(&mut self, key: &str) -> Option<String> {
        if self.owner.as_deref() != Some(key) || !self.can_send() {
            return None;
        }
        self.busy = true;
        self.busy_key = Some(key.to_owned());
        self.notice = None;
        Some(self.text.clone())
    }

    pub(super) fn finish_success(
        &mut self,
        key: &str,
        submitted: &str,
        outcome: &RuntimeSendOutcome,
    ) {
        self.busy = false;
        self.busy_key = None;
        if self.owner.as_deref() == Some(key) && self.text == submitted {
            self.text.clear();
        }
        let replay = if outcome.replayed {
            " · idempotent replay"
        } else {
            ""
        };
        self.notice = Some(TerminalInputNotice {
            key: key.to_owned(),
            kind: TerminalNoticeKind::Success,
            text: format!(
                "Sent · Host turn {} · {}{replay}",
                outcome.turn, outcome.terminal
            ),
        });
    }

    /// Keeps the draft on purpose: the user can inspect, correct or resend it
    /// explicitly. The view never retries on its own.
    pub(super) fn finish_failure(&mut self, key: &str, message: String) {
        self.busy = false;
        self.busy_key = None;
        self.notice = Some(TerminalInputNotice {
            key: key.to_owned(),
            kind: TerminalNoticeKind::Error,
            text: terminal_notice_text(&message),
        });
    }

    /// Reports a refused send without reserving or clearing the draft.
    pub(super) fn block_send(&mut self, key: &str, message: String) {
        self.notice = Some(TerminalInputNotice {
            key: key.to_owned(),
            kind: TerminalNoticeKind::Error,
            text: terminal_notice_text(&message),
        });
    }
}

/// A line is exactly one bounded, printable line. This mirrors the Host
/// client's validation so the Send affordance never looks available for text
/// the wire layer will reject.
pub(super) fn valid_terminal_line(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= TERMINAL_INPUT_MAX_BYTES
        && !text
            .chars()
            .any(|character| character.is_control() || is_bidi_format(character))
}

fn is_bidi_format(character: char) -> bool {
    matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Error and capability text is Host-authored but still untrusted display
/// text. Strip control and bidi characters and bound the notice.
fn terminal_notice_text(text: &str) -> String {
    const MAX_BYTES: usize = 2 * 1024;
    let mut output = String::with_capacity(text.len().min(MAX_BYTES));
    for character in text.chars() {
        if output.len() >= MAX_BYTES {
            output.push('…');
            break;
        }
        if character == '\n'
            || character == '\t'
            || (!character.is_control() && !is_bidi_format(character))
        {
            output.push(character);
        }
    }
    output
}

fn observe_request(terminal: TerminalAddress) -> AttachRequest {
    AttachRequest::observe(terminal, ATTACH_COLS, ATTACH_ROWS)
}

fn run_worker(
    terminal: TerminalAddress,
    expected_core: Option<String>,
    shared: Arc<SharedFeed>,
    cancel: Arc<AtomicBool>,
) {
    let binary = std::env::var_os("UKK_HC_BIN").unwrap_or_else(|| OsString::from("hc"));
    let connection = match FramedConnection::connect(&binary) {
        Ok(connection) => connection,
        Err(error) => {
            shared.set_status(TerminalStatus::Failed(format!(
                "herdr-core attach unavailable: {error}"
            )));
            return;
        }
    };
    if let Some(expected) = expected_core
        .as_deref()
        .filter(|core| !core.trim().is_empty())
    {
        if connection.core_id() != expected {
            shared.set_status(TerminalStatus::Failed(format!(
                "core identity mismatch: binding expects {expected}, connected core is {}",
                connection.core_id()
            )));
            return;
        }
    }
    let attachment = match connection.attach(&observe_request(terminal)) {
        Ok(attachment) => attachment,
        Err(error) => {
            shared.set_status(TerminalStatus::Failed(format!(
                "observe attach refused: {error}"
            )));
            return;
        }
    };
    shared.set_status(TerminalStatus::Attached);

    let mut sanitizer = TerminalSanitizer::default();
    pump_read_only(&attachment, &shared, &cancel, &mut sanitizer);
    let tail = sanitizer.finish();
    shared.finish_text(&tail);

    // Dropping `attachment` sends CLOSE for this stream. Dropping the
    // connection reaps the local `hc connect` child. Neither path calls
    // terminal.kill, terminal.rm, terminal.interrupt or terminal.send.
}

fn pump_read_only<S: ReadOnlyEventSource>(
    source: &S,
    shared: &SharedFeed,
    cancel: &AtomicBool,
    sanitizer: &mut TerminalSanitizer,
) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match source.next_event(ATTACH_POLL) {
            Ok(StreamEvent::Data(bytes)) => {
                let text = sanitizer.push(&bytes);
                shared.append(&text, bytes.len() as u64);
            }
            Ok(StreamEvent::Close(close)) => {
                let tail = sanitizer.finish();
                shared.finish_text(&tail);
                shared.set_status(closed_status(close.reason, close.exit_code, close.signal));
                return;
            }
            Ok(StreamEvent::Facts(_))
            | Ok(StreamEvent::Resize { .. })
            | Ok(StreamEvent::Focus { .. })
            | Ok(StreamEvent::Credit { .. }) => {}
            Err(error) => {
                if matches!(error.as_ref(), HcError::Timeout { .. }) {
                    continue;
                }
                let tail = sanitizer.finish();
                shared.finish_text(&tail);
                match *error {
                    HcError::Closed(reason) => {
                        shared.set_status(closed_status(Some(reason), None, None));
                    }
                    HcError::Backpressure { limit } => {
                        shared.set_status(TerminalStatus::Failed(format!(
                            "terminal stream consumer fell behind the {limit}-byte limit"
                        )));
                    }
                    error => shared.set_status(TerminalStatus::Failed(error.to_string())),
                }
                return;
            }
        }
    }
}

fn closed_status(
    reason: Option<String>,
    exit_code: Option<i32>,
    signal: Option<i32>,
) -> TerminalStatus {
    TerminalStatus::Closed {
        reason: reason.unwrap_or_else(|| "stream ended".to_owned()),
        exit_code,
        signal,
    }
}

impl Workspace {
    /// Reap observer progress off the UI thread. Worker threads only publish
    /// status/text; GPUI reads them and repaints when the generation changes.
    pub(super) fn watch_terminals(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let mut active = false;
            let keep_going = this
                .update(cx, |view, cx| {
                    if view.terminal_feeds.poll_changed() {
                        cx.notify();
                    }
                    active = !view.terminal_feeds.is_empty();
                    true
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
            let delay = if active {
                UI_POLL
            } else {
                Duration::from_millis(500)
            };
            cx.background_executor().timer(delay).await;
        })
        .detach();
    }

    pub(super) fn attach_agent_terminal(
        &mut self,
        pane_id: &str,
        agent_run_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self
            .domain
            .agent_runs
            .iter()
            .find(|run| run.id == agent_run_id)
        else {
            return;
        };
        self.terminal_feeds.attach(
            terminal_pane_key(pane_id),
            run.terminal_address.clone(),
            Some(run.core_id.clone()),
            Some(pane_id.to_owned()),
        );
        cx.notify();
    }

    pub(super) fn attach_host_terminal(&mut self, address: &str, cx: &mut Context<Self>) {
        let key = terminal_detail_key(address);
        self.terminal_feeds.detach_detail_except(&key);
        self.terminal_feeds
            .attach(key, address.to_owned(), None, None);
        cx.notify();
    }

    pub(super) fn detach_terminal(&mut self, key: &str, cx: &mut Context<Self>) {
        self.terminal_feeds.detach(key);
        cx.notify();
    }

    pub(super) fn terminal_pane(
        &self,
        agent_run_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let key = terminal_pane_key(pane_id);
        let binding = self
            .domain
            .agent_runs
            .iter()
            .find(|run| run.id == agent_run_id);
        let matching = binding.and_then(|run| {
            self.terminal_feeds
                .get(&key)
                .filter(|feed| feed.address == run.terminal_address)
                .map(TerminalFeed::snapshot)
        });
        let stale =
            binding.is_some() && self.terminal_feeds.get(&key).is_some() && matching.is_none();

        let (title, subtitle) = match binding {
            Some(run) => (
                format!("Terminal · {}", run.terminal_address),
                format!(
                    "AgentRun {} · core {} · read-only observer",
                    run.id, run.core_id
                ),
            ),
            None => (
                "Unresolved Terminal".to_owned(),
                format!("AgentRun {agent_run_id} is not present in the workspace metadata."),
            ),
        };

        let mut header_actions = div().flex().flex_wrap().items_center().gap_1();
        if let Some(snapshot) = matching.as_ref() {
            header_actions = header_actions
                .child(self.surface_button(
                    format!("{pane_id}-terminal-detach"),
                    "Detach",
                    "Close only this read-only stream; the terminal keeps running".into(),
                    {
                        let key = key.clone();
                        move |this, cx| this.detach_terminal(&key, cx)
                    },
                    cx,
                ))
                .child(self.surface_button(
                    format!("{pane_id}-terminal-copy"),
                    "Copy diagnostics",
                    "Copy terminal observation diagnostics".into(),
                    {
                        let diagnostics = snapshot.diagnostics(&key);
                        move |this, cx| this.copy_repository_diagnostics(diagnostics.clone(), cx)
                    },
                    cx,
                ));
            if snapshot.ended() {
                header_actions = header_actions.child(self.surface_button(
                    format!("{pane_id}-terminal-retry"),
                    "Retry read-only",
                    "Open a fresh observe-only stream for this terminal".into(),
                    {
                        let pane_id = pane_id.to_owned();
                        let agent_run_id = agent_run_id.to_owned();
                        move |this, cx| this.attach_agent_terminal(&pane_id, &agent_run_id, cx)
                    },
                    cx,
                ));
            }
        } else if let Some(_run) = binding {
            let label = if stale {
                "Reattach read-only"
            } else {
                "Attach read-only"
            };
            header_actions = header_actions.child(self.surface_button(
                format!("{pane_id}-terminal-attach"),
                label,
                "Open an observe-only terminal stream after this explicit action".into(),
                {
                    let pane_id = pane_id.to_owned();
                    let agent_run_id = agent_run_id.to_owned();
                    move |this, cx| this.attach_agent_terminal(&pane_id, &agent_run_id, cx)
                },
                cx,
            ));
        }

        let mut body = div().flex().flex_col().flex_1().min_h_0().gap_2().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(MUTED))
                                .child("TERMINAL · READ-ONLY OBSERVATION"),
                        )
                        .child(div().text_sm().text_color(rgb(INK)).child(title))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
                )
                .child(header_actions),
        );

        if let Some(run) = binding {
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(SURFACE))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "binding: {} / {} · Host owns runtime control and terminal lifetime",
                        run.core_id, run.terminal_address
                    )),
            );
        }

        if let Some(snapshot) = matching.as_ref() {
            body = body
                .child(self.terminal_status_row(snapshot, &key))
                .child(self.terminal_output_view(Some(snapshot), &key, None));
        } else if binding.is_none() {
            body = body.child(self.terminal_notice(
                "No AgentRun binding is available, so there is no verified terminal address to observe.",
            ));
        } else if stale {
            body = body.child(self.terminal_notice(
                "The attached stream targets a previous terminal address. Reattach to follow the current binding; the old stream is only dropped, never signalled.",
            ));
        } else {
            body = body.child(self.terminal_notice(
                "Not attached. Attaching opens an observe-only framed stream to this terminal. It sends no keystrokes, never resizes, and never stops, kills or removes the terminal or its Agent.",
            ));
        }

        if matching.is_none() {
            body = body.child(self.terminal_notice(
                "Attach is explicit. Nothing is attached on application load, Session selection or tab switch.",
            ));
        }

        if let (Some(run), Some(snapshot)) = (binding, matching.as_ref()) {
            if !snapshot.ended() {
                body = body.child(self.terminal_input_composer(
                    agent_run_id,
                    pane_id,
                    &run.core_id,
                    &run.terminal_address,
                    cx,
                ));
            }
        }

        body.child(
            div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child("Raw terminal text is displayed read-only. It is not a structured Agent event and is never used to synthesize messages, tool calls, progress or approvals."),
        )
        .into_any_element()
    }

    pub(super) fn terminal_detail_attachment(
        &self,
        terminal: &Terminal,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let key = terminal_detail_key(&terminal.address);
        let matching = self
            .terminal_feeds
            .get(&key)
            .filter(|feed| feed.address == terminal.address)
            .map(TerminalFeed::snapshot);

        let mut actions = div().flex().flex_wrap().items_center().gap_1();
        if let Some(snapshot) = matching.as_ref() {
            actions = actions
                .child(self.surface_button(
                    format!("detail-terminal-detach-{}", terminal.address),
                    "Detach",
                    "Close only this read-only stream; the terminal keeps running".into(),
                    {
                        let key = key.clone();
                        move |this, cx| this.detach_terminal(&key, cx)
                    },
                    cx,
                ))
                .child(self.surface_button(
                    format!("detail-terminal-copy-{}", terminal.address),
                    "Copy diagnostics",
                    "Copy terminal observation diagnostics".into(),
                    {
                        let diagnostics = snapshot.diagnostics(&key);
                        move |this, cx| this.copy_repository_diagnostics(diagnostics.clone(), cx)
                    },
                    cx,
                ));
            if snapshot.ended() {
                actions = actions.child(self.surface_button(
                    format!("detail-terminal-retry-{}", terminal.address),
                    "Retry read-only",
                    "Open a fresh observe-only stream for this terminal".into(),
                    {
                        let address = terminal.address.clone();
                        move |this, cx| this.attach_host_terminal(&address, cx)
                    },
                    cx,
                ));
            }
        } else {
            actions = actions.child(self.surface_button(
                format!("detail-terminal-attach-{}", terminal.address),
                "Attach read-only",
                "Open an observe-only terminal stream for this selected Host terminal".into(),
                {
                    let address = terminal.address.clone();
                    move |this, cx| this.attach_host_terminal(&address, cx)
                },
                cx,
            ));
        }

        let mut view = div()
            .p_5()
            .bg(rgb(WHITE))
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(MUTED))
                                    .child("TERMINAL · READ-ONLY OBSERVATION"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(INK))
                                    .child(format!("Terminal {}", terminal.address)),
                            ),
                    )
                    .child(actions),
            );
        if let Some(snapshot) = matching {
            view = view
                .child(self.terminal_status_row(&snapshot, &key))
                .child(self.terminal_output_view(Some(&snapshot), &key, Some(320.0)));
        } else {
            view = view.child(self.terminal_notice(
                "No stream attached. This selected Host terminal can be observed read-only after an explicit action. This card sends no input or lifecycle control; the bound Session Terminal pane has a separate capability-gated one-line composer.",
            ));
        }
        view
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Observed text is raw terminal output, not an Agent event. It is sanitized for display and bounded in memory."),
            )
            .into_any_element()
    }

    /// Explicit one-line input for one bound Session terminal.
    ///
    /// The composer exists only for an AgentRun binding with a live observe
    /// stream. When the separate agent-control capability is disabled, the
    /// field is absent and the pane states why; it never falls back to the
    /// workspace token or a synthetic success.
    fn terminal_input_composer(
        &self,
        agent_run_id: &str,
        pane_id: &str,
        core_id: &str,
        terminal: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let key = terminal_pane_key(pane_id);
        let capability = self.terminal_input.capability();
        let enabled = capability.is_enabled();
        let busy = self.terminal_input.busy();
        let busy_here = self.terminal_input.busy_for(&key);
        let text = self.terminal_input.text_for(&key).to_owned();
        let notice = self.terminal_input.notice_for(&key).cloned();
        let ready = valid_terminal_line(&text);

        let mut section = div()
            .id(format!("{pane_id}-terminal-input-section"))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child("TERMINAL INPUT · ONE LINE · AGENT-CONTROL"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(if enabled { 0x408463 } else { 0xb06e55 }))
                    .child(capability.label()),
            );

        let notice_view = notice.map(|notice| {
            let color = match notice.kind() {
                TerminalNoticeKind::Success => 0x408463,
                TerminalNoticeKind::Error => 0xb03a3a,
            };
            div()
                .p_2()
                .rounded_md()
                .bg(rgb(WHITE))
                .text_xs()
                .text_color(rgb(color))
                .child(notice.text().to_owned())
                .into_any_element()
        });

        if !enabled {
            let recheck = self.surface_button(
                format!("{pane_id}-terminal-capability-recheck"),
                "Recheck capability",
                "Read the Host's agent-control capability state again".into(),
                |this, cx| this.refresh_terminal_capability(cx),
                cx,
            );
            let mut section =
                section
                    .child(recheck)
                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                        "The Desktop never substitutes the workspace token for terminal input.",
                    ));
            if let Some(notice) = notice_view {
                section = section.child(notice);
            }
            return section.into_any_element();
        }

        let input_focus = self.terminal_input_focus.clone();
        let field_click_focus = self.terminal_input_focus.clone();
        let agent_for_keys = agent_run_id.to_owned();
        let pane_for_keys = pane_id.to_owned();
        let placeholder = if busy_here {
            "Sending this line…".to_owned()
        } else if ready {
            "Press Enter or Send".to_owned()
        } else {
            format!("Type one line for {terminal}…")
        };

        section = section.child(
            div()
                .id(format!("{pane_id}-terminal-input"))
                .role(Role::TextInput)
                .aria_label("terminal one-line input")
                .track_focus(&input_focus)
                .focusable()
                .focus_visible(|style| style.border_color(rgb(ACCENT)))
                .px_3()
                .py_2()
                .min_h(px(38.0))
                .rounded_lg()
                .bg(rgb(BG))
                .border_1()
                .border_color(rgb(LINE))
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, window, cx| {
                    window.focus(&field_click_focus, cx);
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    this.terminal_input_key(&agent_for_keys, &pane_for_keys, event, cx);
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(if text.is_empty() { MUTED } else { INK }))
                        .child(if text.is_empty() {
                            placeholder
                        } else {
                            text.clone()
                        }),
                ),
        );

        section =
            section.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(self.terminal_send_button(
                        agent_run_id,
                        pane_id,
                        enabled && ready && !busy,
                        busy_here,
                        cx,
                    ))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                        "Enter sends · Escape clears · Backspace edits · no keystroke stream",
                    )),
            );

        if let Some(notice) = notice_view {
            section = section.child(notice);
        }

        section
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "binding {} / {} · Host runtime.send requires the separate agent-control token · no resize, interrupt or stop",
                        core_id, terminal
                    )),
            )
            .into_any_element()
    }

    fn terminal_send_button(
        &self,
        agent_run_id: &str,
        pane_id: &str,
        enabled: bool,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let label = if busy { "Sending…" } else { "Send line" };
        let button = div()
            .id(format!("{pane_id}-terminal-send"))
            .role(Role::Button)
            .aria_label("Send this one line to the bound terminal")
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .text_xs()
            .text_color(rgb(ACCENT))
            .child(label);
        if !enabled {
            return button.opacity(0.5).into_any_element();
        }
        let agent_run_id = agent_run_id.to_owned();
        let pane_id = pane_id.to_owned();
        button
            .cursor_pointer()
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.send_terminal_line(&agent_run_id, &pane_id, cx);
            }))
            .into_any_element()
    }

    fn terminal_input_key(
        &mut self,
        agent_run_id: &str,
        pane_id: &str,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let key = terminal_pane_key(pane_id);
        match self.terminal_input.handle_key(&key, event) {
            TerminalKeyAction::Send => self.send_terminal_line(agent_run_id, pane_id, cx),
            TerminalKeyAction::None => cx.notify(),
        }
        cx.stop_propagation();
    }

    /// One explicit user send. Re-resolves the binding and the attached
    /// stream first, so a stale render can never target another terminal.
    pub(super) fn send_terminal_line(
        &mut self,
        agent_run_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_input.busy() {
            return;
        }
        let key = terminal_pane_key(pane_id);
        let Some((core_id, terminal)) = self
            .domain
            .agent_runs
            .iter()
            .find(|run| run.id == agent_run_id)
            .map(|run| (run.core_id.clone(), run.terminal_address.clone()))
        else {
            return;
        };
        let stream_matches = self
            .terminal_feeds
            .get(&key)
            .is_some_and(|feed| feed.address == terminal && feed.is_running());
        if !stream_matches {
            self.terminal_input.block_send(
                &key,
                "The attached stream is no longer live for this binding. Reattach read-only before sending."
                    .into(),
            );
            cx.notify();
            return;
        }
        let Some(line) = self.terminal_input.begin_send(&key) else {
            return;
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let submitted = line.clone();
            let core = core_id.clone();
            let terminal_address = terminal.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    HostClient::from_env().runtime_send(&core, &terminal_address, &submitted)
                })
                .await;
            this.update(cx, |view, cx| {
                match result {
                    Ok(outcome) => view.terminal_input.finish_success(&key, &line, &outcome),
                    Err(error) => {
                        // Show the failure and refresh the truthful capability
                        // state. The line is never resent automatically.
                        view.terminal_input.finish_failure(&key, error.to_string());
                        view.refresh_terminal_capability(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read-only `runtime.status`; it never reads the control token.
    pub(super) fn refresh_terminal_capability(&mut self, cx: &mut Context<Self>) {
        if self.terminal_capability_in_flight {
            return;
        }
        self.terminal_capability_in_flight = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { HostClient::from_env().runtime_status() })
                .await;
            this.update(cx, |view, cx| {
                view.terminal_capability_in_flight = false;
                view.apply_terminal_capability(result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn apply_terminal_capability(
        &mut self,
        result: Result<RuntimeCapabilityStatus, ukk_host_client::HostError>,
    ) {
        let capability = match result {
            Ok(status) => TerminalControlCapability::from_status(&status),
            Err(error) => TerminalControlCapability::Offline {
                error: error.to_string(),
            },
        };
        self.terminal_input.set_capability(capability);
    }

    fn terminal_status_row(&self, snapshot: &FeedSnapshot, key: &str) -> gpui::AnyElement {
        let color = match &snapshot.status {
            TerminalStatus::Connecting => 0x8a6d3b,
            TerminalStatus::Attached => 0x408463,
            TerminalStatus::Closed { .. } => MUTED,
            TerminalStatus::Failed(_) => 0xb06e55,
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(color))
                    .child(snapshot.status.with_progress(snapshot.received)),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "view {key} · {} bytes buffered · {} lines",
                snapshot.bytes, snapshot.lines
            )))
            .into_any_element()
    }

    fn terminal_output_view(
        &self,
        snapshot: Option<&FeedSnapshot>,
        key: &str,
        height: Option<f32>,
    ) -> gpui::AnyElement {
        let text = snapshot
            .map(|snapshot| snapshot.text.as_str())
            .unwrap_or("");
        let status = snapshot.map(|snapshot| snapshot.status.with_progress(snapshot.received));
        let rendered = if text.trim().is_empty() {
            status
                .map(|status| format!("{status}\n\nNo terminal output has been received yet."))
                .unwrap_or_else(|| "Not attached.".to_owned())
        } else {
            text.to_owned()
        };
        let mut output = div()
            .id(format!("{key}-terminal-output"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_3()
            .rounded_md()
            .bg(rgb(0x111318))
            .text_xs()
            .text_color(rgb(0xe8eaf0))
            .font_family(terminal_font())
            .overflow_y_scroll()
            .child(rendered);
        if let Some(height) = height {
            output = output.h(px(height));
        }
        output.into_any_element()
    }

    fn terminal_notice(&self, text: impl Into<String>) -> gpui::AnyElement {
        div()
            .p_3()
            .rounded_md()
            .bg(rgb(SURFACE))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(text.into())
            .into_any_element()
    }
}

fn terminal_pane_key(pane_id: &str) -> String {
    format!("pane:{pane_id}")
}

fn terminal_detail_key(address: &str) -> String {
    format!("detail:{address}")
}

fn terminal_font() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "DejaVu Sans Mono"
    }
    #[cfg(not(any(unix, windows)))]
    {
        ".SystemUIFont"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use ukk_hc_client::StreamClose;

    struct MockStream {
        events: Mutex<VecDeque<StreamEvent>>,
    }

    impl MockStream {
        fn new(events: Vec<StreamEvent>) -> Self {
            Self {
                events: Mutex::new(events.into()),
            }
        }
    }

    impl ReadOnlyEventSource for MockStream {
        fn next_event(&self, _timeout: Duration) -> std::result::Result<StreamEvent, Box<HcError>> {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
                .ok_or_else(|| {
                    Box::new(HcError::Timeout {
                        operation: "mock attach stream".into(),
                        seconds: 0,
                    })
                })
        }
    }

    fn address(raw: &str) -> TerminalAddress {
        TerminalAddress::parse(raw).expect("valid test address")
    }

    #[test]
    fn observe_request_never_asks_for_input_or_resize() {
        let request = observe_request(address("t7"));
        assert!(request.observe);
        assert_eq!(request.history, 0);
        assert!(!request.resize_notifications);
        assert_eq!(request.cols, ATTACH_COLS);
        assert_eq!(request.rows, ATTACH_ROWS);
    }

    #[test]
    fn pump_sanitizes_chunks_and_reports_close_and_progress() {
        let source = MockStream::new(vec![
            StreamEvent::Data(b"\x1b[31mhello\x1b[0m\nwor".to_vec()),
            StreamEvent::Data(b"ld".to_vec()),
            StreamEvent::Close(StreamClose {
                reason: Some("exited".into()),
                exit_code: Some(0),
                signal: None,
            }),
        ]);
        let shared = SharedFeed::default();
        let cancel = AtomicBool::new(false);
        let mut sanitizer = TerminalSanitizer::default();
        pump_read_only(&source, &shared, &cancel, &mut sanitizer);
        let tail = sanitizer.finish();
        shared.finish_text(&tail);

        assert_eq!(shared.text(), "hello\nworld");
        assert_eq!(shared.received(), 20);
        assert_eq!(
            shared.status(),
            TerminalStatus::Closed {
                reason: "exited".into(),
                exit_code: Some(0),
                signal: None,
            }
        );
    }

    #[test]
    fn pump_honours_a_local_detach_without_claiming_program_exit() {
        let source = MockStream::new(vec![StreamEvent::Data(b"partial".to_vec())]);
        let shared = SharedFeed::default();
        let cancel = AtomicBool::new(true);
        let mut sanitizer = TerminalSanitizer::default();
        pump_read_only(&source, &shared, &cancel, &mut sanitizer);
        assert_eq!(shared.status(), TerminalStatus::Connecting);
        assert_eq!(shared.received(), 0);
    }

    #[test]
    fn diagnostics_describe_the_read_only_boundary() {
        let shared = SharedFeed::default();
        shared.append("some output\n", 12);
        let feed = TerminalFeed {
            address: "t7".into(),
            core_id: Some("core-a".into()),
            pane_id: Some("pane-1".into()),
            shared: Arc::new(shared),
            cancel: Arc::new(AtomicBool::new(false)),
            seen_generation: 0,
        };
        let diagnostics = feed.snapshot().diagnostics("pane:pane-1");
        assert!(diagnostics.contains("observe=true (no input, no resize, no stop)"));
        assert!(diagnostics.contains("not an Agent event"));
    }

    fn key_event(name: &str, character: Option<&str>) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: name.to_owned(),
                key_char: character.map(str::to_owned),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn type_text(input: &mut TerminalInputState, key: &str, text: &str) {
        for character in text.chars() {
            let character = character.to_string();
            assert_eq!(
                input.handle_key(key, &key_event(&character, Some(&character))),
                TerminalKeyAction::None,
                "ordinary typing must never send"
            );
        }
    }

    fn enabled_input() -> TerminalInputState {
        let mut input = TerminalInputState::default();
        input.set_capability(TerminalControlCapability::Enabled);
        input
    }

    fn sent(turn: u64, replayed: bool) -> RuntimeSendOutcome {
        RuntimeSendOutcome {
            core_id: "core-1".into(),
            terminal: "t7".into(),
            turn,
            status: Some("working".into()),
            started: Some(true),
            replayed,
        }
    }

    #[test]
    fn terminal_line_validation_is_one_line_and_bounded() {
        assert!(valid_terminal_line("cargo test"));
        assert!(valid_terminal_line("printf 'hi'"));
        for invalid in ["", "   ", "one\ntwo", "one\rtwo", "tab\there", "bell\u{7}"] {
            assert!(!valid_terminal_line(invalid), "{invalid:?}");
        }
        assert!(!valid_terminal_line(
            &"x".repeat(TERMINAL_INPUT_MAX_BYTES + 1)
        ));
        assert!(valid_terminal_line(&"x".repeat(TERMINAL_INPUT_MAX_BYTES)));
    }

    #[test]
    fn typing_is_local_and_only_enter_returns_send() {
        let mut input = enabled_input();
        type_text(&mut input, "pane:p1", "echo hi");
        assert_eq!(input.text_for("pane:p1"), "echo hi");
        assert_eq!(
            input.handle_key("pane:p1", &key_event("enter", None)),
            TerminalKeyAction::Send
        );
        assert!(!input.busy(), "Enter only asks the view to send");
        assert_eq!(
            input.handle_key("pane:p1", &key_event("escape", None)),
            TerminalKeyAction::None
        );
        assert_eq!(input.text_for("pane:p1"), "");
    }

    #[test]
    fn control_shortcuts_and_control_characters_are_ignored() {
        let mut input = enabled_input();
        let mut control = key_event("c", Some("c"));
        control.keystroke.modifiers.control = true;
        assert_eq!(
            input.handle_key("pane:p1", &control),
            TerminalKeyAction::None
        );
        assert_eq!(input.text_for("pane:p1"), "");
        let mut platform = key_event("v", Some("v"));
        platform.keystroke.modifiers.platform = true;
        assert_eq!(
            input.handle_key("pane:p1", &platform),
            TerminalKeyAction::None
        );
        assert_eq!(input.text_for("pane:p1"), "");
        assert_eq!(
            input.handle_key("pane:p1", &key_event("tab", Some("\t"))),
            TerminalKeyAction::None
        );
        assert_eq!(input.text_for("pane:p1"), "");
    }

    #[test]
    fn capability_gates_the_send_action_truthfully() {
        for capability in [
            TerminalControlCapability::Unknown,
            TerminalControlCapability::Disabled { problem: None },
            TerminalControlCapability::Offline {
                error: "Host unavailable".into(),
            },
        ] {
            let mut input = TerminalInputState::default();
            input.set_capability(capability.clone());
            type_text(&mut input, "pane:p1", "echo hi");
            assert_eq!(
                input.handle_key("pane:p1", &key_event("enter", None)),
                TerminalKeyAction::None
            );
            assert!(input.begin_send("pane:p1").is_none());
            assert!(
                capability.label().contains("Send unavailable")
                    || capability.label().contains("Checking")
            );
        }
    }

    #[test]
    fn a_draft_belongs_to_one_terminal_only() {
        let mut input = enabled_input();
        type_text(&mut input, "pane:p1", "ls");
        assert_eq!(input.text_for("pane:p1"), "ls");
        assert_eq!(input.text_for("pane:p2"), "");
        assert!(input.begin_send("pane:p2").is_none());
        // Typing in another pane starts a fresh draft; the old one is dropped
        // rather than risking input to the wrong terminal.
        type_text(&mut input, "pane:p2", "pwd");
        assert_eq!(input.text_for("pane:p1"), "");
        assert_eq!(input.text_for("pane:p2"), "pwd");
    }

    #[test]
    fn draft_is_kept_on_failure_and_cleared_only_after_verified_success() {
        let mut input = enabled_input();
        type_text(&mut input, "pane:p1", "cargo test");
        assert_eq!(input.begin_send("pane:p1").as_deref(), Some("cargo test"));
        assert!(input.busy());
        // The draft stays visible while the Host call is in flight.
        assert_eq!(input.text_for("pane:p1"), "cargo test");
        // Typing is ignored while busy so the submitted text cannot drift.
        type_text(&mut input, "pane:p1", "x");
        assert_eq!(input.text_for("pane:p1"), "cargo test");

        input.finish_failure("pane:p1", "Host denied the send".into());
        assert!(!input.busy());
        assert_eq!(input.text_for("pane:p1"), "cargo test");
        let notice = input.notice_for("pane:p1").unwrap();
        assert_eq!(notice.kind(), TerminalNoticeKind::Error);
        assert!(notice.text().contains("denied"));

        assert_eq!(input.begin_send("pane:p1").as_deref(), Some("cargo test"));
        input.finish_success("pane:p1", "cargo test", &sent(7, false));
        assert!(!input.busy());
        assert_eq!(input.text_for("pane:p1"), "");
        let notice = input.notice_for("pane:p1").unwrap();
        assert_eq!(notice.kind(), TerminalNoticeKind::Success);
        assert!(notice.text().contains("turn 7"));
    }

    #[test]
    fn capability_status_maps_to_truthful_labels() {
        let enabled = TerminalControlCapability::from_status(&RuntimeCapabilityStatus {
            enabled: true,
            capability: "separate-agent-control".into(),
            problem: None,
        });
        assert!(enabled.is_enabled());
        assert!(enabled.label().contains("enabled"));

        let disabled = TerminalControlCapability::from_status(&RuntimeCapabilityStatus {
            enabled: false,
            capability: "separate-agent-control".into(),
            problem: Some("token file is missing".into()),
        });
        assert!(!disabled.is_enabled());
        assert!(disabled.label().contains("disabled"));
        assert!(disabled.label().contains("token file is missing"));

        let offline = TerminalControlCapability::Offline {
            error: "connection refused".into(),
        };
        assert!(offline.label().contains("unavailable"));
        assert!(offline.label().contains("connection refused"));
    }

    #[test]
    fn submitted_line_is_verbatim_without_rewriting() {
        let mut input = enabled_input();
        type_text(&mut input, "pane:p1", "git commit -m 'msg with spaces'");
        assert_eq!(
            input.begin_send("pane:p1"),
            Some("git commit -m 'msg with spaces'".to_owned())
        );
        assert_eq!(input.text_for("pane:p1"), "git commit -m 'msg with spaces'");
    }

    #[test]
    fn success_does_not_clear_text_the_user_changed_after_submitting() {
        let mut input = enabled_input();
        type_text(&mut input, "pane:p1", "one");
        assert_eq!(input.begin_send("pane:p1").as_deref(), Some("one"));
        // A different terminal becomes the owner while the reply is pending.
        type_text(&mut input, "pane:p2", "two");
        input.finish_success("pane:p1", "one", &sent(3, true));
        assert_eq!(input.text_for("pane:p2"), "two");
        let notice = input.notice_for("pane:p1").unwrap();
        assert!(notice.text().contains("idempotent replay"));
    }

    #[test]
    fn attaching_a_stream_alone_cannot_produce_terminal_input() {
        let feeds = TerminalFeeds::default();
        assert!(feeds.is_empty());
        let mut input = enabled_input();
        assert!(!input.busy());
        assert_eq!(input.text_for("pane:p1"), "");
        assert!(input.begin_send("pane:p1").is_none());
        assert_eq!(
            input.handle_key("pane:p1", &key_event("enter", None)),
            TerminalKeyAction::None
        );
    }

    #[test]
    fn notices_strip_control_and_bidi_characters() {
        let text = terminal_notice_text("denied\u{7}\u{202e} now\nretry");
        assert_eq!(text, "denied now\nretry");
        let long = "x".repeat(4096);
        assert!(terminal_notice_text(&long).len() <= 2 * 1024 + 4);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a real herdr-core and UKK_HC_BIN; run manually on macOS"]
    fn real_hc_observe_attach_feeds_text_and_reports_exit() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use ukk_hc_client::RunRequest;

        let binary = std::env::var_os("UKK_HC_BIN").unwrap_or_else(|| OsString::from("hc"));
        let connection = FramedConnection::connect(&binary).expect("real hc framed handshake");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let name = format!("ukk-d3-{}-{unique}", std::process::id());
        let run = connection
            .terminal_run(&RunRequest {
                argv: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf 'ukk-desktop-observe\\n'; sleep 4; printf 'done\\n'".into(),
                ],
                name: Some(name),
                cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                ..RunRequest::default()
            })
            .expect("launch a terminal owned by this test");
        let address = run.terminal.address.clone();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let shared = Arc::new(SharedFeed::default());
            let cancel = Arc::new(AtomicBool::new(false));
            run_worker(
                address.clone(),
                Some(connection.core_id().to_owned()),
                Arc::clone(&shared),
                cancel,
            );
            let text = shared.text();
            assert!(
                text.contains("ukk-desktop-observe"),
                "observed output was {text:?}"
            );
            assert!(
                matches!(
                    shared.status(),
                    TerminalStatus::Closed {
                        exit_code: Some(0),
                        ..
                    }
                ),
                "unexpected close status: {:?}",
                shared.status()
            );
        }));

        // Cleanup only for the terminal this test created. The Desktop view
        // itself has no path to terminal.kill or terminal.rm.
        if let Ok(cleanup) = FramedConnection::connect(&binary) {
            let _ = cleanup.terminal_kill(&address);
            let _ = cleanup.terminal_remove(&address);
        }
        result.unwrap();
    }
}
