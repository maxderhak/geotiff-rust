#![no_main]

//! Fuzz the NEW packed-read accessors on the fork reader.
//!
//! Mirrors `tiff_open.rs`: open arbitrary bytes with a zero-size block cache
//! and default parse budgets, then — on success — drive the packed-read API
//! (`read_image_packed_bytes` / `read_window_packed_bytes` /
//! `read_band[_window]_packed_bytes`) plus a bounded `read_window`. Every call
//! is error-catching (never `unwrap`), so only a PANIC or OOM is a finding; an
//! `Err` return is expected fuzz behaviour and ignored. Reads are bounded by
//! `MAX_DECODED_BYTES` and `MAX_IFDS` so the fuzzer cannot be steered into a
//! huge allocation by a crafted header.

use libfuzzer_sys::fuzz_target;
use tiff_reader::{OpenOptions, TiffFile};

const MAX_DECODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_IFDS: usize = 8;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }

    let file = match TiffFile::from_bytes_with_options(
        data.to_vec(),
        OpenOptions {
            block_cache_bytes: 0,
            block_cache_slots: 0,
            parse_budgets: Default::default(),
            ..Default::default()
        },
    ) {
        Ok(file) => file,
        Err(_) => return,
    };

    for ifd_index in 0..file.ifd_count().min(MAX_IFDS) {
        let Ok(ifd) = file.ifd(ifd_index) else {
            continue;
        };
        let Ok(layout) = ifd.raster_layout() else {
            continue;
        };
        let Ok(row_bytes) = layout.checked_row_bytes() else {
            continue;
        };
        let Some(decoded_len) = row_bytes.checked_mul(layout.height) else {
            continue;
        };
        if decoded_len > MAX_DECODED_BYTES {
            continue;
        }

        let width = layout.width;
        let height = layout.height;
        let bands = layout.samples_per_pixel.max(1);

        // Whole-image packed read (chunky sub-byte / byte-aligned path, or the
        // planar sub-byte fail-loud guard).
        let _ = file.read_image_packed_bytes(ifd_index);

        // A few small packed windows: full extent, a top-left window, and a
        // right/bottom-anchored window to exercise byte-aligned vs bit-granular
        // column offsets and partial edge tiles/strips.
        let windows: [(usize, usize, usize, usize); 3] = [
            (0, 0, height, width),
            (0, 0, height.min(3), width.min(3)),
            (
                height.saturating_sub(2),
                width.saturating_sub(2),
                height.min(2),
                width.min(2),
            ),
        ];
        for &(row_off, col_off, rows, cols) in &windows {
            if rows == 0 || cols == 0 {
                continue;
            }
            let _ = file.read_window_packed_bytes(ifd_index, row_off, col_off, rows, cols);
            // Typed window read; `u8` covers sub-byte and 8-bit paths, and a
            // type mismatch on wider depths just returns an `Err` (ignored).
            let _ = file.read_window::<u8>(ifd_index, row_off, col_off, rows, cols);

            for band in 0..bands {
                let _ = file.read_band_packed_bytes(ifd_index, band);
                let _ =
                    file.read_band_window_packed_bytes(ifd_index, band, row_off, col_off, rows, cols);
            }
        }
    }
});
