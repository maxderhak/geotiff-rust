//! Property-based writer -> reader roundtrips for the fork's NEW capabilities.
//!
//! `proptest_roundtrip.rs` (the template for the mechanics here) covers only
//! byte-aligned sample types (u8/u16/i16/u32/f32/f64) and 1..=3 bands. It does
//! NOT exercise anything the Onyx fork actually added. This file closes that
//! gap with genuinely non-vacuous properties for:
//!
//!   * sub-byte depths {1,2,4}-bit (typed/unpacked read AND the packed
//!     accessors), across LE/BE x strips/tiles x chunky/planar x
//!     None/Lzw/Deflate/Zstd;
//!   * N-ink `Separated` extremes (spp in {6,16}, `InkSet::NotCmyk`, with and
//!     without declared extra samples);
//!   * `IccLab` photometric 9 (3 base samples, 8/16-bit, with/without an extra
//!     sample), proving raw storage passthrough.
//!
//! Every expected value is computed by an INDEPENDENT reimplementation of the
//! spec — an independent block-layout mapping (`expected_position`) for pixels,
//! and an independent MSB-first bit-packer (`independent_msb_pack`, an
//! accumulator, not the fork's index/shift arithmetic) for the packed bytes.
//! Neither calls fork packing/unpacking code, so a mismatch is a real defect,
//! not a tautology.
//!
//! The dedicated `packed_subbyte_read.rs` tests prove the packed accessors copy
//! NON-ZERO on-disk trailing padding verbatim (authored via a bit-flip on the
//! encoded file). A property test writes fresh images, whose padding is always
//! zero, so it cannot distinguish verbatim-copy from repack on the padding
//! bits. What it CAN and does prove — over many random shapes — is that the
//! packed bytes are value-exact against the independent packer on BOTH the
//! byte-aligned window path (verbatim fast path) and the bit-granular window
//! path (repack from bit 0), and that the planar non-band packed guard fires.

use std::io::Cursor;

use proptest::prelude::*;
use tiff_core::{
    ByteOrder, ColorModel, Compression, ExtraSample, InkSet, PhotometricInterpretation,
    PlanarConfiguration,
};
use tiff_reader::{TiffError, TiffFile};
use tiff_writer::{ImageBuilder, TiffVariant, TiffWriter, WriteOptions};

// ---------------------------------------------------------------------------
// Shared independent oracle: block layout mapping + deterministic generators.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Layout {
    Strips { rows_per_strip: u32 },
    Tiles { width: u32, height: u32 },
}

/// The geometry the independent oracle needs to map pixels to storage.
#[derive(Debug, Clone, Copy)]
struct Grid {
    width: u32,
    height: u32,
    bands: u16,
    planar: PlanarConfiguration,
    layout: Layout,
}

/// Deterministic per-(block, offset) seed, independent of the writer's own
/// data path (the writer just receives the values we hand it).
fn sample_seed(block_index: usize, offset: usize) -> u64 {
    (block_index as u64)
        .wrapping_mul(1_000_003)
        .wrapping_add((offset as u64).wrapping_mul(7919))
        .wrapping_add(0x9E37_79B9)
}

/// Independent block-layout mapping: pixel (row, col, band) -> (block, offset),
/// reimplemented from the TIFF strip/tile + chunky/planar spec (mirrors the
/// proven mapping in `proptest_roundtrip.rs`).
fn expected_position(grid: &Grid, row: usize, col: usize, band: usize) -> (usize, usize) {
    let width = grid.width as usize;
    let height = grid.height as usize;
    let bands = grid.bands as usize;
    let planar = matches!(grid.planar, PlanarConfiguration::Planar);
    let block_bands = if planar { 1 } else { bands };

    match grid.layout {
        Layout::Strips { rows_per_strip } => {
            let rps = rows_per_strip as usize;
            let strips_per_plane = height.div_ceil(rps);
            let strip = row / rps;
            let block = if planar {
                band * strips_per_plane + strip
            } else {
                strip
            };
            let offset = ((row % rps) * width + col) * block_bands + if planar { 0 } else { band };
            (block, offset)
        }
        Layout::Tiles {
            width: tw,
            height: th,
        } => {
            let tw = tw as usize;
            let th = th as usize;
            let tiles_across = width.div_ceil(tw);
            let tiles_down = height.div_ceil(th);
            let tile = (row / th) * tiles_across + col / tw;
            let block = if planar {
                band * (tiles_across * tiles_down) + tile
            } else {
                tile
            };
            let offset = ((row % th) * tw + col % tw) * block_bands + if planar { 0 } else { band };
            (block, offset)
        }
    }
}

