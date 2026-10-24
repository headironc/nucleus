use std::num::NonZeroUsize;

use thiserror::Error;

use crate::identity::message;

/// C1 work capacity belongs to this consumer, not environment loading.
pub struct Settings {
    capacity: NonZeroUsize,
    message: message::Limits,
}

impl Settings {
    pub fn new(capacity: usize, message: message::Limits) -> Result<Self, Error> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Error)?;
        Ok(Self { capacity, message })
    }

    pub(super) fn capacity(&self) -> usize {
        self.capacity.get()
    }

    pub(super) fn message(&self) -> &message::Limits {
        &self.message
    }
}

#[derive(Debug, Error)]
#[error("authentication work capacity must be positive")]
pub struct Error;
