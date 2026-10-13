//! Active-target resolution — see `docs/SPEC.md` §7 and `docs/API_CONTRACT.md`.
//!
//! Exactly one file is "the active target" at any moment, and this module is
//! the only place that decides which one. Every command that reads or writes
//! fstab content goes through [`active_target`]; no command accepts a path
//! from the frontend, so a compromised or buggy UI can never redirect a save
//! at a file of its choosing (the one exception, `restore_backup`, takes a
//! path but only honours it if it is one of the backups
//! [`fstab_core::writer::list_backups`] reports for *this* target — see
//! `crate::backups`).
//!
//! ## The policy
//!
//! 1. If `RUSTAB_TARGET_PATH` is set to a non-empty value, that is the
//!    target, verbatim, in both debug and release builds. This is the only
//!    way to point Rustab at the real `/etc/fstab`.
//! 2. Otherwise the target is a per-user sandbox file,
//!    `$XDG_DATA_HOME/rustab/fstab.sandbox` (falling back to
//!    `$HOME/.local/share/rustab/fstab.sandbox`, and to a temp-dir path if
//!    neither variable is set). It is created, with a harmless comment-only
//!    seed, the first time it is resolved — so a fresh checkout has
//!    something safe and valid to load.
//!
//! Rule 2 deliberately applies to **release builds too**. `docs/SPEC.md` §7
//! only mandates the sandbox default for debug builds, but defaulting a
//! release build to `/etc/fstab` would mean the app's very first screen is
//! already pointed at the file whose corruption §2.1 calls the one
//! unacceptable outcome — before the user has agreed to anything. Making
//! "edit the real file" an explicit, deliberate act (setting the env var)
//! costs an informed user one line and protects an uninformed one from the
//! worst case. See `docs/API_CONTRACT.md` for the full reasoning and for the
//! follow-up UX decision this implies (an in-GUI target switch is a future
//! task, not something this layer invents).
//!
//! ## `is_real`
//!
//! [`ActiveTarget::is_real`] is `true` only when the resolved path *is* the
//! real `/etc/fstab`: either literally, or because canonicalizing both paths
//! lands on the same file. The canonicalized comparison is what catches a
//! symlink pointed at `/etc/fstab` (or an `/etc/fstab` symlinked elsewhere,
//! with the env var naming the destination) — in both directions the answer
//! has to be "yes, this is the real thing", because the banner driven by
//! this flag is the user's only warning (`docs/UI_DESIGN.md` §2).

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the target file. Documented in
/// `docs/API_CONTRACT.md` and `.env.example`.
pub const TARGET_PATH_ENV: &str = "RUSTAB_TARGET_PATH";

/// The one path that counts as "the real thing" (see [`ActiveTarget::is_real`]).
pub const REAL_FSTAB_PATH: &str = "/etc/fstab";

/// Directory, relative to the user's data dir, holding the sandbox target.
const SANDBOX_DIR_NAME: &str = "rustab";

/// File name of the default sandbox target.
const SANDBOX_FILE_NAME: &str = "fstab.sandbox";

/// Content a freshly created sandbox file is seeded with: comments only, so
/// it parses cleanly, validates without a single finding, and mounts nothing
/// if a user ever copied it somewhere real by accident.
const SANDBOX_SEED: &str = "\
# Rustab development sandbox - NOT your real /etc/fstab.
#
# This file was created automatically because RUSTAB_TARGET_PATH was not set.
# Editing and saving here never touches the real /etc/fstab. To point Rustab
# at the real file instead, start it with:
#
#     RUSTAB_TARGET_PATH=/etc/fstab rustab
#
# Add entries below in normal fstab syntax, e.g.:
# UUID=00000000-0000-0000-0000-000000000000 /mnt/example ext4 noauto 0 2
";

/// Where the active target came from, so the UI can explain itself ("sandbox
/// default" vs. "you asked for this file").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    /// [`TARGET_PATH_ENV`] was set and non-empty.
    Environment,
    /// No override was set; this is the auto-created per-user sandbox.
    SandboxDefault,
}

/// The resolved active target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTarget {
    /// The path to read and write. Never empty.
    pub path: PathBuf,
    /// `true` only for the real `/etc/fstab` (see the module docs).
    pub is_real: bool,
    /// How this path was chosen.
    pub source: TargetSource,
}

/// Why the active target could not be determined.
///
/// Resolution is deliberately allowed to fail loudly rather than silently
/// fall back to some other path: a target banner showing the wrong file is
/// worse than an error message (`docs/UI_DESIGN.md` §2).
#[derive(Debug)]
pub enum TargetError {
    /// The sandbox file or its directory could not be created.
    SandboxSetup {
        /// The path being created when it failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetError::SandboxSetup { path, source } => write!(
                f,
                "could not prepare the sandbox target file {}: {source}; \
                 set {TARGET_PATH_ENV} to choose a target explicitly",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TargetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TargetError::SandboxSetup { source, .. } => Some(source),
        }
    }
}

