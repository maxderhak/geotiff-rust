//! Bounded-read coverage for single-giant-strip uncompressed TIFFs.
//!
//! Today (pre-fix) `tiff-reader`'s strip decoder reads and decodes the
//! *entire* strip for any windowed request (`strip.rs::read_strip_block`
//! reads `spec.byte_count`, then caches the whole decoded block). For a
//! TIFF laid out as one giant strip spanning the whole image
//! (`RowsPerStrip >= height`), that means reading a single row-band via
//! `TiffFile::read_window` still materializes the whole image in memory.
//!
//! This test proves peak read size is bounded to ~the requested row band
//! (not the whole strip) for the chunky, uncompressed, single-strip
//! trigger, using a deterministic byte-counting source instead of
//! peak-RSS (which is allocator/OS-flaky in a unit test).

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tiff_core::{Compression, PhotometricInterpretation, PlanarConfiguration};
use tiff_reader::error::Result as TiffResult;
use tiff_reader::source::TiffSource;
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

/// In-memory `TiffSource` that records the largest single `read_exact_at`
/// request and forces every read through `read_exact_at` by overriding
/// `as_slice()` to return `None` (otherwise `read_block_payload` would
/// slice the in-memory bytes directly and never call `read_exact_at`).
struct CountingSource {
    bytes: Vec<u8>,
    max_read: AtomicUsize,
    total_reads: AtomicUsize,
}

impl CountingSource {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            max_read: AtomicUsize::new(0),
            total_reads: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        self.max_read.store(0, Ordering::SeqCst);
        self.total_reads.store(0, Ordering::SeqCst);
    }

    fn max_read(&self) -> usize {
        self.max_read.load(Ordering::SeqCst)
    }

    fn total_reads(&self) -> usize {
        self.total_reads.load(Ordering::SeqCst)
    }
}

impl TiffSource for CountingSource {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_exact_at(&self, offset: u64, len: usize) -> TiffResult<Vec<u8>> {
        self.total_reads.fetch_add(1, Ordering::SeqCst);
        self.max_read.fetch_max(len, Ordering::SeqCst);
        let start = usize::try_from(offset).expect("offset fits in usize in this test");
        let end = start + len;
        assert!(
            end <= self.bytes.len(),
            "read_exact_at({offset}, {len}) out of bounds for {} byte source",
            self.bytes.len()
        );
        Ok(self.bytes[start..end].to_vec())
    }

    fn as_slice(&self) -> Option<&[u8]> {
        None
    }
}

const WIDTH: u32 = 256;
const HEIGHT: u32 = 4096;
const ROW_BYTES: usize = WIDTH as usize; // 8-bit, 1 sample per pixel.
const BAND_ROWS: usize = 8;

/// Deterministic per-pixel value so any row/col mixup is caught by an exact
/// pixel comparison.
fn pixel_value(row: u32, col: u32) -> u8 {
    ((row.wrapping_mul(131) + col).wrapping_add(7)) as u8
}

fn build_single_strip_uncompressed_bytes() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(WIDTH, HEIGHT)
        .sample_type::<u8>()
        .samples_per_pixel(1)
        .compression(Compression::None)
        // rows_per_strip == height => exactly one strip spanning the whole
        // image: the trigger this task targets.
        .strips(HEIGHT);
    let handle = writer.add_image(image).unwrap();

    let mut pixels = Vec::with_capacity(WIDTH as usize * HEIGHT as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            pixels.push(pixel_value(row, col));
        }
    }
    writer.write_block(&handle, 0, &pixels).unwrap();
    writer.finish().unwrap();
    buf.into_inner()
}

