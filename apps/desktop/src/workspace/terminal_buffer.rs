//! Bounded, sanitised text for the read-only Desktop terminal observer.
//!
//! Raw PTY bytes are **not** a structured Agent event. This module only
//! removes terminal control sequences so arbitrary program output can be
//! displayed as text. It never parses, classifies or infers tool calls,
//! approvals or Agent state from the bytes.

use std::collections::VecDeque;

/// Hard byte budget for one attached terminal. Older lines are dropped first.
pub(super) const TERMINAL_BUFFER_BYTES: usize = 256 * 1024;
/// Hard line budget for one attached terminal.
pub(super) const TERMINAL_MAX_LINES: usize = 4096;
/// A single very long line (for example a minified log record) is split so one
/// line cannot defeat the byte budget.
pub(super) const TERMINAL_MAX_LINE_BYTES: usize = 8 * 1024;
/// The UI renders only this many trailing lines, keeping the latest output
/// visible without building one enormous text run.
pub(super) const TERMINAL_VIEW_LINES: usize = 400;

/// State of one explicit read-only attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TerminalStatus {
    /// The background worker is spawning `hc connect` or negotiating attach.
    Connecting,
    /// `terminal.attach {observe:true}` succeeded and bytes may arrive.
    Attached,
    /// herdr-core ended the stream, normally because the program exited.
    Closed {
        reason: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    /// The local client could not attach or the stream failed. This is not an
    /// Agent error and never implies the terminal program stopped.
    Failed(String),
}

impl TerminalStatus {
    pub(super) fn label(&self) -> String {
        match self {
            Self::Connecting => "Connecting to herdr-core…".to_owned(),
            Self::Attached => "Attached · read-only observation".to_owned(),
            Self::Closed {
                reason,
                exit_code,
                signal,
            } => {
                let mut label = format!("Stream closed · {reason}");
                if let Some(exit_code) = exit_code {
                    label.push_str(&format!(" · exit {exit_code}"));
                }
                if let Some(signal) = signal {
                    label.push_str(&format!(" · signal {signal}"));
                }
                label
            }
            Self::Failed(message) => format!("Attach failed · {message}"),
        }
    }

    /// Attached plus a byte counter is the visible "progress" state. The byte
    /// count is raw stream bytes consumed, not parsed Agent progress.
    pub(super) fn with_progress(&self, received: u64) -> String {
        match self {
            Self::Attached if received == 0 => "Attached · waiting for output".to_owned(),
            Self::Attached => format!("Attached · {received} bytes received"),
            other => other.label(),
        }
    }

    pub(super) fn has_ended(&self) -> bool {
        matches!(self, Self::Closed { .. } | Self::Failed(_))
    }
}

/// A bounded line ring. It stores already-sanitised text so the UI never has
/// to retain raw escape sequences or an unbounded scrollback.
#[derive(Debug)]
pub(super) struct TerminalBuffer {
    lines: VecDeque<String>,
    bytes: usize,
    partial: String,
    max_bytes: usize,
    max_lines: usize,
    max_line_bytes: usize,
    dropped_lines: u64,
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::new(
            TERMINAL_BUFFER_BYTES,
            TERMINAL_MAX_LINES,
            TERMINAL_MAX_LINE_BYTES,
        )
    }
}

