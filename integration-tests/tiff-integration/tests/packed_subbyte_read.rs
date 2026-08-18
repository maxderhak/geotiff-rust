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

/// Set the trailing padding bits (the bits in each packed row beyond the
/// `data_bits` real sample bits) to 1 for every packed row in every strip of
/// the encoded TIFF `buf`, so the on-disk bytes carry NON-ZERO padding.
///
/// The fork writer always zero-pads, so this is the only way to author an
/// on-disk image whose padding differs from a fresh MSB-first re-pack. It lets
/// the verbatim-read tests prove the packed accessors copy the on-disk bytes
/// literally (padding included) rather than re-packing (which zero-fills).
///
/// `data_bits` is the number of real sample bits per packed row:
/// `width * samples_per_pixel * bits` for chunky, `width * bits` per
/// sample-plane row for planar.
fn set_trailing_padding_bits(buf: &mut [u8], row_bytes: usize, data_bits: usize) {
    let file = TiffFile::from_bytes(buf.to_vec()).unwrap();
    let ifd = file.ifd(0).unwrap();
    let offsets = ifd.strip_offsets().unwrap();
    let counts = ifd.strip_byte_counts().unwrap();
    drop(file);

    let pad = row_bytes * 8 - data_bits;
    assert!(pad > 0, "test image must have real trailing padding bits");
    assert!(pad < 8, "padding must live in a single trailing byte");
    let pad_mask = ((1u16 << pad) - 1) as u8;

    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        let offset = offset as usize;
        let count = count as usize;
        assert_eq!(count % row_bytes, 0, "strip must be a whole number of rows");
        let rows = count / row_bytes;
        for r in 0..rows {
            let last = offset + r * row_bytes + (row_bytes - 1);
            buf[last] |= pad_mask;
        }
    }
}

/// Extract, for each of `rows` rows, the `out_row_bytes` on-disk bytes starting
/// at `src_byte_offset` within each `full_row_bytes`-wide packed row — the exact
/// bytes a byte-aligned verbatim column-window read must reproduce (padding
/// bits included, since the bytes are copied literally out of the row).
fn ondisk_row_byte_window(
    ondisk: &[u8],
    full_row_bytes: usize,
    src_byte_offset: usize,
    out_row_bytes: usize,
    rows: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(out_row_bytes * rows);
    for r in 0..rows {
        let base = r * full_row_bytes + src_byte_offset;
        out.extend_from_slice(&ondisk[base..base + out_row_bytes]);
    }
    out
}

/// Concatenate the on-disk packed bytes of every strip in encoding order —
/// the exact bytes a verbatim full-width packed read must reproduce.
fn ondisk_strip_bytes(buf: &[u8]) -> Vec<u8> {
    let file = TiffFile::from_bytes(buf.to_vec()).unwrap();
    let ifd = file.ifd(0).unwrap();
    let offsets = ifd.strip_offsets().unwrap();
    let counts = ifd.strip_byte_counts().unwrap();
    let mut out = Vec::new();
    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        out.extend_from_slice(&buf[offset as usize..offset as usize + count as usize]);
    }
    out
}

/// The exact bytes of one band's (plane's) strips in encoding order.
fn ondisk_plane_bytes(buf: &[u8], plane: usize, strips_per_plane: usize) -> Vec<u8> {
    let file = TiffFile::from_bytes(buf.to_vec()).unwrap();
    let ifd = file.ifd(0).unwrap();
    let offsets = ifd.strip_offsets().unwrap();
    let counts = ifd.strip_byte_counts().unwrap();
    let mut out = Vec::new();
    for i in 0..strips_per_plane {
        let strip = plane * strips_per_plane + i;
        out.extend_from_slice(
            &buf[offsets[strip] as usize..offsets[strip] as usize + counts[strip] as usize],
        );
    }
    out
}

