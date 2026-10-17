//! Active-target resolution - see `docs/SPEC.md` §7 and `docs/API_CONTRACT.md`.
//!
//! Exactly one file is "the active target" at any moment, and this module is
//! the only place that decides which one. Every command that reads or writes
//! fstab content goes through [`active_target`]; no command accepts a path
//! from the frontend, so a compromised or buggy UI can never redirect a save
//! at a file of its choosing (the one exception, `restore_backup`, takes a
//! path but only honours it if it is one of the backups
//! [`fstab_core::writer::list_backups`] reports for *this* target - see
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
//!    neither variable is set). The first time it is resolved in a given
//!    process it is refreshed from the real `/etc/fstab` - a literal
//!    byte-for-byte copy - so every launch starts from current reality; if
//!    `/etc/fstab` cannot be read it falls back to a harmless comment-only
//!    seed, so a fresh checkout still has something safe and valid to load.
//!
//! ## Sandbox refresh
//!
//! The sandbox is a practice ground, not a persisted fixture. It is
//! re-synced from `/etc/fstab` **once per process launch** (see
//! [`SandboxRefreshState`]), which deliberately discards whatever the
//! previous session left there. Within one run, every later resolution -
//! and [`active_target`] is called fresh by every command - leaves the file
//! completely alone, so the user's own edits and saves survive the session
//! that made them. Because two commands resolve the target concurrently on
//! every launch, [`SandboxRefreshState`] is a completion *barrier*: the
//! resolutions that do not perform the refresh wait for the one that does,
//! so no command can read the target while the refresh is still in flight.
//!
//! The refresh is a plain copy of bytes, never a parse and re-serialize
//! round trip: a copy cannot drift from its source, a round trip can
//! (`docs/SPEC.md` §5). It is published with the same same-directory
//! temp-file-plus-`rename` swap `fstab_core::writer` uses for real saves, so
//! that any reader outside this process's barrier - a second Rustab launching
//! at the same moment, say - still sees whole content rather than a
//! mid-truncate file (see [`refresh_sandbox_from`]). It is also strictly
//! best-effort - an unreadable `/etc/fstab` (missing, or no permission)
//! leaves target resolution working on the seed/previous content rather than
//! failing, the same soft-failure stance this project takes towards absent
//! `lsblk`/`findmnt`.
//!
//! Rule 2 deliberately applies to **release builds too**. `docs/SPEC.md` §7
//! only mandates the sandbox default for debug builds, but defaulting a
//! release build to `/etc/fstab` would mean the app's very first screen is
//! already pointed at the file whose corruption §2.1 calls the one
//! unacceptable outcome - before the user has agreed to anything. Making
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
//! with the env var naming the destination) - in both directions the answer
//! has to be "yes, this is the real thing", because the banner driven by
//! this flag is the user's only warning (`docs/UI_DESIGN.md` §2).

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Environment variable that overrides the target file. Documented in
/// `docs/API_CONTRACT.md` and `.env.example`.
pub const TARGET_PATH_ENV: &str = "RUSTAB_TARGET_PATH";

/// The one path that counts as "the real thing" (see [`ActiveTarget::is_real`]).
pub const REAL_FSTAB_PATH: &str = "/etc/fstab";

/// Directory, relative to the user's data dir, holding the sandbox target.
const SANDBOX_DIR_NAME: &str = "rustab";

/// File name of the default sandbox target.
const SANDBOX_FILE_NAME: &str = "fstab.sandbox";

/// Content the sandbox file falls back to when the real `/etc/fstab` could
/// not be read: comments only, so it parses cleanly, validates without a
/// single finding, and mounts nothing if a user ever copied it somewhere real
/// by accident.
const SANDBOX_SEED: &str = "\
# Rustab development sandbox - NOT your real /etc/fstab.
#
# Rustab normally refreshes this sandbox from your real /etc/fstab once per
# launch, so it starts out as an exact copy of the real file. You are seeing
# this blank template instead because /etc/fstab could not be read the last
# time Rustab started - it is missing, or not readable by this user.
#
# Editing and saving here never touches the real /etc/fstab. To point Rustab
# at the real file instead, start it with:
#
#     RUSTAB_TARGET_PATH=/etc/fstab rustab
#
# Add entries below in normal fstab syntax, e.g.:
# UUID=00000000-0000-0000-0000-000000000000 /mnt/example ext4 noauto 0 2
";

