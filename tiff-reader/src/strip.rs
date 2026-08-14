//! Strip-based data access for TIFF images.

use std::sync::Arc;

#[cfg(feature = "rayon")]
use parking_lot::Mutex;
#[cfg(feature = "rayon")]
use rayon::prelude::*;

use tiff_core::Compression;

use crate::block_decode;
use crate::cache::{BlockCache, BlockKey, BlockKind};
use crate::error::{Error, Result};
use crate::header::ByteOrder;
use crate::ifd::{Ifd, RasterLayout};
use crate::source::TiffSource;
use crate::{
    allocate_decode_output, checked_layout_add, checked_layout_mul, read_block_payload,
    read_gdal_block_payload, validate_block_byte_count, validate_decode_output_len,
    DecodeReadOptions, Window,
};

pub(crate) fn read_window(
    source: &dyn TiffSource,
    ifd: &Ifd,
    byte_order: ByteOrder,
    cache: &BlockCache,
    window: Window,
    options: DecodeReadOptions<'_>,
) -> Result<Vec<u8>> {
    let layout = ifd.raster_layout()?;
    if window.is_empty() {
        return Ok(Vec::new());
    }
    let ifd_offset = ifd.offset();
    let context = block_decode::BlockDecodeContext::new(ifd, layout, byte_order)?;

    let output_len = window.output_len(&layout)?;
    let mut output = allocate_decode_output(output_len, options.decode_output_bytes)?;

    let relevant_specs = collect_strip_specs_for_window(ifd, &layout, window, None)?;

    #[cfg(feature = "rayon")]
    {
        let output = Mutex::new(output.as_mut_slice());
        relevant_specs.par_iter().try_for_each(|&spec| {
            let (block, block_spec) = read_strip_block_for_window(
                source, ifd_offset, cache, spec, &context, window, options,
            )?;
            copy_strip_window_block(
                &mut output.lock(),
                block.as_slice(),
                block_spec,
                &layout,
                window,
            )?;
            Ok::<(), Error>(())
        })?;
    }

    #[cfg(not(feature = "rayon"))]
    for spec in relevant_specs {
        let (block, block_spec) = read_strip_block_for_window(
            source, ifd_offset, cache, spec, &context, window, options,
        )?;
        copy_strip_window_block(&mut output, block.as_slice(), block_spec, &layout, window)?;
    }

    Ok(output)
}

pub(crate) fn read_window_band(
    source: &dyn TiffSource,
    ifd: &Ifd,
    byte_order: ByteOrder,
    cache: &BlockCache,
    window: Window,
    band_index: usize,
    options: DecodeReadOptions<'_>,
) -> Result<Vec<u8>> {
    let layout = ifd.raster_layout()?;
    if band_index >= layout.samples_per_pixel {
        return Err(Error::BandIndexOutOfBounds {
            index: band_index,
            band_count: layout.samples_per_pixel,
        });
    }
    if window.is_empty() {
        return Ok(Vec::new());
    }
    let ifd_offset = ifd.offset();
    let context = block_decode::BlockDecodeContext::new(ifd, layout, byte_order)?;

    let output_len = window.band_output_len(&layout)?;
    let mut output = allocate_decode_output(output_len, options.decode_output_bytes)?;

    let relevant_specs = collect_strip_specs_for_window(ifd, &layout, window, Some(band_index))?;

    #[cfg(feature = "rayon")]
    {
        let output = Mutex::new(output.as_mut_slice());
        relevant_specs.par_iter().try_for_each(|&spec| {
            let (block, block_spec) = read_strip_block_for_window(
                source, ifd_offset, cache, spec, &context, window, options,
            )?;
            copy_strip_band_window_block(
                &mut output.lock(),
                block.as_slice(),
                block_spec,
                &layout,
                window,
                band_index,
            )?;
            Ok::<(), Error>(())
        })?;
    }

    #[cfg(not(feature = "rayon"))]
    for spec in relevant_specs {
        let (block, block_spec) = read_strip_block_for_window(
            source, ifd_offset, cache, spec, &context, window, options,
        )?;
        copy_strip_band_window_block(
            &mut output,
            block.as_slice(),
            block_spec,
            &layout,
            window,
            band_index,
        )?;
    }

    Ok(output)
}

