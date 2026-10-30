//! Private files anchored to a verified mounted filesystem.

use std::{
    fs::File,
    io::{Read as _, Write as _},
    os::fd::OwnedFd,
    path::{Component, Path, PathBuf},
};

use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, StatxAttributes,
        StatxFlags, fchmod, flock, fstat, fsync, mkdirat, openat, renameat_with, statat, statx,
        unlinkat,
    },
    io::Errno,
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Deployment paths and the expected mounted device; opening never creates a missing root.
#[derive(Clone)]
pub struct Settings {
    root: PathBuf,
    mount: PathBuf,
    device: u64,
}

impl Settings {
    pub fn new(root: PathBuf, mount: PathBuf, device: u64) -> Result<Self, Error> {
        for path in [&root, &mount] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
            {
                return Err(Error::Path);
            }
        }
        let relative = root.strip_prefix(&mount).map_err(|_| Error::BelowMount)?;
        if relative.as_os_str().is_empty() {
            return Err(Error::BelowMount);
        }
        Ok(Self {
            root,
            mount,
            device,
        })
    }

    pub fn open(&self) -> Result<Directory, Error> {
        // NOFOLLOW on a whole path only checks its leaf, so also walk the mount's ancestors.
        let mut mount = openat(CWD, "/", DIRECTORY, Mode::empty())?;
        for part in self.mount.components() {
            if let Component::Normal(name) = part {
                mount = openat(&mount, name, DIRECTORY, Mode::empty())?;
            }
        }
        // Inspect the held directory, not a pathname which could name another mount now.
        let evidence = statx(
            &mount,
            "",
            AtFlags::EMPTY_PATH | AtFlags::NO_AUTOMOUNT,
            StatxFlags::BASIC_STATS,
        )?;
        if !evidence
            .stx_attributes_mask
            .contains(StatxAttributes::MOUNT_ROOT)
        {
            return Err(Error::Unsupported);
        }
        if !evidence
            .stx_attributes
            .contains(StatxAttributes::MOUNT_ROOT)
        {
            return Err(Error::MountRoot);
        }
        if fstat(&mount)?.st_dev != self.device {
            return Err(Error::Device);
        }
        let mut root = mount;
        for part in self
            .root
            .strip_prefix(&self.mount)
            .map_err(|_| Error::BelowMount)?
            .components()
        {
            let Component::Normal(name) = part else {
                return Err(Error::Path);
            };
            root = openat(&root, name, DIRECTORY, Mode::empty())?;
            if fstat(&root)?.st_dev != self.device {
                return Err(Error::Device);
            }
        }
        private_directory(&root, self.device)?;
        Ok(Directory {
            root,
            device: self.device,
        })
    }
}

/// All names below this private root are interpreted relative to held directory handles.
pub struct Directory {
    root: OwnedFd,
    device: u64,
}

impl Directory {
    /// Serialize filesystem accounting and publication across independently opened roots.
    /// The held directory inode is the lock identity; callers must not replace that directory.
    pub fn exclusive(&self, segments: &[&str]) -> Result<ExclusiveDirectory, Error> {
        let directory = self.directory(segments, true)?;
        // dup shares an open-file description, which would also share an existing flock.
        let root = openat(&directory, ".", DIRECTORY, Mode::empty())?;
        flock(&root, FlockOperation::LockExclusive)?;
        Ok(ExclusiveDirectory(Directory {
            root,
            device: self.device,
        }))
    }

