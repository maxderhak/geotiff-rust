//! Writer coverage for `MinIsBlack` / `MinIsWhite` with more than one sample.
//!
//! Onyx encodes **spectral** images as `MinIsBlack`/`MinIsWhite` with N
//! samples, where every sample is base image data (a spectral band) — there
//! are no extra samples. The fork writer previously *synthesized* an
//! `ExtraSamples` tag (338) with `N-1` `Unspecified` codes for such an image,
//! emitting a tag the caller never asked for. This suite pins the fixed
//! behavior:
//!
//! * multi-sample `MinIsBlack`/`MinIsWhite` with **no** declared extras writes
//!   **no** `ExtraSamples` tag (spectral N-band data), and round-trips
//!   byte-exact through the tolerant reader; and
//! * a caller that **explicitly** declares extras on a `MinIsBlack`/`MinIsWhite`
//!   image still gets exactly those codes emitted (explicit control preserved).
//!
//! The fix is **universal**: the fork writer no longer pads/synthesizes
//! `ExtraSamples` from the photometric for *any* interpretation — every
//! photometric emits only the caller's declared extras. RGB and Separated-CMYK
//! are covered here too as regressions, proving the tag is likewise **omitted**
//! when nothing is declared (and still emitted verbatim when it is), while the
//! per-photometric over-declaration guard stays loud.

use std::io::Cursor;

use tiff_core::{
    ExtraSample, InkSet, PhotometricInterpretation, TagValue, TAG_EXTRA_SAMPLES,
};
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

/// Write a single-row, chunky, 8-bit image with the given photometric and
/// (optionally) explicit extra samples, returning the serialized TIFF bytes.
fn write_image(
    width: u32,
    samples_per_pixel: u16,
    photometric: PhotometricInterpretation,
    ink_set: Option<InkSet>,
    extra_samples: Vec<ExtraSample>,
    values: &[u8],
) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let mut image = ImageBuilder::new(width, 1)
        .sample_type::<u8>()
        .samples_per_pixel(samples_per_pixel)
        .photometric(photometric)
        .extra_samples(extra_samples)
        .strips(1);
    if let Some(ink_set) = ink_set {
        image = image.ink_set(ink_set);
    }
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, values).unwrap();
    writer.finish().unwrap();
    buf.into_inner()
}

/// Extract the raw `ExtraSamples` (338) codes from the first IFD of a written
/// TIFF, or `None` if the tag is absent.
fn extra_samples_codes(bytes: Vec<u8>) -> (TiffFile, Option<Vec<u16>>) {
    let file = TiffFile::from_bytes(bytes).unwrap();
    let codes = {
        let ifd = file.ifd(0).unwrap();
        ifd.tag(TAG_EXTRA_SAMPLES).map(|tag| match &tag.value {
            TagValue::Short(codes) => codes.clone(),
            other => panic!("ExtraSamples must be SHORT, got {other:?}"),
        })
    };
    (file, codes)
}

// --- Group 1: core fix — no synthesized ExtraSamples tag (RED first) ---------

#[test]
fn minisblack_multisample_no_extras_omits_extra_samples_tag() {
    let width: u32 = 2;
    let spp: u16 = 5; // 5 spectral bands, all base image data
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsBlack,
        None,
        Vec::new(),
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes, None,
        "spectral N-band MinIsBlack must NOT synthesize an ExtraSamples tag"
    );
}

#[test]
fn miniswhite_multisample_no_extras_omits_extra_samples_tag() {
    let width: u32 = 2;
    let spp: u16 = 4;
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsWhite,
        None,
        Vec::new(),
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes, None,
        "spectral N-band MinIsWhite must NOT synthesize an ExtraSamples tag"
    );
}

// --- Group 2: byte-exact round-trip through the tolerant reader --------------

#[test]
fn minisblack_multisample_no_extras_roundtrips_byte_exact() {
    let width: u32 = 3;
    let spp: u16 = 5;
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| (i * 3 + 1) as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsBlack,
        None,
        Vec::new(),
        &values,
    );
    let (file, codes) = extra_samples_codes(bytes);
    assert_eq!(codes, None);

    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.samples_per_pixel(), spp);

    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(
        decoded_values, values,
        "pixel data must round-trip byte-exact despite the absent ExtraSamples tag"
    );
}

#[test]
fn miniswhite_multisample_no_extras_roundtrips_byte_exact() {
    let width: u32 = 3;
    let spp: u16 = 4;
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| (i * 7 + 2) as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsWhite,
        None,
        Vec::new(),
        &values,
    );
    let (file, codes) = extra_samples_codes(bytes);
    assert_eq!(codes, None);

    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.samples_per_pixel(), spp);

    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

// --- Group 3: explicit extras still emitted (explicit control preserved) -----

