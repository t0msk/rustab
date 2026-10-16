//! Test-only scaffolding shared by this crate's unit tests.
//!
//! The whole module is `#[cfg(test)]`-gated from `lib.rs`, so none of it is
//! compiled into the shipped binary.
//!
//! It deliberately duplicates the tiny temp-directory helper that
//! `crates/fstab-core/src/writer.rs` keeps inside its own `mod tests` rather
//! than promoting that one into shared, published API: `fstab-core` is a
//! separate crate whose public surface is the fstab domain (parse, validate,
//! write) and should not grow a test-utility export just because a second
//! crate wants a scratch directory. A dozen lines of RAII are cheaper than a
//! `dev-dependencies` cycle or a published helper nobody outside the tests
//! should call.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique, self-deleting directory under [`std::env::temp_dir`].
///
/// Created on construction and removed recursively on drop, so tests never
/// leave files behind and never depend on each other's leftovers. The name
/// embeds the process id, a caller-supplied tag, and a process-wide counter,
/// which makes it unique across concurrently running test binaries *and*
/// across the threads of one binary (`cargo test` runs tests in parallel by
/// default).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a fresh temp directory tagged with `tag` for easier debugging
    /// when a failing test leaves one behind (e.g. after a panic that aborts
    /// before `Drop`).
    ///
    /// # Panics
    ///
    /// Panics if the directory cannot be created - a test that has no scratch
    /// space cannot produce a meaningful result, so failing loudly and
    /// immediately is better than every later assertion failing obscurely.
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustab-tauri-test-{}-{tag}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("could not create the test temp directory");
        TempDir { path }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Joins `name` (which may contain `/` separators) onto this directory
    /// without creating anything - the returned path is a *name*, and whether
    /// it exists is up to the code under test.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort: a leftover directory in `/tmp` is not worth panicking
        // over (and panicking in `Drop` during an already-failing test would
        // hide the real failure).
        let _ = fs::remove_dir_all(&self.path);
    }
}