fn copy_strip_window_block(
    output: &mut [u8],
    block: &[u8],
    spec: StripBlockSpec,
    layout: &RasterLayout,
    window: Window,
) -> Result<()> {
    let pixel_stride = layout.checked_pixel_stride_bytes()?;
    let window_row_end = window.row_end();
    let output_row_bytes = checked_layout_mul(window.cols, pixel_stride, "window row byte count")?;
    let block_row_end = checked_layout_add(spec.row_start, spec.rows_in_strip, "strip row range")?;
    let copy_row_start = spec.row_start.max(window.row_off);
    let copy_row_end = block_row_end.min(window_row_end);

    if layout.planar_configuration == 1 {
        let src_row_bytes = layout.checked_row_bytes()?;
        let src_col_offset =
            checked_layout_mul(window.col_off, pixel_stride, "strip source column offset")?;
        let copy_bytes_per_row = output_row_bytes;
        for row in copy_row_start..copy_row_end {
            let src_row_index = row - spec.row_start;
            let dest_row_index = row - window.row_off;
            let src_offset = checked_layout_add(
                checked_layout_mul(src_row_index, src_row_bytes, "strip source row offset")?,
                src_col_offset,
                "strip source offset",
            )?;
            let dest_offset =
                checked_layout_mul(dest_row_index, output_row_bytes, "strip output row offset")?;
            let src_end =
                checked_layout_add(src_offset, copy_bytes_per_row, "strip source copy range")?;
            let dest_end =
                checked_layout_add(dest_offset, copy_bytes_per_row, "strip output copy range")?;
            output[dest_offset..dest_end].copy_from_slice(&block[src_offset..src_end]);
        }
    } else {
        let src_row_bytes = layout.checked_sample_plane_row_bytes()?;
        let plane_offset = checked_layout_mul(
            spec.plane,
            layout.bytes_per_sample,
            "strip plane byte offset",
        )?;
        for row in copy_row_start..copy_row_end {
            let src_row_index = row - spec.row_start;
            let dest_row_index = row - window.row_off;
            let src_row_offset =
                checked_layout_mul(src_row_index, src_row_bytes, "strip source row offset")?;
            let src_row_end =
                checked_layout_add(src_row_offset, src_row_bytes, "strip source row range")?;
            let dest_row_offset =
                checked_layout_mul(dest_row_index, output_row_bytes, "strip output row offset")?;
            let dest_row_end =
                checked_layout_add(dest_row_offset, output_row_bytes, "strip output row range")?;
            let src_row = &block[src_row_offset..src_row_end];
            let dest_row = &mut output[dest_row_offset..dest_row_end];
            for col in window.col_off..window.col_end() {
                let src_offset =
                    checked_layout_mul(col, layout.bytes_per_sample, "strip source column offset")?;
                let src_end = checked_layout_add(
                    src_offset,
                    layout.bytes_per_sample,
                    "strip source sample range",
                )?;
                let src = &src_row[src_offset..src_end];
                let dest_col_index = col - window.col_off;
                let pixel_base = checked_layout_add(
                    checked_layout_mul(dest_col_index, pixel_stride, "strip output pixel offset")?,
                    plane_offset,
                    "strip output sample offset",
                )?;
                let pixel_end = checked_layout_add(
                    pixel_base,
                    layout.bytes_per_sample,
                    "strip output sample range",
                )?;
                dest_row[pixel_base..pixel_end].copy_from_slice(src);
            }
        }
    }
    Ok(())
}

