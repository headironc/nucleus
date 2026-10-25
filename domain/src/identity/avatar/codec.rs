//! Static PNG, JPEG and WebP validation followed by one lossless WebP encoding.

use std::io::{Cursor, Write};

use image::{
    DynamicImage, ExtendedColorType, ImageDecoder as _, ImageError, ImageFormat,
    codecs::{
        jpeg::JpegDecoder,
        png::PngDecoder,
        webp::{WebPDecoder, WebPEncoder},
    },
    guess_format,
};

pub(crate) const INPUT_LIMIT: usize = 2 * 1024 * 1024;
pub(crate) const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const MAX_DECODER_ALLOCATION: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_DIMENSION: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Png,
    Jpeg,
    Webp,
}

impl Media {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

pub(crate) struct Output {
    pub(crate) bytes: Vec<u8>,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// The declared media type must agree with the detected static format. The whole image is
/// decoded to RGBA8 after dimension and animation checks, then encoded without input metadata.
pub(crate) fn convert(declared: Media, input: &[u8]) -> Result<Output, Error> {
    if input.is_empty() || input.len() > INPUT_LIMIT {
        return Err(if input.len() > INPUT_LIMIT {
            Error::TooLarge
        } else {
            Error::Invalid
        });
    }
    let detected = guess_format(input).map_err(|_| Error::Invalid)?;
    if detected
        != match declared {
            Media::Png => ImageFormat::Png,
            Media::Jpeg => ImageFormat::Jpeg,
            Media::Webp => ImageFormat::WebP,
        }
    {
        return Err(Error::Unsupported);
    }
    let image = match declared {
        Media::Png => {
            let decoder = PngDecoder::with_limits(Cursor::new(input), limits()).map_err(decode)?;
            if decoder.is_apng().map_err(decode)? {
                return Err(Error::Invalid);
            }
            let (width, height) = decoder.dimensions();
            check_dimensions(width, height)?;
            DynamicImage::from_decoder(decoder).map_err(decode)?
        }
        Media::Jpeg => {
            let mut decoder = JpegDecoder::new(Cursor::new(input)).map_err(decode)?;
            let (width, height) = decoder.dimensions();
            check_dimensions(width, height)?;
            decoder.set_limits(limits()).map_err(decode)?;
            DynamicImage::from_decoder(decoder).map_err(decode)?
        }
        Media::Webp => {
            let mut decoder = WebPDecoder::new(Cursor::new(input)).map_err(decode)?;
            if decoder.has_animation() {
                return Err(Error::Invalid);
            }
            let (width, height) = decoder.dimensions();
            check_dimensions(width, height)?;
            decoder.set_limits(limits()).map_err(decode)?;
            DynamicImage::from_decoder(decoder).map_err(decode)?
        }
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    check_dimensions(width, height)?;
    let mut writer = Bounded::new(OUTPUT_LIMIT);
    WebPEncoder::new_lossless(&mut writer)
        .encode(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
        .map_err(Error::Encode)?;
    let bytes = writer.into_bytes();
    if bytes.is_empty() || bytes.len() > OUTPUT_LIMIT {
        return Err(Error::Encode(ImageError::IoError(std::io::Error::other(
            "encoded avatar is empty or exceeds its limit",
        ))));
    }
    Ok(Output {
        bytes,
        width: u16::try_from(width).map_err(|_| Error::Invalid)?,
        height: u16::try_from(height).map_err(|_| Error::Invalid)?,
    })
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    limits
}

fn check_dimensions(width: u32, height: u32) -> Result<(), Error> {
    if width != height || !(1..=MAX_DIMENSION).contains(&width) {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn decode(_: ImageError) -> Error {
    Error::Invalid
}

struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}

impl Bounded {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other("encoded avatar exceeds its limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("avatar input exceeds 2 MiB")]
    TooLarge,
    #[error("avatar format is unsupported or differs from its declaration")]
    Unsupported,
    #[error("avatar image is malformed, animated or not a square within 1 to 1024 pixels")]
    Invalid,
    #[error("avatar could not be encoded as lossless WebP within 8 MiB")]
    Encode(#[source] ImageError),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use image::ImageEncoder as _;

    use super::*;

    fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .expect("PNG fixture");
        bytes
    }

    #[test]
    fn normalizes_static_formats_and_preserves_rgba_pixels() {
        let rgba = [
            17, 33, 65, 0, 9, 87, 201, 127, 0, 255, 0, 255, 64, 128, 192, 255,
        ];
        let input = png(2, 2, &rgba);
        let output = convert(Media::Png, &input).expect("static PNG");
        assert_eq!((output.width, output.height), (2, 2));
        assert_eq!(
            image::load_from_memory_with_format(&output.bytes, ImageFormat::WebP)
                .expect("output WebP")
                .to_rgba8()
                .as_raw(),
            &rgba
        );
        let reencoded = convert(Media::Webp, &output.bytes).expect("static WebP");
        assert_eq!(
            reencoded.bytes, output.bytes,
            "canonical output is deterministic"
        );
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode(&[32, 128, 240], 1, 1, ExtendedColorType::Rgb8)
            .expect("JPEG fixture");
        let output = convert(Media::Jpeg, &jpeg).expect("static JPEG");
        assert_eq!((output.width, output.height), (1, 1));
        assert_eq!(
            image::load_from_memory_with_format(&output.bytes, ImageFormat::WebP)
                .expect("JPEG output")
                .to_rgba8()
                .get_pixel(0, 0)
                .0[3],
            255
        );
    }

    #[test]
    fn rejects_malformed_mismatched_and_non_square_input() {
        let valid = png(1, 1, &[1, 2, 3, 255]);
        assert!(matches!(
            convert(Media::Jpeg, &valid),
            Err(Error::Unsupported)
        ));
        assert!(matches!(convert(Media::Png, &[]), Err(Error::Invalid)));
        assert!(matches!(
            convert(Media::Png, &valid[..valid.len() / 2]),
            Err(Error::Invalid)
        ));
        assert!(matches!(
            convert(Media::Png, &png(1, 2, &[0; 8])),
            Err(Error::Invalid)
        ));
        assert!(matches!(
            convert(Media::Png, &vec![0; INPUT_LIMIT + 1]),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn enforces_dimensions_before_pixel_expansion() {
        let upper = png(1024, 1024, &vec![0; 1024 * 1024 * 4]);
        assert!(upper.len() < INPUT_LIMIT);
        let output = convert(Media::Png, &upper).expect("inclusive dimension limit");
        assert_eq!((output.width, output.height), (1024, 1024));
        let too_wide = png(1025, 1025, &vec![0; 1025 * 1025 * 4]);
        assert!(too_wide.len() < INPUT_LIMIT);
        assert!(matches!(
            convert(Media::Png, &too_wide),
            Err(Error::Invalid)
        ));
    }

    #[test]
    fn encoded_writer_never_appends_bytes_past_its_limit() {
        let mut writer = Bounded::new(4);
        writer.write_all(b"1234").expect("exact boundary");
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.into_bytes(), b"1234");
    }

    #[test]
    fn png_header_metadata_obeys_allocation_limits_before_pixel_decoding() {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(vec![0; MAX_DECODER_ALLOCATION as usize + 1])
            .expect("compressible ICC fixture");
        encoder
            .write_image(&[1, 2, 3, 255], 1, 1, ExtendedColorType::Rgba8)
            .expect("small PNG with oversized expanded metadata");
        assert!(bytes.len() < INPUT_LIMIT);
        // png discards ancillary ICC data that exceeds its bounded inflater, while retaining
        // a valid image. The bound must already apply when the header is read.
        let mut decoder =
            PngDecoder::with_limits(Cursor::new(&bytes), limits()).expect("bounded header");
        assert_eq!(decoder.icc_profile().expect("bounded metadata"), None);
        let output = convert(Media::Png, &bytes).expect("valid pixels survive discarded metadata");
        assert_eq!(
            image::load_from_memory_with_format(&output.bytes, ImageFormat::WebP)
                .expect("static output")
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [1, 2, 3, 255]
        );
    }

    #[test]
    fn rejects_real_animated_png_before_selecting_any_frame() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_animated(2, 0).expect("APNG");
            let mut writer = encoder.write_header().expect("APNG header");
            writer
                .write_image_data(&[255, 0, 0, 255])
                .expect("first frame");
            writer
                .write_image_data(&[0, 255, 0, 255])
                .expect("second frame");
            writer.finish().expect("complete APNG");
        }
        assert!(
            PngDecoder::new(Cursor::new(&bytes))
                .expect("valid APNG")
                .is_apng()
                .expect("animation metadata")
        );
        assert!(matches!(convert(Media::Png, &bytes), Err(Error::Invalid)));
    }

    #[test]
    fn strips_metadata_without_applying_orientation_and_converts_rgba16() {
        // Little-endian TIFF, one SHORT Orientation entry with value 6 (90 degrees CW).
        let exif = vec![
            73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let pixels: [u16; 16] = [
            65535, 0, 0, 65535, 0, 65535, 0, 65535, 0, 0, 65535, 65535, 0, 0, 0, 0,
        ];
        let native: Vec<_> = pixels.into_iter().flat_map(u16::to_ne_bytes).collect();
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_exif_metadata(exif.clone()).expect("PNG EXIF");
        encoder
            .set_icc_profile(b"fictional-profile".to_vec())
            .expect("PNG ICC");
        encoder
            .write_image(&native, 2, 2, ExtendedColorType::Rgba16)
            .expect("RGBA16 PNG");
        let mut input = PngDecoder::new(Cursor::new(&bytes)).expect("input decoder");
        assert_eq!(input.exif_metadata().expect("EXIF"), Some(exif));
        assert_eq!(
            input.orientation().expect("orientation"),
            image::metadata::Orientation::Rotate90
        );
        let output = convert(Media::Png, &bytes).expect("metadata input");
        let mut decoder = WebPDecoder::new(Cursor::new(&output.bytes)).expect("output decoder");
        assert_eq!(decoder.exif_metadata().expect("output EXIF"), None);
        assert_eq!(decoder.icc_profile().expect("output ICC"), None);
        assert_eq!(
            DynamicImage::from_decoder(decoder)
                .expect("pixels")
                .to_rgba8()
                .as_raw(),
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_real_animated_webp_before_selecting_any_frame() {
        use image::AnimationDecoder as _;

        let mut chunks = Vec::new();
        let mut chunk = |name: &[u8; 4], bytes: &[u8]| {
            chunks.extend_from_slice(name);
            chunks.extend_from_slice(
                &u32::try_from(bytes.len())
                    .expect("fixture chunk length")
                    .to_le_bytes(),
            );
            chunks.extend_from_slice(bytes);
            if !bytes.len().is_multiple_of(2) {
                chunks.push(0);
            }
        };
        // A 2x2 animated RGBA canvas, looping forever. Each ANMF replaces the full canvas.
        chunk(b"VP8X", &[0x12, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
        chunk(b"ANIM", &[0; 6]);
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            let mut encoded = Vec::new();
            WebPEncoder::new_lossless(&mut encoded)
                .encode(&color.repeat(4), 2, 2, ExtendedColorType::Rgba8)
                .expect("static frame");
            assert_eq!(&encoded[12..16], b"VP8L");
            let mut frame = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 10, 0, 0, 2];
            frame.extend_from_slice(&encoded[12..]);
            chunk(b"ANMF", &frame);
        }
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(
            &u32::try_from(chunks.len() + 4)
                .expect("fixture RIFF length")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(b"WEBP");
        bytes.extend(chunks);
        let decoder = WebPDecoder::new(Cursor::new(&bytes)).expect("valid animated container");
        assert!(decoder.has_animation());
        let frames = decoder
            .into_frames()
            .collect_frames()
            .expect("two real decodable frames");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].buffer().get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(frames[1].buffer().get_pixel(0, 0).0, [0, 255, 0, 255]);
        assert!(matches!(convert(Media::Webp, &bytes), Err(Error::Invalid)));
    }
}
