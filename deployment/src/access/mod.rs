//! Private deployment input files.

pub(crate) mod files;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input files must be owned by the effective user, have one link and mode 0600")]
    PrivateFile,
    #[error("private input file operation failed")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    File(#[from] platform::credentials::Error),
}
