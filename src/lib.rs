//! Application assembly and lifecycle for delivered capabilities.

mod access;
mod center;
pub mod credentials;
pub mod database;
pub mod diagnostics;
mod extension;
mod health;
pub mod identity;
mod json;
pub mod keys;
mod listener;
mod operation;
mod path;
pub mod process;
mod query;
mod response;
mod route;
mod router;
pub mod settings;
pub mod source;
pub mod suppliers;
mod workforce;

#[cfg(test)]
mod tests {
    #[test]
    fn native_clippy_failure_fixture() {
        let enabled = std::hint::black_box(true);
        let _observed = enabled == true;
    }
}
