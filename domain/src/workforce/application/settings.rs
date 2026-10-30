//! One configured layout and its immutable private resources, without a template entity.

use std::sync::{Arc, Mutex};

use crate::workforce::{
    application::{Direction, Error, Line, ResourceId, Template, TextArea, photo},
    font,
};

#[derive(Clone)]
pub struct Settings {
    pub(super) directory: Arc<platform::files::Directory>,
    pub(super) template: Template,
    pub(super) font: Arc<Vec<u8>>,
    pub(super) pickups: Vec<String>,
    pub(super) photo_limits: photo::Limits,
    pub(super) resources: [(ResourceId, &'static str, &'static str, i64); 2],
    text: Arc<Mutex<Text>>,
}

struct Text {
    fonts: parley::FontContext,
    layout: parley::LayoutContext<()>,
    family: String,
}

impl Settings {
    pub fn validate(
        template: &Template,
        pickups: &[String],
        background_media: &str,
    ) -> Result<(), Error> {
        photo::format(background_media).map_err(|_| Error::Configuration)?;
        if pickups.is_empty()
            || pickups
                .iter()
                .any(|value| value.trim().is_empty() || value.chars().any(char::is_control))
            || template.background_id == template.font_id
        {
            return Err(Error::Configuration);
        }
        for pickup in pickups {
            // One encoded choice must leave room for the other fields in an 8 KiB preview.
            if serde_json::to_vec(pickup)?.len() > 4096 {
                return Err(Error::Configuration);
            }
        }
        validate_layout(template)
    }
    /// Entry points own source paths and file reading. All processing and durable file writes
    /// finish before startup accepts the database references.
    pub fn new(
        storage: platform::files::Settings,
        template: Template,
        background_media: &str,
        background: Vec<u8>,
        font: Vec<u8>,
        pickups: Vec<String>,
        photo_limits: photo::Limits,
    ) -> Result<Self, Error> {
        Self::validate(&template, &pickups, background_media)?;
        let background =
            photo::prepare(background_media, &background).map_err(|_| Error::Configuration)?;
        let font = font::Upload::new(font)?.sanitize()?;
        ttf_parser::Face::parse(font.as_bytes(), 0).map_err(|_| Error::Configuration)?;
        let directory = Arc::new(storage.open()?);
        publish(&directory, template.background_id, &background.bytes)?;
        publish(&directory, template.font_id, font.as_bytes())?;
        let resources = [
            (
                template.background_id,
                "background",
                "image/webp",
                i64::try_from(background.bytes.len()).map_err(|_| Error::Configuration)?,
            ),
            (
                template.font_id,
                "font",
                font.media_type().as_str(),
                i64::try_from(font.as_bytes().len()).map_err(|_| Error::Configuration)?,
            ),
        ];
        let mut fonts = parley::FontContext {
            collection: parley::fontique::Collection::new(parley::fontique::CollectionOptions {
                system_fonts: false,
                ..Default::default()
            }),
            source_cache: Default::default(),
        };
        let families = fonts.collection.register_fonts(
            parley::fontique::Blob::new(Arc::new(font.as_bytes().to_vec())),
            None,
        );
        let family_id = families.first().ok_or(Error::Configuration)?.0;
        let family = fonts
            .collection
            .family_name(family_id)
            .ok_or(Error::Configuration)?
            .to_owned();
        let text = Arc::new(Mutex::new(Text {
            fonts,
            layout: parley::LayoutContext::new(),
            family,
        }));
        Ok(Self {
            directory,
            template,
            font: Arc::new(font.as_bytes().to_vec()),
            pickups,
            photo_limits,
            resources,
            text,
        })
    }

