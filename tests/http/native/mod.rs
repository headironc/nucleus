//! Finite native mini fixture and non-native self-proof.
//!
//! The foreground supervisor owns this process, private controls and disposable PostgreSQL.
//! The operator contract and acceptance limits live in `docs/engineering/native-mini.md`.

mod control;
mod facts;
mod harness;
mod relay;