    /// The caller must already own the durable intention and serialize this candidate identity.
    pub fn publish(
        &self,
        segments: &[&str],
        name: &str,
        candidate: &str,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let directory = self.directory(segments, true)?;
        let name = checked(name)?;
        let candidate = checked(candidate)?;
        if name == candidate {
            return Err(Error::Path);
        }
        if let Some(file) = open_file(&directory, name, self.device)? {
            if file.read(bytes.len() as u64)? != bytes {
                return Err(Error::Identity);
            }
            // A previous rename may have succeeded before its directory sync failed.
            remove_entry(&directory, candidate, self.device)?;
            fsync(&directory)?;
            return Ok(());
        }
        // A crash may leave an incomplete candidate. It is never the published file.
        remove_entry(&directory, candidate, self.device)?;
        let fd = openat(
            &directory,
            candidate,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        fchmod(&file, Mode::RUSR)?;
        file.sync_all()?;
        match renameat_with(
            &directory,
            candidate,
            &directory,
            name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(Errno::EXIST) => {
                let existing = open_file(&directory, name, self.device)?.ok_or(Error::Identity)?;
                if existing.read(bytes.len() as u64)? != bytes {
                    return Err(Error::Identity);
                }
                remove_entry(&directory, candidate, self.device)?;
            }
            Err(error) => return Err(error.into()),
        }
        fsync(&directory)?;
        Ok(())
    }

    /// Open under the resource lock; retain the handle after releasing that lock.
    pub fn open(&self, segments: &[&str], name: &str) -> Result<Option<Reader>, Error> {
        match self.directory(segments, false) {
            Ok(directory) => open_file(&directory, checked(name)?, self.device),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Remove only the named private object; absence is an idempotent completion.
    pub fn remove(&self, segments: &[&str], name: &str) -> Result<(), Error> {
        match self.directory(segments, false) {
            Ok(directory) => {
                remove_entry(&directory, checked(name)?, self.device)?;
                fsync(&directory)?;
                Ok(())
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn directory(&self, segments: &[&str], create: bool) -> Result<OwnedFd, Error> {
        let mut directory = rustix::io::dup(&self.root)?;
        for segment in segments {
            let segment = checked(segment)?;
            if create {
                match mkdirat(&directory, segment, Mode::RWXU) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let next = openat(&directory, segment, DIRECTORY, Mode::empty())?;
            private_directory(&next, self.device)?;
            if create {
                // Sync every ancestor entry, including a previous incomplete mkdir.
                fsync(&directory)?;
            }
            directory = next;
        }
        Ok(directory)
    }
}

/// Closing the held directory releases its filesystem lock, including after process exit.
pub struct ExclusiveDirectory(Directory);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
}

impl ExclusiveDirectory {
    /// Count every private regular file, including writable or incomplete candidates.
    /// Links, unexpected file types and changes to the expected filesystem fail closed.
    pub fn usage(&self, segments: &[&str]) -> Result<Usage, Error> {
        let root = match self.0.directory(segments, false) {
            Ok(directory) => directory,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Usage::default());
            }
            Err(error) => return Err(error),
        };
        let mut pending = vec![root];
        let mut usage = Usage::default();
        while let Some(directory) = pending.pop() {
            for entry in Dir::read_from(&directory)? {
                let entry = entry?;
                let name = entry.file_name();
                if name.to_bytes() == b"." || name.to_bytes() == b".." {
                    continue;
                }
                let stat = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
                if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                    let child = openat(&directory, name, DIRECTORY, Mode::empty())?;
                    private_directory(&child, self.0.device)?;
                    pending.push(child);
                } else {
                    private_file(&stat, self.0.device)?;
                    usage.files = usage.files.checked_add(1).ok_or(Error::Identity)?;
                    usage.bytes = usage
                        .bytes
                        .checked_add(u64::try_from(stat.st_size).map_err(|_| Error::Identity)?)
                        .ok_or(Error::Identity)?;
                }
            }
        }
        Ok(usage)
    }

    pub fn publish(
        &self,
        segments: &[&str],
        name: &str,
        candidate: &str,
        bytes: &[u8],
    ) -> Result<(), Error> {
        self.0.publish(segments, name, candidate, bytes)
    }
}

/// A held regular-file inode remains readable after its directory entry is unlinked.
pub struct Reader(File);

impl Reader {
    pub fn read(mut self, expected_size: u64) -> Result<Vec<u8>, Error> {
        if self.0.metadata()?.len() != expected_size {
            return Err(Error::Identity);
        }
        let length = usize::try_from(expected_size).map_err(|_| Error::Identity)?;
        let mut bytes = vec![0; length];
        self.0.read_exact(&mut bytes)?;
        let mut extra = [0];
        if self.0.read(&mut extra)? != 0 {
            return Err(Error::Identity);
        }
        Ok(bytes)
    }
}

fn open_file(directory: &OwnedFd, name: &str, device: u64) -> Result<Option<Reader>, Error> {
    // NONBLOCK makes a FIFO fail its type check without suspending the operation.
    let fd = match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let stat = fstat(&fd)?;
    private_file(&stat, device)?;
    if Mode::from_bits_truncate(stat.st_mode).contains(Mode::WUSR) {
        return Err(Error::Identity);
    }
    Ok(Some(Reader(File::from(fd))))
}

fn private_directory(fd: &OwnedFd, device: u64) -> Result<(), Error> {
    let stat = fstat(fd)?;
    if stat.st_dev != device {
        return Err(Error::Device);
    }
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || Mode::from_bits_truncate(stat.st_mode).intersects(Mode::RWXG | Mode::RWXO)
    {
        return Err(Error::PrivateDirectory);
    }
    Ok(())
}

fn private_file(stat: &rustix::fs::Stat, device: u64) -> Result<(), Error> {
    if stat.st_dev != device {
        return Err(Error::Device);
    }
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_nlink != 1
        || Mode::from_bits_truncate(stat.st_mode).intersects(Mode::RWXG | Mode::RWXO)
    {
        return Err(Error::Identity);
    }
    Ok(())
}

fn remove_entry(directory: &OwnedFd, name: &str, device: u64) -> Result<(), Error> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => private_file(&stat, device)?,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn checked(name: &str) -> Result<&str, Error> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || name.contains('/')
        || name.as_bytes().contains(&0)
    {
        return Err(Error::Path);
    }
    Ok(name)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage paths must be absolute without parent traversal")]
    Path,
    #[error("storage root must be below its configured mount")]
    BelowMount,
    #[error("configured storage mount is not a filesystem mount root")]
    MountRoot,
    #[error("the filesystem does not provide mount-root evidence")]
    Unsupported,
    #[error("storage device differs from the configured identity")]
    Device,
    #[error("storage directory is not owned and private")]
    PrivateDirectory,
    #[error("private file identity or content differs")]
    Identity,
    #[error("private file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Errno> for Error {
    fn from(error: Errno) -> Self {
        Self::Io(error.into())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
        mount: PathBuf,
        device: u64,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .expect("private temporary root");
            let device = root.path().metadata().expect("root metadata").dev();
            let mut mount = root.path().to_path_buf();
            loop {
                let stat = statx(
                    CWD,
                    &mount,
                    AtFlags::SYMLINK_NOFOLLOW,
                    StatxFlags::BASIC_STATS,
                )
                .expect("mount evidence");
                if stat.stx_attributes.contains(StatxAttributes::MOUNT_ROOT) {
                    break;
                }
                assert!(mount.pop(), "temporary filesystem has a mount root");
            }
            Self {
                root,
                mount,
                device,
            }
        }
        fn settings(&self) -> Settings {
            Settings::new(
                self.root.path().to_path_buf(),
                self.mount.clone(),
                self.device,
            )
            .expect("settings")
        }
        fn open(&self) -> Directory {
            self.settings().open().expect("real private filesystem")
        }
    }

    #[test]
    fn directory_lock_is_visible_to_another_process_and_released_on_close() {
        let fixture = Fixture::new();
        let directory = fixture.open();
        let held = directory
            .exclusive(&["quota"])
            .expect("held filesystem lock");
        let run = |locked: bool| {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "files::tests::directory_lock_child",
                        "--nocapture",
                    ])
                    .env("NUCLEUS_TEST_LOCK_ROOT", fixture.root.path())
                    .env("NUCLEUS_TEST_LOCK_MOUNT", &fixture.mount)
                    .env("NUCLEUS_TEST_LOCK_DEVICE", fixture.device.to_string())
                    .env("NUCLEUS_TEST_LOCKED", if locked { "yes" } else { "no" })
                    .spawn()
                    .expect("independent test process");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_wait().expect("child status") {
                    assert!(status.success());
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().expect("stop timed-out child");
                    child.wait().expect("reap child");
                    panic!("file lock observation timed out");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        run(true);
        drop(held);
        run(false);
    }

    #[test]
    fn directory_lock_child() {
        let Some(root) = std::env::var_os("NUCLEUS_TEST_LOCK_ROOT") else {
            return;
        };
        let mount = std::env::var_os("NUCLEUS_TEST_LOCK_MOUNT").expect("mount");
        let device = std::env::var("NUCLEUS_TEST_LOCK_DEVICE")
            .expect("device")
            .parse()
            .expect("device number");
        let directory = Settings::new(root.into(), mount.into(), device)
            .expect("settings")
            .open()
            .expect("private root");
        let leaf = directory
            .directory(&["quota"], false)
            .expect("lock directory");
        let result = flock(&leaf, FlockOperation::NonBlockingLockExclusive);
        if std::env::var("NUCLEUS_TEST_LOCKED").expect("lock expectation") == "yes" {
            assert_eq!(result, Err(Errno::WOULDBLOCK));
        } else {
            result.expect("released process lock");
        }
    }

    #[test]
    fn publication_recovers_partial_candidate_without_overwriting_final_content() {
        let fixture = Fixture::new();
        let directory = fixture.open();
        let leaf = directory.directory(&["aa", "bb"], true).expect("shards");
        let fd = openat(
            &leaf,
            "candidate",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        )
        .expect("interrupted candidate");
        File::from(fd).write_all(b"partial").expect("partial bytes");
        directory
            .publish(&["aa", "bb"], "image", "candidate", b"complete")
            .expect("recover publication");
        directory
            .publish(&["aa", "bb"], "image", "candidate", b"complete")
            .expect("replay publication");
        assert!(matches!(
            directory.publish(&["aa", "bb"], "image", "candidate", b"different"),
            Err(Error::Identity)
        ));
        assert_eq!(
            directory
                .open(&["aa", "bb"], "image")
                .expect("open")
                .expect("file")
                .read(8)
                .expect("read"),
            b"complete"
        );
        assert_eq!(
            fixture
                .root
                .path()
                .join("aa/bb/image")
                .metadata()
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        assert!(!fixture.root.path().join("aa/bb/candidate").exists());
        drop(directory);
        assert_eq!(
            fixture
                .open()
                .open(&["aa", "bb"], "image")
                .expect("reopen")
                .expect("file")
                .read(8)
                .expect("read"),
            b"complete"
        );
    }

    #[test]
    fn open_handle_survives_unlink_and_other_identity_remains() {
        let fixture = Fixture::new();
        let directory = fixture.open();
        for name in ["first", "second"] {
            directory
                .publish(&["aa"], name, &format!("{name}.candidate"), b"same")
                .expect("publish");
        }
        let held = directory
            .open(&["aa"], "first")
            .expect("open")
            .expect("held file");
        directory.remove(&["aa"], "first").expect("unlink");
        directory.remove(&["aa"], "first").expect("retry unlink");
        assert!(directory.open(&["aa"], "first").expect("absence").is_none());
        assert_eq!(held.read(4).expect("held inode"), b"same");
        assert_eq!(
            directory
                .open(&["aa"], "second")
                .expect("second")
                .expect("file")
                .read(4)
                .expect("read"),
            b"same"
        );
        assert!(
            directory
                .open(&["missing"], "image")
                .expect("absent shard")
                .is_none()
        );
        directory
            .remove(&["missing"], "image")
            .expect("absent removal");
    }

    #[test]
    fn rejects_wrong_mount_device_and_symbolic_ancestors() {
        let fixture = Fixture::new();
        assert!(matches!(
            Settings::new(
                fixture.root.path().to_path_buf(),
                fixture.mount.clone(),
                fixture.device ^ 1
            )
            .expect("shape")
            .open(),
            Err(Error::Device)
        ));
        let child = fixture.root.path().join("child");
        std::fs::create_dir(&child).expect("child");
        assert!(matches!(
            Settings::new(child, fixture.root.path().to_path_buf(), fixture.device)
                .expect("shape")
                .open(),
            Err(Error::MountRoot)
        ));
        let alias =
            Path::new("/proc/self/root").join(fixture.mount.strip_prefix("/").expect("absolute"));
        let root_alias = alias.join(
            fixture
                .root
                .path()
                .strip_prefix(&fixture.mount)
                .expect("below mount"),
        );
        assert!(
            Settings::new(root_alias, alias, fixture.device)
                .expect("shape")
                .open()
                .is_err()
        );
        let directory = fixture.open();
        symlink("/", fixture.root.path().join("alias")).expect("symlink");
        assert!(directory.open(&["alias"], "file").is_err());
        symlink("/etc/passwd", fixture.root.path().join("linked")).expect("file symlink");
        assert!(directory.open(&[], "linked").is_err());
        assert!(directory.remove(&[], "linked").is_err());
    }

    #[test]
    fn rejects_public_directories_and_writable_or_linked_files() {
        let fixture = Fixture::new();
        let directory = fixture.open();
        directory
            .publish(&["aa"], "image", "candidate", b"bytes")
            .expect("publish");
        let path = fixture.root.path().join("aa/image");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("alter mode");
        assert!(matches!(
            directory.open(&["aa"], "image"),
            Err(Error::Identity)
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("restore mode");
        std::fs::hard_link(&path, fixture.root.path().join("alias")).expect("hard link");
        assert!(matches!(
            directory.open(&["aa"], "image"),
            Err(Error::Identity)
        ));
        std::fs::remove_file(fixture.root.path().join("alias")).expect("remove link");
        std::fs::set_permissions(
            fixture.root.path().join("aa"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("expose shard");
        assert!(matches!(
            directory.open(&["aa"], "image"),
            Err(Error::PrivateDirectory)
        ));
        std::fs::set_permissions(fixture.root.path(), std::fs::Permissions::from_mode(0o755))
            .expect("expose root");
        assert!(matches!(
            fixture.settings().open(),
            Err(Error::PrivateDirectory)
        ));
    }
}