/// Number of samples the writer expects for `block` under `grid`.
fn block_len(grid: &Grid, block: usize) -> usize {
    let width = grid.width as usize;
    let height = grid.height as usize;
    let block_bands = if matches!(grid.planar, PlanarConfiguration::Planar) {
        1
    } else {
        grid.bands as usize
    };
    match grid.layout {
        Layout::Strips { rows_per_strip } => {
            let rps = rows_per_strip as usize;
            let strips_per_plane = height.div_ceil(rps);
            let strip = block % strips_per_plane;
            let rows = rps.min(height - strip * rps);
            rows * width * block_bands
        }
        Layout::Tiles {
            width: tw,
            height: th,
        } => (tw as usize) * (th as usize) * block_bands,
    }
}

/// Independent MSB-first bit-packer, written straight from the TIFF6 rule with
/// a bit accumulator (deliberately NOT the fork's `i / samples_per_byte` +
/// shift arithmetic): within a row the first sample occupies the most
/// significant bits; each row starts fresh on a byte boundary; the final
/// partial byte is left-aligned with low padding bits zero. `bits` must be
/// 1, 2, or 4.
fn independent_msb_pack(rows: &[Vec<u8>], bits: u16) -> Vec<u8> {
    assert!(matches!(bits, 1 | 2 | 4));
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

/// Build the `ImageBuilder` photometric/extra-sample shape for a generic
/// multi-band grayscale/RGB image (mirrors `proptest_roundtrip.rs`): 1 band =
/// MinIsBlack, 2 bands = MinIsBlack + 1 extra, >=3 bands = RGB (+ extras for
/// bands > 3).
fn apply_generic_color(ib: ImageBuilder, bands: u16) -> ImageBuilder {
    if bands >= 3 {
        let extras = (bands - 3) as usize;
        let ib = ib.photometric(PhotometricInterpretation::Rgb);
        if extras > 0 {
            ib.extra_samples(vec![ExtraSample::Unspecified; extras])
        } else {
            ib
        }
    } else if bands == 2 {
        ib.photometric(PhotometricInterpretation::MinIsBlack)
            .extra_samples(vec![ExtraSample::Unspecified])
    } else {
        ib.photometric(PhotometricInterpretation::MinIsBlack)
    }
}

fn writer_with(byte_order: ByteOrder) -> TiffWriter<Cursor<Vec<u8>>> {
    TiffWriter::new(
        Cursor::new(Vec::new()),
        WriteOptions {
            byte_order,
            variant: TiffVariant::Auto,
        },
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Part C1.1 + C1.4 — sub-byte depths {1,2,4}, typed AND packed accessors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct SubByteConfig {
    width: u32,
    height: u32,
    bands: u16,
    bits: u16,
    compression: Compression,
    planar: PlanarConfiguration,
    layout: Layout,
    byte_order: ByteOrder,
}

impl SubByteConfig {
    fn grid(&self) -> Grid {
        Grid {
            width: self.width,
            height: self.height,
            bands: self.bands,
            planar: self.planar,
            layout: self.layout,
        }
    }
    fn value(&self, block: usize, offset: usize) -> u8 {
        (sample_seed(block, offset) % (1u64 << self.bits)) as u8
    }
    /// Independent expected sample for a logical pixel.
    fn expected(&self, row: usize, col: usize, band: usize) -> u8 {
        let (block, offset) = expected_position(&self.grid(), row, col, band);
        self.value(block, offset)
    }
}

fn subbyte_config_strategy() -> impl Strategy<Value = SubByteConfig> {
    let bits = prop_oneof![Just(1u16), Just(2u16), Just(4u16)];
    let compression = prop_oneof![
        Just(Compression::None),
        Just(Compression::Lzw),
        Just(Compression::Deflate),
        Just(Compression::Zstd),
    ];
    let planar = prop_oneof![
        Just(PlanarConfiguration::Chunky),
        Just(PlanarConfiguration::Planar),
    ];
    let byte_order = prop_oneof![Just(ByteOrder::LittleEndian), Just(ByteOrder::BigEndian)];

    (
        1u32..=24,
        1u32..=24,
        1u16..=4,
        bits,
        compression,
        planar,
        prop_oneof![Just(0u8), Just(1u8)], // strips vs tiles
        1u32..=6,                          // rows per strip (clamped to height)
        prop_oneof![Just(16u32), Just(32u32)],
        byte_order,
    )
        .prop_map(
            |(
                width,
                height,
                bands,
                bits,
                compression,
                planar,
                layout_choice,
                rows_per_strip,
                tile_size,
                byte_order,
            )| {
                let layout = if layout_choice == 0 {
                    Layout::Strips {
                        rows_per_strip: rows_per_strip.min(height),
                    }
                } else {
                    Layout::Tiles {
                        width: tile_size,
                        height: tile_size,
                    }
                };
                SubByteConfig {
                    width,
                    height,
                    bands,
                    bits,
                    compression,
                    planar,
                    layout,
                    byte_order,
                }
            },
        )
}

/// Smallest positive column offset whose left edge is byte-aligned in the
/// packed stream, `group` = bits carried per column (`spp*bits` chunky, `bits`
/// per band). Returns `None` if no interior byte-aligned column exists.
fn byte_aligned_col_off(width: usize, group: usize) -> Option<usize> {
    (1..width).find(|&c| (c * group) % 8 == 0)
}

/// Smallest positive column offset whose left edge is NOT byte-aligned.
fn bit_granular_col_off(width: usize, group: usize) -> Option<usize> {
    (1..width).find(|&c| (c * group) % 8 != 0)
}

fn run_subbyte(cfg: SubByteConfig) {
    let grid = cfg.grid();
    let width = cfg.width as usize;
    let height = cfg.height as usize;
    let bands = cfg.bands as usize;

    let mut ib = ImageBuilder::new(cfg.width, cfg.height)
        .bits_per_sample(cfg.bits)
        .samples_per_pixel(cfg.bands)
        .compression(cfg.compression)
        .planar_configuration(cfg.planar);
    ib = apply_generic_color(ib, cfg.bands);
    ib = match cfg.layout {
        Layout::Strips { rows_per_strip } => ib.strips(rows_per_strip),
        Layout::Tiles { width, height } => ib.tiles(width, height),
    };

    let mut writer = writer_with(cfg.byte_order);
    let block_count = ib.checked_block_count().unwrap();
    let handle = writer.add_image(ib).unwrap();
    for block in 0..block_count {
        let len = block_len(&grid, block);
        let samples: Vec<u8> = (0..len).map(|offset| cfg.value(block, offset)).collect();
        writer.write_block(&handle, block, &samples).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let file = TiffFile::from_bytes(bytes).unwrap();

    // --- Typed / unpacked read (read_image), full extent. ---
    let image = file.read_image::<u8>(0).unwrap();
    let expected_shape: &[usize] = if bands == 1 {
        &[height, width]
    } else {
        &[height, width, bands]
    };
    assert_eq!(image.shape(), expected_shape, "{cfg:?}");
    for row in 0..height {
        for col in 0..width {
            for band in 0..bands {
                let actual = if bands == 1 {
                    image[[row, col]]
                } else {
                    image[[row, col, band]]
                };
                assert_eq!(
                    actual,
                    cfg.expected(row, col, band),
                    "{cfg:?} typed read at ({row},{col},{band})"
                );
            }
        }
    }

    // --- Typed / unpacked read (read_window), a sub-window. ---
    let row_off = height / 2;
    let col_off = width / 2;
    let wrows = height - row_off;
    let wcols = width - col_off;
    let window = file
        .read_window::<u8>(0, row_off, col_off, wrows, wcols)
        .unwrap();
    for r in 0..wrows {
        for c in 0..wcols {
            for band in 0..bands {
                let actual = if bands == 1 {
                    window[[r, c]]
                } else {
                    window[[r, c, band]]
                };
                assert_eq!(
                    actual,
                    cfg.expected(row_off + r, col_off + c, band),
                    "{cfg:?} typed window at ({r},{c},{band})"
                );
            }
        }
    }

    // --- Packed accessors (C1.4). ---
    let planar = matches!(cfg.planar, PlanarConfiguration::Planar);

    // Independent chunky (pixel-interleaved) packed rows for the full image and
    // for a column window.
    let chunky_rows = |c0: usize, cn: usize, r0: usize, rn: usize| -> Vec<Vec<u8>> {
        (r0..r0 + rn)
            .map(|r| {
                (c0..c0 + cn)
                    .flat_map(|c| (0..bands).map(move |b| cfg.expected(r, c, b)))
                    .collect::<Vec<u8>>()
            })
            .collect()
    };
    // Independent per-band packed rows.
    let band_rows = |band: usize, c0: usize, cn: usize, r0: usize, rn: usize| -> Vec<Vec<u8>> {
        (r0..r0 + rn)
            .map(|r| (c0..c0 + cn).map(|c| cfg.expected(r, c, band)).collect())
            .collect()
    };

    if planar {
        // §3.6 guard: non-band packed accessors must LOUDLY reject planar
        // sub-byte storage.
        for result in [
            file.read_image_packed_bytes(0),
            file.read_window_packed_bytes(0, 0, 0, height, width),
        ] {
            match result.expect_err("planar sub-byte non-band packed read must error") {
                TiffError::InvalidImageLayout(msg) => {
                    assert!(
                        msg.contains("per-band packed accessors"),
                        "guard must point to per-band accessors, got: {msg}"
                    );
                }
                other => panic!("expected InvalidImageLayout, got {other:?}"),
            }
        }
        // Per-band packed read is the supported path and byte-exact.
        for band in 0..bands {
            let expected = independent_msb_pack(&band_rows(band, 0, width, 0, height), cfg.bits);
            let packed = file.read_band_packed_bytes(0, band).unwrap();
            assert_eq!(packed, expected, "{cfg:?} planar band {band} full packed");

            // Verbatim fast path: byte-aligned right-anchored column window.
            if let Some(c0) = byte_aligned_col_off(width, cfg.bits as usize) {
                let cn = width - c0;
                let exp = independent_msb_pack(&band_rows(band, c0, cn, 0, height), cfg.bits);
                let got = file
                    .read_band_window_packed_bytes(0, band, 0, c0, height, cn)
                    .unwrap();
                assert_eq!(got, exp, "{cfg:?} planar band {band} byte-aligned col window");
            }
            // Repack path: bit-granular (left-misaligned) column window.
            if let Some(c0) = bit_granular_col_off(width, cfg.bits as usize) {
                let cn = width - c0;
                let exp = independent_msb_pack(&band_rows(band, c0, cn, 0, height), cfg.bits);
                let got = file
                    .read_band_window_packed_bytes(0, band, 0, c0, height, cn)
                    .unwrap();
                assert_eq!(got, exp, "{cfg:?} planar band {band} bit-granular col window");
            }
        }
    } else {
        // Chunky: full-image packed == independent pixel-interleaved packing.
        let expected_full = independent_msb_pack(&chunky_rows(0, width, 0, height), cfg.bits);
        assert_eq!(
            file.read_image_packed_bytes(0).unwrap(),
            expected_full,
            "{cfg:?} chunky full packed (read_image_packed_bytes)"
        );
        assert_eq!(
            file.read_window_packed_bytes(0, 0, 0, height, width).unwrap(),
            expected_full,
            "{cfg:?} chunky full packed (read_window_packed_bytes)"
        );

        let group = bands * cfg.bits as usize;
        // Verbatim fast path: byte-aligned right-anchored column window.
        if let Some(c0) = byte_aligned_col_off(width, group) {
            let cn = width - c0;
            let exp = independent_msb_pack(&chunky_rows(c0, cn, 0, height), cfg.bits);
            let got = file.read_window_packed_bytes(0, 0, c0, height, cn).unwrap();
            assert_eq!(got, exp, "{cfg:?} chunky byte-aligned col window");
        }
        // Repack path: bit-granular (left-misaligned) column window.
        if let Some(c0) = bit_granular_col_off(width, group) {
            let cn = width - c0;
            let exp = independent_msb_pack(&chunky_rows(c0, cn, 0, height), cfg.bits);
            let got = file.read_window_packed_bytes(0, 0, c0, height, cn).unwrap();
            assert_eq!(got, exp, "{cfg:?} chunky bit-granular col window");
        }

        // A chunky sub-byte band is bit-interleaved with its neighbours, so a
        // single band is never a whole-byte on-disk run: it re-packs, but the
        // sample values are still byte-exact against the independent packer.
        for band in 0..bands {
            let exp = independent_msb_pack(&band_rows(band, 0, width, 0, height), cfg.bits);
            let got = file.read_band_packed_bytes(0, band).unwrap();
            assert_eq!(got, exp, "{cfg:?} chunky band {band} packed (repack)");
        }
    }
}

// ---------------------------------------------------------------------------
// Part C1.2 — N-ink Separated extremes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct SeparatedConfig {
    width: u32,
    height: u32,
    spp: u16,
    extras: u16,
    planar: PlanarConfiguration,
    compression: Compression,
    rows_per_strip: u32,
    byte_order: ByteOrder,
}

impl SeparatedConfig {
    fn grid(&self) -> Grid {
        Grid {
            width: self.width,
            height: self.height,
            bands: self.spp,
            planar: self.planar,
            layout: Layout::Strips {
                rows_per_strip: self.rows_per_strip.min(self.height),
            },
        }
    }
    fn value(&self, block: usize, offset: usize) -> u8 {
        (sample_seed(block, offset) & 0xFF) as u8
    }
    fn expected(&self, row: usize, col: usize, band: usize) -> u8 {
        let (block, offset) = expected_position(&self.grid(), row, col, band);
        self.value(block, offset)
    }
}

fn separated_config_strategy() -> impl Strategy<Value = SeparatedConfig> {
    let spp = prop_oneof![Just(6u16), Just(16u16)];
    let extras = prop_oneof![Just(0u16), Just(1u16), Just(2u16)];
    let planar = prop_oneof![
        Just(PlanarConfiguration::Chunky),
        Just(PlanarConfiguration::Planar),
    ];
    let compression = prop_oneof![
        Just(Compression::None),
        Just(Compression::Lzw),
        Just(Compression::Deflate),
        Just(Compression::Zstd),
    ];
    let byte_order = prop_oneof![Just(ByteOrder::LittleEndian), Just(ByteOrder::BigEndian)];

    (
        1u32..=6,
        1u32..=6,
        spp,
        extras,
        planar,
        compression,
        1u32..=4,
        byte_order,
    )
        .prop_map(
            |(width, height, spp, extras, planar, compression, rows_per_strip, byte_order)| {
                SeparatedConfig {
                    width,
                    height,
                    spp,
                    extras,
                    planar,
                    compression,
                    rows_per_strip,
                    byte_order,
                }
            },
        )
}

fn run_separated(cfg: SeparatedConfig) {
    let grid = cfg.grid();
    let width = cfg.width as usize;
    let height = cfg.height as usize;
    let spp = cfg.spp as usize;

    let extra_samples = vec![ExtraSample::Unspecified; cfg.extras as usize];
    let ib = ImageBuilder::new(cfg.width, cfg.height)
        .sample_type::<u8>()
        .samples_per_pixel(cfg.spp)
        .photometric(PhotometricInterpretation::Separated)
        .ink_set(InkSet::NotCmyk)
        .extra_samples(extra_samples)
        .compression(cfg.compression)
        .planar_configuration(cfg.planar)
        .strips(cfg.rows_per_strip.min(cfg.height));

    let mut writer = writer_with(cfg.byte_order);
    let block_count = ib.checked_block_count().unwrap();
    let handle = writer.add_image(ib).unwrap();
    for block in 0..block_count {
        let len = block_len(&grid, block);
        let samples: Vec<u8> = (0..len).map(|offset| cfg.value(block, offset)).collect();
        writer.write_block(&handle, block, &samples).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let file = TiffFile::from_bytes(bytes).unwrap();

    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.samples_per_pixel(), cfg.spp, "{cfg:?} spp");
    assert_eq!(ifd.ink_set().unwrap(), Some(InkSet::NotCmyk), "{cfg:?} inkset");
    match ifd.color_model().unwrap() {
        ColorModel::Separated {
            ink_set,
            color_channels,
            extra_samples,
        } => {
            assert_eq!(ink_set, InkSet::NotCmyk, "{cfg:?}");
            assert_eq!(
                color_channels,
                cfg.spp - cfg.extras,
                "{cfg:?} color_channels = spp - extras"
            );
            assert_eq!(extra_samples.len(), cfg.extras as usize, "{cfg:?} extras");
        }
        other => panic!("{cfg:?}: expected ColorModel::Separated, got {other:?}"),
    }

    let image = file.read_image::<u8>(0).unwrap();
    assert_eq!(image.shape(), &[height, width, spp], "{cfg:?} shape");
    for row in 0..height {
        for col in 0..width {
            for band in 0..spp {
                assert_eq!(
                    image[[row, col, band]],
                    cfg.expected(row, col, band),
                    "{cfg:?} at ({row},{col},{band})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Part C1.3 — IccLab photometric 9, raw storage passthrough.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct IccLabConfig {
    width: u32,
    height: u32,
    bits16: bool,
    extra: bool,
    planar: PlanarConfiguration,
    compression: Compression,
    byte_order: ByteOrder,
}

impl IccLabConfig {
    fn spp(&self) -> u16 {
        3 + if self.extra { 1 } else { 0 }
    }
    fn grid(&self) -> Grid {
        Grid {
            width: self.width,
            height: self.height,
            bands: self.spp(),
            planar: self.planar,
            layout: Layout::Strips {
                rows_per_strip: self.height,
            },
        }
    }
    fn u8_value(&self, block: usize, offset: usize) -> u8 {
        // Sweep the full 0..=255 range so a*/b* land on values (e.g. >=128)
        // that would move under any signed<->unsigned reinterpretation.
        (sample_seed(block, offset) & 0xFF) as u8
    }
    fn u16_value(&self, block: usize, offset: usize) -> u16 {
        // Include values straddling 0x8000 so a signed reinterpretation would
        // change them; a byte-exact roundtrip proves raw passthrough.
        (sample_seed(block, offset) & 0xFFFF) as u16
    }
}

fn icclab_config_strategy() -> impl Strategy<Value = IccLabConfig> {
    let planar = prop_oneof![
        Just(PlanarConfiguration::Chunky),
        Just(PlanarConfiguration::Planar),
    ];
    let compression = prop_oneof![
        Just(Compression::None),
        Just(Compression::Lzw),
        Just(Compression::Deflate),
        Just(Compression::Zstd),
    ];
    let byte_order = prop_oneof![Just(ByteOrder::LittleEndian), Just(ByteOrder::BigEndian)];
    (
        1u32..=6,
        1u32..=6,
        any::<bool>(),
        any::<bool>(),
        planar,
        compression,
        byte_order,
    )
        .prop_map(
            |(width, height, bits16, extra, planar, compression, byte_order)| IccLabConfig {
                width,
                height,
                bits16,
                extra,
                planar,
                compression,
                byte_order,
            },
        )
}

fn run_icclab(cfg: IccLabConfig) {
    let grid = cfg.grid();
    let width = cfg.width as usize;
    let height = cfg.height as usize;
    let spp = cfg.spp() as usize;
    let extras = if cfg.extra { 1 } else { 0 };

    // The typed value read differs by depth; write + read + assert per depth.
    macro_rules! roundtrip {
        ($ty:ty, $val:ident) => {{
            let ib = ImageBuilder::new(cfg.width, cfg.height)
                .sample_type::<$ty>()
                .samples_per_pixel(cfg.spp())
                .photometric(PhotometricInterpretation::IccLab)
                .extra_samples(vec![ExtraSample::UnassociatedAlpha; extras])
                .compression(cfg.compression)
                .planar_configuration(cfg.planar)
                .strips(cfg.height);

            let mut writer = writer_with(cfg.byte_order);
            let block_count = ib.checked_block_count().unwrap();
            let handle = writer.add_image(ib).unwrap();
            for block in 0..block_count {
                let len = block_len(&grid, block);
                let samples: Vec<$ty> =
                    (0..len).map(|offset| cfg.$val(block, offset)).collect();
                writer.write_block(&handle, block, &samples).unwrap();
            }
            let bytes = writer.finish().unwrap().into_inner();
            let file = TiffFile::from_bytes(bytes).unwrap();

            let ifd = file.ifd(0).unwrap();
            assert_eq!(
                ifd.photometric_interpretation(),
                Some(PhotometricInterpretation::IccLab.to_code()),
                "{cfg:?} photometric"
            );
            match ifd.color_model().unwrap() {
                ColorModel::IccLab { extra_samples } => {
                    assert_eq!(extra_samples.len(), extras, "{cfg:?} extras");
                }
                other => panic!("{cfg:?}: expected ColorModel::IccLab, got {other:?}"),
            }

            let raw = file.read_image::<$ty>(0).unwrap();
            let shape: &[usize] = &[height, width, spp];
            assert_eq!(raw.shape(), shape, "{cfg:?} shape");
            for row in 0..height {
                for col in 0..width {
                    for band in 0..spp {
                        let (block, offset) = expected_position(&grid, row, col, band);
                        assert_eq!(
                            raw[[row, col, band]],
                            cfg.$val(block, offset),
                            "{cfg:?} raw at ({row},{col},{band})"
                        );
                    }
                }
            }
        }};
    }

    if cfg.bits16 {
        roundtrip!(u16, u16_value);
    } else {
        roundtrip!(u8, u8_value);
    }
}

// ---------------------------------------------------------------------------
// Proptest entry points. Case counts kept modest so CI time stays sane.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    #[test]
    fn subbyte_write_read_is_bit_exact(cfg in subbyte_config_strategy()) {
        run_subbyte(cfg);
    }

    #[test]
    fn nink_separated_write_read_is_byte_exact(cfg in separated_config_strategy()) {
        run_separated(cfg);
    }

    #[test]
    fn icclab_write_read_is_raw_byte_exact(cfg in icclab_config_strategy()) {
        run_icclab(cfg);
    }
}

// A single non-proptest smoke test keeps a fixed regression example that is
// easy to run in isolation and reason about, independent of the random seeds.
#[test]
fn subbyte_smoke_fixed_example() {
    run_subbyte(SubByteConfig {
        width: 9,
        height: 3,
        bands: 4,
        bits: 2,
        compression: Compression::Lzw,
        planar: PlanarConfiguration::Planar,
        layout: Layout::Strips { rows_per_strip: 1 },
        byte_order: ByteOrder::LittleEndian,
    });
}
