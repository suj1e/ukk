//! Canonical unpadded base64url used by the QR and handshake payloads.
//!
//! Decoding rejects non-canonical encodings instead of accepting a second
//! spelling of the same bytes, so a transcript cannot differ while decoding to
//! the same key.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use zeroize::Zeroize;

pub(crate) fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn decode_exact<const N: usize>(value: &str) -> Option<[u8; N]> {
    let mut decoded = URL_SAFE_NO_PAD.decode(value).ok()?;
    let result = decoded.as_slice().try_into().ok();
    decoded.zeroize();
    let bytes: [u8; N] = result?;
    if encode(&bytes) != value {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_noncanonical_forms() {
        let bytes = [7_u8; 32];
        let encoded = encode(&bytes);
        assert_eq!(decode_exact::<32>(&encoded), Some(bytes));
        assert_eq!(decode_exact::<32>(&format!("{encoded}=")), None);
        assert_eq!(decode_exact::<32>(&format!("{encoded}\n")), None);
        assert_eq!(decode_exact::<31>(&encoded), None);
    }
}
