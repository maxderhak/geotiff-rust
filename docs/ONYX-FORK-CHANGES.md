# Onyx fork changes — inventory & upstreaming basis

> **Status: NOT YET SUBMITTED UPSTREAM.** This document is the basis for a
> future upstream contribution to `roteiro-gis/geotiff-rust`, not a submission.
> We are deliberately holding the pull request(s) until broader and more
> independent testing is complete (see *Testing status* and *Known limitations*
> below). This file inventories every change this fork adds on top of the
> upstream base, with the motivation each one carries, so it can be turned into
> one or more reviewer-facing PRs when the time comes.

## 1. Overview — who, why, and against what base

**Who.** These changes come from **Onyx**, which is porting a legacy C++
large-format-printer RIP (Raster Image Processor) to pure Rust. A RIP decodes
input rasters, applies color management and halftone screening, nests/lays out
pages, and emits per-separation output to large-format printers and cutters.

**Why TIFF, and why these changes.** The RIP's TIFF I/O has two hard
requirements that shaped every change in this fork:

1. **Byte-for-byte parity** with the legacy C `tif.c` for the TIFF shapes a RIP
   actually handles (sub-byte halftone separations, N-ink separated output, ICC
   Lab, single-giant-strip inputs). "Close enough" is not acceptable when the
   goal is to prove the Rust port produces bit-identical output to the code it
   replaces.
2. **Bounded streaming.** Input files are routinely multi-GB. Peak memory must
   be bounded by pipeline depth (the row-band a stage is working on), **not** by
   image size — a 10 GB raster must process in a few MB of working set.

Each change below is a **generally-useful TIFF capability** that happens to be
exactly what a production RIP needs — which is why it is worth upstreaming
rather than carrying as a private patch. None of it is Onyx-specific: it is
"write the bit depths and ink counts real print workflows use," "read the Lab
photometric ICC workflows carry," "read the exact packed bytes on disk," and
"don't materialize a whole image to read one row band."

