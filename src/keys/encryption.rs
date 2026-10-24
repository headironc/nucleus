use domain::identity::keys::encryption::Keys;

use crate::{
    credentials::Filename,
    keys::{self, Error},
};

/// Load the fixed personal encryption key file without connecting to any service.
pub fn load() -> Result<Keys, Error> {
    let (active_id, entries) = keys::read(Filename::EncryptionKeys)?;
    match Keys::new(active_id, entries) {
        Ok(keys) => Ok(keys),
        Err(source) => Err(Error::Keys {
            file: Filename::EncryptionKeys,
            source,
        }),
    }
}