/// VERBATIM: a full-width sub-byte chunky read returns the on-disk packed
/// bytes exactly, INCLUDING non-zero trailing padding — proving the read
/// copies packed rows verbatim instead of unpack→repack (which zero-fills
/// padding). Exercises both the single-giant-strip (bounded) path and the
/// multi-strip (cached) path, and confirms the unpacked decode is unchanged.
#[test]
fn chunky_1bit_full_width_packed_read_is_verbatim_with_nonzero_padding() {
    // 9 columns: each row packs to ceil(9/8) = 2 bytes = 16 bits, 9 data + 7 padding.
    let width: u32 = 9;
    let height: u32 = 4;
    let values: Vec<u8> = (0..(width * height))
        .map(|i| (i % 2) as u8)
        .collect::<Vec<_>>();
    let row_bytes = 2usize;
    let data_bits = width as usize; // 1 bit/sample, 1 channel

    for rows_per_strip in [height, 1] {
        let mut buf = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
        let image = ImageBuilder::new(width, height)
            .bits_per_sample(1)
            .strips(rows_per_strip);
        let handle = writer.add_image(image).unwrap();
        let rows_per_block = rows_per_strip as usize;
        let blocks = (height as usize).div_ceil(rows_per_block);
        for block in 0..blocks {
            let start = block * rows_per_block * width as usize;
            let end = ((block + 1) * rows_per_block * width as usize).min(values.len());
            writer.write_block(&handle, block, &values[start..end]).unwrap();
        }
        writer.finish().unwrap();
        let mut bytes = buf.into_inner();

        // Author non-zero on-disk padding.
        set_trailing_padding_bits(&mut bytes, row_bytes, data_bits);
        let expected = ondisk_strip_bytes(&bytes);

        let file = TiffFile::from_bytes(bytes).unwrap();

        // Unpacked decode must be UNCHANGED (padding bits are ignored).
        let unpacked = file
            .read_window_bytes(0, 0, 0, height as usize, width as usize)
            .unwrap();
        assert_eq!(
            unpacked, values,
            "rows_per_strip={rows_per_strip}: unpacked decode must ignore padding bits"
        );

        // Packed read must be VERBATIM to disk (padding included).
        let packed = file
            .read_window_packed_bytes(0, 0, 0, height as usize, width as usize)
            .unwrap();
        assert_eq!(
            packed, expected,
            "rows_per_strip={rows_per_strip}: full-width packed read must be verbatim on-disk bytes incl. padding"
        );
        assert_eq!(packed.len(), row_bytes * height as usize);
    }
}

/// VERBATIM per-band planar read: each plane's full-width sub-byte packed read
/// returns that plane's on-disk bytes exactly, including non-zero padding.
#[test]
fn planar_2bit_per_band_packed_read_is_verbatim_with_nonzero_padding() {
    // width=5, 2 bits/sample: each sample-plane row packs to ceil(5*2/8)=2 bytes
    // = 16 bits, 10 data + 6 padding.
    let width: u32 = 5;
    let height: u32 = 2;
    let spp: u16 = 2;
    let row_bytes = 2usize;
    let data_bits = width as usize * 2; // 2 bits/sample, single plane

    let band_rows: [[[u8; 5]; 2]; 2] = [
        [[0, 1, 2, 3, 0], [3, 2, 1, 0, 1]],
        [[1, 0, 3, 2, 1], [2, 1, 0, 3, 2]],
    ];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .samples_per_pixel(spp)
        .planar_configuration(PlanarConfiguration::Planar)
        .strips(height); // one strip per plane
    let handle = writer.add_image(image).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        writer.write_block(&handle, band, &flat).unwrap();
    }
    writer.finish().unwrap();
    let mut bytes = buf.into_inner();

    set_trailing_padding_bits(&mut bytes, row_bytes, data_bits);

    let file = TiffFile::from_bytes(bytes.clone()).unwrap();
    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.strip_offsets().unwrap().len(), spp as usize);
    drop(file);

    let file = TiffFile::from_bytes(bytes.clone()).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let expected = ondisk_plane_bytes(&bytes, band, 1);
        let packed = file
            .read_band_window_packed_bytes(0, band, 0, 0, height as usize, width as usize)
            .unwrap();
        assert_eq!(
            packed, expected,
            "band {band} full-width packed read must be verbatim on-disk plane bytes incl. padding"
        );
        assert_eq!(packed.len(), row_bytes * height as usize);

        // Unpacked band decode unchanged.
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        let unpacked = file
            .read_band_window_bytes(0, band, 0, 0, height as usize, width as usize)
            .unwrap();
        assert_eq!(unpacked, flat, "band {band}: unpacked decode must ignore padding");
    }
}

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

