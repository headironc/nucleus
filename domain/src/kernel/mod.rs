//! The use-case kernel: one executor for original-operation identity, recovery, the write
//! order and error classification. It holds no business rules; capabilities declare commands
//! and queries on it.

pub mod completion;
pub mod error;
mod executor;
pub mod keys;
pub mod operation;
pub(crate) mod time;
pub(crate) mod tx;

pub use crate::kernel::error::{Error, Fault, FaultKind, Rejection, Stage, Status};
pub(crate) use crate::kernel::executor::{App, Authorization, Command, Order, Query};