fn copy_strip_band_window_block(
    output: &mut [u8],
    block: &[u8],
    spec: StripBlockSpec,
    layout: &RasterLayout,
    window: Window,
    band_index: usize,
) -> Result<()> {
    let pixel_stride = layout.checked_pixel_stride_bytes()?;
    let window_row_end = window.row_end();
    let output_row_bytes = checked_layout_mul(
        window.cols,
        layout.bytes_per_sample,
        "window band row byte count",
    )?;
    let block_row_end = checked_layout_add(spec.row_start, spec.rows_in_strip, "strip row range")?;
    let copy_row_start = spec.row_start.max(window.row_off);
    let copy_row_end = block_row_end.min(window_row_end);

    if layout.planar_configuration == 1 {
        let src_row_bytes = layout.checked_row_bytes()?;
        let band_offset =
            checked_layout_mul(band_index, layout.bytes_per_sample, "band byte offset")?;
        for row in copy_row_start..copy_row_end {
            let src_row_index = row - spec.row_start;
            let dest_row_index = row - window.row_off;
            let src_row_offset =
                checked_layout_mul(src_row_index, src_row_bytes, "strip source row offset")?;
            let src_row_end =
                checked_layout_add(src_row_offset, src_row_bytes, "strip source row range")?;
            let dest_row_offset =
                checked_layout_mul(dest_row_index, output_row_bytes, "strip output row offset")?;
            let dest_row_end =
                checked_layout_add(dest_row_offset, output_row_bytes, "strip output row range")?;
            let src_row = &block[src_row_offset..src_row_end];
            let dest_row = &mut output[dest_row_offset..dest_row_end];
            for col in window.col_off..window.col_end() {
                let src_base = checked_layout_add(
                    checked_layout_mul(col, pixel_stride, "strip source column offset")?,
                    band_offset,
                    "strip source band offset",
                )?;
                let dest_col_index = col - window.col_off;
                let dest_base = checked_layout_mul(
                    dest_col_index,
                    layout.bytes_per_sample,
                    "strip output sample offset",
                )?;
                let src_end = checked_layout_add(
                    src_base,
                    layout.bytes_per_sample,
                    "strip source sample range",
                )?;
                let dest_end = checked_layout_add(
                    dest_base,
                    layout.bytes_per_sample,
                    "strip output sample range",
                )?;
                dest_row[dest_base..dest_end].copy_from_slice(&src_row[src_base..src_end]);
            }
        }
    } else {
        let src_row_bytes = layout.checked_sample_plane_row_bytes()?;
        let src_col_offset = checked_layout_mul(
            window.col_off,
            layout.bytes_per_sample,
            "strip source column offset",
        )?;
        let copy_bytes_per_row = output_row_bytes;
        for row in copy_row_start..copy_row_end {
            let src_row_index = row - spec.row_start;
            let dest_row_index = row - window.row_off;
            let src_offset = checked_layout_add(
                checked_layout_mul(src_row_index, src_row_bytes, "strip source row offset")?,
                src_col_offset,
                "strip source offset",
            )?;
            let dest_offset =
                checked_layout_mul(dest_row_index, output_row_bytes, "strip output row offset")?;
            let src_end =
                checked_layout_add(src_offset, copy_bytes_per_row, "strip source copy range")?;
            let dest_end =
                checked_layout_add(dest_offset, copy_bytes_per_row, "strip output copy range")?;
            output[dest_offset..dest_end].copy_from_slice(&block[src_offset..src_end]);
        }
    }
    Ok(())
}