/// The non-band packed accessors must LOUDLY reject planar sub-byte storage
/// (whose on-disk layout is per-plane) rather than silently returning
/// re-interleaved bytes. The per-band accessors remain the supported path.
#[test]
fn planar_subbyte_non_band_packed_read_is_rejected() {
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
    for (band, rows) in band_rows.iter().enumerate() {
        for (row, values) in rows.iter().enumerate() {
            let block_index = band * height as usize + row;
            writer.write_block(&handle, block_index, values).unwrap();
        }
    }
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();

    for result in [
        file.read_image_packed_bytes(0),
        file.read_window_packed_bytes(0, 0, 0, height as usize, width as usize),
    ] {
        let err = result.expect_err("planar sub-byte non-band packed read must error");
        match err {
            tiff_reader::TiffError::InvalidImageLayout(msg) => {
                assert!(
                    msg.contains("per-band packed accessors"),
                    "error must direct caller to the per-band packed accessors, got: {msg}"
                );
                assert!(
                    msg.contains("PlanarConfiguration"),
                    "error must name the offending planar configuration, got: {msg}"
                );
            }
            other => panic!("expected InvalidImageLayout, got {other:?}"),
        }
    }

    // The supported per-band path still works and is byte-exact.
    for (band, rows) in band_rows.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        let expected = pack_msb_first(&flat, width as usize, height as usize, 2);
        let packed = file.read_band_packed_bytes(0, band).unwrap();
        assert_eq!(
            packed, expected,
            "band {band} per-band packed read must still work"
        );
    }
}

/// A sub-byte column sub-window (`col_off > 0` / partial width) re-packs the
/// selected samples starting fresh at bit 0 of each output row — a valid packed
/// representation of the sub-window, pinned here so the behavior tied to the
/// planar guard is locked down.
#[test]
fn subbyte_col_offset_repacks_from_bit_zero() {
    // 4-bit, single-band, 6 wide x 2 rows chunky.
    let width: u32 = 6;
    let height: u32 = 2;
    let values: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(4)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();

    // Window cols [2..5) of each row: row0 -> [3,4,5], row1 -> [9,10,11].
    let col_off = 2usize;
    let cols = 3usize;
    let sub: Vec<u8> = vec![3, 4, 5, 9, 10, 11];
    let expected = pack_msb_first(&sub, cols, height as usize, 4);
    // 3 samples * 4 bits = 12 bits -> 2 bytes per row, fresh from bit 0.
    assert_eq!(expected.len(), 2 * height as usize);

    let packed = file
        .read_window_packed_bytes(0, 0, col_off, height as usize, cols)
        .unwrap();
    assert_eq!(
        packed, expected,
        "col_off>0 sub-byte window must re-pack the sub-window fresh from bit 0"
    );
}

