//! `fstab-core` is a GUI-free Rust library for parsing, validating, and
//! atomically rewriting `/etc/fstab`-style files.
//!
//! This crate has no Tauri or GUI dependency of any kind, so its parsing,
//! validation, and atomic-write logic can be unit-tested in isolation and
//! reused outside of the Rustab GUI (e.g. from a future CLI). See
//! `docs/SPEC.md` in the workspace root for the full requirements this
//! crate implements.

pub mod discovery;
pub mod model;
pub mod parser;
pub mod validate;
pub mod writer;
