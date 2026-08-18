//! Coverage for the `write_block` LZW path, in two independent legs.
//!
//! Context (see `docs/ONYX-FORK-CHANGES.md` §5): the concern is whether an LZW
//! strip written through `write_block` with `Compression::Lzw` — for the
//! halftone-shaped **sub-byte** (1/2/4-bit) case — is byte-for-byte the same
//! strip a `tif.c`-correct writer would produce (leading `ClearCode`, code
//! stream, trailing `EndOfInformation`). Two things must hold, and the fork
//! reader alone cannot prove the second (it shares the writer's
//! packing<->unpacking, so a writer→fork-reader round trip is self-consistent
//! by construction):
//!
//! 1. The LZW **encoder** is one-shot / finished — proven byte-identical to
//!    `into_stream().encode_all()` by the `tiff-writer` unit test
//!    `compress_lzw_matches_one_shot_encode_all_and_is_decodable`.
//! 2. The bytes **fed** to LZW are the `tif.c`-correct MSB-first packed rows,
//!    produced by `compress_block_subbyte` BEFORE the encoder runs.
//!
//! The `writeblock_lzw_..._roundtrips_exactly` tests below cover the composed
//! write→fork-reader round trip (necessary, but self-consistent). The
//! `writeblock_subbyte_lzw_packing_matches_independent_msb_packer` tests close
//! leg 2: they LZW-decode the raw strip bytes and compare against an
//! **independent** MSB-first bit-packer written here (NOT the fork's
//! `pack_subbyte_rows` / `unpack_subbyte_block`). Chaining that against fact 1
//! shows the fork's sub-byte LZW strip equals `encode_all(tif.c-correct
//! packing)` — i.e. exactly the `encode_all`/`tif.c`-validated stream Onyx's
//! option (b) used. `subbyte_lzw_decodes_with_independent_image_rs_tiff` adds a
//! fully independent decoder leg where image-rs `tiff` supports the shape.

use std::io::Cursor;

use tiff_core::{Compression, PlanarConfiguration};
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};
use weezl::BitOrder;

/// Deterministic high-entropy byte stream (xorshift), each value masked into
/// the sub-byte range so it packs without error.
fn pseudo_random_samples(count: usize, max_value: u8) -> Vec<u8> {
    let mut state: u32 = 0x9E37_79B9;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.push((state as u8) & max_value);
    }
    out
}

/// Chunky sub-byte round trip through the incremental `write_block` LZW path.
fn roundtrip_chunky_subbyte_lzw(bits_per_sample: u16, width: u32, height: u32, values: &[u8]) {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(bits_per_sample)
        .compression(Compression::Lzw)
        .strips(height); // single strip => one large incremental LZW encode
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(
        decoded_values, values,
        "incremental write_block LZW output was not decodable byte-exact by the strict reader \
         (bits={bits_per_sample}, {width}x{height})"
    );
}

#[test]
fn chunky_2bit_large_strip_lzw_roundtrips_exactly() {
    // 4096 cols x 8 rows @ 2bpp => 1024 bytes/row, one 8192-byte high-entropy
    // strip, exercising the encoder's dictionary / code-size machinery well
    // beyond a trivial input (the exact code-size transitions are not asserted).
    let width = 4096u32;
    let height = 8u32;
    let values = pseudo_random_samples((width * height) as usize, 0b11);
    roundtrip_chunky_subbyte_lzw(2, width, height, &values);
}

#[test]
fn chunky_1bit_large_strip_lzw_roundtrips_exactly() {
    let width = 8192u32;
    let height = 8u32;
    let values = pseudo_random_samples((width * height) as usize, 0b1);
    roundtrip_chunky_subbyte_lzw(1, width, height, &values);
}

#[test]
fn chunky_4bit_large_strip_lzw_roundtrips_exactly() {
    let width = 2048u32;
    let height = 8u32;
    let values = pseudo_random_samples((width * height) as usize, 0b1111);
    roundtrip_chunky_subbyte_lzw(4, width, height, &values);
}

