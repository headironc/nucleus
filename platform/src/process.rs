//! The entrypoint selects a stable local lock path; the running owner keeps the file open.

use std::{
    fmt,
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use thiserror::Error;

pub struct Instance(File);

impl Instance {
    /// The parent directory must be trusted and the path shared by all starts.
    /// This does not prove that old database transactions have ended.
    pub fn acquire(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::Path);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Path);
        }
        file.try_lock()?;
        Ok(Self(file))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Closing also releases the lock; never unlink the shared lock identity.
        let _ = self.0.unlock();
    }
}

#[derive(Error)]
pub enum Error {
    #[error("instance lock requires an absolute regular file path")]
    Path,
    #[error("instance lock file operation failed")]
    File(#[from] io::Error),
    #[error("instance lock is unavailable")]
    Lock(#[from] std::fs::TryLockError),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::os::unix::fs::symlink;

    use crate::process::Instance;

    #[test]
    fn holds_the_same_file_identity_until_the_owner_ends() {
        let directory = tempfile::tempdir().expect("temporary lock directory");
        let path = directory.path().join("instance");
        let instance = Instance::acquire(&path).expect("first instance");
        assert!(Instance::acquire(&path).is_err());
        drop(instance);
        let replacement = Instance::acquire(&path).expect("restart");
        let link = directory.path().join("link");
        symlink(&path, &link).expect("symlink fixture");
        assert!(Instance::acquire(&link).is_err());
        assert!(Instance::acquire(directory.path()).is_err());
        assert!(Instance::acquire(std::path::Path::new("relative")).is_err());
        drop(replacement);
        assert!(path.exists());
    }
}