/// VERBATIM col sub-window: a `col_off > 0` window whose left edge is
/// byte-aligned in the packed stream and whose right edge runs to the image
/// width is a contiguous whole-byte sub-range of each on-disk row, so the
/// packed read copies those bytes literally — INCLUDING the row's non-zero
/// trailing padding bits — instead of re-packing (which would zero-fill the
/// padding). This assertion is RED on the pre-extension repack path (padding
/// zeroed) and GREEN once the verbatim path covers the byte-aligned window.
#[test]
fn chunky_1bit_right_anchored_byte_aligned_col_window_is_verbatim_with_nonzero_padding() {
    // 13 columns, 1 bit/sample: each row packs to ceil(13/8) = 2 bytes = 16 bits,
    // 13 data + 3 padding. Window cols [8..13): left edge at bit 8 (byte 1),
    // right edge at the image width, so it is exactly on-disk byte[1] of each row.
    let width: u32 = 13;
    let height: u32 = 3;
    let values: Vec<u8> = (0..(width * height)).map(|i| (i % 2) as u8).collect();
    let full_row_bytes = 2usize;
    let data_bits = width as usize; // 1 bit/sample, 1 channel
    let col_off = 8usize;
    let cols = 5usize; // col_end = 13 = width
    let src_byte_offset = 1usize; // 8 bits / 8
    let out_row_bytes = 1usize; // ceil(5/8)

    for rows_per_strip in [height, 1] {
        let mut buf = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
        let image = ImageBuilder::new(width, height)
            .bits_per_sample(1)
            .strips(rows_per_strip);
        let handle = writer.add_image(image).unwrap();
        let rows_per_block = rows_per_strip as usize;
        let blocks = (height as usize).div_ceil(rows_per_block);
        for block in 0..blocks {
            let start = block * rows_per_block * width as usize;
            let end = ((block + 1) * rows_per_block * width as usize).min(values.len());
            writer.write_block(&handle, block, &values[start..end]).unwrap();
        }
        writer.finish().unwrap();
        let mut bytes = buf.into_inner();

        set_trailing_padding_bits(&mut bytes, full_row_bytes, data_bits);
        let ondisk = ondisk_strip_bytes(&bytes);
        let expected = ondisk_row_byte_window(
            &ondisk,
            full_row_bytes,
            src_byte_offset,
            out_row_bytes,
            height as usize,
        );

        let file = TiffFile::from_bytes(bytes).unwrap();

        // Unpacked decode of the same window is unchanged (padding ignored).
        let unpacked_window = file
            .read_window_bytes(0, 0, col_off, height as usize, cols)
            .unwrap();
        let expected_samples: Vec<u8> = (0..height as usize)
            .flat_map(|r| {
                (col_off..col_off + cols).map(move |c| ((r * width as usize + c) % 2) as u8)
            })
            .collect();
        assert_eq!(
            unpacked_window, expected_samples,
            "rows_per_strip={rows_per_strip}: unpacked col window must be unchanged"
        );

        // Packed col window must be VERBATIM to the on-disk byte sub-range,
        // padding bits included.
        let packed = file
            .read_window_packed_bytes(0, 0, col_off, height as usize, cols)
            .unwrap();
        assert_eq!(
            packed, expected,
            "rows_per_strip={rows_per_strip}: byte-aligned col window must copy on-disk bytes verbatim (padding preserved)"
        );
        assert_eq!(packed.len(), out_row_bytes * height as usize);
    }
}

/// VERBATIM per-band col sub-window on planar storage: a byte-aligned,
/// right-anchored `col_off > 0` window of one plane copies that plane's on-disk
/// byte sub-range literally, non-zero padding included — RED on the repack path,
/// GREEN once the per-band verbatim path covers the byte-aligned window.
#[test]
fn planar_2bit_band_right_anchored_byte_aligned_col_window_is_verbatim_with_nonzero_padding() {
    // width=6, 2 bits/sample: each sample-plane row packs to ceil(6*2/8)=2 bytes
    // = 16 bits, 12 data + 4 padding. Window cols [4..6): left edge at bit 8
    // (byte 1), right edge at the image width -> on-disk plane byte[1].
    let width: u32 = 6;
    let height: u32 = 2;
    let spp: u16 = 2;
    let full_row_bytes = 2usize;
    let data_bits = width as usize * 2;
    let col_off = 4usize;
    let cols = 2usize; // col_end = 6 = width
    let src_byte_offset = 1usize; // 4*2 bits / 8
    let out_row_bytes = 1usize; // ceil(2*2/8)

    let band_rows: [[[u8; 6]; 2]; 2] = [
        [[0, 1, 2, 3, 1, 2], [3, 2, 1, 0, 3, 1]],
        [[1, 0, 3, 2, 2, 0], [2, 1, 0, 3, 1, 3]],
    ];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .samples_per_pixel(spp)
        .planar_configuration(PlanarConfiguration::Planar)
        .strips(height); // one strip per plane
    let handle = writer.add_image(image).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        writer.write_block(&handle, band, &flat).unwrap();
    }
    writer.finish().unwrap();
    let mut bytes = buf.into_inner();

    set_trailing_padding_bits(&mut bytes, full_row_bytes, data_bits);

    let file = TiffFile::from_bytes(bytes.clone()).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let plane = ondisk_plane_bytes(&bytes, band, 1);
        let expected = ondisk_row_byte_window(
            &plane,
            full_row_bytes,
            src_byte_offset,
            out_row_bytes,
            height as usize,
        );
        let packed = file
            .read_band_window_packed_bytes(0, band, 0, col_off, height as usize, cols)
            .unwrap();
        assert_eq!(
            packed, expected,
            "band {band}: byte-aligned col window must copy on-disk plane bytes verbatim (padding preserved)"
        );
        assert_eq!(packed.len(), out_row_bytes * height as usize);

        // Unpacked band col window unchanged.
        let unpacked = file
            .read_band_window_bytes(0, band, 0, col_off, height as usize, cols)
            .unwrap();
        let expected_samples: Vec<u8> = rows
            .iter()
            .flat_map(|row| row[col_off..col_off + cols].iter().copied())
            .collect();
        assert_eq!(unpacked, expected_samples, "band {band}: unpacked col window unchanged");
    }
}