/// Resolves the active target from the process environment, creating and
/// seeding the sandbox file if that is what the policy selects.
///
/// # Errors
///
/// Returns [`TargetError::SandboxSetup`] if the sandbox path had to be
/// created and could not be. An env-var-provided path is never created here:
/// a target that does not exist yet is a perfectly valid first-save target
/// and [`fstab_core::writer::write_document`] handles it.
pub fn active_target() -> Result<ActiveTarget, TargetError> {
    resolve(
        env::var_os(TARGET_PATH_ENV).as_deref(),
        &default_sandbox_dir(),
    )
}

/// The env-independent core of [`active_target`], parameterized on the raw
/// env value and the directory the sandbox would live in.
///
/// Split out so tests can exercise every branch of the policy without
/// mutating process-global environment state (which races with other tests
/// in the same process).
fn resolve(env_value: Option<&OsStr>, sandbox_dir: &Path) -> Result<ActiveTarget, TargetError> {
    match env_value.filter(|value| !is_blank(value)) {
        Some(value) => {
            let path = PathBuf::from(value);
            Ok(ActiveTarget {
                is_real: is_real_fstab(&path),
                path,
                source: TargetSource::Environment,
            })
        }
        None => {
            let path = sandbox_dir.join(SANDBOX_FILE_NAME);
            ensure_sandbox_file(&path)?;
            // Belt and braces: the sandbox path is constructed from a data
            // dir and a fixed file name and so cannot be `/etc/fstab`, but
            // the flag is computed the same way for both branches rather
            // than hardcoded `false`, so no future change to the sandbox
            // location can quietly produce a "sandbox" that is the real file
            // with a calm green banner over it.
            Ok(ActiveTarget {
                is_real: is_real_fstab(&path),
                path,
                source: TargetSource::SandboxDefault,
            })
        }
    }
}

/// Treats an all-whitespace (or empty) env value as "not set", so
/// `RUSTAB_TARGET_PATH=` from a stray line in a shell profile falls back to
/// the sandbox instead of resolving to the empty path.
fn is_blank(value: &OsStr) -> bool {
    value.as_encoded_bytes().iter().all(u8::is_ascii_whitespace)
}

/// Returns `true` if `path` is the real `/etc/fstab`, comparing literally
/// first and then by canonicalized path so that a symlink in either
/// direction is still recognized. A path that cannot be canonicalized (e.g.
/// it does not exist) is only matched literally.
fn is_real_fstab(path: &Path) -> bool {
    is_same_file(path, Path::new(REAL_FSTAB_PATH))
}

/// Returns `true` if `path` and `other` name the same file: either the same
/// literal path, or two paths that canonicalize to one destination (so a
/// symlink in either direction still counts). A path that cannot be
/// canonicalized — most often because it does not exist — is only matched
/// literally.
///
/// The generalization of [`is_real_fstab`], exposed because the elevated
/// writer has to apply the identical test to a target path it received as
/// *data* rather than resolved itself (see [`crate::privileged`]). Using one
/// implementation for both means the check that decides "this is the real
/// `/etc/fstab`, escalate" and the check that decides "I am allowed to write
/// this" can never disagree.
pub(crate) fn is_same_file(path: &Path, other: &Path) -> bool {
    if path == other {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(other)) {
        (Ok(resolved), Ok(resolved_other)) => resolved == resolved_other,
        _ => false,
    }
}