/// Completion barrier for the once-per-process sandbox refresh: it both
/// elects the one resolution that refreshes the file from the real
/// `/etc/fstab` *and* holds every concurrent resolution back until that
/// refresh has actually finished.
///
/// Exactly one instance lives for the lifetime of the app process (inside
/// [`active_target`]), which is what makes "refresh once per launch" mean
/// what it says. It is a *parameter* of [`resolve`] rather than a static
/// [`resolve`] reads itself, for the same reason `env_value` and
/// `sandbox_dir` are: a module-level barrier would be shared by every test in
/// the binary, and "the first resolution" would then depend on test
/// execution order. Each test builds its own, and one fresh instance *is*
/// one simulated process launch.
///
/// ## Why a barrier and not just a latch
///
/// Electing a writer is only half the problem. The frontend fires
/// `get_target_info` and `list_entries` as two independent tasks on mount, so
/// two commands resolve the target simultaneously on every launch, and each
/// one then reads the target's bytes *separately*, after resolution returns
/// (see `crate::commands::list_entries`). A plain "did I win the race?" latch
/// let the loser return immediately - while the winner was still reading
/// `/etc/fstab`, writing its temp file and renaming - and hand the frontend
/// the pre-refresh content (an empty entries table on a machine with a
/// perfectly populated `/etc/fstab`).
///
/// [`OnceLock::get_or_init`] solves both halves with one primitive: the
/// closure runs for the first caller only, every other caller *blocks* until
/// it has fully returned, and all of them then get the same outcome. So no
/// resolution can return - and therefore no command can read the target -
/// before the refresh (or its seed fallback) has completed.
struct SandboxRefreshState(OnceLock<Result<(), SandboxSetupFailure>>);

impl SandboxRefreshState {
    /// A state that has not refreshed anything yet - i.e. a fresh process.
    ///
    /// `const` so the production instance can be a plain `static` with no
    /// lazy-initialization machinery.
    const fn new() -> Self {
        SandboxRefreshState(OnceLock::new())
    }

    /// Runs `setup` exactly once for the lifetime of this state, blocking
    /// every concurrent caller until it has returned, and hands all callers
    /// - the one that ran it included - the same outcome.
    fn ensure_once<F>(&self, setup: F) -> Result<(), SandboxSetupFailure>
    where
        F: FnOnce() -> Result<(), SandboxSetupFailure>,
    {
        self.0.get_or_init(setup).clone()
    }

    /// Whether the one-time setup has already run (and so will never run
    /// again for this state).
    ///
    /// Test-only: production code asks [`ensure_once`](Self::ensure_once) to
    /// do the right thing rather than inspecting the barrier.
    #[cfg(test)]
    fn has_already_run(&self) -> bool {
        self.0.get().is_some()
    }
}

/// A clonable record of the one failure the once-per-process sandbox setup
/// can report: [`seed_sandbox_file_if_missing`] could not create or stat the
/// sandbox file.
///
/// [`SandboxRefreshState`] has to hand the *same* cached outcome to every
/// caller after the first, and `io::Error` is not [`Clone`], so the error is
/// kept behind an [`Arc`] and a fresh equivalent `io::Error` is rebuilt per
/// caller by [`as_io_error`](Self::as_io_error). This is purely internal
/// bookkeeping: what leaves this module is still
/// [`TargetError::SandboxSetup`] with an `io::Error` source, exactly as
/// before.
#[derive(Debug, Clone)]
struct SandboxSetupFailure(Arc<io::Error>);

impl SandboxSetupFailure {
    /// An `io::Error` equivalent to the one that failed: same
    /// [`io::ErrorKind`], same message. A copy rather than the original
    /// because the original has to stay in the barrier for the next caller.
    fn as_io_error(&self) -> io::Error {
        io::Error::new(self.0.kind(), self.0.to_string())
    }
}

