//! HTTP integration tests, one module per capability.
//!
//! Capability modules exercise HTTP contracts against the application started in-process, with
//! assembly and fixtures in `application`, `database` and `supplier`. `service` is the
//! delivered-binary check that runs the built executable from its external configuration.

#![allow(clippy::expect_used)]

mod application;
mod appointment;
mod authentication;
mod avatar;
mod badge;
mod cancellation;
mod center;
mod correction;
mod database;
mod directory;
mod grant;
mod invitation;
mod mobile;
mod number;
mod protocol;
mod revocation;
mod role;
mod service;
mod supplier;
mod suspension;
mod website;