    pub(super) fn text(&self, text: &str, area: &TextArea) -> Result<Vec<Line>, Error> {
        let font = ttf_parser::Face::parse(&self.font, 0).map_err(|_| Error::Configuration)?;
        let units = f64::from(font.units_per_em());
        let scale = f64::from(area.font_size) / units;
        let mut engine = self.text.lock().map_err(|_| Error::Worker)?;
        let Text {
            fonts,
            layout,
            family,
        } = &mut *engine;
        let mut builder = layout.ranged_builder(fonts, text, 1.0, false);
        builder.push_default(parley::StyleProperty::FontFamily(
            parley::FontFamily::Source(family.as_str().into()),
        ));
        // Values are scaled to millimetres before Parley's f32 boundary; no integer casts.
        let size = f64::from(area.font_size) / 1000.0;
        let size: f32 = size.to_string().parse().map_err(|_| Error::Configuration)?;
        let line_height: f32 = (f64::from(area.line_height) / 1000.0)
            .to_string()
            .parse()
            .map_err(|_| Error::Configuration)?;
        builder.push_default(parley::StyleProperty::FontSize(size));
        builder.push_default(parley::StyleProperty::LineHeight(
            parley::LineHeight::Absolute(line_height),
        ));
        let mut layout: parley::Layout<()> = builder.build(text);
        let width: f32 = (f64::from(area.area.width) / 1000.0)
            .to_string()
            .parse()
            .map_err(|_| Error::Configuration)?;
        layout.break_all_lines(if area.wrap { Some(width) } else { None });
        layout.align(parley::Alignment::Left, parley::AlignmentOptions::default());
        let mut result = Vec::new();
        for line in layout.lines() {
            let mut left = 0_f64;
            let mut right = f64::from(line.metrics().advance) * 1000.0;
            for item in line.items() {
                let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                    return Err(Error::Configuration);
                };
                for glyph in run.positioned_glyphs() {
                    if glyph.id == 0 {
                        return Err(Error::MissingGlyph);
                    }
                    let glyph_id = ttf_parser::GlyphId(
                        u16::try_from(glyph.id).map_err(|_| Error::MissingGlyph)?,
                    );
                    if let Some(bounds) = font.glyph_bounding_box(glyph_id) {
                        left =
                            left.min(f64::from(glyph.x) * 1000.0 + f64::from(bounds.x_min) * scale);
                        right = right
                            .max(f64::from(glyph.x) * 1000.0 + f64::from(bounds.x_max) * scale);
                        let top = f64::from(glyph.y) * 1000.0 - f64::from(bounds.y_max) * scale;
                        let bottom = f64::from(glyph.y) * 1000.0 - f64::from(bounds.y_min) * scale;
                        if top < 0.0 || bottom > f64::from(area.area.height) {
                            return Err(Error::TextOverflow);
                        }
                    }
                }
            }
            if right - left > f64::from(area.area.width) {
                return Err(Error::TextOverflow);
            }
            let range = line.text_range();
            result.push(Line {
                start: range.start,
                end: range.end,
                x_um: f64::from(area.area.x) - left,
                baseline_um: f64::from(area.area.y) + f64::from(line.metrics().baseline) * 1000.0,
                width_um: right - left,
                direction: if layout.is_rtl() {
                    Direction::Rtl
                } else {
                    Direction::Ltr
                },
            });
        }
        if result.is_empty() || f64::from(layout.height()) * 1000.0 > f64::from(area.area.height) {
            return Err(Error::TextOverflow);
        }
        Ok(result)
    }
}

