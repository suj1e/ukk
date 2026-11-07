//! Framed-mode wire format: `[channel u32 LE][kind u8][len u32 LE][payload]`.
//!
//! The payload limit and the kind table come from herdr-core's
//! `schema/README.md` at v0.1.18. Unknown kinds are preserved so a forward
//! compatible core can add one without this client inventing behaviour.

use crate::error::{FrameError, HcError, Result};
use crate::types::MAX_FRAME_PAYLOAD;
use std::io::{Read, Write};
use std::sync::Mutex;

pub const HEADER_LEN: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Data = 0,
    Credit = 1,
    Close = 2,
    Resize = 3,
    Focus = 4,
    Facts = 5,
    AutomaticInput = 6,
}

impl FrameKind {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Data,
            1 => Self::Credit,
            2 => Self::Close,
            3 => Self::Resize,
            4 => Self::Focus,
            5 => Self::Facts,
            6 => Self::AutomaticInput,
            _ => return None,
        })
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Data {
        channel: u32,
        payload: Vec<u8>,
    },
    Credit {
        channel: u32,
        bytes: u32,
    },
    Close {
        channel: u32,
        payload: Vec<u8>,
    },
    Resize {
        channel: u32,
        cols: u16,
        rows: u16,
    },
    Focus {
        channel: u32,
        gained: bool,
    },
    Facts {
        channel: u32,
        payload: Vec<u8>,
    },
    AutomaticInput {
        channel: u32,
        payload: Vec<u8>,
    },
    /// A kind this client does not know. The connection ignores it, matching
    /// the documented rule that old cores ignore unknown local frame kinds.
    Unknown {
        channel: u32,
        kind: u8,
        payload: Vec<u8>,
    },
}

impl Frame {
    pub fn channel(&self) -> u32 {
        match self {
            Self::Data { channel, .. }
            | Self::Credit { channel, .. }
            | Self::Close { channel, .. }
            | Self::Resize { channel, .. }
            | Self::Focus { channel, .. }
            | Self::Facts { channel, .. }
            | Self::AutomaticInput { channel, .. }
            | Self::Unknown { channel, .. } => *channel,
        }
    }

    pub fn payload_len(&self) -> usize {
        match self {
            Self::Data { payload, .. }
            | Self::Close { payload, .. }
            | Self::Facts { payload, .. }
            | Self::AutomaticInput { payload, .. }
            | Self::Unknown { payload, .. } => payload.len(),
            Self::Credit { .. } | Self::Resize { .. } | Self::Focus { .. } => 0,
        }
    }
}

/// Reads frames from a byte stream. The reader never allocates more than the
/// documented payload limit: an oversized header is rejected before the
/// length is used.
pub struct FrameReader<R: Read> {
    inner: R,
}