#[test]
fn chunky_8bit_large_strip_lzw_roundtrips_exactly() {
    let width = 1024u32;
    let height = 16u32;
    let values = pseudo_random_samples((width * height) as usize, 0xFF);
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .sample_type::<u8>()
        .compression(Compression::Lzw)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

/// The Onyx halftone shape: 2-bit, 4-channel, planar-separate (PlanarConfig=2),
/// with a large (multi-kilobyte) high-entropy strip per band.
#[test]
fn planar_2bit_4channel_large_strip_lzw_roundtrips_exactly() {
    let width = 4096u32;
    let height = 8u32;
    let samples_per_pixel = 4u16;

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .samples_per_pixel(samples_per_pixel)
        .planar_configuration(PlanarConfiguration::Planar)
        .compression(Compression::Lzw)
        .strips(height); // one strip per band
    let handle = writer.add_image(image).unwrap();

    let plane_samples = (width * height) as usize;
    let mut bands: Vec<Vec<u8>> = Vec::new();
    for band in 0..samples_per_pixel as usize {
        // Distinct per-band entropy so no band trivially compresses.
        let mut band_values = pseudo_random_samples(plane_samples, 0b11);
        for (i, v) in band_values.iter_mut().enumerate() {
            *v = (*v ^ ((band as u8).wrapping_mul(1 + (i as u8 & 1)))) & 0b11;
        }
        writer.write_block(&handle, band, &band_values).unwrap();
        bands.push(band_values);
    }
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    for (band, expected) in bands.iter().enumerate() {
        let decoded = file.read_band::<u8>(0, band).unwrap();
        let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        assert_eq!(
            &decoded_values, expected,
            "band {band} incremental write_block LZW output not decodable byte-exact"
        );
    }
}

// ----------------------------------------------------------------------------
// Leg 2 — the bytes FED to LZW are the tif.c-correct MSB-first packed rows.
//
// The round-trips above go fork-writer -> fork-reader, which share the same
// packing<->unpacking, so they cannot expose a packing-level divergence from a
// `tif.c` oracle. These tests LZW-decode the raw strip bytes and compare against
// an INDEPENDENT MSB-first bit-packer implemented here from the TIFF6 rule --
// it does not call the fork's `pack_subbyte_rows` / `unpack_subbyte_block`.
//
// Chained with the `tiff-writer` unit proof that `compress_lzw` output is
// byte-identical to `into_stream().encode_all()`, an equal decode here means the
// fork's sub-byte LZW strip == `encode_all(tif.c-correct packing)` == exactly
// the `encode_all`/`tif.c`-validated stream that Onyx's option (b) produced.
// ----------------------------------------------------------------------------

/// Independent MSB-first bit-packer, written straight from the TIFF6 rule (not
/// the fork's shift/index arithmetic): within each row the first sample
/// occupies the most-significant bits; each row starts fresh on a byte
/// boundary; the final partial byte is left-aligned with low padding bits zero.
fn independent_msb_pack(rows: &[Vec<u8>], bits: u16) -> Vec<u8> {
    let mask: u32 = (1u32 << bits) - 1;
    let mut out = Vec::new();
    for row in rows {
        let mut acc: u32 = 0;
        let mut nbits: u32 = 0;
        for &sample in row {
            acc = (acc << bits) | (u32::from(sample) & mask);
            nbits += u32::from(bits);
            while nbits >= 8 {
                nbits -= 8;
                out.push(((acc >> nbits) & 0xFF) as u8);
            }
        }
        if nbits > 0 {
            out.push(((acc << (8 - nbits)) & 0xFF) as u8);
        }
    }
    out
}

fn lzw_decode_tiff(compressed: &[u8]) -> Vec<u8> {
    weezl::decode::Decoder::with_tiff_size_switch(BitOrder::Msb, 8)
        .decode(compressed)
        .expect("strip must LZW-decode to completion")
}

/// Raw on-disk bytes of one strip, extracted from the resident file slice
/// (`from_bytes` sources expose it) without going through the pixel decoder.
fn strip_bytes(file: &TiffFile, strip_index: usize) -> Vec<u8> {
    let ifd = file.ifd(0).unwrap();
    let offsets = ifd.strip_offsets().expect("strip offsets");
    let counts = ifd.strip_byte_counts().expect("strip byte counts");
    let raw = file
        .raw_bytes()
        .expect("in-memory file exposes resident bytes");
    let off = offsets[strip_index] as usize;
    let len = counts[strip_index] as usize;
    raw[off..off + len].to_vec()
}

/// Deterministic halftone-ish per-row sample values (each in `0..2^bits`),
/// covering the full sub-byte range across the row.
fn halftone_rows(width: usize, height: usize, spp: usize, bits: u16) -> Vec<Vec<u8>> {
    let modulus = 1u32 << bits;
    (0..height)
        .map(|r| {
            (0..width * spp)
                .map(|i| {
                    let ch = i % spp;
                    (((r * 7 + i * 3 + ch * 5) as u32) % modulus) as u8
                })
                .collect()
        })
        .collect()
}

#[test]
fn writeblock_subbyte_lzw_packing_matches_independent_msb_packer_chunky() {
    // (bits, width, height, spp) — widths chosen to leave partial trailing bytes.
    for &(bits, width, height, spp) in &[
        (1u16, 17usize, 3usize, 1usize),
        (2, 13, 4, 1),
        (4, 7, 5, 1),
        (2, 5, 3, 4), // chunky multi-channel: pixel-interleaved sub-byte row
    ] {
        let rows = halftone_rows(width, height, spp, bits);
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();

        let mut buf = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
        let mut image = ImageBuilder::new(width as u32, height as u32)
            .bits_per_sample(bits)
            .compression(Compression::Lzw)
            .strips(height as u32);
        if spp > 1 {
            image = image.samples_per_pixel(spp as u16);
        }
        let handle = writer.add_image(image).unwrap();
        writer.write_block(&handle, 0, &flat).unwrap();
        writer.finish().unwrap();

        let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
        let decoded_packed = lzw_decode_tiff(&strip_bytes(&file, 0));
        let expected_packed = independent_msb_pack(&rows, bits);
        assert_eq!(
            decoded_packed, expected_packed,
            "chunky bits={bits} {width}x{height} spp={spp}: bytes fed to LZW are not \
             tif.c-correct MSB-first packed rows"
        );
    }
}

#[test]
fn writeblock_subbyte_lzw_packing_matches_independent_msb_packer_planar() {
    // The Onyx halftone shape: 2-bit, 4-channel, planar-separate, one strip/band.
    let bits = 2u16;
    let width = 5usize;
    let height = 3usize;
    let spp = 4usize;
    let modulus = 1u32 << bits;

    let bands: Vec<Vec<Vec<u8>>> = (0..spp)
        .map(|b| {
            (0..height)
                .map(|r| {
                    (0..width)
                        .map(|c| (((b * 11 + r * 7 + c * 3) as u32) % modulus) as u8)
                        .collect()
                })
                .collect()
        })
        .collect();

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width as u32, height as u32)
        .bits_per_sample(bits)
        .samples_per_pixel(spp as u16)
        .planar_configuration(PlanarConfiguration::Planar)
        .compression(Compression::Lzw)
        .strips(height as u32); // RowsPerStrip=height => one strip per band
    let handle = writer.add_image(image).unwrap();
    for (b, rows) in bands.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        writer.write_block(&handle, b, &flat).unwrap();
    }
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    for (b, rows) in bands.iter().enumerate() {
        // strip index == band index when there is one strip per plane.
        let decoded_packed = lzw_decode_tiff(&strip_bytes(&file, b));
        let expected_packed = independent_msb_pack(rows, bits);
        assert_eq!(
            decoded_packed, expected_packed,
            "planar band {b}: bytes fed to LZW are not tif.c-correct MSB-first packed rows"
        );
    }
}