impl From<io::Error> for SandboxSetupFailure {
    fn from(error: io::Error) -> Self {
        SandboxSetupFailure(Arc::new(error))
    }
}

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

/// Resolves the active target from the process environment, creating,
/// refreshing or seeding the sandbox file if that is what the policy selects.
///
/// The first call in a process refreshes the sandbox from the real
/// `/etc/fstab`; later calls leave it alone (see the module docs). The
/// refresh source is deliberately the hardcoded [`REAL_FSTAB_PATH`] and not
/// configurable - [`TARGET_PATH_ENV`] chooses the *target*, never where a
/// sandbox is seeded from.
///
/// # Errors
///
/// Returns [`TargetError::SandboxSetup`] if the sandbox path had to be
/// created and could not be. An env-var-provided path is never created here:
/// a target that does not exist yet is a perfectly valid first-save target
/// and [`fstab_core::writer::write_document`] handles it.
pub fn active_target() -> Result<ActiveTarget, TargetError> {
    /// The one real process-lifetime barrier: whichever command resolves the
    /// target first does the per-launch sandbox refresh, and the others wait
    /// for it.
    static SANDBOX_REFRESH_STATE: SandboxRefreshState = SandboxRefreshState::new();

    resolve(
        env::var_os(TARGET_PATH_ENV).as_deref(),
        &default_sandbox_dir(),
        Path::new(REAL_FSTAB_PATH),
        &SANDBOX_REFRESH_STATE,
    )
}

