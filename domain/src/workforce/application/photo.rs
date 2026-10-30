//! Fully decoded, metadata-free photographs; the caller has already confirmed the crop.

use std::io::{Cursor, Write};

use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageFormat,
    codecs::{
        jpeg::JpegDecoder,
        png::PngDecoder,
        webp::{WebPDecoder, WebPEncoder},
    },
};

pub const INPUT_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024 + 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
const MAX_PIXELS: u64 = 16_000_000;

pub(super) struct Prepared {
    pub(super) bytes: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) fn format(media_type: &str) -> Result<ImageFormat, Error> {
    match media_type {
        "image/jpeg" => Ok(ImageFormat::Jpeg),
        "image/png" => Ok(ImageFormat::Png),
        "image/webp" => Ok(ImageFormat::WebP),
        _ => Err(Error::Unsupported),
    }
}

pub(super) fn prepare(media_type: &str, bytes: &[u8]) -> Result<Prepared, Error> {
    if bytes.is_empty() || bytes.len() > INPUT_LIMIT {
        return Err(Error::Invalid);
    }
    let format = format(media_type)?;
    if image::guess_format(bytes).map_err(|_| Error::Invalid)? != format {
        return Err(Error::Unsupported);
    }
    let image = match format {
        ImageFormat::Jpeg => {
            decode(JpegDecoder::new(Cursor::new(bytes)).map_err(|_| Error::Invalid)?)?
        }
        ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(Cursor::new(bytes), limits())
                .map_err(|_| Error::Invalid)?;
            if decoder.is_apng().map_err(|_| Error::Invalid)? {
                return Err(Error::Invalid);
            }
            decode(decoder)?
        }
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(|_| Error::Invalid)?;
            if decoder.has_animation() {
                return Err(Error::Invalid);
            }
            decode(decoder)?
        }
        _ => return Err(Error::Unsupported),
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut writer = Bounded(Vec::new());
    WebPEncoder::new_lossless(&mut writer)
        .encode(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
        .map_err(Error::Encode)?;
    Ok(Prepared {
        bytes: writer.0,
        width,
        height,
    })
}

fn decode(mut decoder: impl ImageDecoder) -> Result<DynamicImage, Error> {
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(Error::Invalid);
    }
    decoder.set_limits(limits()).map_err(|_| Error::Invalid)?;
    let orientation = decoder.orientation().map_err(|_| Error::Invalid)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| Error::Invalid)?;
    // Preserve the displayed orientation while removing all original metadata.
    image.apply_orientation(orientation);
    Ok(image)
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

struct Bounded(Vec<u8>);

impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > OUTPUT_LIMIT {
            return Err(std::io::Error::other(
                "encoded photograph exceeds the RGBA budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("photograph must be a static complete image within the input and pixel budgets")]
    Invalid,
    #[error("photograph format is unsupported or differs from the declared media type")]
    Unsupported,
    #[error("photograph encoding failed")]
    Encode(#[source] image::ImageError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat};

    use super::{INPUT_LIMIT, prepare};

    #[test]
    fn preserves_rectangular_pixels_and_removes_original_container() {
        let image = DynamicImage::new_rgb8(120, 160);
        for (format, media) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::WebP, "image/webp"),
        ] {
            let mut input = Cursor::new(Vec::new());
            image.write_to(&mut input, format).expect("fixture image");
            let prepared = prepare(media, input.get_ref()).expect("accepted photograph");
            assert_eq!((prepared.width, prepared.height), (120, 160));
            assert_eq!(
                image::guess_format(&prepared.bytes).expect("format"),
                ImageFormat::WebP
            );
            let decoded = image::load_from_memory(&prepared.bytes).expect("whole output decodes");
            assert_eq!((decoded.width(), decoded.height()), (120, 160));
            assert!(prepare("image/gif", input.get_ref()).is_err());
        }
    }

    #[test]
    fn rejects_over_budget_malformed_mismatched_and_excessive_pixel_inputs() {
        assert!(prepare("image/png", &[]).is_err());
        assert!(prepare("image/png", &[0; 32]).is_err());
        assert!(prepare("image/png", &vec![0; INPUT_LIMIT + 1]).is_err());
        for (width, height) in [(4097, 1), (4001, 4000)] {
            let mut input = Cursor::new(Vec::new());
            DynamicImage::new_luma8(width, height)
                .write_to(&mut input, ImageFormat::Png)
                .expect("fixture");
            assert!(prepare("image/png", input.get_ref()).is_err());
        }
        let mut input = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 3)
            .write_to(&mut input, ImageFormat::Png)
            .expect("fixture");
        assert!(prepare("image/jpeg", input.get_ref()).is_err());
        let truncated = &input.get_ref()[..input.get_ref().len() / 2];
        assert!(prepare("image/png", truncated).is_err());
    }
}