fn collect_strip_specs_for_window(
    ifd: &Ifd,
    layout: &RasterLayout,
    window: Window,
    band_index: Option<usize>,
) -> Result<Vec<StripBlockSpec>> {
    let offsets = ifd
        .strip_offsets()
        .ok_or(Error::TagNotFound(crate::ifd::TAG_STRIP_OFFSETS))?;
    let counts = ifd
        .strip_byte_counts()
        .ok_or(Error::TagNotFound(crate::ifd::TAG_STRIP_BYTE_COUNTS))?;
    if offsets.len() != counts.len() {
        return Err(Error::InvalidImageLayout(format!(
            "StripOffsets has {} entries but StripByteCounts has {}",
            offsets.len(),
            counts.len()
        )));
    }

    let rows_per_strip = ifd.rows_per_strip();
    if rows_per_strip == 0 {
        return Err(Error::InvalidImageLayout(
            "RowsPerStrip must be greater than zero".into(),
        ));
    }
    let rows_per_strip = rows_per_strip as usize;
    let strips_per_plane = layout.height.div_ceil(rows_per_strip);
    let expected = match layout.planar_configuration {
        1 => strips_per_plane,
        2 => strips_per_plane
            .checked_mul(layout.samples_per_pixel)
            .ok_or_else(strip_count_overflow)?,
        planar => return Err(Error::UnsupportedPlanarConfiguration(planar)),
    };
    if offsets.len() != expected {
        return Err(Error::InvalidImageLayout(format!(
            "expected {expected} strips, found {}",
            offsets.len()
        )));
    }

    let first_strip = window.row_off / rows_per_strip;
    let last_strip = window
        .row_end()
        .div_ceil(rows_per_strip)
        .min(strips_per_plane);
    let plane_range = if layout.planar_configuration == 1 {
        0..1
    } else if let Some(band_index) = band_index {
        band_index..band_index + 1
    } else {
        0..layout.samples_per_pixel
    };
    let spec_count = (last_strip - first_strip)
        .checked_mul(plane_range.end - plane_range.start)
        .ok_or_else(strip_count_overflow)?;
    let mut specs = Vec::with_capacity(spec_count);

    for plane in plane_range {
        for plane_strip_index in first_strip..last_strip {
            let strip_index = if layout.planar_configuration == 1 {
                plane_strip_index
            } else {
                plane
                    .checked_mul(strips_per_plane)
                    .and_then(|base| base.checked_add(plane_strip_index))
                    .ok_or_else(strip_count_overflow)?
            };
            let row_start = plane_strip_index
                .checked_mul(rows_per_strip)
                .ok_or_else(strip_count_overflow)?;
            let rows_in_strip = rows_per_strip.min(layout.height.saturating_sub(row_start));
            specs.push(StripBlockSpec {
                index: strip_index,
                plane,
                row_start,
                offset: offsets[strip_index],
                byte_count: counts[strip_index],
                rows_in_strip,
            });
        }
    }

    Ok(specs)
}

fn strip_count_overflow() -> Error {
    Error::InvalidImageLayout("strip count overflows usize".into())
}

#[derive(Clone, Copy)]
struct StripBlockSpec {
    index: usize,
    plane: usize,
    row_start: usize,
    offset: u64,
    byte_count: u64,
    rows_in_strip: usize,
}

fn read_strip_block(
    source: &dyn TiffSource,
    ifd_offset: u64,
    cache: &BlockCache,
    spec: StripBlockSpec,
    context: &block_decode::BlockDecodeContext<'_>,
    options: DecodeReadOptions<'_>,
) -> Result<Arc<Vec<u8>>> {
    let decode_request = block_decode::BlockDecodeRequest {
        context,
        compressed: &[],
        index: spec.index,
        block_width: context.layout.width,
        block_height: spec.rows_in_strip,
    };
    let decoded_len = block_decode::decoded_block_len(&decode_request)?;
    validate_decode_output_len(decoded_len, options.decode_output_bytes)?;

    let cache_key = BlockKey {
        ifd_offset,
        kind: BlockKind::Strip,
        block_index: spec.index,
    };
    if let Some(cached) = cache.get(&cache_key) {
        return Ok(cached);
    }

    // GDAL SPARSE_OK semantics: a block with no on-disk payload (zero offset
    // or zero byte count) decodes as implicit zero fill.
    if spec.offset == 0 || spec.byte_count == 0 {
        let decoded = allocate_decode_output(decoded_len, options.decode_output_bytes)?;
        return Ok(cache.insert(cache_key, decoded));
    }

    let byte_count_limit = block_decode::compressed_block_byte_count_limit(&decode_request)?;
    let compressed = match options.gdal_structural_metadata {
        Some(metadata) => read_gdal_block_payload(
            source,
            metadata,
            context.byte_order,
            spec.offset,
            spec.byte_count,
            byte_count_limit,
            spec.index,
        )?,
        None => read_block_payload(
            source,
            spec.offset,
            spec.byte_count,
            byte_count_limit,
            spec.index,
        )?,
    };

    let decoded = block_decode::decode_compressed_block(block_decode::BlockDecodeRequest {
        context,
        compressed: &compressed,
        index: spec.index,
        block_width: context.layout.width,
        block_height: spec.rows_in_strip,
    })?;
    Ok(cache.insert(cache_key, decoded))
}