/// BOUNDARY (repack): a chunky sub-byte window whose LEFT edge is NOT
/// byte-aligned in the packed stream (`col_off * samples_per_pixel * bits`
/// not a multiple of 8) is genuinely bit-granular — its bytes cannot be a
/// verbatim copy of an on-disk range — so it re-packs fresh from bit 0 with
/// zero-filled padding, even when the on-disk row carries non-zero padding.
#[test]
fn chunky_1bit_bit_granular_col_window_repacks_padding_zeroed() {
    let width: u32 = 13;
    let height: u32 = 3;
    let values: Vec<u8> = (0..(width * height)).map(|i| ((i * 7 + 1) % 2) as u8).collect();
    let full_row_bytes = 2usize;
    let data_bits = width as usize;
    // col_off = 3 -> 3 bits into the row: NOT byte-aligned. col_end = 13 = width.
    let col_off = 3usize;
    let cols = 10usize;

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(1)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();
    let mut bytes = buf.into_inner();

    set_trailing_padding_bits(&mut bytes, full_row_bytes, data_bits);

    let file = TiffFile::from_bytes(bytes).unwrap();

    let mut sub: Vec<u8> = Vec::new();
    for r in 0..height as usize {
        for c in col_off..col_off + cols {
            sub.push(values[r * width as usize + c]);
        }
    }
    let expected = pack_msb_first(&sub, cols, height as usize, 1);
    // 10 bits -> 2 bytes/row, padding (bits 10..16) zeroed by the repack.
    assert_eq!(expected.len(), 2 * height as usize);

    let packed = file
        .read_window_packed_bytes(0, 0, col_off, height as usize, cols)
        .unwrap();
    assert_eq!(
        packed, expected,
        "bit-granular (left-misaligned) col window must re-pack fresh from bit 0 with zero padding"
    );
}

/// BOUNDARY (repack): a planar per-band window whose left edge is not
/// byte-aligned re-packs fresh from bit 0 (zero padding). Also exercises the
/// band-window `col_off > 0` accessor directly.
#[test]
fn planar_2bit_band_bit_granular_col_window_repacks_from_bit_zero() {
    let width: u32 = 6;
    let height: u32 = 2;
    let spp: u16 = 2;
    let full_row_bytes = 2usize;
    let data_bits = width as usize * 2;
    // col_off = 1 -> 2 bits into the plane row: NOT byte-aligned.
    let col_off = 1usize;
    let cols = 5usize; // col_end = 6 = width

    let band_rows: [[[u8; 6]; 2]; 2] = [
        [[0, 1, 2, 3, 1, 2], [3, 2, 1, 0, 3, 1]],
        [[1, 0, 3, 2, 2, 0], [2, 1, 0, 3, 1, 3]],
    ];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, height)
        .bits_per_sample(2)
        .samples_per_pixel(spp)
        .planar_configuration(PlanarConfiguration::Planar)
        .strips(height);
    let handle = writer.add_image(image).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let flat: Vec<u8> = rows.iter().flatten().copied().collect();
        writer.write_block(&handle, band, &flat).unwrap();
    }
    writer.finish().unwrap();
    let mut bytes = buf.into_inner();

    set_trailing_padding_bits(&mut bytes, full_row_bytes, data_bits);

    let file = TiffFile::from_bytes(bytes).unwrap();
    for (band, rows) in band_rows.iter().enumerate() {
        let sub: Vec<u8> = rows
            .iter()
            .flat_map(|row| row[col_off..col_off + cols].iter().copied())
            .collect();
        let expected = pack_msb_first(&sub, cols, height as usize, 2);
        let packed = file
            .read_band_window_packed_bytes(0, band, 0, col_off, height as usize, cols)
            .unwrap();
        assert_eq!(
            packed, expected,
            "band {band}: bit-granular col window must re-pack fresh from bit 0 with zero padding"
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
