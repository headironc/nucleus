use domain::identity::keys::challenge::Keys;

use crate::{
    credentials::Filename,
    keys::{self, Error},
};

/// Load the fixed Challenge proof key file without connecting to any service.
pub fn load() -> Result<Keys, Error> {
    let (active_id, entries) = keys::read(Filename::ChallengeKeys)?;
    match Keys::new(active_id, entries) {
        Ok(keys) => Ok(keys),
        Err(source) => Err(Error::Keys {
            file: Filename::ChallengeKeys,
            source,
        }),
    }
}