/// RED/GREEN gate: reading the image in small row-bands must never touch
/// more than ~one row-band's worth of source bytes at a time, and the
/// pixels read through the bounded path must equal the known written
/// values (proving the fix didn't corrupt output).
#[test]
fn single_giant_strip_uncompressed_reads_bounded_to_row_band() {
    let data = build_single_strip_uncompressed_bytes();
    let whole_strip_bytes = WIDTH as usize * HEIGHT as usize;
    let source = Arc::new(CountingSource::new(data));
    let file = TiffFile::from_source(source.clone()).unwrap();
    // Exclude IFD-parsing reads (header/tags) from the bound check below.
    source.reset();

    let mut row = 0usize;
    while row < HEIGHT as usize {
        let band_rows = BAND_ROWS.min(HEIGHT as usize - row);
        let window = file
            .read_window::<u8>(0, row, 0, band_rows, WIDTH as usize)
            .unwrap();
        let (values, offset) = window.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        for r in 0..band_rows {
            for c in 0..WIDTH as usize {
                let expected = pixel_value((row + r) as u32, c as u32);
                let actual = values[r * WIDTH as usize + c];
                assert_eq!(
                    actual,
                    expected,
                    "pixel mismatch at row {} col {}",
                    row + r,
                    c
                );
            }
        }
        row += band_rows;
    }

    let max_read = source.max_read();
    let bound = ROW_BYTES * BAND_ROWS;
    assert!(
        max_read <= bound,
        "max single read was {max_read} bytes; expected <= {bound} bytes (one row-band). \
         whole-strip byte count is {whole_strip_bytes} bytes -- a read anywhere near that \
         size means the whole giant strip was materialized instead of just the row band."
    );
    // Sanity: prove the bound is actually meaningful for this fixture (the
    // row-band is a tiny fraction of the whole strip).
    assert!(bound * 16 < whole_strip_bytes);
}

/// Regression: a *multi-strip* uncompressed image (rows_per_strip well
/// below height) must keep reading and decoding correctly -- the bounded
/// trigger must not fire for it (each strip's `rows_in_strip` != the full
/// image height, so it takes the pre-existing whole-strip/cached path).
#[test]
fn multi_strip_uncompressed_regression_still_correct() {
    const ROWS_PER_STRIP: u32 = 64;
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(WIDTH, HEIGHT)
        .sample_type::<u8>()
        .samples_per_pixel(1)
        .compression(Compression::None)
        .strips(ROWS_PER_STRIP);
    let handle = writer.add_image(image).unwrap();

    let strips = HEIGHT.div_ceil(ROWS_PER_STRIP);
    for strip_index in 0..strips as usize {
        let strip_row_start = strip_index as u32 * ROWS_PER_STRIP;
        let rows_in_strip = ROWS_PER_STRIP.min(HEIGHT - strip_row_start);
        let mut block = Vec::with_capacity(rows_in_strip as usize * WIDTH as usize);
        for r in 0..rows_in_strip {
            for c in 0..WIDTH {
                block.push(pixel_value(strip_row_start + r, c));
            }
        }
        writer.write_block(&handle, strip_index, &block).unwrap();
    }
    writer.finish().unwrap();

    let data = buf.into_inner();
    let source = Arc::new(CountingSource::new(data));
    let file = TiffFile::from_source(source.clone()).unwrap();
    source.reset();

    let mut row = 0usize;
    while row < HEIGHT as usize {
        let band_rows = BAND_ROWS.min(HEIGHT as usize - row);
        let window = file
            .read_window::<u8>(0, row, 0, band_rows, WIDTH as usize)
            .unwrap();
        let (values, offset) = window.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        for r in 0..band_rows {
            for c in 0..WIDTH as usize {
                assert_eq!(
                    values[r * WIDTH as usize + c],
                    pixel_value((row + r) as u32, c as u32)
                );
            }
        }
        row += band_rows;
    }
    assert!(source.total_reads() > 0);
}