/// Fetches the decoded block for `spec`, bounded to the rows `window`
/// actually needs when the strip is eligible for the bounded scanline path
/// (see [`read_strip_block_bounded`]); otherwise falls back to
/// [`read_strip_block`]'s whole-strip, cached read.
///
/// Returns the decoded bytes alongside a `StripBlockSpec` describing which
/// rows those bytes actually cover -- callers must use the returned spec
/// (not the original) when copying rows out of the block, since a bounded
/// read's block only contains `window`'s rows, not the full strip.
fn read_strip_block_for_window(
    source: &dyn TiffSource,
    ifd_offset: u64,
    cache: &BlockCache,
    spec: StripBlockSpec,
    context: &block_decode::BlockDecodeContext<'_>,
    window: Window,
    options: DecodeReadOptions<'_>,
) -> Result<(Arc<Vec<u8>>, StripBlockSpec)> {
    if let Some(bounded) = read_strip_block_bounded(source, spec, context, window, options)? {
        return Ok(bounded);
    }
    let block = read_strip_block(source, ifd_offset, cache, spec, context, options)?;
    Ok((block, spec))
}

/// Bounded single-giant-strip scanline read.
///
/// Trigger: a chunky or per-plane strip that spans the *whole* image
/// (`spec.row_start == 0 && spec.rows_in_strip == layout.height`, i.e.
/// `strips_per_plane == 1` for this plane) with `Compression::None`, and
/// not subsampled non-JPEG YCbCr (whose on-disk row layout groups multiple
/// output rows into one chroma-subsampling unit, so a linear
/// `offset + row * row_bytes` seek does not correspond to a row boundary).
///
/// For an eligible strip, only the byte range covering `window`'s rows is
/// read from `source` and decoded -- never the whole strip -- and the
/// result is **not** cached under the strip's cache key: a single giant
/// strip has only one strip index, so caching a partial-row decode there
/// would silently serve stale/incomplete rows to a later request for a
/// *different* row range within the same strip. Returns `Ok(None)` when
/// the strip is not eligible, so the caller falls back to the existing
/// whole-strip, cached path unchanged.
///
/// Uncompressed TIFF decode is row-independent (byte-order swap, sub-byte
/// unpack, and the TIFF horizontal/floating-point predictors all reset at
/// the start of every row -- see `block_decode::decode_compressed_block`'s
/// `decoded.chunks_exact_mut(row_bytes)` loop), so decoding a contiguous
/// row sub-range yields byte-identical rows to decoding the whole strip.
fn read_strip_block_bounded(
    source: &dyn TiffSource,
    spec: StripBlockSpec,
    context: &block_decode::BlockDecodeContext<'_>,
    window: Window,
    options: DecodeReadOptions<'_>,
) -> Result<Option<(Arc<Vec<u8>>, StripBlockSpec)>> {
    // GDAL-wrapped blocks carry a size-prefix/trailer around the payload;
    // a linear row-range seek would land inside that framing, not at a row
    // boundary.
    if options.gdal_structural_metadata.is_some() {
        return Ok(None);
    }
    if context.compression != Some(Compression::None) {
        return Ok(None);
    }
    let layout = &context.layout;
    if spec.row_start != 0 || spec.rows_in_strip != layout.height {
        return Ok(None);
    }
    if context.is_subsampled_ycbcr_non_jpeg() {
        return Ok(None);
    }

    // Validate the strip's declared on-disk byte count against the SAME
    // full-strip budget the whole-strip path enforces, so a malformed or
    // oversized `StripByteCounts` value is rejected with the identical
    // "block read budget" error, before any bytes are read -- regardless
    // of how small the bounded sub-range we actually need happens to be.
    let full_decode_request = block_decode::BlockDecodeRequest {
        context,
        compressed: &[],
        index: spec.index,
        block_width: layout.width,
        block_height: spec.rows_in_strip,
    };
    let full_byte_count_limit =
        block_decode::compressed_block_byte_count_limit(&full_decode_request)?;
    validate_block_byte_count(spec.index, spec.byte_count, full_byte_count_limit)?;

    let row_bytes = if layout.bits_per_sample < 8 {
        if layout.planar_configuration == 1 {
            layout.checked_packed_row_bytes()?
        } else {
            layout.checked_packed_sample_plane_row_bytes()?
        }
    } else if layout.planar_configuration == 1 {
        layout.checked_row_bytes()?
    } else {
        layout.checked_sample_plane_row_bytes()?
    };

    let block_row_end = checked_layout_add(spec.row_start, spec.rows_in_strip, "strip row range")?;
    let clip_row_start = spec.row_start.max(window.row_off);
    let clip_row_end = block_row_end.min(window.row_end());
    if clip_row_end <= clip_row_start {
        return Ok(None);
    }
    let clip_rows = clip_row_end - clip_row_start;
    let row_offset_in_strip = clip_row_start - spec.row_start;

    let byte_offset_in_strip =
        checked_layout_mul(row_offset_in_strip, row_bytes, "strip bounded byte offset")?;
    let byte_len = checked_layout_mul(clip_rows, row_bytes, "strip bounded byte length")?;

    // Safety net: only take the bounded path when the computed byte range
    // stays within the strip's declared on-disk extent. If metadata is
    // unusual enough that it wouldn't (e.g. a StripByteCounts value smaller
    // than the exact expected size), defer to the whole-strip path so that
    // edge-case/malformed-metadata behavior is completely unchanged.
    let byte_offset_in_strip_u64 = byte_offset_in_strip as u64;
    let byte_len_u64 = byte_len as u64;
    let fits_declared_extent = spec
        .offset
        .checked_add(spec.byte_count)
        .zip(
            spec.offset
                .checked_add(byte_offset_in_strip_u64)
                .and_then(|v| v.checked_add(byte_len_u64)),
        )
        .is_some_and(|(strip_end, bounded_end)| bounded_end <= strip_end);
    if !fits_declared_extent {
        return Ok(None);
    }

    let decode_request = block_decode::BlockDecodeRequest {
        context,
        compressed: &[],
        index: spec.index,
        block_width: layout.width,
        block_height: clip_rows,
    };
    let decoded_len = block_decode::decoded_block_len(&decode_request)?;
    validate_decode_output_len(decoded_len, options.decode_output_bytes)?;

    let clipped_spec = StripBlockSpec {
        row_start: clip_row_start,
        rows_in_strip: clip_rows,
        ..spec
    };

    // GDAL SPARSE_OK semantics: a block with no on-disk payload (zero
    // offset or zero byte count) decodes as implicit zero fill.
    if spec.offset == 0 || spec.byte_count == 0 {
        let decoded = allocate_decode_output(decoded_len, options.decode_output_bytes)?;
        return Ok(Some((Arc::new(decoded), clipped_spec)));
    }

    let byte_offset = spec.offset + byte_offset_in_strip_u64;
    let byte_count_limit = block_decode::compressed_block_byte_count_limit(&decode_request)?;
    let compressed = read_block_payload(
        source,
        byte_offset,
        byte_len_u64,
        byte_count_limit,
        spec.index,
    )?;

    let decoded = block_decode::decode_compressed_block(block_decode::BlockDecodeRequest {
        context,
        compressed: &compressed,
        index: spec.index,
        block_width: layout.width,
        block_height: clip_rows,
    })?;

    Ok(Some((Arc::new(decoded), clipped_spec)))
}
