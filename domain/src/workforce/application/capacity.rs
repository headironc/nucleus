//! Physical capacity for retained application files, including unpublished candidates.

use serde::Deserialize;

use super::Error;
use crate::identity::person;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    person_files: u64,
    person_bytes: u64,
    total_files: u64,
    total_bytes: u64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "Input")]
pub struct Limits {
    person_files: u64,
    person_bytes: u64,
    total_files: u64,
    total_bytes: u64,
}

impl TryFrom<Input> for Limits {
    type Error = Error;

    fn try_from(input: Input) -> Result<Self, Self::Error> {
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
    ) -> Result<Self, Error> {
        if person_files == 0
            || person_bytes == 0
            || person_files > total_files
            || person_bytes > total_bytes
        {
            return Err(Error::Configuration);
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
        collection: &str,
        person: person::Id,
        id: &str,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let storage = directory.exclusive(&["workforce", collection])?;
        let person = person.to_string();
        let total = storage.usage(&[])?;
        let owned = storage.usage(&[&person])?;
        let size = u64::try_from(bytes.len()).map_err(|_| Error::Configuration)?;
        for (usage, files, budget) in [
            (total, self.total_files, self.total_bytes),
            (owned, self.person_files, self.person_bytes),
        ] {
            if usage.files >= files || size > budget.saturating_sub(usage.bytes) {
                return Err(Error::Capacity);
            }
        }
        storage.publish(&[&person], id, &uuid::Uuid::new_v4().to_string(), bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{
        os::unix::fs::{MetadataExt, PermissionsExt},
        sync::{Arc, Barrier},
    };

    use super::*;

    fn storage() -> (tempfile::TempDir, platform::files::Settings) {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("private files");
        let device = root.path().metadata().expect("device").dev();
        let mut mount = root.path().to_path_buf();
        while let Some(parent) = mount.parent() {
            if parent.metadata().expect("ancestor").dev() != device {
                break;
            }
            mount = parent.to_path_buf();
        }
        let settings = platform::files::Settings::new(root.path().to_path_buf(), mount, device)
            .expect("storage");
        (root, settings)
    }

    #[test]
    fn badges_count_retained_and_incomplete_files_across_restart_and_keep_photo_budgets_separate() {
        let (root, settings) = storage();
        let person = person::Id::allocate();
        let other = person::Id::allocate();
        let limits = Limits::new(2, 6, 4, 9).expect("limits");
        let directory = settings.open().expect("storage");
        limits
            .publish(&directory, "badges", person, "first", b"abc")
            .expect("retained badge");
        assert!(matches!(
            limits.publish(&directory, "badges", person, "too-large", b"abcd"),
            Err(Error::Capacity)
        ));
        let candidate = root
            .path()
            .join("workforce/badges")
            .join(person.to_string())
            .join("incomplete");
        std::fs::write(&candidate, b"xyz").expect("interrupted candidate");
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600))
            .expect("private candidate");
        drop(directory);
        let directory = settings.open().expect("restarted instance");
        assert!(matches!(
            limits.publish(&directory, "badges", person, "refused", b"a"),
            Err(Error::Capacity)
        ));
        limits
            .publish(&directory, "badges", other, "last", b"abc")
            .expect("exact total byte boundary");
        assert!(matches!(
            limits.publish(&directory, "badges", other, "refused", b"a"),
            Err(Error::Capacity)
        ));
        assert!(matches!(
            Limits::new(2, 99, 4, 99)
                .expect("file limits")
                .publish(&directory, "badges", person, "refused", b"a"),
            Err(Error::Capacity)
        ));
        assert!(matches!(
            Limits::new(3, 99, 3, 99)
                .expect("file limits")
                .publish(&directory, "badges", other, "refused", b"a"),
            Err(Error::Capacity)
        ));
        assert_eq!(
            directory
                .open(&["workforce", "badges", &person.to_string()], "first")
                .expect("stored file")
                .expect("retained badge")
                .read(3)
                .expect("existing read"),
            b"abc"
        );
        Limits::new(1, 3, 1, 3)
            .expect("photo limits")
            .publish(&directory, "photos", person, "photo", b"abc")
            .expect("independent photo capacity");
        let held = directory.exclusive(&["workforce", "badges"]).expect("lock");
        assert_eq!(
            held.usage(&[]).expect("physical usage"),
            platform::files::Usage { files: 3, bytes: 9 }
        );
    }

    #[test]
    fn independent_badge_instances_cannot_overrun_person_or_total_capacity() {
        for same_person in [true, false] {
            let (_root, settings) = storage();
            let person = person::Id::allocate();
            let limits = if same_person {
                Limits::new(1, 3, 2, 6)
            } else {
                Limits::new(1, 3, 1, 3)
            }
            .expect("limits");
            let ready = Arc::new(Barrier::new(3));
            let (send, receive) = std::sync::mpsc::channel();
            let tasks: Vec<_> = (0..2)
                .map(|index| {
                    let directory = settings.open().expect("independent root");
                    let ready = Arc::clone(&ready);
                    let send = send.clone();
                    let person = if same_person {
                        person
                    } else {
                        person::Id::allocate()
                    };
                    std::thread::spawn(move || {
                        ready.wait();
                        send.send(limits.publish(
                            &directory,
                            "badges",
                            person,
                            &index.to_string(),
                            b"abc",
                        ))
                        .expect("receiver");
                    })
                })
                .collect();
            ready.wait();
            let results: Vec<_> = (0..2)
                .map(|_| {
                    receive
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect("bounded publication")
                })
                .collect();
            for task in tasks {
                task.join().expect("publisher ended");
            }
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|result| matches!(result, Err(Error::Capacity)))
                    .count(),
                1
            );
            let directory = settings.open().expect("reopened root");
            let held = directory.exclusive(&["workforce", "badges"]).expect("lock");
            assert_eq!(
                held.usage(&[]).expect("physical usage"),
                platform::files::Usage { files: 1, bytes: 3 }
            );
        }
    }
}