/// The env-independent core of [`active_target`], parameterized on the raw
/// env value, the directory the sandbox would live in, the file the sandbox
/// is refreshed from, and the once-per-process refresh barrier.
///
/// Split out so tests can exercise every branch of the policy without
/// mutating process-global environment state (which races with other tests
/// in the same process) - and, for the same reason, without reading or
/// writing the machine's real `/etc/fstab` or a shared refresh barrier.
/// Production passes `Path::new(REAL_FSTAB_PATH)` and the single static
/// barrier in [`active_target`]; tests pass a temp file and a barrier of
/// their own.
fn resolve(
    env_value: Option<&OsStr>,
    sandbox_dir: &Path,
    real_fstab_path: &Path,
    refresh_state: &SandboxRefreshState,
) -> Result<ActiveTarget, TargetError> {
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
            ensure_sandbox_file(&path, real_fstab_path, refresh_state)?;
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
/// canonicalized - most often because it does not exist - is only matched
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

/// Prepares the sandbox file (and its parent directory) for use.
///
/// The first resolution of a process replaces the sandbox with a
/// byte-for-byte copy of `real_fstab_path`, deliberately discarding whatever
/// a previous session left there; if that copy could not be made it falls
/// back to the long-standing behaviour instead - create the file with
/// [`SANDBOX_SEED`] if it does not exist yet, and leave a file that does
/// exist completely alone, including an empty one.
///
/// Every *other* resolution does not redo any of that, and - this is the
/// point of [`SandboxRefreshState`] being a barrier rather than a latch -
/// does not return until the first one has finished, so no command can read
/// the target mid-refresh.
///
/// "Does not redo any of that" now includes the existence check: the one-time
/// work runs exactly once per process, where the previous latch-based version
/// still re-ran [`seed_sandbox_file_if_missing`] (a no-op once the file
/// exists) on every single resolution. The one narrow consequence: if
/// something *outside* Rustab deletes the sandbox file mid-session - the app
/// itself never does - it is no longer silently reseeded on the next
/// resolution, and the following read simply reports a missing file, which
/// `crate::commands::list_entries` already renders as an empty, non-existent
/// target rather than an error.
///
/// The parent directory is created on every call, outside the barrier: it is
/// an idempotent no-op once the directory exists, and keeping it unguarded
/// means a resolution still reports a genuinely unusable sandbox directory
/// even if the one-time refresh already happened.
fn ensure_sandbox_file(
    path: &Path,
    real_fstab_path: &Path,
    refresh_state: &SandboxRefreshState,
) -> Result<(), TargetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| TargetError::SandboxSetup {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    refresh_state
        .ensure_once(|| {
            if refresh_sandbox_from(path, real_fstab_path) {
                Ok(())
            } else {
                seed_sandbox_file_if_missing(path).map_err(SandboxSetupFailure::from)
            }
        })
        .map_err(|failure| TargetError::SandboxSetup {
            path: path.to_path_buf(),
            source: failure.as_io_error(),
        })
}

/// Copies `real_fstab_path` over the sandbox file verbatim, returning whether
/// it worked.
///
/// A literal byte copy, never a parse-and-reserialize round trip: the copy is
/// then incapable of differing from its source, which is exactly the property
/// `docs/SPEC.md` §5 cares about.
///
/// ## Why this is a temp file plus `rename`, not an `fs::write`
///
/// Writing the bytes straight onto `sandbox_path` would be an
/// open-truncate-write, which is *not* atomic with respect to a concurrent
/// reader of the same path. Within this process [`SandboxRefreshState`]
/// already keeps the other startup resolution from reading until this
/// function has returned, but that barrier only binds callers that share it:
/// a second Rustab process launching at the same moment, or anything else
/// reading the sandbox, has no such ordering. With a plain write such a
/// reader can land between the truncate and the completed write and see an
/// empty document.
///
/// So the bytes go to a sibling temp file in the sandbox's own directory and
/// are then `rename`d onto it, the same way [`fstab_core::writer`] publishes
/// real saves (and for the same reason - `rename(2)` is atomic *within one
/// filesystem*, which is precisely what "a sibling in the same directory"
/// guarantees). A concurrent reader therefore observes either the complete
/// previous content or the complete new content, never a partial file.
///
/// This is deliberately *only* the atomicity half of a real save: no
/// validation, no backup, no permission carry-over, no `fsync`. The refresh is
/// a convenience copy into a scratch file, not a durable publish - if a crash
/// mid-refresh loses it, the next launch simply refreshes again. It also means
/// a sandbox path that is somehow a symlink gets replaced by a regular file
/// rather than written *through*, which for a sandbox is the safer of the two.
///
/// Every step is best-effort and failure is silent by design. An unreadable
/// `/etc/fstab` is the expected case on a machine that has none, and a failed
/// copy leaves the sandbox exactly as it was - the caller falls back to the
/// seed/leave-alone path, which reports a genuine setup failure if there is
/// one. Target resolution must not die because a convenience refresh could not
/// happen.
fn refresh_sandbox_from(sandbox_path: &Path, real_fstab_path: &Path) -> bool {
    let Ok(bytes) = fs::read(real_fstab_path) else {
        return false;
    };
    let Some(temp_path) = refresh_temp_path(sandbox_path) else {
        return false;
    };
    if fs::write(&temp_path, bytes).is_err() {
        // The temp file may or may not exist (create could have failed, or a
        // partial write could have left it behind); removing it is best-effort
        // and its own error is ignored so it cannot mask the real cause.
        let _ = fs::remove_file(&temp_path);
        return false;
    }
    if fs::rename(&temp_path, sandbox_path).is_err() {
        let _ = fs::remove_file(&temp_path);
        return false;
    }
    true
}

/// The sibling temp file [`refresh_sandbox_from`] stages the copy in:
/// `.<sandbox file name>.rustab-refresh.tmp.<pid>`, in the sandbox's own
/// directory.
///
/// Same directory, so the `rename` onto the sandbox cannot cross a filesystem
/// boundary and is therefore atomic. Leading dot so a stray leftover stays out
/// of the way. The `.rustab-refresh.tmp.` infix is deliberately distinct from
/// both naming schemes `fstab_core::writer` owns - the backup grammar
/// (`<name>.rustab-backup.<secs>.<nanos>`, which `list_backups` scans for) and
/// that module's own save temp file (`.<name>.rustab.tmp.<pid>`) - so this file
/// can never be mistaken for a backup the user might restore, nor for a save
/// in flight. The pid keeps two Rustab processes launching at the same instant
/// from staging into the same temp path.
///
/// `None` only for a path with no final file-name component, which the
/// constructed sandbox path never is; returning it rather than panicking keeps
/// the whole refresh on its one best-effort failure route.
fn refresh_temp_path(sandbox_path: &Path) -> Option<PathBuf> {
    let file_name = sandbox_path.file_name()?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".rustab-refresh.tmp.{}", std::process::id()));
    Some(sandbox_path.with_file_name(temp_name))
}

