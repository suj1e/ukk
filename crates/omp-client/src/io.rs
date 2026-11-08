//! Bounded line reading. A child cannot make the Host allocate without limit.

use std::io::{self, BufRead};

pub(crate) enum LineRead {
    Line(Vec<u8>),
    Eof,
    /// The line exceeded `max`. It was drained through its newline so the
    /// next frame can still be parsed; the bytes are never buffered.
    Oversized {
        bytes: usize,
    },
}

/// Read one `\n`-terminated line, keeping at most `max` bytes. An oversized
/// line is drained up to `drain_cap`; beyond that the stream is considered
/// broken and the caller stops the child.
pub(crate) fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max: usize,
    drain_cap: usize,
) -> io::Result<LineRead> {
    let mut line: Vec<u8> = Vec::new();
    let mut oversized = false;
    let mut oversized_bytes = 0usize;
    loop {
        let (consume, outcome) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                if oversized {
                    return Ok(LineRead::Oversized {
                        bytes: oversized_bytes,
                    });
                }
                if line.is_empty() {
                    return Ok(LineRead::Eof);
                }
                return Ok(LineRead::Line(line));
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            if oversized {
                if let Some(position) = newline {
                    (
                        position + 1,
                        Some(LineRead::Oversized {
                            bytes: oversized_bytes.saturating_add(position),
                        }),
                    )
                } else {
                    oversized_bytes = oversized_bytes.saturating_add(buffer.len());
                    (buffer.len(), None)
                }
            } else {
                let available = newline.unwrap_or(buffer.len());
                if line.len() + available > max {
                    oversized = true;
                    oversized_bytes = line.len() + available;
                    line.clear();
                    line.shrink_to_fit();
                    if let Some(position) = newline {
                        (
                            position + 1,
                            Some(LineRead::Oversized {
                                bytes: oversized_bytes,
                            }),
                        )
                    } else {
                        (available, None)
                    }
                } else if let Some(position) = newline {
                    line.extend_from_slice(&buffer[..position]);
                    (
                        position + 1,
                        Some(LineRead::Line(std::mem::take(&mut line))),
                    )
                } else {
                    line.extend_from_slice(buffer);
                    (available, None)
                }
            }
        };
        reader.consume(consume);
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        if oversized && oversized_bytes > drain_cap {
            return Err(io::Error::other(
                "oversized frame never terminated by a newline",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn read(input: &[u8], max: usize) -> LineRead {
        let mut reader = BufReader::new(input);
        read_bounded_line(&mut reader, max, max * 2).unwrap()
    }

    #[test]
    fn reads_lines_and_stops_at_eof() {
        match read(b"one\ntwo\n", 16) {
            LineRead::Line(bytes) => assert_eq!(bytes, b"one"),
            _ => panic!("expected a line"),
        }
        let mut reader = BufReader::new(&b""[..]);
        assert!(matches!(
            read_bounded_line(&mut reader, 16, 32).unwrap(),
            LineRead::Eof
        ));
    }

    #[test]
    fn oversized_line_is_drained_without_buffering() {
        let mut reader = BufReader::new(&b"aaaaaaaaa\nok\n"[..]);
        match read_bounded_line(&mut reader, 4, 64).unwrap() {
            LineRead::Oversized { bytes } => assert_eq!(bytes, 9),
            _ => panic!("expected an oversized line"),
        }
        match read_bounded_line(&mut reader, 4, 64).unwrap() {
            LineRead::Line(bytes) => assert_eq!(bytes, b"ok"),
            _ => panic!("expected the next line to survive"),
        }
    }
}
