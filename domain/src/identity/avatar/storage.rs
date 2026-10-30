//! Avatar identities select private paths; the platform owns anchored filesystem operations.

use std::sync::Arc;

pub use platform::files::{Error, Settings};

use crate::identity::avatar::Id;

#[derive(Clone)]
pub(crate) struct Storage(Arc<Inner>);

struct Inner {
    directory: platform::files::Directory,
    #[cfg(test)]
    _temporary: Option<tempfile::TempDir>,
}

impl Storage {
    pub(crate) fn open(settings: &Settings) -> Result<Self, Error> {
        Ok(Self(Arc::new(Inner {
            directory: settings.open()?,
            #[cfg(test)]
            _temporary: None,
        })))
    }

    #[cfg(test)]
    pub(crate) fn temporary_for_test() -> Result<Self, Error> {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let device = root.path().metadata()?.dev();
        let mut mount = root.path().to_path_buf();
        while let Some(parent) = mount.parent() {
            if parent.metadata()?.dev() != device {
                break;
            }
            mount = parent.to_path_buf();
        }
        // Test input discovery does not relax production mount or private-directory checks.
        let settings = Settings::new(root.path().to_path_buf(), mount, device)?;
        Ok(Self(Arc::new(Inner {
            directory: settings.open()?,
            _temporary: Some(root),
        })))
    }

    pub(crate) fn publish(&self, id: Id, bytes: &[u8]) -> Result<(), Error> {
        let (first, second, name) = path(id);
        self.0.directory.publish(
            &["avatars", &first, &second],
            &name,
            &format!("{name}.candidate"),
            bytes,
        )
    }

    pub(crate) fn open_file(&self, id: Id) -> Result<Option<platform::files::Reader>, Error> {
        let (first, second, name) = path(id);
        self.0.directory.open(&["avatars", &first, &second], &name)
    }

    pub(crate) fn read(&self, id: Id, size: u64) -> Result<Vec<u8>, Error> {
        self.open_file(id)?.ok_or(Error::Identity)?.read(size)
    }

    pub(crate) fn remove(&self, id: Id) -> Result<(), Error> {
        let (first, second, name) = path(id);
        // Both names belong to the same durable intention until adoption is resolved.
        self.0
            .directory
            .remove(&["avatars", &first, &second], &name)?;
        self.0
            .directory
            .remove(&["avatars", &first, &second], &format!("{name}.candidate"))
    }
}

fn path(id: Id) -> (String, String, String) {
    let bytes = id.as_bytes();
    (
        hex::encode(&bytes[..1]),
        hex::encode(&bytes[1..2]),
        uuid::Uuid::from_bytes(*bytes).to_string(),
    )
}
