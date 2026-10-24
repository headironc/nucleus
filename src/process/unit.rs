use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

const GROUP: &str = "/system.slice/nucleus.service";

pub(super) fn verify() -> Result<(), Error> {
    let membership = fs::read_to_string("/proc/self/cgroup")?;
    if membership.trim() != format!("0::{GROUP}") {
        return Err(Error::Group);
    }
    empty_except(
        &PathBuf::from("/sys/fs/cgroup").join(GROUP.trim_start_matches('/')),
        std::process::id(),
    )
}

fn empty_except(directory: &Path, process_id: u32) -> Result<(), Error> {
    for id in fs::read_to_string(directory.join("cgroup.procs"))?.split_whitespace() {
        if id.parse::<u32>() != Ok(process_id) {
            return Err(Error::Occupied);
        }
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            empty_except(&entry.path(), process_id)?;
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("application must run in the fixed nucleus.service cgroup")]
    Group,
    #[error("application cgroup still contains another process")]
    Occupied,
    #[error("cannot verify application cgroup state")]
    Read(#[from] io::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::fs;

    use crate::process::unit::{Error, empty_except};

    #[test]
    fn checks_descendants_and_fails_closed_when_state_is_missing() {
        let root = tempfile::tempdir().expect("group fixture");
        fs::write(root.path().join("cgroup.procs"), "42\n").expect("self");
        assert!(empty_except(root.path(), 42).is_ok());
        let child = root.path().join("child");
        fs::create_dir(&child).expect("descendant");
        assert!(matches!(empty_except(root.path(), 42), Err(Error::Read(_))));
        fs::write(child.join("cgroup.procs"), "43\n").expect("old child");
        assert!(matches!(
            empty_except(root.path(), 42),
            Err(Error::Occupied)
        ));
        fs::write(child.join("cgroup.procs"), "").expect("exited child");
        assert!(empty_except(root.path(), 42).is_ok());
    }
}