#[test]
fn minisblack_with_explicit_extras_still_emits_exact_codes() {
    let width: u32 = 2;
    // base = 1 (MinIsBlack) + 2 declared extras => spp = 3.
    let spp: u16 = 3;
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsBlack,
        None,
        vec![ExtraSample::Unspecified, ExtraSample::AssociatedAlpha],
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes,
        Some(vec![
            ExtraSample::Unspecified.to_code(),
            ExtraSample::AssociatedAlpha.to_code(),
        ]),
        "explicitly declared extras must be emitted exactly, unchanged"
    );
}

#[test]
fn miniswhite_with_explicit_extras_still_emits_exact_codes() {
    let width: u32 = 2;
    let spp: u16 = 2; // base 1 + 1 declared extra
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::MinIsWhite,
        None,
        vec![ExtraSample::UnassociatedAlpha],
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(codes, Some(vec![ExtraSample::UnassociatedAlpha.to_code()]));
}

#[test]
fn minisblack_over_declared_extras_is_rejected() {
    // spp=2 but 3 declared extras: extra_samples.len() > spp must stay a loud error.
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(2, 1)
        .sample_type::<u8>()
        .samples_per_pixel(2)
        .photometric(PhotometricInterpretation::MinIsBlack)
        .extra_samples(vec![
            ExtraSample::Unspecified,
            ExtraSample::Unspecified,
            ExtraSample::Unspecified,
        ])
        .strips(1);
    let result = writer.add_image(image);
    assert!(
        result.is_err(),
        "declaring more ExtraSamples than samples_per_pixel must error"
    );
}

// --- Group 4: universal no-synthesis for every fixed-base photometric --------
//
// The fix is UNIVERSAL: no photometric synthesizes ExtraSamples from its base.
// A caller that declares no extras gets no tag 338, whatever the photometric.
// The reader still tolerates the absent tag (defaulting excess channels to
// Unspecified in memory), so pixels round-trip byte-exact.

#[test]
fn rgb_spp4_no_declared_extras_omits_extra_samples_tag() {
    let width: u32 = 2;
    let spp: u16 = 4; // RGB base 3 + 1 undeclared channel (formerly a synthesized extra)
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::Rgb,
        None,
        Vec::new(),
        &values,
    );
    let (file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes, None,
        "RGB spp=4 with no declared extras must NOT synthesize an ExtraSamples tag"
    );

    // Round-trip: pixels come back byte-identical despite the absent tag.
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

#[test]
fn rgb_with_explicit_extra_still_emits_it() {
    let width: u32 = 2;
    let spp: u16 = 4; // RGB base 3 + 1 declared alpha
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::Rgb,
        None,
        vec![ExtraSample::UnassociatedAlpha],
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(codes, Some(vec![ExtraSample::UnassociatedAlpha.to_code()]));
}

#[test]
fn rgb_over_declared_extras_still_rejected() {
    // RGB spp=4: implied = 1. Declaring 2 extras must stay a loud error
    // (per-photometric `<= implied` guard, NOT loosened to `<= spp`).
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(2, 1)
        .sample_type::<u8>()
        .samples_per_pixel(4)
        .photometric(PhotometricInterpretation::Rgb)
        .extra_samples(vec![
            ExtraSample::UnassociatedAlpha,
            ExtraSample::Unspecified,
        ])
        .strips(1);
    assert!(
        writer.add_image(image).is_err(),
        "RGB spp=4 with 2 declared extras (implied=1) must still error"
    );
}

#[test]
fn separated_cmyk_spp6_no_declared_extras_omits_extra_samples_tag() {
    let width: u32 = 2;
    let spp: u16 = 6; // CMYK base 4 + 2 undeclared channels (formerly synthesized extras)
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::Separated,
        Some(InkSet::Cmyk),
        Vec::new(),
        &values,
    );
    let (file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes, None,
        "Separated CMYK spp=6 with no declared extras must NOT synthesize extras"
    );

    // Reader still derives base=4 for Cmyk and defaults the 2 excess channels to
    // Unspecified IN MEMORY from the absent tag; pixels round-trip byte-exact.
    let ifd = file.ifd(0).unwrap();
    match ifd.color_model().unwrap() {
        tiff_core::ColorModel::Cmyk { extra_samples } => {
            assert_eq!(
                extra_samples.len(),
                2,
                "reader still models 4 base inks + 2 (in-memory) extras for spp=6"
            );
        }
        other => panic!("expected ColorModel::Cmyk, got {other:?}"),
    }
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

#[test]
fn separated_cmyk_with_explicit_extras_still_emits_them() {
    let width: u32 = 2;
    let spp: u16 = 6; // CMYK base 4 + 2 declared spots
    let values: Vec<u8> = (0..(width as u16 * spp)).map(|i| i as u8).collect();

    let bytes = write_image(
        width,
        spp,
        PhotometricInterpretation::Separated,
        Some(InkSet::Cmyk),
        vec![ExtraSample::Unspecified, ExtraSample::Unspecified],
        &values,
    );
    let (_file, codes) = extra_samples_codes(bytes);
    assert_eq!(
        codes,
        Some(vec![
            ExtraSample::Unspecified.to_code(),
            ExtraSample::Unspecified.to_code(),
        ]),
        "explicitly declared spot extras must still be emitted"
    );
}