/// Creates the sandbox file with [`SANDBOX_SEED`] if it does not exist yet,
/// leaving an existing one (even an empty one) completely alone.
///
/// Reports the bare `io::Error`; its caller is the one that knows the path
/// belongs in a [`TargetError::SandboxSetup`] (and has to park the failure in
/// the refresh barrier first, where a non-`Clone` error cannot live - see
/// [`SandboxSetupFailure`]).
fn seed_sandbox_file_if_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => fs::write(path, SANDBOX_SEED),
        Err(err) => Err(err),
    }
}

/// The directory the default sandbox file lives in:
/// `$XDG_DATA_HOME/rustab`, else `$HOME/.local/share/rustab`, else
/// `<temp dir>/rustab`.
///
/// Resolved with plain XDG rules rather than Tauri's path API so that target
/// resolution needs no `AppHandle` - the elevated helper subprocess (which
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// A realistic refresh source: content a parse-and-reserialize round trip
    /// could plausibly normalize (tabs, runs of spaces, no trailing newline),
    /// so a test asserting equality really is asserting a literal byte copy.
    const REAL_FSTAB_SAMPLE: &str = "\
# /etc/fstab: static file system information.
UUID=11111111-2222-3333-4444-555555555555\t/\text4\trw,relatime\t0 1
LABEL=data   /mnt/data   xfs   defaults,nofail   0   2
/swapfile none swap defaults 0 0";

    /// A `real_fstab_path` that is guaranteed not to exist, for the tests
    /// whose subject is not the refresh at all.
    fn no_real_fstab(dir: &TempDir) -> PathBuf {
        dir.join("not-a-real-fstab")
    }

    #[test]
    fn env_override_is_used_verbatim() {
        let dir = TempDir::new("target-env");
        let explicit = dir.join("somewhere/else/fstab.custom");
        let sandbox_dir = dir.join("sandbox");

        let target = resolve(
            Some(explicit.as_os_str()),
            &sandbox_dir,
            &no_real_fstab(&dir),
            &SandboxRefreshState::new(),
        )
        .expect("an explicit path must resolve");

        assert_eq!(target.path, explicit);
        assert_eq!(target.source, TargetSource::Environment);
        assert!(!target.is_real);
        // An env-provided target is never created on the user's behalf.
        assert!(!explicit.exists());
        assert!(!sandbox_dir.exists(), "the sandbox must not be touched");
    }

    /// The env-override branch must not touch the sandbox *even on a fresh
    /// process with a perfectly readable refresh source*: the per-launch
    /// refresh is part of choosing the sandbox, not something that happens on
    /// the side. It also proves the refresh barrier is left untriggered, so a
    /// later resolution that does fall through to the sandbox still gets its
    /// refresh.
    #[test]
    fn an_env_override_never_refreshes_the_sandbox() {
        let dir = TempDir::new("target-env-no-refresh");
        let explicit = dir.join("fstab.custom");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");
        let refresh_state = SandboxRefreshState::new();

        resolve(
            Some(explicit.as_os_str()),
            &sandbox_dir,
            &real_fstab,
            &refresh_state,
        )
        .expect("an explicit path must resolve");

        assert!(!sandbox_dir.exists(), "the sandbox must not be touched");
        assert!(
            !refresh_state.has_already_run(),
            "the refresh must still be pending"
        );
        // And the proof that "pending" means it is still available: a later
        // sandbox resolution sharing this state does get its refresh.
        let target = resolve(None, &sandbox_dir, &real_fstab, &refresh_state)
            .expect("the sandbox must resolve");
        assert_eq!(
            fs::read(&target.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "a sandbox resolution after an env-override one must still refresh"
        );
    }

    #[test]
    fn blank_env_override_falls_back_to_the_sandbox() {
        let dir = TempDir::new("target-blank");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = no_real_fstab(&dir);
        let refresh_state = SandboxRefreshState::new();

        for blank in ["", "   ", "\t"] {
            let target = resolve(
                Some(OsStr::new(blank)),
                &sandbox_dir,
                &real_fstab,
                &refresh_state,
            )
            .expect("a blank override must fall back to the sandbox");
            assert_eq!(target.source, TargetSource::SandboxDefault);
            assert_eq!(target.path, sandbox_dir.join(SANDBOX_FILE_NAME));
        }
    }

    #[test]
    fn default_target_is_the_sandbox_and_is_never_the_real_fstab() {
        let dir = TempDir::new("target-default");
        let sandbox_dir = dir.join("data/rustab");

        let target = resolve(
            None,
            &sandbox_dir,
            &no_real_fstab(&dir),
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve");

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

    /// The core new behaviour: a fresh process starts the sandbox off as an
    /// exact copy of the real file. Asserted on bytes, because the copy must
    /// be a copy and not a parse/reserialize round trip.
    #[test]
    fn the_first_resolution_of_a_process_copies_the_real_fstab_into_the_sandbox() {
        let dir = TempDir::new("target-refresh-first");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let target = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("the sandbox must resolve");

        assert_eq!(target.source, TargetSource::SandboxDefault);
        assert_eq!(
            fs::read(&target.path).expect("the sandbox must be readable"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "the sandbox must be a byte-for-byte copy of the real file"
        );
    }

    /// Every resolution after the first in the same process must leave the
    /// file alone - [`active_target`] runs per command, so anything else
    /// would throw away a save the moment the next `list_entries` arrived.
    /// This also carries the (still true) half of the retired
    /// `sandbox_file_is_created_with_valid_seed_content_once` test that
    /// asserted an existing sandbox file is never overwritten.
    #[test]
    fn a_later_resolution_in_the_same_process_leaves_the_users_edits_alone() {
        let dir = TempDir::new("target-refresh-same-process");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");
        let refresh_state = SandboxRefreshState::new();

        let target =
            resolve(None, &sandbox_dir, &real_fstab, &refresh_state).expect("first resolution");

        // The user edits and saves in this session.
        fs::write(&target.path, "# edited by the user\n").expect("write");

        for _ in 0..3 {
            let again = resolve(None, &sandbox_dir, &real_fstab, &refresh_state)
                .expect("the sandbox must resolve again");
            assert_eq!(again.path, target.path);
            assert_eq!(
                fs::read_to_string(&again.path).expect("read"),
                "# edited by the user\n",
                "an in-session resolution must leave the sandbox untouched"
            );
        }
    }

    /// The other half of "once per launch": a genuinely new process - a brand
    /// new refresh state over the same sandbox directory - re-syncs, throwing
    /// away whatever the previous session left behind. That discard is the
    /// intended behaviour, not a side effect.
    #[test]
    fn a_new_process_re_copies_the_real_fstab_over_the_previous_sessions_sandbox() {
        let dir = TempDir::new("target-refresh-new-process");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, "# first launch\n").expect("write");

        let first = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("first process");
        fs::write(&first.path, "# left over from the previous session\n").expect("write");

        // The real file has meanwhile changed, too.
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let second = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("second process");

        assert_eq!(second.path, first.path);
        assert_eq!(
            fs::read(&second.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "a new process must re-copy the current real file"
        );
    }

    /// Fallback sub-case A, inherited from the retired
    /// `sandbox_file_is_created_with_valid_seed_content_once`: with no
    /// readable `/etc/fstab` the blank seed is still what a first run gets,
    /// and it must still parse and validate without a single finding.
    #[test]
    fn an_unreadable_real_fstab_falls_back_to_the_valid_seed() {
        let dir = TempDir::new("target-seed");
        let sandbox_dir = dir.join("data/rustab");

        let target = resolve(
            None,
            &sandbox_dir,
            &no_real_fstab(&dir),
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve even with no real fstab");
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
    }

    /// Fallback sub-case B: an unreadable `/etc/fstab` must not cost the user
    /// the sandbox they already have. Blanking a populated sandbox back to
    /// the seed would be a data loss the refresh was never meant to cause.
    #[test]
    fn an_unreadable_real_fstab_leaves_an_existing_sandbox_alone() {
        let dir = TempDir::new("target-seed-existing");
        let sandbox_dir = dir.join("data/rustab");
        let sandbox_file = sandbox_dir.join(SANDBOX_FILE_NAME);
        fs::create_dir_all(&sandbox_dir).expect("mkdir");
        fs::write(&sandbox_file, "# from an earlier session\n").expect("write");

        let target = resolve(
            None,
            &sandbox_dir,
            &no_real_fstab(&dir),
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve even with no real fstab");

        assert_eq!(target.path, sandbox_file);
        assert_eq!(
            fs::read_to_string(&sandbox_file).expect("read"),
            "# from an earlier session\n",
            "an existing sandbox must not be replaced with the blank seed"
        );
    }

    /// Lists the entry names of a directory, sorted, so a test can assert
    /// about what the refresh left behind and not just about the file it was
    /// aiming at.
    fn entry_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("the directory must be readable")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The refresh stages its copy in a sibling temp file and `rename`s it
    /// into place; a successful refresh must leave no trace of that staging
    /// behind, or the sandbox directory would accumulate one stray file per
    /// launch.
    #[test]
    fn a_successful_refresh_leaves_no_temp_file_behind() {
        let dir = TempDir::new("target-refresh-no-temp");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let target = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("the sandbox must resolve");

        assert_eq!(
            entry_names(&sandbox_dir),
            vec![SANDBOX_FILE_NAME.to_owned()]
        );
        assert_eq!(
            fs::read(&target.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes()
        );
    }

    /// The failure path of the same staging: when the swap cannot happen the
    /// temp file must be cleaned up, `refresh_sandbox_from` must report
    /// `false` (so the caller falls back to seed/leave-alone), and the
    /// sandbox path itself must be untouched.
    ///
    /// The `rename` is made to fail by pointing the sandbox path at a
    /// directory - renaming a regular file onto a directory is an error on
    /// any Unix, and unlike a permission trick it behaves the same whether
    /// the suite runs as root or not.
    #[test]
    fn a_failed_refresh_cleans_up_its_temp_file() {
        let dir = TempDir::new("target-refresh-cleanup");
        let sandbox_dir = dir.join("data/rustab");
        let sandbox_path = sandbox_dir.join(SANDBOX_FILE_NAME);
        fs::create_dir_all(&sandbox_path).expect("mkdir");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        assert!(
            !refresh_sandbox_from(&sandbox_path, &real_fstab),
            "a refresh that cannot be swapped into place must report failure"
        );

        assert_eq!(
            entry_names(&sandbox_dir),
            vec![SANDBOX_FILE_NAME.to_owned()],
            "the staging temp file must not survive a failed refresh"
        );
        assert!(
            sandbox_path.is_dir(),
            "a failed refresh must leave the sandbox path as it found it"
        );
    }

    /// The temp file is a sibling of the sandbox, which is what makes the
    /// `rename` same-filesystem and therefore atomic, and its name must not
    /// be mistakable for one of `fstab_core::writer`'s backups.
    #[test]
    fn the_refresh_temp_file_is_a_sibling_that_is_not_a_backup_name() {
        let sandbox_path = Path::new("/data/rustab").join(SANDBOX_FILE_NAME);
        let temp_path = refresh_temp_path(&sandbox_path).expect("a named file has a temp sibling");

        assert_eq!(temp_path.parent(), sandbox_path.parent());
        let name = temp_path
            .file_name()
            .expect("the temp path has a file name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(&format!(".{SANDBOX_FILE_NAME}.")));
        assert!(name.contains(".rustab-refresh.tmp."));
        assert!(name.ends_with(&std::process::id().to_string()));
        assert!(
            !name.contains(".rustab-backup."),
            "the temp file must not look like a backup: {name}"
        );
    }

    /// The barrier itself, first half: the work runs exactly once per state,
    /// however often it is asked. Two concurrent commands resolving at
    /// startup must not both decide they are the one doing the copy.
    #[test]
    fn the_refresh_runs_exactly_once_per_state() {
        let state = SandboxRefreshState::new();
        let runs = AtomicUsize::new(0);
        let count_one_run = || {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };

        assert!(!state.has_already_run());
        for _ in 0..6 {
            state
                .ensure_once(count_one_run)
                .expect("setup must succeed");
            assert!(state.has_already_run());
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the refresh must run once, not once per resolution"
        );

        // A separate state is a separate process and runs its own refresh.
        SandboxRefreshState::new()
            .ensure_once(count_one_run)
            .expect("setup must succeed");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// The barrier itself, second half - and the deterministic proof of the
    /// bug this whole mechanism exists to prevent: a caller that does *not*
    /// perform the refresh must not return until the caller that does has
    /// finished it. Electing one writer is not enough; the losers used to
    /// fall straight through and let their command read the target
    /// mid-refresh.
    ///
    /// Deterministic rather than luck-dependent: the one-time work sleeps a
    /// fixed 50ms before stamping its completion instant, and every thread
    /// stamps its own instant the moment its call returns. If any thread were
    /// allowed past the barrier early, its instant would land *before* the
    /// completion stamp, which no scheduling order can disguise.
    #[test]
    fn every_caller_waits_until_the_refresh_has_finished() {
        const THREADS: usize = 8;
        const WORK_DURATION: Duration = Duration::from_millis(50);

        let state = SandboxRefreshState::new();
        let runs = AtomicUsize::new(0);
        let finished_at: Mutex<Option<Instant>> = Mutex::new(None);
        let all_ready = Barrier::new(THREADS);

        let returned_at: Vec<Instant> = thread::scope(|scope| {
            let threads: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        // Maximize the overlap: nobody calls in until
                        // everybody is at the start line.
                        all_ready.wait();
                        state
                            .ensure_once(|| {
                                runs.fetch_add(1, Ordering::SeqCst);
                                thread::sleep(WORK_DURATION);
                                *finished_at.lock().expect("the stamp slot must lock") =
                                    Some(Instant::now());
                                Ok(())
                            })
                            .expect("setup must succeed");
                        Instant::now()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("no thread may panic"))
                .collect()
        });

        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "concurrent callers must not each run the refresh"
        );
        let finished = finished_at
            .lock()
            .expect("the stamp slot must lock")
            .expect("the refresh must have run");
        for (index, returned) in returned_at.iter().enumerate() {
            assert!(
                *returned >= finished,
                "caller {index} returned before the refresh had finished"
            );
        }
    }

    /// The regression test shaped like the actual bug: `get_target_info` and
    /// `list_entries` resolve the target concurrently on every launch, and
    /// each then reads the resolved path *separately*. Before the barrier,
    /// the resolution that did not perform the refresh could return while the
    /// other was still mid-copy, and its command handed the frontend the
    /// pre-refresh content - an empty entries table on a machine with a
    /// populated `/etc/fstab`.
    ///
    /// Honest about its own limits: this test alone would not have failed on
    /// every single run against the old code. The losing path was a single
    /// cheap `symlink_metadata` call, so whether it actually overtook the
    /// winner's read-write-rename depended on scheduling.
    /// `every_caller_waits_until_the_refresh_has_finished` is the
    /// deterministic proof; this one is the readable statement of what the
    /// user-visible bug was.
    #[test]
    fn concurrent_resolutions_all_read_the_complete_refreshed_sandbox() {
        const THREADS: usize = 8;

        let dir = TempDir::new("target-refresh-concurrent");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");
        let refresh_state = SandboxRefreshState::new();
        let all_ready = Barrier::new(THREADS);

        thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    all_ready.wait();
                    let target = resolve(None, &sandbox_dir, &real_fstab, &refresh_state)
                        .expect("every concurrent resolution must succeed");
                    // Exactly what `commands::list_entries` does: its own
                    // read of the path, after resolution returned.
                    let content = fs::read(&target.path).expect("the sandbox must be readable");
                    assert_eq!(
                        content,
                        REAL_FSTAB_SAMPLE.as_bytes(),
                        "a concurrent resolution saw {} bytes instead of the refreshed file",
                        content.len()
                    );
                });
            }
        });

        // And the refresh really was the one-per-launch copy, not a seed.
        assert_eq!(
            entry_names(&sandbox_dir),
            vec![SANDBOX_FILE_NAME.to_owned()],
            "the concurrent refresh must leave no staging temp file behind"
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
