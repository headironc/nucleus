//! Rasterize only retained application content, with independent canvas and encoder bounds.

use std::{io::Write, sync::Arc};

use image::{ExtendedColorType, ImageFormat, RgbaImage, codecs::webp::WebPEncoder};
use serde::Deserialize;

use super::{Error, Line, Snapshot, TextArea};

pub const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const MAX_PIXELS: u64 = 4_194_304;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "u32")]
pub struct Resolution(u32);

impl TryFrom<u32> for Resolution {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self, Error> {
        if value == 0 {
            return Err(Error::Configuration);
        }
        Ok(Self(value))
    }
}

impl Resolution {
    pub(super) fn dimensions(self, width: u32, height: u32) -> Result<(u32, u32), Error> {
        let pixels = |value| {
            u32::try_from((u64::from(value) * u64::from(self.0)).div_ceil(25_400))
                .map_err(|_| Error::Configuration)
        };
        let (width, height) = (pixels(width)?, pixels(height)?);
        if width == 0
            || height == 0
            || width > 4096
            || height > 4096
            || u64::from(width) * u64::from(height) > MAX_PIXELS
        {
            return Err(Error::Configuration);
        }
        Ok((width, height))
    }
}

pub(super) fn encode(
    snapshot: &Snapshot,
    resolution: Resolution,
    background: &[u8],
    photo: &[u8],
    font: Vec<u8>,
) -> Result<Vec<u8>, Error> {
    let layout = &snapshot.template.layout;
    let (width, height) = resolution.dimensions(layout.width, layout.height)?;
    let scale = f64::from(resolution.0) / 25_400.0;
    let decode = |bytes: &[u8]| {
        image::load_from_memory_with_format(bytes, ImageFormat::WebP)
            .map(|image| image.to_rgba8())
            .map_err(Error::RenderImage)
    };
    let mut canvas = image::imageops::resize(
        &decode(background)?,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    let mut photo = decode(photo)?;
    let area = &layout.photo;
    let factor = (f64::from(area.width) * scale / f64::from(photo.width()))
        .min(f64::from(area.height) * scale / f64::from(photo.height()));
    let x = (f64::from(area.x) + f64::from(area.width) / 2.0) * scale
        - f64::from(photo.width()) * factor / 2.0;
    let y = (f64::from(area.y) + f64::from(area.height) / 2.0) * scale
        - f64::from(photo.height()) * factor / 2.0;
    let mut text = Text::new(font)?;
    // Tiny-skia consumes premultiplied RGBA; WebP consumes straight RGBA.
    for pixel in canvas.pixels_mut().chain(photo.pixels_mut()) {
        for channel in 0..3 {
            pixel[channel] = ((u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255) as u8;
        }
    }
    let source = tiny_skia::PixmapRef::from_bytes(photo.as_raw(), photo.width(), photo.height())
        .ok_or(Error::Configuration)?;
    let mut target = tiny_skia::PixmapMut::from_bytes(canvas.as_mut(), width, height)
        .ok_or(Error::Configuration)?;
    target.draw_pixmap(
        0,
        0,
        source,
        &tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bicubic,
            ..Default::default()
        },
        tiny_skia::Transform::from_row(factor as f32, 0.0, 0.0, factor as f32, x as f32, y as f32),
        None,
    );
    for (value, area, lines) in [
        (snapshot.name.as_str(), &layout.name, &snapshot.text.name),
        (
            snapshot.printed_number.as_str(),
            &layout.number,
            &snapshot.text.number,
        ),
        (
            snapshot.department.printed.as_str(),
            &layout.department,
            &snapshot.text.department,
        ),
        (
            snapshot.printed_title.as_str(),
            &layout.title,
            &snapshot.text.title,
        ),
    ] {
        text.draw(&mut canvas, value, area, lines, scale)?;
    }
    for pixel in canvas.pixels_mut() {
        if pixel[3] != 0 {
            for channel in 0..3 {
                pixel[channel] = ((u32::from(pixel[channel]) * 255 + u32::from(pixel[3]) / 2)
                    / u32::from(pixel[3]))
                .min(255) as u8;
            }
        }
    }
    let mut output = Bounded(Vec::new());
    WebPEncoder::new_lossless(&mut output)
        .encode(canvas.as_raw(), width, height, ExtendedColorType::Rgba8)
        .map_err(Error::RenderImage)?;
    Ok(output.0)
}

struct Text {
    bytes: Arc<Vec<u8>>,
    fonts: parley::FontContext,
    layouts: parley::LayoutContext<()>,
    family: String,
}

impl Text {
    fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        let bytes = Arc::new(bytes);
        let (fonts, family) = super::settings::fonts(Arc::clone(&bytes))?;
        Ok(Self {
            bytes,
            fonts,
            layouts: parley::LayoutContext::new(),
            family,
        })
    }

    fn draw(
        &mut self,
        canvas: &mut RgbaImage,
        value: &str,
        area: &TextArea,
        lines: &[Line],
        scale: f64,
    ) -> Result<(), Error> {
        let face = ttf_parser::Face::parse(&self.bytes, 0).map_err(|_| Error::Configuration)?;
        let layout = super::settings::shape(
            &mut self.fonts,
            &mut self.layouts,
            &self.family,
            value,
            area,
        )?;
        if layout.lines().count() != lines.len() {
            return Err(Error::TextOverflow);
        }
        let mut end = 0;
        for (line, shaped) in lines.iter().zip(layout.lines()) {
            let range = shaped.text_range();
            let direction = if layout.is_rtl() {
                super::Direction::Rtl
            } else {
                super::Direction::Ltr
            };
            if line.start != end
                || range.start != line.start
                || range.end != line.end
                || line.direction != direction
            {
                return Err(Error::Configuration);
            }
            let value = value
                .get(line.start..line.end)
                .ok_or(Error::Configuration)?;
            end = line.end;
            for item in shaped.items() {
                let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                    return Err(Error::Configuration);
                };
                for glyph in run.positioned_glyphs() {
                    if glyph.id == 0 {
                        return Err(Error::MissingGlyph);
                    }
                    let id = ttf_parser::GlyphId(
                        u16::try_from(glyph.id).map_err(|_| Error::MissingGlyph)?,
                    );
                    let mut path = Outline(tiny_skia::PathBuilder::new());
                    if face.outline_glyph(id, &mut path).is_none() {
                        let blank = value.chars().any(|character| {
                                (character.is_whitespace() || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{fe00}'..='\u{fe0f}'))
                                    && face.glyph_index(character) == Some(id)
                            });
                        if blank {
                            continue;
                        }
                        return Err(Error::MissingGlyph);
                    }
                    let path = path.0.finish().ok_or(Error::Configuration)?;
                    let x = (line.x_um + f64::from(glyph.x) * 1000.0) * scale;
                    let y = (line.baseline_um
                        + f64::from(glyph.y - shaped.metrics().baseline) * 1000.0)
                        * scale;
                    let size = f64::from(area.font_size) * scale / f64::from(face.units_per_em());
                    let transform = tiny_skia::Transform::from_row(
                        size as f32,
                        0.0,
                        0.0,
                        -size as f32,
                        x as f32,
                        y as f32,
                    );
                    let bounds = path
                        .bounds()
                        .transform(transform)
                        .ok_or(Error::Configuration)?;
                    let left = f64::from(area.area.x) * scale;
                    let top = f64::from(area.area.y) * scale;
                    if f64::from(bounds.left()) < left - 0.01
                        || f64::from(bounds.top()) < top - 0.01
                        || f64::from(bounds.right())
                            > f64::from(area.area.x + area.area.width) * scale + 0.01
                        || f64::from(bounds.bottom())
                            > f64::from(area.area.y + area.area.height) * scale + 0.01
                    {
                        return Err(Error::TextOverflow);
                    }
                    let (w, h) = canvas.dimensions();
                    let mut pixels = tiny_skia::PixmapMut::from_bytes(canvas.as_mut(), w, h)
                        .ok_or(Error::Configuration)?;
                    pixels.fill_path(
                        &path,
                        &tiny_skia::Paint::default(),
                        tiny_skia::FillRule::Winding,
                        transform,
                        None,
                    );
                }
            }
        }
        if end != value.len() {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}

struct Outline(tiny_skia::PathBuilder);
impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x: f32, y: f32, x1: f32, y1: f32) {
        self.0.quad_to(x, y, x1, y1);
    }
    fn curve_to(&mut self, x: f32, y: f32, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.0.cubic_to(x, y, x1, y1, x2, y2);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > OUTPUT_LIMIT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "badge encoding exceeds its output budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn resolution_uses_physical_units_and_checks_before_allocation() {
        let resolution = Resolution::try_from(300).expect("explicit PPI");
        assert_eq!(
            resolution.dimensions(85_000, 54_000).expect("fixture"),
            (1004, 638)
        );
        assert!(Resolution::try_from(0).is_err());
        assert!(resolution.dimensions(u32::MAX, 54_000).is_err());
        assert!(
            Resolution::try_from(25_400)
                .expect("PPI")
                .dimensions(2049, 2048)
                .is_err()
        );
    }

    #[test]
    fn encoder_writer_refuses_the_first_byte_over_its_independent_limit() {
        let mut output = Bounded(vec![0; OUTPUT_LIMIT - 1]);
        assert!(output.write_all(&[1, 2]).is_err());
        assert_eq!(output.0.len(), OUTPUT_LIMIT - 1);
        output.write_all(&[1]).expect("exact bound");
        assert!(output.write_all(&[2]).is_err());
    }

    #[test]
    fn retained_text_and_transparency_produce_complete_lossless_webp() {
        let template = super::super::fixture_template();
        let lines = |y| json!([{"start":0,"end":3,"xUm":20000.0,"baselineUm":y,"widthUm":5000.0,"direction":"ltr"}]);
        let snapshot: Snapshot = serde_json::from_value(json!({
            "name":"Ada", "number":{"subtype":100,"year":2026,"serial":1}, "printedNumber":"Ada",
            "department":{"id":"00000000-0000-4000-8000-000000000001","name":"Ada","printed":"Ada"},
            "position":null,"title":null,"printedTitle":"Ada", "photoId":"00000000-0000-4000-8000-000000000002",
            "template":template,"pickup":"Fixture", "text":{"name":lines(5000.0),"number":lines(16000.0),"department":lines(27000.0),"title":lines(38000.0)}
        })).expect("retained snapshot");
        let webp = |width, height, pixel| {
            let rgba = RgbaImage::from_pixel(width, height, image::Rgba(pixel));
            let mut bytes = Vec::new();
            WebPEncoder::new_lossless(&mut bytes)
                .encode(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
                .expect("fixture WebP");
            bytes
        };
        let output = encode(
            &snapshot,
            Resolution::try_from(300).expect("PPI"),
            &webp(85, 54, [0, 0, 0, 0]),
            &webp(4, 8, [255, 0, 0, 128]),
            include_bytes!("../../../tests/fixtures/fonts/SourceSans3-Regular.ttf").to_vec(),
        )
        .expect("render");
        assert_eq!(&output[12..16], b"VP8L");
        let image = image::load_from_memory_with_format(&output, ImageFormat::WebP)
            .expect("complete WebP")
            .to_rgba8();
        assert_eq!(image.dimensions(), (1004, 638));
        assert_eq!(image.get_pixel(1003, 637).0, [0, 0, 0, 0]);
        assert!(image.pixels().any(|pixel| pixel.0 == [255, 0, 0, 128]));
        assert!(image.pixels().any(|pixel| pixel[3] == 255 && pixel[0] == 0));
        let mut missing = snapshot;
        missing.name = "\u{10ffff}".to_owned();
        missing.text.name[0].end = missing.name.len();
        assert!(matches!(
            encode(
                &missing,
                Resolution::try_from(300).expect("PPI"),
                &webp(85, 54, [0, 0, 0, 0]),
                &webp(4, 8, [255, 0, 0, 128]),
                include_bytes!("../../../tests/fixtures/fonts/SourceSans3-Regular.ttf").to_vec()
            ),
            Err(Error::MissingGlyph)
        ));
    }
}
