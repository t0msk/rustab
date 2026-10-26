//! The GUI-free half of Rustab, shared by the GUI and the helper.
//!
//! Everything here is compiled into **both** of Rustab's executables:
//!
//! - `rustab`, the Tauri GUI, which uses the DTOs to talk to the frontend
//!   and the protocol types to talk to the elevated side;
//! - `rustab-helper`, the GUI-free binary `pkexec` runs as root, which is
//!   nothing but [`protocol::handle_privileged_request`] behind an argv
//!   check (see `crates/rustab-helper`).
//!
//! That is the reason this crate exists at all. The elevated code used to
//! live inside the `rustab` crate, which depends on `tauri` and therefore
//! on GTK, WebKitGTK and the system glibc - so the only way to run it as
//! root was to re-execute the GUI binary. A helper that must be linkable
//! as a self-contained static binary cannot depend on any of that, so the
//! elevated half moved here, to a crate whose entire dependency list is
//! `fstab-core`, `serde` and `serde_json`. **Keep it that way**: a `tauri`,
//! `log` or GUI dependency added here silently ends up in the helper.
//!
//! Module map:
//!
//! - [`dto`]: serializable types crossing the Tauri boundary, and their
//!   lossless conversions to/from `fstab-core`'s domain types. They also
//!   form the bodies of the elevated protocol's requests and responses,
//!   which is why they live on this side of the split.
//! - [`backups`]: listing a file's backups and resolving a
//!   frontend-supplied backup path against that list - called on both
//!   sides of the privilege boundary.
//! - [`target_identity`]: the hardcoded `/etc/fstab` path, the file-identity
//!   test the elevated side applies to a target it received as data, and
//!   the external-change guard it re-checks before writing.
//! - [`protocol`]: the elevated side in full - the request/response union,
//!   the exit codes, and the one function that performs an operation.
//!
//! The unprivileged halves - `pkexec`/`flatpak-spawn` plumbing, exit-code
//! interpretation, target resolution, the Tauri commands - stay in the
//! `rustab` crate. Nothing here spawns a process or reads an environment
//! variable.

pub mod backups;
pub mod dto;
pub mod protocol;
pub mod target_identity;

#[cfg(test)]
mod testutil;
