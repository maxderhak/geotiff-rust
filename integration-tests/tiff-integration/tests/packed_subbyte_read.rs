//! Round-trip coverage for the packed sub-byte READ path.
//!
//! Unlike the existing `subbyte_roundtrip` tests (which assert the *unpacked*
//! one-byte-per-sample read matches the written values), these assert the
//! new packed read API returns the exact on-disk MSB-first packed storage
//! bytes: `ceil(width*bits/8)` per chunky row, or per sample-plane row for a
//! single band. For byte-aligned depths (8/16-bit), the packed API returns
//! the identical bytes to the existing unpacked storage-byte API.

use std::io::Cursor;

use tiff_core::PlanarConfiguration;
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

/// Canonical MSB-first packing of `samples_per_row` samples per row into
/// `ceil(samples_per_row*bits/8)` bytes, matching the TIFF sub-byte storage
/// convention. `bits` must be 1, 2, or 4 (so no sample straddles a byte).
fn pack_msb_first(unpacked: &[u8], samples_per_row: usize, rows: usize, bits: u16) -> Vec<u8> {
    assert!(matches!(bits, 1 | 2 | 4));
    let bits = bits as usize;
    let mask = ((1u16 << bits) - 1) as u8;
    let samples_per_byte = 8 / bits;
    let row_bytes = (samples_per_row * bits).div_ceil(8);
    let mut out = vec![0u8; row_bytes * rows];
    for row in 0..rows {
        let src = &unpacked[row * samples_per_row..(row + 1) * samples_per_row];
        let dst = &mut out[row * row_bytes..(row + 1) * row_bytes];
        for (i, &sample) in src.iter().enumerate() {
            let byte_index = i / samples_per_byte;
            let within = i % samples_per_byte;
            let shift = 8 - bits * (within + 1);
            dst[byte_index] |= (sample & mask) << shift;
        }
    }
    out
}

#[test]
fn chunky_1bit_packed_read_matches_ondisk_bytes() {
    // 9 columns x 2 rows: partial trailing byte per row (9 bits -> 2 bytes).
    let width: u32 = 9;
    let height: u32 = 2;
    let values: Vec<u8> = vec![1, 0, 1, 1, 0, 0, 1, 0, 1, 0, 1, 1, 1, 1, 0, 0, 1, 1];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(1)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();

    let expected = pack_msb_first(&values, width as usize, height as usize, 1);
    let packed = file
        .read_window_packed_bytes(0, 0, 0, height as usize, width as usize)
        .unwrap();
    assert_eq!(
        packed, expected,
        "1-bit chunky packed read must be byte-exact"
    );
}

#[test]
fn chunky_4bit_packed_read_matches_ondisk_bytes() {
    // 3 columns x 2 rows: partial trailing byte per row (3 samples -> 2 bytes).
    let width: u32 = 3;
    let height: u32 = 2;
    let values: Vec<u8> = vec![0, 5, 15, 1, 14, 8];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(4)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();

    let expected = pack_msb_first(&values, width as usize, height as usize, 4);
    let packed = file.read_image_packed_bytes(0).unwrap();
    assert_eq!(
        packed, expected,
        "4-bit chunky packed read must be byte-exact"
    );
}

/// The Onyx shape: 2-bit, 4-channel, planar-separate (PlanarConfig=2),
/// RowsPerStrip=1 (LineInterleaved). Each band's packed read must equal the
/// on-disk per-plane packed rows.
#[test]
fn planar_2bit_4channel_packed_read_matches_ondisk_plane_bytes() {
    let width: u32 = 5;
    let height: u32 = 3;
    let samples_per_pixel: u16 = 4;

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

    let blocks_per_plane = height as usize;
    for (band, rows) in band_rows.iter().enumerate() {
        for (row, values) in rows.iter().enumerate() {
            let block_index = band * blocks_per_plane + row;
            writer.write_block(&handle, block_index, values).unwrap();
        }
    }
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.rows_per_strip(), 1);

    for (band, rows) in band_rows.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        let expected = pack_msb_first(&flat, width as usize, height as usize, 2);
        let packed = file
            .read_band_window_packed_bytes(0, band, 0, 0, height as usize, width as usize)
            .unwrap();
        assert_eq!(
            packed, expected,
            "band {band} packed plane read must be byte-exact"
        );
        // Each 2-bit, 5-wide row packs into ceil(5*2/8) = 2 bytes.
        assert_eq!(packed.len(), 2 * height as usize);
    }
}

/// Assert the packed API returns identical bytes to the existing unpacked
/// storage-byte API for a byte-aligned image, across the whole-window and
/// single-band views. `label` names the depth for failure messages.
fn assert_packed_equals_unpacked(
    file: &TiffFile,
    width: usize,
    height: usize,
    spp: usize,
    label: &str,
) {
    let unpacked = file.read_window_bytes(0, 0, 0, height, width).unwrap();
    let packed = file
        .read_window_packed_bytes(0, 0, 0, height, width)
        .unwrap();
    assert_eq!(
        packed, unpacked,
        "{label}: packed window must equal unpacked window for byte-aligned depth"
    );
    // Also confirm packed != empty (guards against both paths trivially agreeing on nothing).
    assert!(
        !packed.is_empty(),
        "{label}: packed window unexpectedly empty"
    );

    for band in 0..spp {
        let unpacked_band = file
            .read_band_window_bytes(0, band, 0, 0, height, width)
            .unwrap();
        let packed_band = file
            .read_band_window_packed_bytes(0, band, 0, 0, height, width)
            .unwrap();
        assert_eq!(
            packed_band, unpacked_band,
            "{label} band={band}: packed band must equal unpacked band"
        );
    }
}

/// For byte-aligned depths, the packed API returns the identical bytes to the
/// existing unpacked storage-byte API. Proven for both 8-bit and 16-bit, for
/// the whole-window and single-band views.
#[test]
fn byte_aligned_depths_packed_equals_unpacked() {
    let width: u32 = 4;
    let height: u32 = 3;

    // 8-bit, 3-channel.
    {
        let spp: u16 = 3;
        let sample_count = (width * height) as usize * spp as usize;
        let samples: Vec<u8> = (0..sample_count)
            .map(|i| (i as u8).wrapping_mul(37))
            .collect();

        let mut buf = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
        let image = ImageBuilder::new(width, height)
            .bits_per_sample(8)
            .samples_per_pixel(spp)
            .strips(height);
        let handle = writer.add_image(image).unwrap();
        writer.write_block(&handle, 0, &samples).unwrap();
        writer.finish().unwrap();

        let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
        assert_packed_equals_unpacked(
            &file,
            width as usize,
            height as usize,
            spp as usize,
            "8-bit",
        );
    }

    // 16-bit, 2-channel.
    {
        let spp: u16 = 2;
        let sample_count = (width * height) as usize * spp as usize;
        let samples: Vec<u16> = (0..sample_count)
            .map(|i| (i as u16).wrapping_mul(1103) ^ 0xA5A5)
            .collect();

        let mut buf = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
        let image = ImageBuilder::new(width, height)
            .bits_per_sample(16)
            .samples_per_pixel(spp)
            .strips(height);
        let handle = writer.add_image(image).unwrap();
        writer.write_block(&handle, 0, &samples).unwrap();
        writer.finish().unwrap();

        let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
        assert_packed_equals_unpacked(
            &file,
            width as usize,
            height as usize,
            spp as usize,
            "16-bit",
        );
    }
}
