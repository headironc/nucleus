//! Deployment input and file-source reading for the fixed badge template.

use std::{io::Read, path::PathBuf};

use domain::workforce::application;
use serde::Deserialize;
use serde_with::{json::JsonString, serde_as};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Workforce {
    pub(crate) application: Input,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    background_path: PathBuf,
    background_media_type: String,
    font_path: PathBuf,
    #[serde_as(as = "JsonString")]
    template: application::Template,
    #[serde_as(as = "JsonString")]
    pickups: Vec<String>,
    #[serde_as(as = "JsonString")]
    photo_limits: application::photo::Limits,
    pixels_per_inch: application::render::Resolution,
}

pub(crate) struct Settings {
    input: Input,
    storage: platform::files::Settings,
}

impl Settings {
    pub(crate) fn new(input: Input, storage: platform::files::Settings) -> Result<Self, Error> {
        application::Settings::validate(
            &input.template,
            &input.pickups,
            &input.background_media_type,
        )?;
        for path in [&input.background_path, &input.font_path] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(Error::Path);
            }
        }
        Ok(Self { input, storage })
    }

    pub(crate) fn prepare(self) -> Result<application::Settings, Error> {
        let background = read(&self.input.background_path, application::photo::INPUT_LIMIT)?;
        let font = read(
            &self.input.font_path,
            usize::try_from(domain::workforce::font::MAX_SIZE).map_err(|_| Error::Size)?,
        )?;
        Ok(application::Settings::new(
            self.storage,
            self.input.template,
            &self.input.background_media_type,
            background,
            font,
            self.input.pickups,
            self.input.photo_limits,
            self.input.pixels_per_inch,
        )?)
    }
}

fn read(path: &std::path::Path, limit: usize) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Path);
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit).map_err(|_| Error::Size)? + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Size);
    }
    Ok(bytes)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("badge source paths must be absolute regular files")]
    Path,
    #[error("badge source exceeds its byte limit")]
    Size,
    #[error("badge source reading failed")]
    Read(#[from] std::io::Error),
    #[error("badge template preparation failed")]
    Application(#[from] application::Error),
}
