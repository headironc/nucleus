use std::{
    fs::OpenOptions,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use platform::credentials;
use serde::de::DeserializeOwned;

use crate::access::Error;

pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                Error::PrivateFile
            } else {
                Error::Io(error)
            }
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(Error::PrivateFile);
    }
    Ok(credentials::read(file)?)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
    };

    use crate::access::{Error, files::read};

    fn private(directory: &tempfile::TempDir, contents: &[u8]) -> std::path::PathBuf {
        let path = directory.path().join("input");
        fs::write(&path, contents).expect("private fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
        path
    }

    #[test]
    fn reads_private_complete_json() {
        let directory = tempfile::tempdir().expect("private fixture");
        let path = private(&directory, br#"{"values": [1, 2, 3]}"#);
        assert_eq!(
            read::<serde_json::Value>(&path).expect("complete JSON"),
            serde_json::json!({"values": [1, 2, 3]})
        );
    }

    #[test]
    fn rejects_unsafe_mode_symlinks_hardlinks_and_fifos() {
        let directory = tempfile::tempdir().expect("private fixture");
        let path = private(&directory, b"{}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
        assert!(matches!(read::<Vec<u8>>(&path), Err(Error::PrivateFile)));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore mode");
        let link = directory.path().join("link");
        symlink(&path, &link).expect("fixture link");
        assert!(matches!(read::<Vec<u8>>(&link), Err(Error::PrivateFile)));
        let hardlink = directory.path().join("hardlink");
        fs::hard_link(&path, &hardlink).expect("fixture hard link");
        assert!(matches!(
            read::<Vec<u8>>(&hardlink),
            Err(Error::PrivateFile)
        ));

        let fifo = directory.path().join("fifo");
        assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .status()
                .expect("create fixture FIFO")
                .success()
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = read::<Vec<u8>>(&fifo);
            drop(directory);
            sender
                .send(matches!(result, Err(Error::PrivateFile)))
                .expect("FIFO result");
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("FIFO never waits for a writer")
        );
        worker.join().expect("FIFO worker ended");
    }
}
