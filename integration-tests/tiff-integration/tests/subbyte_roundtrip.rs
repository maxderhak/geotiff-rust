//! Round-trip coverage for sub-byte (1/2/4-bit) TIFF writes.
//!
//! The writer packs sub-byte samples MSB-first within each byte, matching
//! the fork reader's `unpack_subbyte_block` (tiff-reader/src/block_decode.rs).
//! These tests write via `tiff_writer::ImageBuilder` and read back via
//! `tiff_reader::TiffFile`, asserting the unpacked pixel values round-trip
//! exactly for chunky, planar-separate (RowsPerStrip=1, LineInterleaved),
//! and tiled layouts.

use std::io::Cursor;

use tiff_core::PlanarConfiguration;
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

/// Write a single-band, chunky image with `bits_per_sample` bits per sample
/// and read it back, asserting an exact round trip.
fn roundtrip_chunky_subbyte(bits_per_sample: u16, width: u32, height: u32, values: &[u8]) {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(bits_per_sample)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.bits_per_sample().unwrap(), vec![bits_per_sample]);

    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

#[test]
fn chunky_1bit_roundtrips_exactly() {
    // 9 columns x 2 rows: exercises a partial trailing byte per row (9 bits -> 2 bytes).
    let values: Vec<u8> = vec![1, 0, 1, 1, 0, 0, 1, 0, 1, 0, 1, 1, 1, 1, 0, 0, 1, 1];
    roundtrip_chunky_subbyte(1, 9, 2, &values);
}

#[test]
fn chunky_4bit_roundtrips_exactly() {
    // 3 columns x 2 rows: exercises a partial trailing byte per row (3 samples -> 2 bytes).
    let values: Vec<u8> = vec![0, 5, 15, 1, 14, 8];
    roundtrip_chunky_subbyte(4, 3, 2, &values);
}

/// The Onyx shape: 2-bit, 4-channel, planar-separate (PlanarConfig=2),
/// RowsPerStrip=1 (LineInterleaved).
#[test]
fn planar_2bit_4channel_rows_per_strip_1_roundtrips_exactly() {
    let width: u32 = 5;
    let height: u32 = 3;
    let samples_per_pixel: u16 = 4;

    // Per-band, per-row 2-bit sample values (each in 0..=3), width=5.
    let band_rows: [[[u8; 5]; 3]; 4] = [
        [[0, 1, 2, 3, 0], [3, 2, 1, 0, 1], [1, 1, 2, 2, 3]],
        [[3, 3, 3, 3, 3], [0, 0, 0, 0, 0], [2, 0, 1, 3, 2]],
        [[1, 0, 3, 2, 1], [2, 1, 0, 3, 2], [0, 3, 1, 2, 0]],
        [[2, 2, 0, 1, 3], [1, 3, 2, 0, 1], [3, 0, 2, 1, 0]],
    ];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .samples_per_pixel(samples_per_pixel)
        .planar_configuration(PlanarConfiguration::Planar)
        .strips(1);
    let handle = writer.add_image(image).unwrap();

    let blocks_per_plane = height as usize; // RowsPerStrip=1
    for band in 0..samples_per_pixel as usize {
        for row in 0..height as usize {
            let block_index = band * blocks_per_plane + row;
            writer
                .write_block(&handle, block_index, &band_rows[band][row])
                .unwrap();
        }
    }
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let ifd = file.ifd(0).unwrap();
    assert_eq!(
        ifd.bits_per_sample().unwrap(),
        vec![2, 2, 2, 2],
        "BitsPerSample must be recorded per sample"
    );
    assert_eq!(ifd.rows_per_strip(), 1);

    for band in 0..samples_per_pixel as usize {
        let decoded = file.read_band::<u8>(0, band).unwrap();
        let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        let expected: Vec<u8> = band_rows[band].iter().flatten().copied().collect();
        assert_eq!(decoded_values, expected, "band {band} mismatch");
    }
}

/// Tiled sub-byte write: 2-bit, single band, tile 16x16 over a 5x3 image
/// (exercises the tile-padding path already used by >=8-bit tiled tests).
#[test]
fn tiled_2bit_roundtrips_exactly() {
    let width: u32 = 5;
    let height: u32 = 3;

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .tiles(16, 16);
    let handle = writer.add_image(image).unwrap();

    // Full padded 16x16 tile; only the top-left 5x3 region is meaningful.
    let mut tile = vec![0u8; 16 * 16];
    let mut expected_image = vec![0u8; (width * height) as usize];
    for row in 0..height as usize {
        for col in 0..width as usize {
            let value = ((row * 5 + col * 3 + 1) % 4) as u8;
            tile[row * 16 + col] = value;
            expected_image[row * width as usize + col] = value;
        }
    }
    writer.write_block(&handle, 0, &tile).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, expected_image);
}
