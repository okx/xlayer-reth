//! Shared compression helpers for the flashblocks wire protocol.
//!
//! The wire format is auto-detecting: a payload is treated as legacy uncompressed
//! JSON if its first non-whitespace byte is `{`, otherwise it is decoded as a
//! brotli-compressed frame. This lets us upgrade the producer to emit compressed
//! frames without breaking older subscribers, and lets us replay legacy
//! persistence files unchanged.

use std::{borrow::Cow, io};

use alloy_primitives::Bytes;
use brotli::enc::BrotliEncoderParams;

/// Brotli quality level. Q5 is the empirical sweet spot for JSON-shaped
/// flashblock payloads — sub-10ms encode at ~5x size reduction on 1.2MB inputs.
pub const BROTLI_QUALITY: u32 = 5;

/// Brotli sliding-window size in log2 bytes. 22 is the brotli default.
pub const BROTLI_LGWIN: u32 = 22;

/// Compress `json` bytes with brotli at [`BROTLI_QUALITY`].
pub fn brotli_encode(json: &[u8]) -> io::Result<Bytes> {
    let mut compressed = Vec::with_capacity(json.len() / 4);
    let params = BrotliEncoderParams {
        quality: BROTLI_QUALITY as i32,
        lgwin: BROTLI_LGWIN as i32,
        ..Default::default()
    };
    let mut input = json;
    brotli::BrotliCompress(&mut input, &mut compressed, &params)?;
    Ok(Bytes::from(compressed))
}

/// Decode a wire frame, auto-detecting between legacy uncompressed JSON and
/// brotli-compressed bytes.
///
/// Returns the input borrowed if it is already JSON (leading `{` after
/// optional whitespace), otherwise allocates a decompressed buffer.
pub fn try_decompress(bytes: &[u8]) -> io::Result<Cow<'_, [u8]>> {
    if bytes.trim_ascii_start().starts_with(b"{") {
        return Ok(Cow::Borrowed(bytes));
    }

    let mut decompressor = brotli::Decompressor::new(bytes, 4096);
    let mut decompressed = Vec::new();
    io::copy(&mut decompressor, &mut decompressed)?;
    Ok(Cow::Owned(decompressed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brotli_round_trip() {
        let original = br#"{"hello":"world","numbers":[1,2,3],"nested":{"a":"b"}}"#;
        let compressed = brotli_encode(original).expect("encode succeeds");
        assert!(!compressed.is_empty(), "compressed output is non-empty");
        let decoded = try_decompress(&compressed).expect("decompress succeeds");
        assert_eq!(decoded.as_ref(), original);
    }

    #[test]
    fn brotli_compresses_repetitive_input() {
        // EIP-7928 access list JSON is highly repetitive; expect significant compression.
        let original: Vec<u8> =
            br#"{"address":"0x0000000000000000000000000000000000000042","balance_changes":[{"block_access_index":0,"post_balance":"0x100"}]}"#
                .repeat(100);
        let compressed = brotli_encode(&original).expect("encode succeeds");
        assert!(compressed.len() < original.len() / 5, "expected >5x compression ratio");
    }

    #[test]
    fn auto_detect_legacy_json_passthrough() {
        let json = br#"{"key":"value"}"#;
        let result = try_decompress(json).expect("passthrough succeeds");
        assert_eq!(result.as_ref(), json);
        assert!(matches!(result, Cow::Borrowed(_)), "legacy JSON must be borrowed, not copied");
    }

    #[test]
    fn auto_detect_legacy_json_with_leading_whitespace() {
        let json = b"   \n\t  {\"key\":\"value\"}";
        let result = try_decompress(json).expect("passthrough succeeds");
        assert_eq!(result.as_ref(), json);
    }

    #[test]
    fn auto_detect_brotli_decompresses() {
        let original = br#"{"key":"value","data":"some larger payload for brotli to compress"}"#;
        let compressed = brotli_encode(original).expect("encode");
        assert!(
            !compressed.starts_with(b"{"),
            "compressed output must not start with `{{` (would defeat auto-detect)"
        );
        let result = try_decompress(&compressed).expect("decompress succeeds");
        assert_eq!(result.as_ref(), original);
        assert!(matches!(result, Cow::Owned(_)), "compressed input must allocate decoded buffer");
    }

    #[test]
    fn malformed_brotli_returns_err() {
        // Bytes that don't start with `{` and aren't valid brotli.
        let garbage = &[0xff_u8, 0xff, 0xff, 0xff, 0xff];
        let result = try_decompress(garbage);
        assert!(result.is_err(), "malformed brotli must error, not silently pass through");
    }
}