impl TerminalBuffer {
    pub(super) fn new(max_bytes: usize, max_lines: usize, max_line_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            partial: String::new(),
            max_bytes,
            max_lines,
            max_line_bytes,
            dropped_lines: 0,
        }
    }

    /// Append sanitised text. `\n` starts a new displayed line; `\r` is
    /// ignored so CRLF output does not produce blank rows.
    pub(super) fn push(&mut self, text: &str) {
        for character in text.chars() {
            match character {
                '\n' => self.flush_partial(),
                '\r' => {}
                character if character.is_control() => {}
                character => {
                    self.partial.push(character);
                    if self.partial.len() >= self.max_line_bytes {
                        self.flush_partial();
                    }
                }
            }
        }
    }

    fn flush_partial(&mut self) {
        let line = std::mem::take(&mut self.partial);
        self.bytes = self.bytes.saturating_add(line.len().saturating_add(1));
        self.lines.push_back(line);
        while !self.lines.is_empty()
            && (self.bytes > self.max_bytes || self.lines.len() > self.max_lines)
        {
            let Some(dropped) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(dropped.len().saturating_add(1));
            self.dropped_lines = self.dropped_lines.saturating_add(1);
        }
    }

    /// The complete bounded history as plain text.
    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        let mut output = String::with_capacity(self.bytes.min(self.max_bytes));
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(line);
        }
        if !self.partial.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.partial);
        }
        output
    }

    /// Only the newest `max_lines` rendered lines. This keeps the visible tail
    /// live without copying the full ring into a single text run.
    pub(super) fn tail(&self, max_lines: usize) -> String {
        if max_lines == 0 {
            return String::new();
        }
        let mut output = String::new();
        let start = self.lines.len().saturating_sub(max_lines);
        for (index, line) in self.lines.iter().skip(start).enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(line);
        }
        if !self.partial.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.partial);
        }
        output
    }

    pub(super) fn line_count(&self) -> usize {
        self.lines.len() + usize::from(!self.partial.is_empty())
    }

    pub(super) fn byte_len(&self) -> usize {
        self.bytes + self.partial.len()
    }

    pub(super) fn dropped_lines(&self) -> u64 {
        self.dropped_lines
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

/// Incremental terminal-output sanitiser.
///
/// CSI and OSC sequences may be split across arbitrary `DATA` frames, so the
/// scanner keeps a small state between chunks. Invalid UTF-8 is replaced
/// lossily; a trailing partial code point is retained until the next chunk.
/// NUL, BEL, backspace and other C0 controls are removed. Newlines and tabs
/// survive so the output remains readable.
#[derive(Debug, Default)]
pub(super) struct TerminalSanitizer {
    state: ScanState,
    pending_utf8: Vec<u8>,
}

impl TerminalSanitizer {
    pub(super) fn push(&mut self, bytes: &[u8]) -> String {
        let mut output = String::with_capacity(bytes.len().min(4096));
        for &byte in bytes {
            self.step(byte, &mut output);
        }
        self.drain_utf8(&mut output, false);
        output
    }

    /// Flush any trailing incomplete UTF-8 code point as U+FFFD.
    pub(super) fn finish(&mut self) -> String {
        let mut output = String::new();
        self.drain_utf8(&mut output, true);
        self.state = ScanState::Ground;
        output
    }

    fn step(&mut self, byte: u8, output: &mut String) {
        match self.state {
            ScanState::Ground => match byte {
                0x1b => {
                    self.drain_utf8(output, true);
                    self.state = ScanState::Escape;
                }
                // C1 CSI/OSC and other control strings are only recognised
                // when they cannot be a UTF-8 continuation byte.
                0x9b if self.pending_utf8.is_empty() => {
                    self.drain_utf8(output, true);
                    self.state = ScanState::Csi;
                }
                0x9d if self.pending_utf8.is_empty() => {
                    self.drain_utf8(output, true);
                    self.state = ScanState::Osc;
                }
                0x90 | 0x98 | 0x9e | 0x9f if self.pending_utf8.is_empty() => {
                    self.drain_utf8(output, true);
                    self.state = ScanState::ControlString;
                }
                0x9c if self.pending_utf8.is_empty() => {
                    self.drain_utf8(output, true);
                }
                b'\n' => {
                    self.drain_utf8(output, true);
                    output.push('\n');
                }
                b'\t' => {
                    self.drain_utf8(output, true);
                    output.push('\t');
                }
                0x00..=0x1f | 0x7f => {
                    self.drain_utf8(output, true);
                }
                _ => {
                    self.pending_utf8.push(byte);
                    self.drain_utf8(output, false);
                }
            },
            ScanState::Escape => match byte {
                0x1b => self.state = ScanState::Escape,
                b'[' => self.state = ScanState::Csi,
                b']' => self.state = ScanState::Osc,
                b'P' | b'X' | b'^' | b'_' => self.state = ScanState::ControlString,
                0x20..=0x2f => {}
                0x30..=0x7e => self.state = ScanState::Ground,
                0x18 | 0x1a => self.state = ScanState::Ground,
                _ => self.state = ScanState::Ground,
            },
            ScanState::Csi => match byte {
                0x1b => self.state = ScanState::Escape,
                0x9b => self.state = ScanState::Csi,
                0x20..=0x3f => {}
                0x40..=0x7e => self.state = ScanState::Ground,
                0x18 | 0x1a => self.state = ScanState::Ground,
                _ => self.state = ScanState::Ground,
            },
            ScanState::Osc => match byte {
                0x07 => self.state = ScanState::Ground,
                0x1b => self.state = ScanState::OscEscape,
                0x9c => self.state = ScanState::Ground,
                _ => {}
            },
            ScanState::OscEscape => match byte {
                b'\\' => self.state = ScanState::Ground,
                0x1b => self.state = ScanState::OscEscape,
                _ => self.state = ScanState::Osc,
            },
            ScanState::ControlString => match byte {
                0x1b => self.state = ScanState::ControlStringEscape,
                0x9c => self.state = ScanState::Ground,
                _ => {}
            },
            ScanState::ControlStringEscape => match byte {
                b'\\' => self.state = ScanState::Ground,
                _ => self.state = ScanState::ControlString,
            },
        }
    }

    fn drain_utf8(&mut self, output: &mut String, final_flush: bool) {
        loop {
            match std::str::from_utf8(&self.pending_utf8) {
                Ok(text) => {
                    output.push_str(text);
                    self.pending_utf8.clear();
                    return;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        if let Ok(text) = std::str::from_utf8(&self.pending_utf8[..valid]) {
                            output.push_str(text);
                        }
                    }
                    match error.error_len() {
                        Some(invalid) => {
                            output.push('\u{FFFD}');
                            self.pending_utf8.drain(..valid + invalid);
                        }
                        None => {
                            self.pending_utf8.drain(..valid);
                            if final_flush && !self.pending_utf8.is_empty() {
                                output.push('\u{FFFD}');
                                self.pending_utf8.clear();
                            }
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_but_keeps_text_and_newlines() {
        let mut sanitizer = TerminalSanitizer::default();
        let text = sanitizer.push(b"hello \x1b[31mred\x1b[0m\n\x1b]0;title\x07world");
        assert_eq!(text, "hello red\nworld");
    }

    #[test]
    fn strips_sequences_split_across_chunks() {
        let mut sanitizer = TerminalSanitizer::default();
        assert_eq!(sanitizer.push(b"before\x1b[3"), "before");
        assert_eq!(sanitizer.push(b"1mred"), "red");
        assert_eq!(sanitizer.push(b"\x1b]8;;http://x\x1b\\link"), "link");
    }

    #[test]
    fn replaces_invalid_utf8_lossily_and_completes_split_code_points() {
        let mut sanitizer = TerminalSanitizer::default();
        assert_eq!(sanitizer.push(b"a\xffb"), "a\u{FFFD}b");
        assert_eq!(sanitizer.push(b"\xE2\x82"), "");
        assert_eq!(sanitizer.push(b"\xAC"), "€");
        assert_eq!(sanitizer.push(b"tail\xE2"), "tail");
        assert_eq!(sanitizer.finish(), "\u{FFFD}");
    }

    #[test]
    fn removes_control_characters_but_keeps_tabs() {
        let mut sanitizer = TerminalSanitizer::default();
        assert_eq!(sanitizer.push(b"a\x00b\x07c\x08d\te"), "abcd\te");
    }

    #[test]
    fn buffer_bounds_bytes_and_lines() {
        let mut buffer = TerminalBuffer::new(16, 3, 8);
        buffer.push("one\ntwo\nthree\nfour\n");
        assert!(buffer.byte_len() <= 16);
        assert!(buffer.line_count() <= 3);
        assert!(buffer.dropped_lines() >= 1);
        assert!(!buffer.text().contains("one"));
        assert!(buffer.text().contains("four"));
    }

    #[test]
    fn buffer_keeps_partial_line_without_inventing_a_newline() {
        let mut buffer = TerminalBuffer::new(64, 8, 16);
        buffer.push("alpha\nbeta");
        assert_eq!(buffer.text(), "alpha\nbeta");
        buffer.push(" gamma");
        assert_eq!(buffer.text(), "alpha\nbeta gamma");
        buffer.push("\n");
        assert_eq!(buffer.text(), "alpha\nbeta gamma");
    }

    #[test]
    fn buffer_splits_an_unbounded_single_line() {
        let mut buffer = TerminalBuffer::new(64, 8, 8);
        buffer.push("0123456789abcdef");
        assert!(buffer.line_count() >= 2);
        assert!(buffer.byte_len() <= 64);
    }

    #[test]
    fn tail_renders_only_the_newest_lines() {
        let mut buffer = TerminalBuffer::default();
        buffer.push("one\ntwo\nthree\nfour\n");
        assert_eq!(buffer.tail(2), "three\nfour");
    }

    #[test]
    fn status_mapping_is_truthful() {
        assert_eq!(
            TerminalStatus::Connecting.label(),
            "Connecting to herdr-core…"
        );
        assert_eq!(
            TerminalStatus::Attached.with_progress(0),
            "Attached · waiting for output"
        );
        assert_eq!(
            TerminalStatus::Attached.with_progress(12),
            "Attached · 12 bytes received"
        );
        assert!(TerminalStatus::Closed {
            reason: "exited".into(),
            exit_code: Some(0),
            signal: None,
        }
        .has_ended());
        assert!(!TerminalStatus::Attached.has_ended());
        assert!(TerminalStatus::Failed("no hc".into())
            .label()
            .contains("no hc"));
    }
}
