use domain::identity::keys::mobile::Keys;

use crate::{
    credentials::Filename,
    keys::{self, Error},
};

/// Load the fixed mobile key file without connecting to any service.
pub fn load() -> Result<Keys, Error> {
    let (active_id, entries) = keys::read(Filename::MobileKeys)?;
    match Keys::new(active_id, entries) {
        Ok(keys) => Ok(keys),
        Err(source) => Err(Error::Keys {
            file: Filename::MobileKeys,
            source,
        }),
    }
}
