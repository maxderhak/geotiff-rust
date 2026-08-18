//! Round-trip coverage for the `write_block` **incremental LZW** path.
//!
//! Regression guard for the "Incremental `write_block` LZW interop" limitation
//! (see `docs/ONYX-FORK-CHANGES.md` §5): LZW strip bytes produced by
//! `write_block` with `Compression::Lzw` must be decodable by a **strict TIFF
//! LZW decoder** — here, the fork's own reader (`TiffFile::read_image` /
//! `read_band`). These tests write via `write_block` (NOT `write_block_raw`,
//! NOT a self-`encode_all`) and assert a byte-exact round trip.
//!
//! The fixtures are deliberately large and high-entropy so the LZW dictionary
//! grows past 9 bits (forcing 9->10->11->12-bit code-size switches) and past
//! `MAX_ENTRIES` (forcing a `ClearCode`) within a single strip — the halftone-
//! shaped sub-byte planar case that first exposed the interop failure.

use std::io::Cursor;

use tiff_core::{Compression, PlanarConfiguration};
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

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
    // 4096 cols x 8 rows @ 2bpp => 1024 bytes/row, 8192-byte strip: forces the
    // LZW dictionary through 9->10->11->12-bit code sizes and a ClearCode.
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
/// with a large strip per band so a single incremental LZW encode spans several
/// code-size switches.
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