/// Creates the sandbox file (and its parent directory) with [`SANDBOX_SEED`]
/// if it does not exist yet. A file that already exists is left completely
/// alone, including an empty one.
fn ensure_sandbox_file(path: &Path) -> Result<(), TargetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| TargetError::SandboxSetup {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::write(path, SANDBOX_SEED).map_err(|source| TargetError::SandboxSetup {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(source) => Err(TargetError::SandboxSetup {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The directory the default sandbox file lives in:
/// `$XDG_DATA_HOME/rustab`, else `$HOME/.local/share/rustab`, else
/// `<temp dir>/rustab`.
///
/// Resolved with plain XDG rules rather than Tauri's path API so that target
/// resolution needs no `AppHandle` — the elevated helper subprocess (which
/// never starts a Tauri runtime) and the unit tests can both use it.
fn default_sandbox_dir() -> PathBuf {
    let base = non_empty_env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            non_empty_env("HOME").map(|home| {
                let mut path = PathBuf::from(home);
                path.push(".local");
                path.push("share");
                path
            })
        })
        .unwrap_or_else(env::temp_dir);
    base.join(SANDBOX_DIR_NAME)
}

/// Reads an environment variable, treating a blank value as absent.
fn non_empty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !is_blank(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn env_override_is_used_verbatim() {
        let dir = TempDir::new("target-env");
        let explicit = dir.join("somewhere/else/fstab.custom");
        let sandbox_dir = dir.join("sandbox");

        let target = resolve(Some(explicit.as_os_str()), &sandbox_dir)
            .expect("an explicit path must resolve");

        assert_eq!(target.path, explicit);
        assert_eq!(target.source, TargetSource::Environment);
        assert!(!target.is_real);
        // An env-provided target is never created on the user's behalf.
        assert!(!explicit.exists());
        assert!(!sandbox_dir.exists(), "the sandbox must not be touched");
    }

    #[test]
    fn blank_env_override_falls_back_to_the_sandbox() {
        let dir = TempDir::new("target-blank");
        let sandbox_dir = dir.join("data/rustab");

        for blank in ["", "   ", "\t"] {
            let target = resolve(Some(OsStr::new(blank)), &sandbox_dir)
                .expect("a blank override must fall back to the sandbox");
            assert_eq!(target.source, TargetSource::SandboxDefault);
            assert_eq!(target.path, sandbox_dir.join(SANDBOX_FILE_NAME));
        }
    }

    #[test]
    fn default_target_is_the_sandbox_and_is_never_the_real_fstab() {
        let dir = TempDir::new("target-default");
        let sandbox_dir = dir.join("data/rustab");

        let target = resolve(None, &sandbox_dir).expect("the sandbox must resolve");

        assert_eq!(target.path, sandbox_dir.join(SANDBOX_FILE_NAME));
        assert_eq!(target.source, TargetSource::SandboxDefault);
        assert!(!target.is_real, "the default target must never be real");
        assert_ne!(target.path, Path::new(REAL_FSTAB_PATH));
        assert!(
            !target.path.starts_with("/etc"),
            "the default target must not live in /etc"
        );
    }

    /// The build-profile half of the SPEC §7 guarantee: whatever the profile,
    /// resolving with no override must not yield `/etc/fstab`. Asserted via
    /// the real `default_sandbox_dir()` too, not just an injected one, so a
    /// mistake in that function is caught as well.
    #[test]
    fn real_default_sandbox_dir_is_never_the_real_fstab() {
        let path = default_sandbox_dir().join(SANDBOX_FILE_NAME);
        assert_ne!(path, Path::new(REAL_FSTAB_PATH));
        assert!(!is_real_fstab(&path));
        assert!(path.ends_with(SANDBOX_FILE_NAME));
    }

    #[test]
    fn sandbox_file_is_created_with_valid_seed_content_once() {
        let dir = TempDir::new("target-seed");
        let sandbox_dir = dir.join("data/rustab");

        let target = resolve(None, &sandbox_dir).expect("the sandbox must resolve");
        assert!(target.path.exists(), "the sandbox file must be created");

        let seeded = fs::read_to_string(&target.path).expect("the sandbox must be readable");
        assert_eq!(seeded, SANDBOX_SEED);

        // The seed must be loadable and must not produce a single finding,
        // so a first run never opens on a file the user has to fix.
        let doc = fstab_core::parser::parse(&seeded);
        assert_eq!(doc.to_string(), seeded, "the seed must round-trip");
        assert_eq!(doc.entries().count(), 0);
        let report = fstab_core::validate::validate(&doc);
        assert!(report.is_ok(), "the seed must validate: {report:?}");

        // A second resolution must not overwrite user edits.
        fs::write(&target.path, "# edited by the user\n").expect("write");
        let again = resolve(None, &sandbox_dir).expect("the sandbox must resolve again");
        assert_eq!(again.path, target.path);
        assert_eq!(
            fs::read_to_string(&again.path).expect("read"),
            "# edited by the user\n",
            "an existing sandbox file must be left untouched"
        );
    }

    #[test]
    fn is_real_fstab_matches_the_literal_path_only() {
        assert!(is_real_fstab(Path::new(REAL_FSTAB_PATH)));
        assert!(!is_real_fstab(Path::new("/etc/fstab.rustab-backup")));
        assert!(!is_real_fstab(Path::new("/tmp/fstab")));
        assert!(!is_real_fstab(Path::new("fstab")));
    }

    /// The canonicalized half of `is_real`: a symlink pointing at the real
    /// file must still be flagged real. Skipped (not failed) if the machine
    /// genuinely has no `/etc/fstab` to link to.
    #[test]
    fn is_real_fstab_follows_a_symlink_to_the_real_file() {
        if fs::canonicalize(REAL_FSTAB_PATH).is_err() {
            return;
        }
        let dir = TempDir::new("target-symlink");
        let link = dir.join("sneaky-sandbox-name");
        std::os::unix::fs::symlink(REAL_FSTAB_PATH, &link).expect("symlink");

        assert!(
            is_real_fstab(&link),
            "a symlink to /etc/fstab must be reported as the real target"
        );

        let decoy = dir.join("decoy");
        fs::write(&decoy, "# not fstab\n").expect("write");
        assert!(!is_real_fstab(&decoy));
    }

    #[test]
    fn target_error_message_names_the_env_var_and_path() {
        let err = TargetError::SandboxSetup {
            path: PathBuf::from("/nope/rustab"),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        let message = err.to_string();
        assert!(message.contains("/nope/rustab"));
        assert!(message.contains(TARGET_PATH_ENV));
    }
}
