//! Regression test for the LZW decode boundary case fixed in
//! `decompress_lzw` (`tiff-reader/src/filters.rs`): once decoded output
//! reaches the caller-declared exact block size, the decoder must return
//! success immediately rather than also validating whatever bits (if any)
//! remain in the input past that point.
//!
//! Fixture: `synthetic_exact_size_trailing_bits.lzw` is a **fully synthetic**
//! (no third-party or licensed image data) LZW stream built as follows:
//!
//! 1. Generate a deterministic 300-byte payload with the LCG below (seed
//!    `0x2545F491`, the same generator this test uses to recompute the
//!    expected bytes -- so the fixture's *expected decoded content* never
//!    needs to be hand-copied into the test).
//! 2. LZW-encode that payload with `weezl`'s TIFF-compatible encoder
//!    (`Encoder::with_tiff_size_switch`), which emits a leading `ClearCode`,
//!    the data codes, a trailing `EndOfInformation` code, then pads to a
//!    byte boundary -- a normal, cleanly-terminated stream.
//! 3. Drop the last byte of that stream. That byte holds only the tail of
//!    the `EndOfInformation` code plus its padding bits; every
//!    payload-producing code is still fully intact and decodes correctly.
//!    What remains after the last real code is now an incomplete code
//!    fragment that cannot be completed from the truncated input.
//!
//! This reproduces precisely the condition the fix guards: decoding the
//! truncated stream with `decoded_len_limit` set to the payload's exact
//! length (300) makes `decompress_lzw` produce all 300 correct bytes and
//! *then* attempt to read one more code from the leftover fragment. Verified
//! directly against this fixture: with the guard removed, that attempt
//! surfaces as `Error::DecompressionFailed { reason: "LZW: stream ended
//! before end marker" }` even though the 300 real bytes were already decoded
//! correctly; with the guard in place (current code), decoding returns
//! `Ok` with the exact original 300 bytes.

use std::path::{Path, PathBuf};

use tiff_reader::filters;

const PAYLOAD_LEN: usize = 300;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../testdata/lzw-libtiff")
        .join("synthetic_exact_size_trailing_bits.lzw")
}

/// Regenerate the deterministic synthetic payload the fixture encodes.
/// Same LCG used to build the fixture in the first place (see module docs):
/// `state = state.wrapping_mul(1_103_515_245).wrapping_add(12345)`, taking
/// bits 16..24 of `state` as each output byte.
fn synthetic_payload(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u32 = 0x2545_F491;
    for _ in 0..len {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
        out.push((state >> 16) as u8);
    }
    out
}

#[test]
fn decodes_synthetic_lzw_stream_at_exact_declared_size_with_trailing_bits() {
    let compressed = std::fs::read(fixture()).expect("read synthetic LZW fixture");
    // 1 byte shorter than the full, cleanly-EOI-terminated encode of the
    // 300-byte payload -- the dropped byte held only EOI tail + padding.
    assert_eq!(compressed.len(), 345);

    let expected = synthetic_payload(PAYLOAD_LEN);

    // This is the call that fails without the fix:
    //   DecompressionFailed { index: 0, reason: "LZW: stream ended before end marker" }
    // even though the 300 real payload bytes are already fully decoded by
    // the time that error would otherwise fire.
    let decoded = filters::decompress(
        /* LZW compression code */ 5,
        &compressed,
        0,
        None,
        PAYLOAD_LEN,
    )
    .expect("decode synthetic LZW stream at exact declared size");

    assert_eq!(decoded.len(), PAYLOAD_LEN);
    assert_eq!(decoded, expected);
}

#[test]
fn full_untruncated_stream_still_decodes_cleanly() {
    // Sanity/contrast case: re-encoding the same payload and decoding the
    // *full*, untruncated stream (real EndOfInformation intact) must still
    // succeed byte-exact -- the fix only changes behavior for the
    // exact-boundary-with-leftover-bits case above, never the normal path.
    let expected = synthetic_payload(PAYLOAD_LEN);
    let mut encoder = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let full_compressed = encoder.encode(&expected).expect("weezl encode payload");

    let decoded = filters::decompress(5, &full_compressed, 0, None, PAYLOAD_LEN)
        .expect("decode full, cleanly-terminated stream");
    assert_eq!(decoded, expected);
}