/// Regression: a *compressed* single-giant-strip image must not take the
/// bounded byte-range-read path (compressed bytes are not randomly
/// addressable per row) -- it must keep decoding correctly via the
/// pre-existing whole-strip path.
#[test]
fn compressed_single_strip_regression_still_correct() {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(WIDTH, HEIGHT)
        .sample_type::<u8>()
        .samples_per_pixel(1)
        .compression(Compression::Deflate)
        .strips(HEIGHT);
    let handle = writer.add_image(image).unwrap();

    let mut pixels = Vec::with_capacity(WIDTH as usize * HEIGHT as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            pixels.push(pixel_value(row, col));
        }
    }
    writer.write_block(&handle, 0, &pixels).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();

    let mut row = 0usize;
    while row < HEIGHT as usize {
        let band_rows = BAND_ROWS.min(HEIGHT as usize - row);
        let window = file
            .read_window::<u8>(0, row, 0, band_rows, WIDTH as usize)
            .unwrap();
        let (values, offset) = window.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        for r in 0..band_rows {
            for c in 0..WIDTH as usize {
                assert_eq!(
                    values[r * WIDTH as usize + c],
                    pixel_value((row + r) as u32, c as u32)
                );
            }
        }
        row += band_rows;
    }
}

/// Bonus coverage (not required by the trigger, but confirmed to "fall out
/// naturally" per the task brief): planar single-strip-*per-plane* RGB also
/// takes the bounded path, since each plane's spec independently satisfies
/// `row_start == 0 && rows_in_strip == height`.
#[test]
fn planar_single_strip_per_plane_uncompressed_reads_bounded_to_row_band() {
    const P_WIDTH: u32 = 64;
    const P_HEIGHT: u32 = 512;
    const P_BAND_ROWS: usize = 8;
    fn plane_pixel(band: u32, row: u32, col: u32) -> u8 {
        ((band.wrapping_mul(97) + row.wrapping_mul(131) + col).wrapping_add(3)) as u8
    }

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(P_WIDTH, P_HEIGHT)
        .sample_type::<u8>()
        .samples_per_pixel(3)
        .photometric(PhotometricInterpretation::Rgb)
        .planar_configuration(PlanarConfiguration::Planar)
        .compression(Compression::None)
        .strips(P_HEIGHT);
    let handle = writer.add_image(image).unwrap();
    for band in 0..3u32 {
        let mut plane = Vec::with_capacity(P_WIDTH as usize * P_HEIGHT as usize);
        for row in 0..P_HEIGHT {
            for col in 0..P_WIDTH {
                plane.push(plane_pixel(band, row, col));
            }
        }
        writer.write_block(&handle, band as usize, &plane).unwrap();
    }
    writer.finish().unwrap();

    let whole_plane_bytes = P_WIDTH as usize * P_HEIGHT as usize;
    let source = Arc::new(CountingSource::new(buf.into_inner()));
    let file = TiffFile::from_source(source.clone()).unwrap();
    source.reset();

    let mut row = 0usize;
    while row < P_HEIGHT as usize {
        let band_rows = P_BAND_ROWS.min(P_HEIGHT as usize - row);
        let window = file
            .read_window::<u8>(0, row, 0, band_rows, P_WIDTH as usize)
            .unwrap();
        let (values, offset) = window.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        for r in 0..band_rows {
            for c in 0..P_WIDTH as usize {
                for band in 0..3usize {
                    let expected = plane_pixel(band as u32, (row + r) as u32, c as u32);
                    let actual = values[(r * P_WIDTH as usize + c) * 3 + band];
                    assert_eq!(actual, expected, "row {} col {} band {band}", row + r, c);
                }
            }
        }
        row += band_rows;
    }

    let max_read = source.max_read();
    let bound = P_WIDTH as usize * P_BAND_ROWS;
    assert!(
        max_read <= bound,
        "planar max single read was {max_read} bytes; expected <= {bound} bytes. \
         whole per-plane strip is {whole_plane_bytes} bytes."
    );
    assert!(bound * 8 < whole_plane_bytes);
}
