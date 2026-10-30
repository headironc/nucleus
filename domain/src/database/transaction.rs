//! Compatibility entrypoints for consumers of the kernel's transaction completion primitives.

pub use crate::kernel::tx::{Completion, EndError, Known, Outcome, commit, rollback};
pub(crate) use crate::kernel::tx::{Error, begin, begin_current, confirm, object, shared};
