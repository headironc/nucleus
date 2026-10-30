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
use serde::Deserialize;

use super::{Error as ApplicationError, ResourceId};
use crate::identity::person;

/// Deployment capacity for retained photographs, including unsuccessful preparations.
#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "LimitInput")]
pub struct Limits {
    person_files: u64,
    person_bytes: u64,
    total_files: u64,
    total_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitInput {
    person_files: u64,
    person_bytes: u64,
    total_files: u64,
    total_bytes: u64,
}

impl TryFrom<LimitInput> for Limits {
    type Error = ApplicationError;

    fn try_from(input: LimitInput) -> Result<Self, Self::Error> {
        Self::new(
            input.person_files,
            input.person_bytes,
            input.total_files,
            input.total_bytes,
        )
    }
}

impl Limits {
    pub fn new(
        person_files: u64,
        person_bytes: u64,
        total_files: u64,
        total_bytes: u64,
    ) -> Result<Self, ApplicationError> {
        if person_files == 0
            || person_bytes == 0
            || person_files > total_files
            || person_bytes > total_bytes
        {
            return Err(ApplicationError::Configuration);
        }
        Ok(Self {
            person_files,
            person_bytes,
            total_files,
            total_bytes,
        })
    }

    pub(super) fn publish(
        &self,
        directory: &platform::files::Directory,
        person: person::Id,
        id: ResourceId,
        bytes: &[u8],
    ) -> Result<(), ApplicationError> {
        let storage = directory.exclusive(&["workforce", "photos"])?;
        let person = person.to_string();
        let total = storage.usage(&[])?;
        let owned = storage.usage(&[&person])?;
        let size = u64::try_from(bytes.len()).map_err(|_| ApplicationError::Configuration)?;
        for (usage, files, budget) in [
            (total, self.total_files, self.total_bytes),
            (owned, self.person_files, self.person_bytes),
        ] {
            if usage.files >= files || size > budget.saturating_sub(usage.bytes) {
                return Err(ApplicationError::PhotoCapacity);
            }
        }
        storage.publish(
            &[&person],
            &id.to_string(),
            &uuid::Uuid::new_v4().to_string(),
            bytes,
        )?;
        Ok(())
    }
}

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
    use std::{
        io::Cursor,
        os::unix::fs::{MetadataExt, PermissionsExt},
        sync::{Arc, Barrier},
    };

    use image::{DynamicImage, ImageFormat};

    use super::{INPUT_LIMIT, Limits, prepare};

    fn storage() -> (tempfile::TempDir, platform::files::Settings) {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("private photos");
        let device = directory.path().metadata().expect("device").dev();
        let mut mount = directory.path().to_path_buf();
        while let Some(parent) = mount.parent() {
            if parent.metadata().expect("ancestor").dev() != device {
                break;
            }
            mount = parent.to_path_buf();
        }
        let settings =
            platform::files::Settings::new(directory.path().to_path_buf(), mount, device)
                .expect("storage");
        (directory, settings)
    }

    #[test]
    fn capacity_input_requires_all_positive_and_consistent_limits() {
        for input in [
            r#"{}"#,
            r#"{"person_files":0,"person_bytes":1,"total_files":1,"total_bytes":1}"#,
            r#"{"person_files":1,"person_bytes":0,"total_files":1,"total_bytes":1}"#,
            r#"{"person_files":2,"person_bytes":1,"total_files":1,"total_bytes":1}"#,
            r#"{"person_files":1,"person_bytes":2,"total_files":1,"total_bytes":1}"#,
            r#"{"person_files":1,"person_bytes":1,"total_files":1,"total_bytes":1,"extra":1}"#,
        ] {
            assert!(serde_json::from_str::<Limits>(input).is_err());
        }
        assert!(
            serde_json::from_str::<Limits>(
                r#"{"person_files":1,"person_bytes":1,"total_files":1,"total_bytes":1}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn retained_photos_and_incomplete_orphans_count_after_reopening_storage() {
        let (root, settings) = storage();
        let person = crate::identity::person::Id::allocate();
        let other = crate::identity::person::Id::allocate();
        let limits = Limits::new(2, 6, 4, 9).expect("capacity");
        let directory = settings.open().expect("storage");
        limits
            .publish(&directory, person, super::ResourceId::allocate(), b"abc")
            .expect("first photo");
        assert!(
            matches!(
                limits.publish(&directory, person, super::ResourceId::allocate(), b"abcd"),
                Err(super::ApplicationError::PhotoCapacity)
            ),
            "bytes are checked independently of file count"
        );
        // A failed file publication can leave a writable candidate without database metadata.
        let candidate = root
            .path()
            .join("workforce/photos")
            .join(person.to_string())
            .join("incomplete");
        std::fs::write(&candidate, b"xyz").expect("interrupted candidate");
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600))
            .expect("private writable candidate");
        drop(directory);
        let directory = settings.open().expect("restarted storage");
        assert!(matches!(
            limits.publish(&directory, person, super::ResourceId::allocate(), b"a"),
            Err(super::ApplicationError::PhotoCapacity)
        ));
        limits
            .publish(&directory, other, super::ResourceId::allocate(), b"abc")
            .expect("another member's remaining capacity");
        assert!(matches!(
            limits.publish(&directory, other, super::ResourceId::allocate(), b"a"),
            Err(super::ApplicationError::PhotoCapacity)
        ));
        assert!(
            matches!(
                Limits::new(2, 99, 4, 99).expect("file limits").publish(
                    &directory,
                    person,
                    super::ResourceId::allocate(),
                    b"a"
                ),
                Err(super::ApplicationError::PhotoCapacity)
            ),
            "per-person file count is independent of bytes"
        );
        assert!(
            matches!(
            Limits::new(3, 99, 3, 99).expect("file limits").publish(
                    &directory,
                    other,
                    super::ResourceId::allocate(),
                    b"a"
                ),
                Err(super::ApplicationError::PhotoCapacity)
            ),
            "total file count is independent of bytes"
        );
        let held = directory
            .exclusive(&["workforce", "photos"])
            .expect("accounting lock");
        assert_eq!(
            held.usage(&[]).expect("all physical files"),
            platform::files::Usage { files: 3, bytes: 9 }
        );
    }

    #[test]
    fn two_members_and_independent_instances_compete_for_one_total_slot() {
        let (_root, settings) = storage();
        let limits = Limits::new(1, 3, 1, 3).expect("capacity");
        let ready = Arc::new(Barrier::new(3));
        let (send, receive) = std::sync::mpsc::channel();
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let directory = settings.open().expect("independent instance");
                let ready = Arc::clone(&ready);
                let send = send.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    let result = limits.publish(
                        &directory,
                        crate::identity::person::Id::allocate(),
                        super::ResourceId::allocate(),
                        b"abc",
                    );
                    send.send(result).expect("result receiver");
                })
            })
            .collect();
        ready.wait();
        let results: Vec<_> = (0..2)
            .map(|_| {
                receive
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("bounded publishers")
            })
            .collect();
        for task in tasks {
            task.join().expect("publisher ended");
        }
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(super::ApplicationError::PhotoCapacity)))
                .count(),
            1
        );
        let directory = settings.open().expect("reopened storage");
        let held = directory
            .exclusive(&["workforce", "photos"])
            .expect("accounting lock");
        assert_eq!(
            held.usage(&[]).expect("only the accepted file"),
            platform::files::Usage { files: 1, bytes: 3 }
        );
    }

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
