//! Round-trip coverage for N-channel Separated (arbitrary ink count) TIFF
//! writes.
//!
//! The fork reader already handles arbitrary ink counts for
//! `PhotometricInterpretation::Separated` with a non-`Cmyk` `InkSet`: it
//! computes `color_channels = samples_per_pixel - extra_samples.len()`
//! (`tiff-reader/src/ifd.rs`, `Ifd::color_model`). The writer's job is to
//! mirror that so a write -> read round trip is byte-exact for any ink
//! count >= 1, while leaving the `InkSet::Cmyk` (4 base inks) default
//! behavior unchanged.

use std::io::Cursor;

use tiff_core::{ColorModel, ExtraSample, InkSet, PhotometricInterpretation};
use tiff_reader::TiffFile;
use tiff_writer::{ImageBuilder, TiffWriter, WriteOptions};

/// Write a single-row, chunky, 8-bit Separated image and read it back,
/// returning the opened `TiffFile` plus the raw decoded pixel values so
/// callers can assert on both the IFD metadata and the pixels.
fn roundtrip_separated(
    width: u32,
    samples_per_pixel: u16,
    ink_set: InkSet,
    extra_samples: Vec<ExtraSample>,
    values: &[u8],
) -> (TiffFile, Vec<u8>) {
    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let image = ImageBuilder::new(width, 1)
        .sample_type::<u8>()
        .samples_per_pixel(samples_per_pixel)
        .photometric(PhotometricInterpretation::Separated)
        .ink_set(ink_set)
        .extra_samples(extra_samples)
        .strips(1);
    let handle = writer.add_image(image).unwrap();
    writer.write_block(&handle, 0, values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let decoded = {
        let ifd = file.ifd(0).unwrap();
        assert_eq!(ifd.samples_per_pixel(), samples_per_pixel);
        let decoded = file.read_image::<u8>(0).unwrap();
        let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
        assert_eq!(offset, Some(0));
        decoded_values
    };
    assert_eq!(&decoded, values, "pixel data must round-trip byte-exact");
    (file, decoded)
}

#[test]
fn separated_6channel_notcmyk_roundtrips_exactly() {
    let width: u32 = 2;
    let samples_per_pixel: u16 = 6;
    // 2 pixels x 6 channels, all distinct so any channel mixup is caught.
    let values: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 61, 51, 41, 31, 21, 11];

    let (file, _decoded) = roundtrip_separated(
        width,
        samples_per_pixel,
        InkSet::NotCmyk,
        Vec::new(),
        &values,
    );

    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.ink_set().unwrap(), Some(InkSet::NotCmyk));
    match ifd.color_model().unwrap() {
        ColorModel::Separated {
            ink_set,
            color_channels,
            extra_samples,
        } => {
            assert_eq!(ink_set, InkSet::NotCmyk);
            assert_eq!(color_channels, samples_per_pixel);
            assert!(extra_samples.is_empty());
        }
        other => panic!("expected ColorModel::Separated, got {other:?}"),
    }
}

#[test]
fn separated_16channel_notcmyk_roundtrips_exactly() {
    let width: u32 = 2;
    let samples_per_pixel: u16 = 16;
    let values: Vec<u8> = (0..(width as u16 * samples_per_pixel))
        .map(|i| i as u8)
        .collect();

    let (file, _decoded) = roundtrip_separated(
        width,
        samples_per_pixel,
        InkSet::NotCmyk,
        Vec::new(),
        &values,
    );

    let ifd = file.ifd(0).unwrap();
    assert_eq!(ifd.ink_set().unwrap(), Some(InkSet::NotCmyk));
    match ifd.color_model().unwrap() {
        ColorModel::Separated {
            ink_set,
            color_channels,
            extra_samples,
        } => {
            assert_eq!(ink_set, InkSet::NotCmyk);
            assert_eq!(color_channels, samples_per_pixel);
            assert!(extra_samples.is_empty());
        }
        other => panic!("expected ColorModel::Separated, got {other:?}"),
    }
}

/// spp=7 with 1 declared ExtraSample -> 6 base ink channels
/// (base = samples_per_pixel - extra_samples.len()).
#[test]
fn separated_notcmyk_with_extra_samples_computes_base_from_remainder() {
    let width: u32 = 2;
    let samples_per_pixel: u16 = 7;
    let values: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

    let (file, _decoded) = roundtrip_separated(
        width,
        samples_per_pixel,
        InkSet::NotCmyk,
        vec![ExtraSample::Unspecified],
        &values,
    );

    let ifd = file.ifd(0).unwrap();
    match ifd.color_model().unwrap() {
        ColorModel::Separated {
            ink_set,
            color_channels,
            extra_samples,
        } => {
            assert_eq!(ink_set, InkSet::NotCmyk);
            assert_eq!(color_channels, 6);
            assert_eq!(extra_samples, vec![ExtraSample::Unspecified]);
        }
        other => panic!("expected ColorModel::Separated, got {other:?}"),
    }
}

/// Regression: spp=4 with the default (Cmyk) InkSet must still resolve to
/// the fixed 4-ink `ColorModel::Cmyk` path, unaffected by the new N-ink
/// logic for `NotCmyk`/`Unknown`.
#[test]
fn separated_cmyk_default_spp4_unchanged() {
    let width: u32 = 2;
    let samples_per_pixel: u16 = 4;
    let values: Vec<u8> = vec![0, 64, 128, 255, 255, 128, 64, 0];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let handle = writer
        .add_image(
            ImageBuilder::new(width, 1)
                .sample_type::<u8>()
                .samples_per_pixel(samples_per_pixel)
                .photometric(PhotometricInterpretation::Separated)
                .ink_set(InkSet::Cmyk)
                .strips(1),
        )
        .unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let ifd = file.ifd(0).unwrap();
    assert!(matches!(
        ifd.color_model().unwrap(),
        ColorModel::Cmyk { extra_samples } if extra_samples.is_empty()
    ));
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}

/// Regression: spp=6 with Cmyk InkSet must still take 4 base inks + 2
/// ExtraSamples (never the new N-ink NotCmyk arithmetic).
#[test]
fn separated_cmyk_with_extra_samples_unchanged() {
    let width: u32 = 2;
    let samples_per_pixel: u16 = 6;
    let values: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 60, 50, 40, 30, 20, 10];

    let mut buf = Cursor::new(Vec::new());
    let mut writer = TiffWriter::new(&mut buf, WriteOptions::default()).unwrap();
    let handle = writer
        .add_image(
            ImageBuilder::new(width, 1)
                .sample_type::<u8>()
                .samples_per_pixel(samples_per_pixel)
                .photometric(PhotometricInterpretation::Separated)
                .ink_set(InkSet::Cmyk)
                .strips(1),
        )
        .unwrap();
    writer.write_block(&handle, 0, &values).unwrap();
    writer.finish().unwrap();

    let file = TiffFile::from_bytes(buf.into_inner()).unwrap();
    let ifd = file.ifd(0).unwrap();
    match ifd.color_model().unwrap() {
        ColorModel::Cmyk { extra_samples } => {
            assert_eq!(extra_samples.len(), 2, "4 base inks + 2 extras for spp=6");
        }
        other => panic!("expected ColorModel::Cmyk, got {other:?}"),
    }
    let decoded = file.read_image::<u8>(0).unwrap();
    let (decoded_values, offset) = decoded.into_raw_vec_and_offset();
    assert_eq!(offset, Some(0));
    assert_eq!(decoded_values, values);
}