/// Fully independent decoder leg: decode the fork's sub-byte LZW write with the
/// image-rs `tiff` crate — a separate LZW decoder (its `lzw` feature, not
/// `weezl`) and a separate TIFF reader. This cross-checks both the LZW codec
/// and the on-disk sub-byte bytes without any fork code on the read side.
///
/// image-rs `tiff` 0.11.3 does not unpack 4-bit gray to one sample per byte;
/// `read_image` returns the LZW-decompressed *packed* on-disk row bytes as
/// `DecodingResult::U8`. So the correct expectation is the INDEPENDENT MSB-first
/// packing (the same value leg-2's weezl-based tests assert), reached here via a
/// completely different LZW implementation. Width is byte-aligned (8 * 4 bits =
/// 4 bytes/row) to avoid depending on image-rs's partial-byte padding
/// interpretation; partial-trailing-byte widths are covered by the weezl-based
/// chunky test above. Planar N-channel sub-byte, which image-rs does not
/// support, is left to the leg-2 packing chain (see report).
#[test]
fn subbyte_lzw_decodes_with_independent_image_rs_tiff() {
    use tiff::decoder::{Decoder, DecodingResult};

    let bits = 4u16;
    let width = 8usize;
    let height = 4usize;
    let rows = halftone_rows(width, height, 1, bits);
    let flat: Vec<u8> = rows.iter().flatten().copied().collect();

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width as u32, height as u32)
        .bits_per_sample(bits)
        .compression(Compression::Lzw)
        .strips(height as u32);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &flat).unwrap();
    writer.finish().unwrap();
    let bytes = buf.into_inner();

    let expected_packed = independent_msb_pack(&rows, bits);
    let mut decoder = Decoder::new(Cursor::new(bytes))
        .expect("image-rs tiff must parse the fork's sub-byte LZW file");
    match decoder.read_image() {
        Ok(DecodingResult::U8(image_rs_bytes)) => {
            assert_eq!(
                image_rs_bytes, expected_packed,
                "image-rs tiff's independent LZW decode of the fork strip does not match \
                 the independent MSB-first packing of the known input"
            );
        }
        Ok(other) => panic!("unexpected image-rs decode result variant: {other:?}"),
        Err(e) => panic!("image-rs tiff failed to decode the fork's 4-bit LZW image: {e}"),
    }
}