impl<R: Read> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// `Ok(None)` means a clean EOF between frames. A partial header or
    /// payload is a hard error: the byte stream cannot be resynchronised.
    pub fn read_frame(&mut self) -> Result<Option<Frame>> {
        let mut header = [0u8; HEADER_LEN];
        let mut read = 0;
        while read < HEADER_LEN {
            match self.inner.read(&mut header[read..]) {
                Ok(0) if read == 0 => return Ok(None),
                Ok(0) => {
                    return Err(FrameError::Truncated {
                        expected: HEADER_LEN,
                        got: read,
                    }
                    .into())
                }
                Ok(count) => read += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let channel = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
        let kind = header[4];
        let length = u32::from_le_bytes(header[5..9].try_into().expect("4 bytes")) as usize;
        if length > MAX_FRAME_PAYLOAD {
            return Err(FrameError::Oversized {
                length,
                limit: MAX_FRAME_PAYLOAD,
            }
            .into());
        }
        let mut payload = vec![0u8; length];
        let mut read = 0;
        while read < length {
            match self.inner.read(&mut payload[read..]) {
                Ok(0) => {
                    return Err(FrameError::Truncated {
                        expected: length,
                        got: read,
                    }
                    .into())
                }
                Ok(count) => read += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        decode_frame(channel, kind, payload)
    }
}

fn decode_frame(channel: u32, kind: u8, payload: Vec<u8>) -> Result<Option<Frame>> {
    let Some(kind) = FrameKind::from_u8(kind) else {
        return Ok(Some(Frame::Unknown {
            channel,
            kind,
            payload,
        }));
    };
    Ok(Some(match kind {
        FrameKind::Data => Frame::Data { channel, payload },
        FrameKind::AutomaticInput => Frame::AutomaticInput { channel, payload },
        FrameKind::Facts => Frame::Facts { channel, payload },
        FrameKind::Close => Frame::Close { channel, payload },
        FrameKind::Credit => {
            if payload.len() != 4 {
                return Err(FrameError::InvalidPayload(format!(
                    "CREDIT payload must be 4 bytes, got {}",
                    payload.len()
                ))
                .into());
            }
            Frame::Credit {
                channel,
                bytes: u32::from_le_bytes(payload.try_into().expect("4 bytes")),
            }
        }
        FrameKind::Resize => {
            if payload.len() != 4 {
                return Err(FrameError::InvalidPayload(format!(
                    "RESIZE payload must be 4 bytes, got {}",
                    payload.len()
                ))
                .into());
            }
            Frame::Resize {
                channel,
                cols: u16::from_le_bytes(payload[0..2].try_into().expect("2 bytes")),
                rows: u16::from_le_bytes(payload[2..4].try_into().expect("2 bytes")),
            }
        }
        FrameKind::Focus => {
            if payload.len() != 1 {
                return Err(FrameError::InvalidPayload(format!(
                    "FOCUS payload must be 1 byte, got {}",
                    payload.len()
                ))
                .into());
            }
            Frame::Focus {
                channel,
                gained: payload[0] != 0,
            }
        }
    }))
}

/// Serializes exactly one frame. The returned bytes are safe to write in one
/// `write_all`, so concurrent senders cannot interleave headers.
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>> {
    let (channel, kind_byte, payload): (u32, u8, Vec<u8>) = match frame {
        Frame::Data { channel, payload } => (*channel, FrameKind::Data.as_u8(), payload.clone()),
        Frame::AutomaticInput { channel, payload } => {
            (*channel, FrameKind::AutomaticInput.as_u8(), payload.clone())
        }
        Frame::Facts { channel, payload } => (*channel, FrameKind::Facts.as_u8(), payload.clone()),
        Frame::Close { channel, payload } => (*channel, FrameKind::Close.as_u8(), payload.clone()),
        Frame::Credit { channel, bytes } => (
            *channel,
            FrameKind::Credit.as_u8(),
            bytes.to_le_bytes().to_vec(),
        ),
        Frame::Resize {
            channel,
            cols,
            rows,
        } => {
            let mut payload = Vec::with_capacity(4);
            payload.extend_from_slice(&cols.to_le_bytes());
            payload.extend_from_slice(&rows.to_le_bytes());
            (*channel, FrameKind::Resize.as_u8(), payload)
        }
        Frame::Focus { channel, gained } => {
            (*channel, FrameKind::Focus.as_u8(), vec![u8::from(*gained)])
        }
        Frame::Unknown {
            channel,
            kind,
            payload,
        } => (*channel, *kind, payload.clone()),
    };
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(FrameError::Oversized {
            length: payload.len(),
            limit: MAX_FRAME_PAYLOAD,
        }
        .into());
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend_from_slice(&channel.to_le_bytes());
    bytes.push(kind_byte);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

/// A writer shared by the connection and every attachment. Each frame is
/// serialized first and written under one mutex, so interleaving is
/// impossible even with concurrent input.
pub struct FrameWriter {
    inner: Mutex<Box<dyn Write + Send>>,
}

impl FrameWriter {
    pub fn new(writer: impl Write + Send + 'static) -> Self {
        Self {
            inner: Mutex::new(Box::new(writer)),
        }
    }

    pub fn send(&self, frame: &Frame) -> Result<()> {
        let bytes = encode_frame(frame)?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| HcError::Protocol("frame writer lock poisoned".into()))?;
        inner.write_all(&bytes)?;
        inner.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_all(bytes: Vec<u8>) -> Result<Vec<Frame>> {
        let mut reader = FrameReader::new(Cursor::new(bytes));
        let mut frames = Vec::new();
        while let Some(frame) = reader.read_frame()? {
            frames.push(frame);
        }
        Ok(frames)
    }

    #[test]
    fn round_trips_every_documented_kind() {
        let frames = vec![
            Frame::Data {
                channel: 3,
                payload: b"hello".to_vec(),
            },
            Frame::Credit {
                channel: 3,
                bytes: 1024,
            },
            Frame::Close {
                channel: 3,
                payload: b"{\"reason\":\"exited\"}".to_vec(),
            },
            Frame::Resize {
                channel: 3,
                cols: 120,
                rows: 40,
            },
            Frame::Focus {
                channel: 3,
                gained: true,
            },
            Frame::Facts {
                channel: 3,
                payload: b"{}".to_vec(),
            },
            Frame::AutomaticInput {
                channel: 3,
                payload: b"\x1b[1;1R".to_vec(),
            },
            Frame::Unknown {
                channel: 9,
                kind: 77,
                payload: b"future".to_vec(),
            },
        ];
        let mut bytes = Vec::new();
        for frame in &frames {
            bytes.extend(encode_frame(frame).unwrap());
        }
        assert_eq!(read_all(bytes).unwrap(), frames);
    }

    #[test]
    fn partial_header_and_payload_are_typed_truncation() {
        let mut complete = encode_frame(&Frame::Data {
            channel: 1,
            payload: vec![7; 32],
        })
        .unwrap();
        complete.truncate(4);
        assert!(matches!(
            read_all(complete),
            Err(HcError::Frame(FrameError::Truncated { .. }))
        ));

        let mut complete = encode_frame(&Frame::Data {
            channel: 1,
            payload: vec![7; 32],
        })
        .unwrap();
        complete.truncate(20);
        assert!(matches!(
            read_all(complete),
            Err(HcError::Frame(FrameError::Truncated { .. }))
        ));
    }

    #[test]
    fn oversized_length_is_rejected_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(FrameKind::Data.as_u8());
        bytes.extend_from_slice(&((MAX_FRAME_PAYLOAD as u32) + 1).to_le_bytes());
        assert!(matches!(
            read_all(bytes),
            Err(HcError::Frame(FrameError::Oversized { .. }))
        ));
    }

    #[test]
    fn malformed_control_payloads_are_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(FrameKind::Credit.as_u8());
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        assert!(matches!(
            read_all(bytes),
            Err(HcError::Frame(FrameError::InvalidPayload(_)))
        ));
    }

    #[test]
    fn clean_eof_between_frames_is_not_an_error() {
        let bytes = encode_frame(&Frame::Data {
            channel: 1,
            payload: b"x".to_vec(),
        })
        .unwrap();
        assert_eq!(read_all(bytes).unwrap().len(), 1);
    }
}