**Base.** This fork branches from upstream at commit
`cf87152a1e2ec15c47337b48a37d892f64635678` (`Merge pull request #92 from
roteiro-gis/ian/dev`, 2026-08-12), which corresponds to the workspace at
version `0.8.1`. The fork is now the range `cf87152..0e6d907`: eight
behavior/API commits (`cf87152..e1d41e8`, §2 table) plus five follow-on
commits that resolve the doc's own open items rather than add new capability
— `602f5ae` + `32a9c4e` (root-cause + regression-guard the §5 LZW-interop
question), `c1fec81` (extend the §3.7 verbatim packed read to byte-aligned
column sub-windows), and `4a773c3` + `0e6d907` (property tests + a fuzz
target for the fork's new capabilities). They are additive: no existing
public API is removed or altered in behavior, and every upstream test still
passes.

**License compatibility.** This fork retains upstream's dual
**`MIT OR Apache-2.0`** licensing (`LICENSE-MIT`, `LICENSE-APACHE`, and
`license = "MIT OR Apache-2.0"` in the workspace manifest), so these changes are
license-compatible with upstream and carry no additional licensing constraints
for a contribution.

## 2. Summary of changes

| # | Change | Commit(s) | Crates touched | Public API added / changed |
|---|--------|-----------|----------------|----------------------------|
| 1 | Sub-byte (1/2/4-bit) write | `2cd42f3` | `tiff-writer`, `geotiff-writer` | `ImageBuilder::bits_per_sample(1\|2\|4)` now accepted (validated); `BlockEncodingOptions.bits_per_sample` field |
| 2 | N-channel `Separated` write | `0e7db53` | `tiff-writer` | `Separated` + `InkSet::NotCmyk` / `InkSet::Unknown(_)` writes an arbitrary ink count; `ImageBuilder::separated_base_samples` helper (private) |
| 3 | Bounded single-giant-strip read | `8c35d93` | `tiff-reader` | No new public API; `TiffFile::read_window` / `read_window_band` / `read_image` gain a bounded internal path. `strip.rs::read_strip_block_for_window` / `read_strip_block_bounded` (crate-internal); two helpers promoted to `pub(crate)` |
| 4 | Decode + write `IccLab` (photometric 9) | `b2cf15a` | `tiff-core`, `tiff-reader`, `tiff-writer` | `PhotometricInterpretation::IccLab`; `ColorModel::IccLab { extra_samples }`; `ImageBuilder::photometric(IccLab)` accepted |
| 5 | Packed sub-byte read | `af42280` | `tiff-reader` | `TiffFile::read_image_packed_bytes`, `read_window_packed_bytes`, `read_band_packed_bytes`, `read_band_window_packed_bytes` (+ `*_from_ifd` variants) |
| 6 | Planar packed sub-byte guard | `a126bd7` | `tiff-reader` | No new API; non-band packed accessors now `Err(Error::InvalidImageLayout)` on planar sub-byte |
| 7 | Verbatim packed read (perf) | `e1d41e8` | `tiff-reader` | No new API; packed accessors return decompressed packed rows verbatim on the full-width fast path |
| 8 | No photometric-synthesized `ExtraSamples` — emit only what the caller declares | _(this session)_ | `tiff-writer` | No new API; every photometric writes `ExtraSamples` only when explicitly declared (matches libtiff `TIFFTAG_EXTRASAMPLES` semantics) |

(`bfe8bdb` is a trivial clippy `needless_range_loop` cleanup inside the sub-byte
write test and adds no product behavior; it is folded into change #1.)

Changes 5–7 form one logical unit — the **packed read** API and its refinements
— and would most naturally be reviewed together (or as a small stack).

### 2.1 Follow-on commits (this session): validation + one refinement

None of these five commits change public API surface beyond §3.7's refinement
(row 7a below); four of the five are test/validation-only. They exist because
`docs/ONYX-FORK-CHANGES.md` (§5/§6, as they stood) flagged open questions —
this table row records how each was resolved.

| # | Change | Commit(s) | Crates touched | Nature |
|---|--------|-----------|----------------|--------|
| 7a | Verbatim packed read — extend to byte-aligned column sub-windows | `c1fec81` | `tiff-reader` | Refinement of #7 (new `packed_col_window_is_byte_aligned` gate; no new public API — same accessors, more cases now zero-copy/padding-exact) |
| 8 | `write_block` LZW interop — investigate + guard | `602f5ae`, `32a9c4e` | `tiff-writer` (doc/test), `tiff-integration` (tests) | Test/doc-only. Root-caused the §5 concern and found it does not reproduce at this HEAD (see §5 below); added a regression guard so it cannot silently regress |
| 9 | Property tests for new capabilities + packed-read fuzz target | `4a773c3`, `0e6d907` | `tiff-integration`, `fuzz` | Test-only. `proptest_new_shapes.rs` (sub-byte, N-ink Separated, IccLab, across strips/tiles/byte-order/compression) + a `cargo-fuzz` target (`tiff_packed_read`), compiled and corpus-seeded but not yet run (needs nightly + Linux CI) |

Row 7a is best reviewed alongside the #5–7 packed-read stack. Rows 8–9 are
validation-only and can be reviewed independently (or folded into the PR that
introduces the capability they validate).

## 3. Changes in detail

### 3.1 Sub-byte (1/2/4-bit) write — `2cd42f3`

**Motivation.** Halftone/screened RIP output is sub-byte dot data: each printed
separation is typically **1, 2, or 4 bits per sample** (the screen's dot-size
resolution). A RIP therefore has to *write* sub-byte rasters. Upstream could
already *read* sub-byte depths but the writer hard-rejected any
`BitsPerSample` other than 8/16/32/64, so screened output could not be emitted
at all.

**What changed.** `ImageBuilder::bits_per_sample` now accepts `1 | 2 | 4` in
addition to `8 | 16 | 32 | 64`. Sub-byte selection is *validated loudly* rather
than silently mis-encoded: it requires `SampleFormat::Uint`, `Predictor::None`,
and a non-LERC compression, because predictors and LERC are not defined for
packed sub-byte rows in this writer. Invalid combinations are rejected at
`add_image()` time. `tiff_writer::compress::compress_block` routes
`bits_per_sample < 8` through a new `compress_block_subbyte` path that packs
one-byte-per-sample values **MSB-first** into row bytes — the exact inverse of
the reader's `unpack_subbyte_block` — before compression (None / LZW / Deflate /
Zstd). Packed row sizing reuses `tiff_core::RasterLayout`'s
`checked_packed_row_bytes_for_width` helper rather than re-deriving the bit math.
Chunky, planar-separate (including `RowsPerStrip=1` line-interleaved), and tiled
layouts all round-trip byte-exact; tiled sub-byte needed no special-casing since
block framing was already layout-agnostic.

**Public API.** `ImageBuilder::bits_per_sample(1 | 2 | 4)` accepted;
`BlockEncodingOptions` gains a `bits_per_sample: u16` field. The geotiff-writer
COG path passes `T::BITS_PER_SAMPLE` through unchanged (COG never uses sub-byte
packing).

**Tests.** `integration-tests/tiff-integration/tests/subbyte_roundtrip.rs`
(write → read byte-exact for the sub-byte depths across chunky / planar /
line-interleaved / tiled).

**Upstreamability.** Symmetric completion of an existing reader capability;
purely additive to the writer; loudly gated against undefined combinations. Low
risk.

### 3.2 N-channel `Separated` write — `0e7db53`

**Motivation.** Large-format printing is **N-ink**: CMYK *plus* spot inks and
light inks (a 4+N channel separated image). The writer must emit an arbitrary
ink count, not just CMYK's fixed four. The reader already supported this — it
derives `ColorModel::Separated`'s `color_channels` as
`samples_per_pixel - extra_samples.len()` — so before this change a non-CMYK
separated image could be *read* but never *written* back, i.e. it could not
round-trip.

**What changed.** A shared `ImageBuilder::separated_base_samples` helper now
backs both places that previously hardcoded `4` (`validate_color_model` and
`effective_extra_samples`). `InkSet::Cmyk` (and the absent-InkSet default) is
unchanged: fixed 4 base inks, remainder as `ExtraSamples`. `InkSet::NotCmyk` /
`InkSet::Unknown(_)` now derive `base = samples_per_pixel - extra_samples.len()`
(requiring at least 1, mirroring the reader's "must have ≥1 base ink" check).
There is still **no `NumberOfInks` tag** written — the ink count stays implicit
on both sides, so write and read remain symmetric.

**Public API.** No new public type surface; `Separated` images with
`InkSet::NotCmyk` / `InkSet::Unknown(_)` are now writable.
`separated_base_samples` is a private helper.

**Tests.** `integration-tests/tiff-integration/tests/nink_separated_write.rs`:
6-channel and 16-channel `NotCmyk` (0 extras), a `NotCmyk` case with declared
extras (spp=7, 1 extra → 6 inks), and CMYK-unchanged regressions (spp=4 → 4
inks; spp=6 → 4 inks + 2 extras).

**Upstreamability.** Makes the writer symmetric with the existing reader;
CMYK behavior is provably unchanged (regression cases). Low risk.

### 3.3 Bounded single-giant-strip read — `8c35d93`

**Motivation.** RIP inputs are multi-GB, and a *very* common shape is a single
uncompressed strip spanning the whole image (`RowsPerStrip >= height`). Upstream
read the whole strip for any window request, so reading even one row band
materialized the entire image. The RIP streams row-bands, so it needs a windowed
read that touches only the rows a band needs, keeping peak memory bounded by
pipeline depth.

**What changed.** `TiffFile::read_window` / `read_window_band` (and `read_image`,
which is a full-window `read_window`) gain a bounded scanline path
(`strip.rs::read_strip_block_for_window` / `read_strip_block_bounded`) that fires
only when a strip spans the whole image (chunky or per-plane) **and**
`Compression::None` **and** there is no GDAL structural-metadata framing **and**
it is not subsampled non-JPEG YCbCr (whose on-disk layout groups rows into chroma
units, breaking a linear per-row seek). For an eligible strip it reads and
decodes only the `H * row_bytes` range the requested rows need. This is
sound because uncompressed TIFF decode is row-independent in this fork (byte-order
swap, sub-byte unpack, and predictors all reset every row), so a decoded row
sub-range is byte-identical to slicing those rows out of a whole-strip decode.

The bounded result is deliberately **not cached** under the strip's block-cache
key: a single giant strip has only one strip index, so caching a partial-row
decode there would serve stale/incomplete rows to a later request for a different
row range of the same strip. Ineligible strips (multi-strip, compressed, GDAL,
subsampled YCbCr) fall back to the unchanged whole-strip, cached path. The
bounded path still runs the same `validate_block_byte_count` budget check against
the full strip's declared byte count, so a corrupt/hostile `StripByteCounts`
value is rejected before any read even when the small range it needs would fit.

**Public API.** None. Internally, `BlockDecodeContext::is_subsampled_ycbcr_non_jpeg`
and the crate-root `validate_block_byte_count` move from private to `pub(crate)`
so `strip.rs` can reuse them instead of duplicating the checks.

**Tests.** `integration-tests/tiff-integration/tests/single_strip_bounded.rs`:
a deterministic bounded-read gate (256×4096 single-strip image read in 8-row
bands through a byte-counting source peaks at one row band, ~2048 bytes, instead
of the ~1 MB strip; every pixel matches a per-(row,col) fixture), plus
regressions asserting multi-strip, compressed-single-strip, and
planar-single-strip-per-plane layouts still work.

**Upstreamability.** A real, general memory-bound improvement for a widely-seen
file shape; carefully scoped to only the layouts where a linear per-row seek is
provably correct; the no-cache reasoning is subtle and is called out in the diff
and here for reviewers. Medium review surface, low behavioral risk.

### 3.4 Decode + write `IccLab` — photometric 9 — `b2cf15a`

**Motivation.** Print and color workflows carry ICC L\*a\*b\* imagery, which uses
TIFF `PhotometricInterpretation = 9`. Upstream had no mapping for code 9, so
every pixel-decode entry errored `unsupported photometric interpretation 9`.

**What changed.** `PhotometricInterpretation` gains an `IccLab` variant with
`from_code(9)` / `to_code() == 9`, and `ColorModel` gains a matching
`IccLab { extra_samples }`. Storage layout is identical to `CieLab` (photometric
8): three base samples (L\*, a\*, b\*) plus any extra samples. The reader decodes
it exactly as `CieLab`, handing back the **raw** storage samples untransformed —
the fork does *no* signed/unsigned a\*/b\* reinterpretation for either Lab
photometric; that is a consumer concern. The `IccLab` variant is kept **distinct**
from `CieLab` precisely so a consumer can tell the unsigned ICC a\*/b\* encoding
apart from CIELab's signed encoding. `ImageBuilder` accepts
`photometric(PhotometricInterpretation::IccLab)` (3 base inks, symmetric with the
reader), enabling byte-exact write→read round-trips.

**Public API.** `PhotometricInterpretation::IccLab`;
`ColorModel::IccLab { extra_samples: Vec<ExtraSample> }`;
`ImageBuilder::photometric(IccLab)` accepted. The pixel decoder's `can_passthrough`
and `decode_pixels` treat `IccLab` alongside `CieLab`.

**Tests.** Added cases in
`integration-tests/tiff-integration/tests/tiff_writer_roundtrip.rs` (photometric-9
write→read round-trip), plus a `tiff-core` unit test asserting
`from_code(9) == Some(IccLab)`, `to_code() == 9`, and that 9 is distinct from 8.

**Upstreamability.** Fills a genuine gap (a standard TIFF photometric code that
upstream rejects). The "distinct variant, no reinterpretation" design is a
deliberate choice worth a reviewer's attention — see §4.

### 3.5 Packed sub-byte read — `af42280`

**Motivation.** The RIP's canonical halftone payload is the **packed, MSB-first,
on-disk sub-byte representation** — the exact bytes legacy `tif.c` reads. The
existing reader unpacks sub-byte samples to one byte per sample, which is the
wrong shape (and the wrong bytes) for a consumer that wants the on-disk packed
form for byte-parity. So the RIP needs to read the raw packed bytes directly.

**What changed.** New packed-storage accessors on `TiffFile` that return the raw
on-disk MSB-first packed bytes for sub-byte images instead of the one-byte-per-
sample unpacked output. For byte-aligned depths (8/16/32/64-bit) the packed bytes
are identical to the unpacked storage bytes, so a caller can use the packed path
for all depths. Implemented as a clean **parallel path**: it reuses the existing
bounded/windowed unpacked decode, then re-packs sub-byte samples MSB-first (the
exact inverse of `block_decode::unpack_subbyte_block`). `block_decode`, the block
cache, and the strip/tile copy machinery are untouched, so the existing unpacked
API and all its tests are unchanged. Compression is still decompressed; only the
sub-byte bit-unpack is skipped; reads stay bounded per strip/tile.

**Public API.** `TiffFile::read_image_packed_bytes(ifd_index)`,
`read_window_packed_bytes(ifd_index, row_off, col_off, rows, cols)`,
`read_band_packed_bytes(ifd_index, band_index)`,
`read_band_window_packed_bytes(ifd_index, band_index, row_off, col_off, rows,
cols)`, each with an `*_from_ifd` variant taking `&Ifd`. Per-interleaved-row
packed length is `ceil(cols * samples_per_pixel * bits / 8)`; per-sample-plane
row for a single band is `ceil(cols * bits / 8)`. With `col_off > 0` or a
partial-width window, samples are re-packed starting fresh at bit 0 of each
output row (not a copy of a mid-byte on-disk range).

**Tests.** `integration-tests/tiff-integration/tests/packed_subbyte_read.rs`:
2-bit 4-channel planar-LineInterleaved (`RowsPerStrip=1`), 1-bit and 4-bit chunky
(byte-exact against an independent MSB-first packing), and 8/16-bit (packed ==
unpacked), plus crate-level unit tests for the repack helper.

**Upstreamability.** Additive parallel API; leaves the whole existing decode path
and its tests untouched. The "packed accessors parallel to unpacked accessors"
shape is a design point worth discussing upstream — see §4.

### 3.6 Planar packed sub-byte guard — `a126bd7`

**Motivation.** The whole-image packed accessors (`read_image_packed_bytes` /
`read_window_packed_bytes` and their `_from_ifd` forms) route through the
interleaved window decode. For `PlanarConfiguration=2` sub-byte storage, the
on-disk layout is **per-plane**, so those accessors cannot express the on-disk
bytes — they would re-interleave the planes and return bytes that do not match
disk, while the API docs promise exact on-disk bytes. Fail-loud is better than
silent-wrong.

**What changed.** A runtime guard in `decode_window_packed_bytes` rejects sub-byte
(`bits < 8`) non-chunky (planar) layouts with `Error::InvalidImageLayout`,
directing callers to the per-band packed accessors (which work for both chunky and
planar). Byte-aligned planar and all chunky layouts are unaffected. The rustdoc on
`read_image_packed_bytes` / `read_window_packed_bytes` (and the CHANGELOG entry) is
qualified: exact-on-disk holds for chunky; planar sub-byte errors; and the
`col_off > 0` / partial-width fresh-from-bit-0 re-packing is documented on the
window/band accessors.

**Public API.** No new surface; the two non-band packed accessors now return
`Err(Error::InvalidImageLayout)` for planar sub-byte instead of wrong bytes.
(`Error::InvalidImageLayout` is an existing variant, reused here.)

**Tests.** `packed_subbyte_read.rs` gains
`planar_subbyte_non_band_packed_read_is_rejected` (the guard fires with the
specific error and message substrings; the per-band path is still byte-exact) and
`subbyte_col_offset_repacks_from_bit_zero` (pins fresh-from-bit-0 semantics for
`col_off > 0`).

**Upstreamability.** A correctness guardrail on the API added in §3.5; embodies
the fork's "fail loud, never silently return wrong bytes" stance. Low risk.

### 3.7 Verbatim packed read — perf — `e1d41e8`

**Motivation.** For byte-parity *and* for large rasters, the packed read should
avoid needless work and must preserve the on-disk **trailing padding bits**
byte-exact. The §3.5 repack path zero-fills padding and does a full
unpack→repack pass; for a full-width read that is both slower and can differ from
disk in the padding bits.

**What changed.** For a full-width sub-byte window, the packed accessors now
return the decompressed packed rows **verbatim** — skipping the per-sample unpack
pass, the per-sample repack pass, and the intermediate one-byte-per-sample buffer
— which also preserves the on-disk trailing padding bits byte-exact. A `packed`
flag threads through `block_decode` (skips `unpack_subbyte_block`, keeps
endianness/predictor), strip read / bounded / for-window, and the **block cache
key** (so packed and unpacked decodes of the same block never collide). A
full-width whole-row copy replaces the interleave copy on the packed path.

Verbatim applies to full-width chunky strips
(`read_image` / `read_window_packed_bytes`) and full-width planar per-band reads
(`read_band[_window]_packed_bytes`). The existing repack path is kept
(documented) for column sub-windows (`col_off > 0` or `cols < width`,
bit-granular), chunky bit-interleaved single bands, and tiled sub-byte (per-tile
packing does not align to a full-width row). The planar non-band rejection (§3.6)
and the unpacked/decoded API are unchanged.

**Extended (this session) — `c1fec81`.** The verbatim path above only covered
*full-width* windows. A follow-on commit widened it to every **byte-aligned
column sub-window**, not just full width: a `col_off > 0` / partial-width
sub-byte window is now returned verbatim (exact on-disk bytes, including
trailing padding) whenever its byte range is a contiguous whole-byte
sub-range of the packed row — the left edge starts on a byte boundary
(`col_off * samples_per_pixel * bits` a multiple of 8 for a chunky
interleaved row, or `col_off * bits` for a planar band) *and* the right edge
either ends on a byte boundary or runs to the image width. This covers, for
example, a window anchored at the right image edge whose left edge happens
to fall on a byte boundary — previously that case re-packed and zero-filled
the on-disk padding; now it is copied verbatim. A new
`packed_col_window_is_byte_aligned(col_off, col_end, width, unit_bits)`
predicate (`tiff-reader/src/lib.rs`) encodes the rule, and
`copy_strip_packed_window_block` (`tiff-reader/src/strip.rs`) takes an
explicit byte offset + output row width so it can slice the sub-range out of
each full-width decoded row (full-width is the offset-0 special case of the
same code path). Two boundaries remain, and are unaffected by this change —
see §5.

**Public API.** None — same accessors as §3.5, now faster and padding-exact on
the full-width fast path.

**Tests.** `packed_subbyte_read.rs` gains coverage asserting the verbatim path
preserves on-disk padding bits and that packed/unpacked cache entries do not
collide, across chunky and planar full-width reads.

**Upstreamability.** Performance + exactness refinement of §3.5, cleanly scoped to
where a whole-row copy is valid, with a documented fallback everywhere else. The
cache-key extension (packed vs unpacked) is a correctness detail reviewers should
note.

### 3.8 No photometric-synthesized `ExtraSamples` — emit only what the caller declares — _(this session)_

**Motivation.** Onyx encodes **spectral** rasters as `MinIsBlack`/`MinIsWhite`
with N samples, where every sample is a spectral band — all base image data,
with **no** extra samples. The writer previously *synthesized* `ExtraSamples`
(tag 338) for such an image, and — more broadly — for *any* photometric whose
`samples_per_pixel` exceeded its fixed base: `effective_extra_samples()` derived
a base sample count per photometric (1 for `MinIsBlack`/`MinIsWhite`/`Palette`/
`Mask`, 3 for `Rgb`/`YCbCr`/`CieLab`/`IccLab`, 4 for `Separated` `InkSet::Cmyk`),
then `effective_extra_samples_for_base()` PADDED the declared list to
`spp − base` `Unspecified` codes, and the IFD emitter wrote the tag because the
padded list was non-empty. So a spectral 5-band `MinIsBlack` got a bogus
4-entry `ExtraSamples` tag, an `Rgb` spp=4 got 1 synthesized `Unspecified`, a
`Separated` CMYK spp=6 got 2 — in every case a tag the caller never asked for,
describing base image channels as extra samples.

**What changed (one uniform change).** `effective_extra_samples_for_base()` no
longer pads: after the existing per-photometric over-declaration guard it returns
`self.extra_samples.clone()` directly — the extras the caller **explicitly**
declared via `ImageBuilder::extra_samples(...)`, and nothing more. This is a
single change routed through the shared path, so it is **uniform across every
photometric**: a caller that declares no extras writes **no** `ExtraSamples` tag;
a caller that declares extras writes exactly those codes. `implied_extra_samples`
(`spp − base`) is still computed purely to keep the over-declaration guard **tight
and per-photometric** (`declared > implied` → loud `InvalidConfig`; e.g. `Rgb`
spp=4 still rejects >1 declared extra) — the guard is deliberately **not** loosened
to a flat `<= samples_per_pixel`, which would let a caller null out an `Rgb`
image's colour channels.

**Reader — unchanged.** `resolve_fixed_model_extra_samples`
(`tiff-reader/src/ifd.rs`) already tolerates an absent `ExtraSamples` tag: for a
fixed-base photometric with `spp = N`, `declared = 0`, it resizes to `N − base`
`Unspecified` **in memory** and returns `Ok`, so a file written without the tag
reads back with byte-identical pixels (extras are metadata, never pixel bytes).
The complaint was the *written* tag, not the in-memory model, so the reader is
deliberately left as-is (proven by write→read byte-exact round-trip tests for
`MinIsBlack`/`MinIsWhite`, `Rgb` spp=4, and `Separated` CMYK spp=6).

**Public API.** None. Images simply no longer carry a synthesized `ExtraSamples`
tag unless the caller declared extras.

**Tests.** `integration-tests/tiff-integration/tests/minisx_extra_samples_write.rs`:
(1) multi-sample `MinIsBlack` (spp=5) and `MinIsWhite` (spp=4) with no declared
extras assert tag 338 is **absent** (these were the red-first cases); (2) the
same, read back via `TiffFile`, assert `samples_per_pixel == N` and byte-exact
pixels; (3) explicit-declaration cases across photometrics still emit exactly the
declared codes (`MinIsBlack`/`MinIsWhite`, `Rgb`+alpha, `Separated` CMYK+2 spot);
(4) the universal-scope cases — `Rgb` spp=4 and `Separated` CMYK spp=6 with no
declared extras now write **no** tag (formerly 1 and 2 synthesized) and round-trip
pixel-identical, with the reader still modelling `base + (N−base)` extras in
memory; over-declaration still errors loudly for both `MinIsBlack` and `Rgb`.
The pre-existing `nink_separated_write.rs` and `proptest_*` suites (which assert
on the *reader's* in-memory model, or declare their extras explicitly) stay green.

**Upstreamability — matches libtiff.** This *aligns* the fork with libtiff rather
than diverging from it. libtiff writes `ExtraSamples` only when the application
explicitly sets it: `tif_dirwrite.c` gates the field on `FIELD_EXTRASAMPLES`
(verified in `tiff-4.0.3` — `TIFFWriteDirectorySec` writes tag 338 only under
`TIFFFieldSet(tif, FIELD_EXTRASAMPLES)`), and `tif_dir.c`'s `setExtraSamples` is
the only setter — the tag is never derived from photometric. On read, libtiff
leniently defaults excess channels to `EXTRASAMPLE_UNSPECIFIED` without requiring
the tag — exactly what this fork's reader already does (the tolerant resize
above). So after this change the fork writes `ExtraSamples` solely when the client
declares it, and both sides default excess channels leniently, matching libtiff.
The write client owns declaring its real extras. (One mild semantic note for
reviewers: a strict reader that *expects* every extra plane of a multi-channel
image to be tagged `ExtraSamples` would see undeclared planes here as image data;
this is the intended interpretation and, per the above, is consistent with
libtiff's documented lenient default.)

## 4. Design decisions worth upstream discussion

- **`IccLab` is a distinct variant, not folded into `CieLab`.** Storage decode is
  identical, so a "just decode 9 as 8" approach would work mechanically. We kept
  them distinct so a downstream consumer can distinguish ICC's unsigned a\*/b\*
  encoding from CIELab's signed encoding — the encoding difference is real even
  though the *storage bytes* are read the same way. An upstream maintainer may
  prefer a different modeling (e.g. a flag on one variant); worth a conversation.
- **The fork does no Lab a\*/b\* canonicalization.** For both photometric 8 and 9
  the reader returns raw storage samples; signed/unsigned reinterpretation is left
  to the consumer. This keeps the reader honest (it reports what's on disk) but
  means an upstream consumer must not assume the reader normalized Lab.
- **Packed accessors parallel the unpacked accessors** rather than adding a mode
  flag to the existing ones. This keeps the existing API and its tests untouched
  and makes the packed contract explicit in the method name. The alternative (a
  parameter or options flag) would be more compact but would touch every existing
  call site's semantics.
- **Bounded single-strip reads are intentionally not cached.** With one strip
  index, a cached partial-row decode would be served for a different row range —
  so the bounded path skips the cache by design. This trades a (nonexistent, for
  this shape) cache benefit for correctness.
- **Fail-loud planar packed guard.** Rather than silently re-interleaving planar
  sub-byte into chunky-looking bytes, the non-band packed accessors error and
  point at the per-band accessors. Consistent with the project's "reject, don't
  silently mis-encode" posture elsewhere.
- **Sub-byte write rejects predictors and LERC** at build time rather than
  emitting undefined output. Same posture as above.
- **The reader's `Separated ⇒ ≥4 samples` rule is kept, NOT relaxed.**
  `color_model()` (`tiff-reader/src/ifd.rs`) defaults an absent `InkSet` to
  `Cmyk` (per TIFF 6.0 §17) and then requires `SamplesPerPixel ≥ 4`, refusing a
  3-sample `Separated` image with *"Separated ... requires at least 4 samples"*.
  Onyx hit a real file that trips this — a driver pass-through tagged
  `Photometric=5` with `spp=3` and no `InkSet`/`InkNames` — and evaluated
  relaxing the rule to derive `color_channels = spp − extra_samples` (the same
  N-ink derivation the explicit-`InkSet=2` branch already uses). **We decided
  against relaxing it, on 2026-08-18, because the reference implementation draws
  the same line.** libtiff is two-tier: its *raw* decode path
  (`tif_read.c`, `TIFFReadScanline`/`TIFFReadEncodedStrip`) is
  photometric-agnostic and reads such a file as opaque samples with no color
  meaning, but its *color-aware* path (`tif_getimage.c`, `TIFFRGBAImageOK`)
  **refuses** it identically — `if (td->td_samplesperpixel < 4) { "Sorry, can
  not handle separated image with Samples/pixel=..." }` — and never fabricates a
  3-ink interpretation (its defaulted `NumberOfInks` is hardcoded `4`, not
  `spp`; `tif_aux.c`). This fork's `color_model()` is the analogue of libtiff's
  color-aware tier, so refusing a 3-sample `Separated` is *correct* behavior for
  it, not a gap. A 3-sample `Separated` file is internally contradictory
  (CMYK-default implies 4 inks) and is almost always mislabeled RGB; the right
  fixes are (a) read it through a raw/opaque tier if truly needed, and (b) stop
  emitting the mislabeled tag at the source — which is what Onyx did (the
  emitter already writes `Photometric=2` for RGB; the offending file was a stale
  pre-fix capture). An upstream maintainer who *wants* a permissive N-ink
  `Separated` reader could add it as an explicit opt-in tier rather than
  loosening the default color model — worth a conversation, but the strict
  default should stay the default.

## 5. Known limitations & deferred follow-ons

These are the honest gaps that remain after this session's follow-on work
(`602f5ae`/`32a9c4e`, `c1fec81`, `4a773c3`/`0e6d907`) — see §6 for what that
work resolved and what is still outstanding before we ask upstream to review.

- **`write_block` LZW interop — RESOLVED, was based on an inaccurate premise.**
  This bullet previously described the fork's `write_block` LZW output as
  incrementally encoded and un-decodable by at least one strict TIFF LZW
  decoder. Investigation (`602f5ae`, `32a9c4e`) found the premise inaccurate:
  `compress_lzw` has been a **one-shot** `weezl` `Encoder::encode` call
  (`into_vec().encode_all()`, which emits the `EndOfInformation` end marker)
  since the crate was created — `git log -S 'fn compress_lzw'` shows no
  incremental `encode_bytes`-loop form ever existed — and its output is
  byte-identical to `into_stream().encode_all()` (the form Onyx's own
  self-encode workaround uses), proven by a new unit test
  (`compress_lzw_matches_one_shot_encode_all_and_is_decodable`,
  `tiff-writer/src/compress.rs`). A second question — whether the *bytes fed
  to* that encoder for sub-byte writes are `tif.c`-correct — was closed by
  LZW-decoding the fork's raw written strip bytes and comparing them against
  an independent, from-scratch MSB-first bit-packer (not the fork's own
  `pack_subbyte_rows`/`unpack_subbyte_block`) for 1/2/4-bit chunky (including
  multi-channel pixel-interleaved) and 2-bit 4-channel planar-separate
  (`writeblock_subbyte_lzw_packing_matches_independent_msb_packer_{chunky,planar}`,
  `tiff-integration/tests/writeblock_lzw_roundtrip.rs`). An image-rs `tiff`
  0.11.3 decode (a fully independent, non-`weezl` LZW implementation) confirms
  the chunky 4-bit case end-to-end; it does not support planar N-channel
  sub-byte, so that shape relies on the packing-chain proof rather than a
  second independent decoder. Composing both facts: the fork's sub-byte LZW
  strip is byte-identical to `encode_all(tif.c-correct packing)` — exactly
  the stream Onyx's self-encode workaround produces — so this is no longer an
  open interop risk for the shapes this fork writes. The earlier failure
  Onyx saw is attributed to an immature/absent sub-byte write path or a stale
  fork pin predating sub-byte write (`2cd42f3`, landed this same session),
  not an encoder defect; a regression guard (the unit test above, plus five
  round-trip integration tests) now pins the contract so it cannot silently
  regress.
- **Verbatim packed read scope — narrowed.** The zero-copy verbatim fast path
  (§3.7) originally covered full-width chunky strips and full-width planar
  bands only. It has been extended (`c1fec81`) to every **byte-aligned**
  column sub-window as well (chunky non-band and planar per-band), covering
  in particular a `col_off > 0` window anchored at the image's right edge.
  What remains repack-only, and is expected to stay that way (not zero-copy,
  but pixel-correct): a genuinely **bit-granular** column window (a mid-byte
  `col_off`, or a partial-width window ending mid-byte before the image
  edge), a **chunky bit-interleaved single band** (structurally never a
  whole-byte on-disk run), and **tiled** sub-byte storage (on-disk tiles are
  tile-row-packed, a different framing from the accessors' image-row-packed
  contract).
- **Planar Lab de-interleave.** Lab (photometric 8/9) a\*/b\*
  canonicalization is a **consumer concern the fork deliberately does not
  perform** — the reader returns raw Lab storage samples without
  normalization for both `CieLab` and `IccLab`. An upstream consumer should
  not be surprised by this. (The `col_off>0` sub-byte doc-placement nit
  previously noted here was fixed in `c1fec81`: the note now lives on
  `read_band_window_packed_bytes`, the accessor that actually takes
  `col_off`/`cols`.)

## 6. Testing status

- **Fork's own tests are green.** Every change above is covered by the fork's own
  round-trip / parity integration tests (listed per change in §3) plus unit
  tests, and the full suite passes. No existing upstream test was weakened or
  removed; the CMYK-separated and unpacked-read paths carry explicit regression
  cases proving unchanged behavior.
- **Downstream byte-exact validation.** Every change has additionally been
  exercised downstream by Onyx against a **byte-exact `tif.c` oracle** (the legacy
  C RIP's TIFF I/O), which is the strongest validation available for these shapes.
- **Property-based coverage of the new capabilities (this session).**
  `integration-tests/tiff-integration/tests/proptest_new_shapes.rs` sweeps
  sub-byte (1/2/4-bit) write/read, N-ink `Separated` write/read (spp 6/16),
  and `IccLab` write/read (8/16-bit) — each across chunky/planar, strips
  (varied strip count) *and* tiles, LE/BE byte order, and
  None/LZW/Deflate/Zstd compression, checked against an independent
  pixel-position oracle and an independent MSB-first packing oracle (neither
  reimplements the fork's own indexing/packing code), 96 randomized cases per
  strategy. A deliberately seeded one-line bug in the *oracle* (not the fork)
  was confirmed to turn all four tests red, showing the assertions are
  load-bearing rather than vacuous.
- **A `cargo-fuzz` target exists for the packed-read API but has not been
  run.** `fuzz/fuzz_targets/tiff_packed_read.rs` opens arbitrary bytes and
  drives all four packed accessors (whole-image, windowed, per-band,
  per-band-windowed) with bounded budgets, panicking/OOM as the only
  failure signal; it compiles and lints clean on stable, and its corpus is
  seeded (including the two Onyx sub-byte shapes), but the actual libFuzzer
  run requires `cargo-fuzz` + a nightly toolchain, unavailable in this
  Windows/stable dev environment. **The run itself is still outstanding** —
  it belongs on CI/Linux.
- **Why we are not submitting yet.** The `write_block` LZW interop question
  raised in an earlier draft of this document has been root-caused and
  resolved (§5) — it is no longer a gate. What remains before asking an
  upstream maintainer to review: actually **running** the `tiff_packed_read`
  fuzz target (CI/Linux, nightly), and any wider real-file interop the
  maintainer wants beyond the GDAL fixtures the existing upstream suite
  already exercises (e.g. broader GDAL/libtiff round-trips specifically for
  the new sub-byte/N-ink/IccLab shapes, should the maintainer ask for it).
  **This document is the basis for that future request, not the request
  itself.**

## 7. License / compatibility

This fork is licensed identically to upstream: **MIT OR Apache-2.0**
(`LICENSE-MIT` + `LICENSE-APACHE`, `license = "MIT OR Apache-2.0"` in the
workspace manifest). All changes here are contributed under the same dual
license, so incorporating them upstream introduces no new licensing constraints.
The changes are additive and preserve upstream's public API and existing test
behavior, so they are intended to be mergeable without a breaking-change release.