pub(super) fn validate_layout(template: &Template) -> Result<(), Error> {
    let layout = &template.layout;
    if layout.width == 0 || layout.height == 0 {
        return Err(Error::Configuration);
    }
    for area in [
        &layout.photo,
        &layout.name.area,
        &layout.number.area,
        &layout.department.area,
        &layout.title.area,
    ] {
        if area.width == 0
            || area.height == 0
            || area
                .x
                .checked_add(area.width)
                .is_none_or(|x| x > layout.width)
            || area
                .y
                .checked_add(area.height)
                .is_none_or(|y| y > layout.height)
        {
            return Err(Error::Configuration);
        }
    }
    if [
        &layout.name,
        &layout.number,
        &layout.department,
        &layout.title,
    ]
    .iter()
    .any(|area| area.font_size == 0 || area.line_height == 0)
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

pub(super) fn publish(
    directory: &platform::files::Directory,
    id: ResourceId,
    bytes: &[u8],
) -> Result<(), Error> {
    directory.publish(
        &["workforce"],
        &id.to_string(),
        &uuid::Uuid::new_v4().to_string(),
        bytes,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{
        io::Cursor,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };

    use crate::workforce::application::{Direction, Error, Settings, TextArea, fixture_template};

    fn settings(font: &[u8]) -> (tempfile::TempDir, Settings) {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("private fixture files");
        let device = directory.path().metadata().expect("device").dev();
        let mut mount = directory.path().to_path_buf();
        while let Some(parent) = mount.parent() {
            if parent.metadata().expect("ancestor").dev() != device {
                break;
            }
            mount = parent.to_path_buf();
        }
        let mut background = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(85, 54)
            .write_to(&mut background, image::ImageFormat::Png)
            .expect("background");
        let settings = Settings::new(
            platform::files::Settings::new(directory.path().to_path_buf(), mount, device)
                .expect("storage"),
            fixture_template(),
            "image/png",
            background.into_inner(),
            font.to_vec(),
            vec!["Fixture pickup".to_owned()],
            super::photo::Limits::new(100, 1 << 30, 1000, 10 << 30).expect("fixture capacity"),
        )
        .expect("valid isolated template");
        (directory, settings)
    }

    fn preserves(text: &str, lines: &[crate::workforce::application::Line]) {
        let mut offset = 0;
        for line in lines {
            assert_eq!(
                line.start, offset,
                "every source byte belongs to a complete line"
            );
            assert!(text.is_char_boundary(line.start) && text.is_char_boundary(line.end));
            offset = line.end;
        }
        assert_eq!(offset, text.len());
    }

    #[test]
    fn pickup_budget_counts_json_quotes_and_escaping() {
        let template = fixture_template();
        for (value, accepted) in [
            ("a".repeat(4094), true),
            ("a".repeat(4095), false),
            ("\"".repeat(2047), true),
            ("\"".repeat(2048), false),
            ("\\".repeat(2047), true),
            ("\\".repeat(2048), false),
        ] {
            assert_eq!(
                Settings::validate(&template, &[value], "image/png").is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn long_chinese_wraps_and_ink_and_height_overflow_are_explicit() {
        let (_directory, settings) = settings(include_bytes!(
            "../../../tests/fixtures/fonts/NotoSansSC-Regular.otf"
        ));
        let text = "临床医学检验科室护理综合服务中心".repeat(3);
        let area = &settings.template.layout.department;
        let lines = settings
            .text(&text, area)
            .expect("legal Chinese glyphs wrap");
        assert!(lines.len() >= 2);
        preserves(&text, &lines);
        assert!(
            lines
                .iter()
                .all(|line| line.direction == Direction::Ltr && line.width_um <= 60_000.0)
        );
        let mut short = area.clone();
        short.area.height = 1;
        assert!(matches!(
            settings.text(&text, &short),
            Err(Error::TextOverflow)
        ));
        let mut single = area.clone();
        single.wrap = false;
        assert!(matches!(
            settings.text(&text, &single),
            Err(Error::TextOverflow)
        ));
        assert!(matches!(
            settings.text("\u{10ffff}", area),
            Err(Error::MissingGlyph)
        ));
    }

    #[test]
    fn combining_clusters_and_arabic_mixed_direction_use_real_shaping() {
        let (_directory, settings) = settings(include_bytes!(
            "../../../tests/fixtures/fonts/NotoSansArabic.ttf"
        ));
        let area = &settings.template.layout.name;
        for text in ["سلام 123 ABC", "ABC 123 سلام", "x\u{301}x\u{301}x\u{301}"] {
            let lines = settings.text(text, area).expect("complete mixed text");
            preserves(text, &lines);
            assert_eq!(lines.len(), 1);
            assert_eq!(
                lines[0].direction,
                if text.starts_with('س') {
                    Direction::Rtl
                } else {
                    Direction::Ltr
                }
            );
        }
        let connected = settings.text("سلام", area).expect("Arabic connected word");
        assert!(
            connected[0].width_um < f64::from(area.area.width) / 2.0,
            "RTL measurements describe the actual line, not its right-alignment offset"
        );
        let face = ttf_parser::Face::parse(&settings.font, 0).expect("same legal font");
        let unshaped: u32 = "سلام"
            .chars()
            .map(|character| {
                u32::from(
                    face.glyph_hor_advance(face.glyph_index(character).expect("nominal glyph"))
                        .expect("advance"),
                )
            })
            .sum();
        let unshaped =
            f64::from(unshaped) * f64::from(area.font_size) / f64::from(face.units_per_em());
        assert!(
            (connected[0].width_um - unshaped).abs() > 0.1,
            "joining changes nominal character advances: shaped={}, nominal={unshaped}",
            connected[0].width_um
        );
    }

    #[test]
    fn measured_units_match_font_advance_and_long_words_are_not_compressed() {
        let (_directory, settings) = settings(include_bytes!(
            "../../../tests/fixtures/fonts/SourceSans3-Regular.ttf"
        ));
        let area: &TextArea = &settings.template.layout.name;
        let lines = settings.text("M", area).expect("one Latin glyph");
        let face = ttf_parser::Face::parse(&settings.font, 0).expect("font");
        let advance = face
            .glyph_hor_advance(face.glyph_index('M').expect("M"))
            .expect("advance");
        let expected =
            f64::from(advance) * f64::from(area.font_size) / f64::from(face.units_per_em());
        assert!(
            (lines[0].width_um - expected).abs() < 0.1,
            "micrometres preserve font metric units"
        );
        assert!(matches!(
            settings.text(&"W".repeat(64), area),
            Err(Error::TextOverflow)
        ));
    }
}
