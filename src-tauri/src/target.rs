//! Active-target resolution - see `docs/SPEC.md` §7 and `docs/API_CONTRACT.md`.
//!
//! Exactly one file is "the active target" at any moment, and this module
//! is the only place that decides which one. Every command that reads or
//! writes fstab content goes through [`active_target`]; no command accepts
//! a path from the frontend (the one exception, `restore_backup`, only
//! honours a path if it's one of the backups
//! [`fstab_core::writer::list_backups`] reports for *this* target - see
//! `crate::backups`).
//!
//! ## The policy
//!
//! 1. If `RUSTAB_TARGET_PATH` is set to a non-empty value, that's the
//!    target, verbatim. It's written outright - whatever path it names is
//!    the only file a save touches (elevated if it happens to *be*
//!    `/etc/fstab`, in-process otherwise) - the one way to run Rustab
//!    without it writing the real `/etc/fstab`.
//! 2. Otherwise the target is the per-user working draft,
//!    `$XDG_DATA_HOME/rustab/fstab.draft` (falling back through
//!    `$HOME/.local/share/rustab/fstab.draft` to a temp-dir path). The GUI
//!    reads and edits this file, but a save writes the real `/etc/fstab`
//!    and then updates the draft to match - the draft's job is to keep
//!    in-progress edits across a restart or crash, not to withhold writes
//!    from the real file. If `/etc/fstab` can't be read at all, a missing
//!    draft falls back to a harmless comment-only seed.
//!
//! ## Draft refresh: conditional, not once-per-launch
//!
//! The draft is re-synced from `/etc/fstab` at most once per process
//! launch (see [`SandboxRefreshState`]), only if the real file's current
//! content differs from what Rustab last knew it to have. "Last knew" is
//! persisted in a marker file beside the draft (see
//! [`last_known_real_marker`]), updated on both a refresh and a successful
//! save of the real file ([`sync_sandbox_after_save`]).
//!
//! That conditionality is the point: refreshing unconditionally would
//! discard an unsaved draft on every restart. So: if the marker matches
//! the real content and the draft file exists, nothing happens. If they
//! differ, or there's no marker, or the draft is gone, the draft is
//! replaced from `/etc/fstab` and the marker updated. If `/etc/fstab` is
//! unreadable, nothing is compared; a missing draft is seeded, an
//! existing one is left alone.
//!
//! Within one run, every later resolution skips the check, so edits made
//! during the session survive it. Since commands can resolve the target
//! concurrently, [`SandboxRefreshState`] is a completion *barrier*:
//! resolutions that don't do the one-time work wait for the one that does.
//!
//! The refresh is a plain byte copy, never parse-and-reserialize, so it
//! can't drift from its source (`docs/SPEC.md` §5). It's published with
//! the same temp-file-plus-rename swap `fstab_core::writer` uses for real
//! saves, so a concurrent reader never sees a mid-truncate file (see
//! [`publish_bytes`]). It's also best-effort: an unreadable `/etc/fstab`
//! leaves target resolution working on the previous content rather than failing.
//!
//! ## The marker's second job: external-change detection
//!
//! The same marker also answers, right as a write is about to commit,
//! "has the real `/etc/fstab` changed outside Rustab since we last synced
//! with it?" That's [`ExternalChangeGuard`], comparing the marker's
//! recorded bytes against the file's current bytes - never the draft
//! against the real file, since a draft differing from the marker is just
//! unsaved edits. A detected difference blocks the write and is reported
//! with both sides of the diff (`WriteErrorKindDto::ExternalChange`); the
//! user can cancel or ask for the write anyway via `allow_external_change`.
//!
//! ## How the real `/etc/fstab` is read at all
//!
//! Every read of the real file here goes through an injected
//! [`crate::host::RealFileReader`] rather than `fs::read` directly, since
//! this process may be inside a Flatpak sandbox where `/etc` isn't
//! reachable directly. Production uses [`crate::host::real_file_reader`]
//! (plain `fs::read`, or `flatpak-spawn --host` inside a sandbox); tests
//! pass [`crate::host::DirectFs`] or a double pointed at a temp file. See
//! [`crate::host`] for the mechanism.
//!
//! The *draft* and its marker are never read that way - they live in the
//! user's own data directory, which a sandboxed process sees normally.
//!
//! Rule 2 applies to release builds too, and the real file is still only
//! ever *read* here, never written: the elevated writer
//! (`crate::privileged`) is the only thing that writes `/etc/fstab`. See
//! `docs/API_CONTRACT.md` §2 for the reasoning.
//!
//! ## `is_real`
//!
//! [`ActiveTarget::is_real`] is `true` only when the resolved path *is*
//! the real `/etc/fstab`, literally or by canonicalized identity (catching
//! a symlink in either direction) - the banner driven by this flag is the
//! user's only warning (`docs/UI_DESIGN.md` §2).
//!
//! The flag doesn't answer "does a save reach `/etc/fstab`?" - a
//! [`TargetSource::SandboxDefault`] target has `is_real == false` but
//! saves to the real file anyway. It answers "is the path in
//! [`ActiveTarget::path`] itself the real file?", which the
//! [`TargetSource::Environment`] save branch needs to know whether
//! writing *that path* requires elevation.

use crate::host::{self, RealFileReader};
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

/// File name of the default sandbox target - the working draft.
const SANDBOX_FILE_NAME: &str = "fstab.draft";

/// The pre-T38 name of the working draft file. Used **only** by
/// [`migrate_legacy_sandbox_file_name`] to recognize a pre-existing
/// installation's draft, marker and backups and rename them onto the current
/// [`SANDBOX_FILE_NAME`] - nothing is ever newly written under this name.
const LEGACY_SANDBOX_FILE_NAME: &str = "fstab.sandbox";

/// Suffix of the marker file that records the real `/etc/fstab`'s content
/// as of the last time Rustab synced with it (see
/// [`last_known_real_marker`]).
///
/// Distinct from every other naming scheme around these files
/// (`fstab_core::writer`'s backup grammar, that module's save temp file,
/// this module's publish temp file), so a marker is never mistaken for a
/// backup the user might restore or Rustab might prune.
const LAST_KNOWN_REAL_SUFFIX: &str = ".rustab-lastknown";

/// Content the sandbox file falls back to when the real `/etc/fstab` could
/// not be read: comments only, so it parses cleanly, validates without a
/// single finding, and mounts nothing if a user ever copied it somewhere real
/// by accident.
const SANDBOX_SEED: &str = "\
# Rustab working draft - this file is NOT your /etc/fstab.
#
# Rustab normally fills this draft with a copy of your real /etc/fstab. You
# are seeing this blank template instead because /etc/fstab could not be read
# the last time Rustab started - it is missing, or not readable by this user.
#
# This draft is where your in-progress edits live between app restarts.
# Saving in Rustab writes your real /etc/fstab (through a single pkexec
# prompt) and then updates this draft to match. To edit some other file
# instead - and leave the real /etc/fstab completely untouched - start Rustab
# with:
#
#     RUSTAB_TARGET_PATH=/path/to/some/test-fstab rustab
#
# Add entries below in normal fstab syntax, e.g.:
# UUID=00000000-0000-0000-0000-000000000000 /mnt/example ext4 noauto 0 2
";

/// Completion barrier for the once-per-process draft sync: elects the one
/// resolution that performs the sync, and holds every concurrent
/// resolution back until it has actually finished.
///
/// The elected resolution compares the real `/etc/fstab` against the
/// marker and *then decides* whether a refresh is needed at all (see
/// [`refresh_sandbox_if_real_changed`]).
///
/// Exactly one instance lives for the app process's lifetime (inside
/// [`active_target`]), which is what makes "at most one sync per launch"
/// true. A parameter of [`resolve`] rather than a static it reads itself,
/// so each test builds its own and isn't affected by test execution order.
///
/// ## Why a barrier and not just a latch
///
/// Electing a writer is only half the problem: the frontend fires
/// `get_target_info` and `list_entries` as two independent tasks on
/// mount, so two commands resolve the target simultaneously, each then
/// reading the target's bytes separately after resolution returns. A
/// plain "did I win the race?" latch would let the loser return
/// immediately - while the winner was still mid-refresh - handing the
/// frontend stale pre-refresh content.
///
/// [`OnceLock::get_or_init`] solves both halves at once: the closure runs
/// for the first caller only, every other caller blocks until it fully
/// returns, and all get the same outcome. So no resolution can return
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
/// refreshing, or seeding the sandbox file if the policy selects that.
///
/// The first call in a process refreshes the draft from the real
/// `/etc/fstab` if it has changed since Rustab last looked; later calls do
/// nothing to it (see module docs). The refresh source is the hardcoded
/// [`REAL_FSTAB_PATH`], not configurable - [`TARGET_PATH_ENV`] chooses the
/// *target*, never where a draft is seeded from.
///
/// # Errors
///
/// Returns [`TargetError::SandboxSetup`] if the sandbox path had to be
/// created and could not be. An env-var-provided path is never created here:
/// a target that does not exist yet is a perfectly valid first-save target
/// and [`fstab_core::writer::write_document`] handles it.
pub fn active_target() -> Result<ActiveTarget, TargetError> {
    /// The one real process-lifetime barrier: whichever command resolves the
    /// target first does the per-launch draft sync (refresh-if-changed), and
    /// the others wait for it.
    static SANDBOX_REFRESH_STATE: SandboxRefreshState = SandboxRefreshState::new();

    resolve(
        env::var_os(TARGET_PATH_ENV).as_deref(),
        &default_sandbox_dir(),
        Path::new(REAL_FSTAB_PATH),
        host::real_file_reader(),
        &SANDBOX_REFRESH_STATE,
    )
}

/// The env-independent core of [`active_target`], parameterized on the raw
/// env value, the sandbox directory, the file the sandbox is refreshed
/// from, how that file is read, and the once-per-process refresh barrier.
///
/// Split out so tests can exercise every policy branch without mutating
/// process-global environment state or touching the machine's real
/// `/etc/fstab` or a shared refresh barrier. Production passes
/// `Path::new(REAL_FSTAB_PATH)`, [`crate::host::real_file_reader`], and
/// the single static barrier in [`active_target`]; tests pass their own.
fn resolve(
    env_value: Option<&OsStr>,
    sandbox_dir: &Path,
    real_fstab_path: &Path,
    real_reader: &dyn RealFileReader,
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
            ensure_sandbox_file(&path, real_fstab_path, real_reader, refresh_state)?;
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

/// Returns `true` if `path` and `other` name the same file: same literal
/// path, or both canonicalize to one destination (so a symlink in either
/// direction still counts). A path that can't be canonicalized is only
/// matched literally.
///
/// The generalization of [`is_real_fstab`], exposed because the elevated
/// writer applies the identical test to a target path it received as data
/// (see [`crate::privileged`]) - one implementation means the "is this the
/// real file, escalate" check and the "am I allowed to write this" check
/// can never disagree.
pub(crate) fn is_same_file(path: &Path, other: &Path) -> bool {
    if path == other {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(other)) {
        (Ok(resolved), Ok(resolved_other)) => resolved == resolved_other,
        _ => false,
    }
}

/// Prepares the sandbox file - the working draft - and its parent
/// directory for use.
///
/// The first resolution of a process performs the one-time draft sync: it
/// replaces the draft with a byte-for-byte copy of `real_fstab_path` only
/// if that content differs from the marker (or there's no marker, or the
/// draft is gone) - see [`refresh_sandbox_if_real_changed`] and the module
/// docs. If they already agree, the draft is left completely alone,
/// unsaved edits included - this is what lets the draft survive a
/// restart. If the real file can't be read or the copy fails, it falls
/// back to creating the file with [`SANDBOX_SEED`] if missing, leaving an
/// existing one alone.
///
/// Every other resolution skips all of that, and - the point of
/// [`SandboxRefreshState`] being a barrier, not a latch - doesn't return
/// until the first one has finished, so no command reads the target
/// mid-refresh. One consequence: if something outside Rustab deletes the
/// sandbox file mid-session, it's no longer reseeded until the next
/// process launch; the next read just reports a missing file.
///
/// The parent directory is created on every call, outside the barrier
/// (an idempotent no-op once it exists), so a resolution still reports a
/// genuinely unusable sandbox directory even after the one-time sync.
fn ensure_sandbox_file(
    path: &Path,
    real_fstab_path: &Path,
    real_reader: &dyn RealFileReader,
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
            migrate_legacy_sandbox_file_name(path);
            if refresh_sandbox_if_real_changed(path, real_fstab_path, real_reader) {
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

// ---------------------------------------------------------------------------
// Legacy file name migration (T38)
// ---------------------------------------------------------------------------

/// One-time migration from the old draft file name
/// ([`LEGACY_SANDBOX_FILE_NAME`], `fstab.sandbox`) to the current one
/// ([`SANDBOX_FILE_NAME`], `fstab.draft`), run as the first step inside
/// [`ensure_sandbox_file`]'s refresh barrier, before anything looks the
/// new path up.
///
/// The `new_path.exists()` check below *is* the "runs once per
/// installation" gate - the first successful rename makes it true for
/// every later call. No separate flag or marker file needed.
///
/// `fs::rename` never removes its source unless the destination lands, so
/// a failed rename leaves the old file byte-intact under its old name;
/// the worst outcome is [`seed_sandbox_file_if_missing`] creating a fresh
/// file at the new path, as it would for a genuine first launch - never
/// silent data loss, and the old file stays recoverable by hand.
///
/// Every rename (draft, marker, each backup) is independent and
/// best-effort: an `io::Error` from one doesn't stop the others, and this
/// function never returns an error to its caller.
fn migrate_legacy_sandbox_file_name(new_path: &Path) {
    if new_path.exists() {
        return; // already migrated, or a fresh install with its own file
    }
    let legacy_path = new_path.with_file_name(LEGACY_SANDBOX_FILE_NAME);
    // The main draft rename is independent of the marker/backup migrations
    // below: a legacy marker or backup can exist without the legacy draft
    // itself (or vice versa), and each piece that is actually present is
    // migrated on its own rather than all-or-nothing.
    if legacy_path.exists() {
        let _ = fs::rename(&legacy_path, new_path);
    }
    migrate_legacy_marker(&legacy_path, new_path);
    migrate_legacy_backups(&legacy_path, new_path);
}

/// Migrates the legacy last-known-real marker (if any) onto the new draft's
/// marker path, independently of whether the main draft rename succeeded.
///
/// Uses the existing [`last_known_real_marker`] for both paths rather than
/// hand-constructing the [`LAST_KNOWN_REAL_SUFFIX`] again, so the two can
/// never drift apart.
fn migrate_legacy_marker(legacy_path: &Path, new_path: &Path) {
    let (Some(legacy_marker), Some(new_marker)) = (
        last_known_real_marker(legacy_path),
        last_known_real_marker(new_path),
    ) else {
        return;
    };
    if legacy_marker.exists() && !new_marker.exists() {
        let _ = fs::rename(&legacy_marker, &new_marker);
    }
}

/// Migrates every automatic and manual backup of the legacy draft name
/// onto the equivalent name under [`SANDBOX_FILE_NAME`], preserving the
/// rest of each backup's file name (timestamp, `.manual` suffix) verbatim.
///
/// Enumerates via [`fstab_core::writer::list_backups`] rather than
/// reimplementing the naming grammar. Each rename is independent and
/// best-effort, and skips (never overwrites) an existing destination.
fn migrate_legacy_backups(legacy_path: &Path, new_path: &Path) {
    let Ok(backups) = fstab_core::writer::list_backups(legacy_path) else {
        return;
    };
    for backup in backups {
        let Some(file_name) = backup.path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(suffix) = file_name.strip_prefix(LEGACY_SANDBOX_FILE_NAME) else {
            continue;
        };
        let new_backup_path = new_path.with_file_name(format!("{SANDBOX_FILE_NAME}{suffix}"));
        if !new_backup_path.exists() {
            let _ = fs::rename(&backup.path, &new_backup_path);
        }
    }
}

/// The once-per-launch draft sync: copies `real_fstab_path` over the draft
/// verbatim if it differs from what Rustab last recorded, and records the
/// copied bytes as the new "last known real content".
///
/// Returns `true` if the draft can be considered dealt with (refreshed, or
/// no refresh needed), `false` if the caller should fall back to
/// [`seed_sandbox_file_if_missing`] (unreadable real file, or the copy
/// couldn't be published).
///
/// Skips the refresh only if the marker holds byte-for-byte what
/// `real_fstab_path` holds *now* **and** the draft file still exists; a
/// missing or differing marker, or a missing draft, means refresh.
///
/// The draft is published before the marker, and the marker's own
/// failure is swallowed, so the only possible inconsistency is a marker
/// older than the draft (cheap: one redundant copy next launch), never a
/// marker newer than the draft (which would skip a needed refresh forever).
///
/// The read goes through `real_reader` rather than a plain `fs::read`,
/// since inside a Flatpak sandbox the real `/etc/fstab` isn't reachable
/// directly (see [`crate::host`]). Every failure mode still lands on
/// `false` here, so broken host access just falls back to seeding.
fn refresh_sandbox_if_real_changed(
    sandbox_path: &Path,
    real_fstab_path: &Path,
    real_reader: &dyn RealFileReader,
) -> bool {
    let Ok(real_bytes) = real_reader.read(real_fstab_path) else {
        return false;
    };
    if sandbox_is_in_sync(sandbox_path, &real_bytes) {
        return true;
    }
    if publish_bytes(sandbox_path, &real_bytes).is_err() {
        return false;
    }
    // Best-effort, deliberately: a marker that could not be written costs the
    // next launch a redundant refresh, which is not worth failing a sync that
    // otherwise succeeded over.
    let _ = record_last_known_real(sandbox_path, &real_bytes);
    true
}

/// Whether the draft is already in sync with `real_bytes`: the marker records
/// exactly those bytes *and* the draft file still exists (see
/// [`refresh_sandbox_if_real_changed`]).
fn sandbox_is_in_sync(sandbox_path: &Path, real_bytes: &[u8]) -> bool {
    let recorded = last_known_real_marker(sandbox_path).and_then(|marker| fs::read(marker).ok());
    recorded.is_some_and(|recorded| recorded == real_bytes) && sandbox_path.exists()
}

/// The marker file recording the real `/etc/fstab`'s content as of
/// Rustab's last sync point: `<sandbox file name>.rustab-lastknown`, in
/// the sandbox's own directory.
///
/// Stores the bytes themselves rather than a hash: the workspace has no
/// hashing dependency, `std::hash`'s `DefaultHasher` isn't stable across
/// Rust versions or process runs (so it can't be compared across
/// launches), and a copy costs only kilobytes, exposes nothing new (a
/// copy of a world-readable file), and is directly inspectable.
///
/// `None` only for a path with no file-name component, which the
/// constructed sandbox path never is; returning it rather than panicking
/// keeps every caller on its one best-effort failure route.
pub(crate) fn last_known_real_marker(sandbox_path: &Path) -> Option<PathBuf> {
    let file_name = sandbox_path.file_name()?;
    let mut marker_name = OsString::from(file_name);
    marker_name.push(LAST_KNOWN_REAL_SUFFIX);
    Some(sandbox_path.with_file_name(marker_name))
}

/// Records `bytes` as the real `/etc/fstab`'s content as of now, i.e. moves
/// Rustab's "last known reality" marker forward.
fn record_last_known_real(sandbox_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let marker = last_known_real_marker(sandbox_path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} has no file-name component, so no last-known-content marker can be kept \
                 beside it",
                sandbox_path.display()
            ),
        )
    })?;
    publish_bytes(&marker, bytes)
}

/// Updates the working draft **and** the last-known-real marker to
/// `bytes`, in that order - the one call `crate::commands`'s save path
/// makes after the elevated writer has put those same bytes into the
/// real `/etc/fstab`.
///
/// Doing both together is what keeps a save from being seen as external
/// drift by the next launch: without the marker update,
/// [`refresh_sandbox_if_real_changed`] would find the real file different
/// from its stale marker and overwrite the draft Rustab just mirrored into.
///
/// Each file is published with the same temp-file-plus-rename swap a
/// refresh uses, draft before marker, for the same reason as there: a
/// crash in between only leaves a stale marker (self-heals next launch),
/// never a marker claiming a draft is current when it isn't.
///
/// # Errors
///
/// The first `io::Error` of the two publishes - not a failed save, since
/// the real file already holds `bytes`. The caller surfaces it as a
/// non-fatal warning, and the next launch repairs the draft on its own.
pub(crate) fn sync_sandbox_after_save(sandbox_path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_bytes(sandbox_path, bytes)?;
    record_last_known_real(sandbox_path, bytes)
}

// ---------------------------------------------------------------------------
// External-change detection (T36)
// ---------------------------------------------------------------------------

/// What a real-file write knows about `/etc/fstab`'s content before it
/// starts, and whether the user has already chosen to overwrite an
/// external change.
///
/// The whole input of the "did the real file change under us?" check,
/// travelling as two flat fields inside `PrivilegedRequest::Write` so the
/// check can run *again*, inside the elevated process, immediately
/// before `write_document` runs - the authoritative check, since
/// `pkexec`'s password wait leaves a window for the file to change that a
/// pre-prompt-only check would miss.
///
/// The comparison is the marker's recorded bytes vs. `/etc/fstab`'s
/// current bytes, never the draft against the real file - a draft
/// differing from the marker is just unsaved edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChangeGuard {
    /// The content Rustab last knew the real `/etc/fstab` to hold - the
    /// [`last_known_real_marker`]'s content, as recorded by the last refresh
    /// or the last successful save ([`sync_sandbox_after_save`]).
    ///
    /// `None` means no usable record (no marker yet, an unreadable one,
    /// non-UTF-8 content, or a target kind that keeps no marker at all -
    /// see [`ExternalChangeGuard::unchecked`]). With nothing to compare
    /// against, no external change can be established and the write
    /// proceeds - the alternative, blocking every write a best-effort
    /// marker write had failed for, would make the file uneditable for a
    /// reason the user can't see or fix.
    pub last_known_real_content: Option<String>,
    /// `true` only when the user has been shown the external diff and
    /// explicitly asked for the write anyway, making
    /// [`ExternalChangeGuard::detect`] report nothing for this one
    /// request.
    ///
    /// A separate signal from [`Self::last_known_real_content`], since
    /// "the user confirmed" and "Rustab never knew" must stay distinguishable.
    pub allow_external_change: bool,
}

/// An external change to the real `/etc/fstab`, as detected by
/// [`ExternalChangeGuard::detect`]: both sides of the diff the user is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChange {
    /// What Rustab last knew the file to hold - the "old" side.
    pub last_known: String,
    /// What the file holds right now - the "new" side, i.e. the content the
    /// write would replace.
    pub current: String,
}

impl ExternalChangeGuard {
    /// The guard for a default-launch write: the recorded marker content
    /// beside `sandbox_path`, plus the user's explicit "write anyway" answer.
    ///
    /// Every failure to read the marker lands on `None` (no record) rather
    /// than on an error, for the reason spelled out on
    /// [`Self::last_known_real_content`].
    pub fn from_marker(sandbox_path: &Path, allow_external_change: bool) -> Self {
        let last_known_real_content = last_known_real_marker(sandbox_path)
            .and_then(|marker| fs::read(marker).ok())
            // Non-UTF-8 marker bytes cannot be carried through the JSON
            // request, and a session could not have loaded such a file in the
            // first place (`crate::commands::list_entries` refuses non-UTF-8
            // content outright), so this is treated as "no usable record".
            .and_then(|bytes| String::from_utf8(bytes).ok());
        ExternalChangeGuard {
            last_known_real_content,
            allow_external_change,
        }
    }

    /// The guard for a write that has no marker to compare against at all:
    /// case 3 of `crate::commands::save_document_with` (an explicit
    /// `RUSTAB_TARGET_PATH=/etc/fstab`), which deliberately keeps no
    /// last-known-content marker.
    ///
    /// Carries no record, so [`Self::detect`] reports nothing and that path
    /// behaves exactly as it did before T36. `allow_external_change` is
    /// `false` because nothing was confirmed - the check is skipped for want
    /// of an expectation, not for want of a warning.
    pub fn unchecked() -> Self {
        ExternalChangeGuard {
            last_known_real_content: None,
            allow_external_change: false,
        }
    }

    /// Compares what Rustab last knew against what `real_fstab` holds right
    /// now, returning both sides when they differ.
    ///
    /// `None` - "no external change to report" - for each of:
    ///
    /// - the user already confirmed the overwrite
    ///   ([`Self::allow_external_change`]);
    /// - there is no usable record to compare against
    ///   ([`Self::last_known_real_content`]);
    /// - `real_fstab` cannot be read at all (missing, or no permission).
    ///   Nothing can be established in that case, and a file that is *gone*
    ///   is not an edit to warn about: the write recreates it, the same
    ///   first-ever-save state [`refresh_sandbox_if_real_changed`] already
    ///   treats as "nothing to compare";
    /// - the bytes match.
    ///
    /// The comparison is byte-exact against the recorded bytes. Only the
    /// *reported* current content is lossy ([`String::from_utf8_lossy`]),
    /// since it exists to be rendered as a diff.
    ///
    /// This is the check as the **elevated helper** performs it
    /// (`crate::privileged::handle_write`), which is always looking at the
    /// real filesystem (started by `pkexec` directly, or by
    /// `flatpak-spawn --host pkexec` outside any sandbox). So a plain
    /// `fs::read` is right here, and this is Rustab's one authoritative
    /// read (see [`Self::detect_with`] for the unprivileged side's
    /// courtesy check, which may have a sandbox boundary in the way).
    pub fn detect(&self, real_fstab: &Path) -> Option<ExternalChange> {
        self.detect_with(real_fstab, &host::DirectFs)
    }

    /// [`Self::detect`] with the read of `real_fstab` injected, for the
    /// unprivileged GUI process, which can't read that file with
    /// `fs::read` inside a Flatpak sandbox (see [`crate::host`]).
    ///
    /// Only *where the bytes come from* differs; the verdict is computed
    /// identically for both callers. An unreadable real file still means
    /// "report no change", so broken host access just degrades to no
    /// courtesy check rather than blocking saves.
    pub(crate) fn detect_with(
        &self,
        real_fstab: &Path,
        real_reader: &dyn RealFileReader,
    ) -> Option<ExternalChange> {
        if self.allow_external_change {
            return None;
        }
        let last_known = self.last_known_real_content.as_deref()?;
        let current = real_reader.read(real_fstab).ok()?;
        if current == last_known.as_bytes() {
            return None;
        }
        Some(ExternalChange {
            last_known: last_known.to_string(),
            current: String::from_utf8_lossy(&current).into_owned(),
        })
    }
}

/// Writes `bytes` onto `path` as a sibling temp file plus a `rename`,
/// reporting the first `io::Error` of the sequence.
///
/// Used for both files this module owns: the working draft and its
/// last-known-real marker.
///
/// A plain `fs::write` would open-truncate-write, which isn't atomic
/// against a concurrent reader (a second Rustab process, say) - such a
/// reader could land between the truncate and the completed write and
/// see an empty document. So the bytes go to a sibling temp file and are
/// `rename`d onto the destination, the same way [`fstab_core::writer`]
/// publishes real saves, for the same reason (`rename(2)` is atomic
/// within one filesystem).
///
/// A literal byte copy, never parse-and-reserialize, so it can't differ
/// from its source (`docs/SPEC.md` §5).
///
/// Only the atomicity half of a real save: no validation, no backup, no
/// permission carry-over, no `fsync`. The draft is a convenience copy in
/// a scratch directory, not a durable publish - a crash mid-publish just
/// means the next launch syncs again. A symlinked draft path is replaced
/// by a regular file rather than written through, the safer choice for a
/// scratch file.
fn publish_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp_path = refresh_temp_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no final file-name component", path.display()),
        )
    })?;
    if let Err(err) = fs::write(&temp_path, bytes) {
        // The temp file may or may not exist (create could have failed, or a
        // partial write could have left it behind); removing it is best-effort
        // and its own error is ignored so it cannot mask the real cause.
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

/// The sibling temp file [`publish_bytes`] stages its copy in:
/// `.<destination file name>.rustab-refresh.tmp.<pid>`, in the
/// destination's own directory (so the rename is atomic).
///
/// The `.rustab-refresh.tmp.` infix is distinct from `fstab_core::writer`'s
/// backup grammar and its own save temp file, so this is never mistaken
/// for a backup or a save in flight. The pid keeps concurrent Rustab
/// processes from staging into the same temp path.
///
/// `None` only for a path with no file-name component, which neither the
/// sandbox path nor its marker ever is.
fn refresh_temp_path(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".rustab-refresh.tmp.{}", std::process::id()));
    Some(path.with_file_name(temp_name))
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

/// The directory the default sandbox file lives in - Rustab's own data
/// directory, [`rustab_data_dir`].
///
/// A thin alias, not its own resolution logic: the draft lives directly
/// in the data directory, while everything else Rustab keeps lives in a
/// named subdirectory of it (see [`crate::backup_location`]).
fn default_sandbox_dir() -> PathBuf {
    rustab_data_dir()
}

/// Rustab's own per-user data directory: `$XDG_DATA_HOME/rustab`, else
/// `$HOME/.local/share/rustab`, else `<temp dir>/rustab`.
///
/// The single source of truth for "where does Rustab keep its own
/// files", used by [`default_sandbox_dir`] and by
/// [`crate::backup_location`]'s fallback directory, so neither grows a
/// second, drifting copy of the XDG rules.
///
/// Resolved with plain XDG rules rather than Tauri's path API, so target
/// resolution needs no `AppHandle` - the elevated helper (no Tauri
/// runtime) and unit tests can both use it.
pub(crate) fn rustab_data_dir() -> PathBuf {
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
    use crate::host::{DirectFs, StubReader};
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
            &DirectFs,
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
            &DirectFs,
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
        let target = resolve(None, &sandbox_dir, &real_fstab, &DirectFs, &refresh_state)
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
                &DirectFs,
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
            &DirectFs,
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

    /// The marker path a given sandbox path's sync state is recorded in.
    fn marker_of(sandbox_path: &Path) -> PathBuf {
        last_known_real_marker(sandbox_path).expect("a named file has a marker sibling")
    }

    /// A first run with no marker at all: the draft starts off as an exact
    /// copy of the real file, and the marker is created holding the same
    /// bytes. Asserted on bytes, because the copy must be a copy and not a
    /// parse/reserialize round trip.
    #[test]
    fn the_first_resolution_of_a_process_copies_the_real_fstab_into_the_sandbox() {
        let dir = TempDir::new("target-refresh-first");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let target = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve");

        assert_eq!(target.source, TargetSource::SandboxDefault);
        assert_eq!(
            fs::read(&target.path).expect("the sandbox must be readable"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "the sandbox must be a byte-for-byte copy of the real file"
        );
        assert_eq!(
            fs::read(marker_of(&target.path)).expect("the marker must have been created"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "the refresh must record what it copied as the last known real content"
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

        let target = resolve(None, &sandbox_dir, &real_fstab, &DirectFs, &refresh_state)
            .expect("first resolution");

        // The user edits and saves in this session.
        fs::write(&target.path, "# edited by the user\n").expect("write");

        for _ in 0..3 {
            let again = resolve(None, &sandbox_dir, &real_fstab, &DirectFs, &refresh_state)
                .expect("the sandbox must resolve again");
            assert_eq!(again.path, target.path);
            assert_eq!(
                fs::read_to_string(&again.path).expect("read"),
                "# edited by the user\n",
                "an in-session resolution must leave the sandbox untouched"
            );
        }
    }

    /// T34, half one: a new process whose `/etc/fstab` **has** changed since
    /// the last sync re-copies it, discarding the previous session's draft.
    /// That discard is deliberate - the real file genuinely moved out from
    /// under the draft, and current reality wins.
    #[test]
    fn a_new_process_re_copies_the_real_fstab_when_it_changed_since_the_last_sync() {
        let dir = TempDir::new("target-refresh-new-process");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, "# first launch\n").expect("write");

        let first = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("first process");
        fs::write(&first.path, "# left over from the previous session\n").expect("write");

        // Something outside Rustab edited the real file between launches.
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let second = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("second process");

        assert_eq!(second.path, first.path);
        assert_eq!(
            fs::read(&second.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "a changed real file must be re-copied over the draft"
        );
        assert_eq!(
            fs::read(marker_of(&second.path)).expect("read the marker"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "and the marker must move forward with it"
        );
    }

    /// T34, half two - the behaviour change this task exists for: a new
    /// process whose `/etc/fstab` has **not** changed since the last sync
    /// leaves the draft completely alone, unsaved edits included. Before T34
    /// this same scenario discarded them on every single restart.
    #[test]
    fn a_new_process_leaves_the_draft_alone_when_the_real_fstab_is_unchanged() {
        let dir = TempDir::new("target-refresh-unchanged");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let first = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("first process");
        // The user edits in that session and does not save.
        let draft = "# an unsaved draft the user wants back after a restart\n";
        fs::write(&first.path, draft).expect("write");

        // A new launch. Nothing touched the real file in the meantime.
        let second = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("second process");

        assert_eq!(second.path, first.path);
        assert_eq!(
            fs::read_to_string(&second.path).expect("read"),
            draft,
            "an unchanged real file must not cost the user their draft"
        );
    }

    /// A pre-T34 sandbox - a draft with no marker beside it - is treated the
    /// same as a changed real file: refreshed, and a marker created. That is
    /// the deliberately safe direction for the unknown case.
    #[test]
    fn a_draft_with_no_marker_is_refreshed_and_gets_one() {
        let dir = TempDir::new("target-refresh-no-marker");
        let sandbox_dir = dir.join("data/rustab");
        let sandbox_file = sandbox_dir.join(SANDBOX_FILE_NAME);
        fs::create_dir_all(&sandbox_dir).expect("mkdir");
        fs::write(&sandbox_file, "# a pre-T34 sandbox\n").expect("write");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");
        assert!(!marker_of(&sandbox_file).exists());

        let target = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve");

        assert_eq!(
            fs::read(&target.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "a missing marker must be treated as 'changed'"
        );
        assert_eq!(
            fs::read(marker_of(&target.path)).expect("read the marker"),
            REAL_FSTAB_SAMPLE.as_bytes()
        );
    }

    /// The marker matching is not enough on its own: if the draft file itself
    /// is gone (something outside Rustab deleted it between sessions), the
    /// launch must still re-create it rather than skip the refresh and leave
    /// the session with no file to read.
    #[test]
    fn a_deleted_draft_is_recreated_even_though_the_marker_still_matches() {
        let dir = TempDir::new("target-refresh-deleted-draft");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let first = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("first process");
        fs::remove_file(&first.path).expect("remove the draft");
        assert!(
            marker_of(&first.path).exists(),
            "the marker must have survived"
        );

        let second = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("second process");

        assert_eq!(
            fs::read(&second.path).expect("the draft must be back"),
            REAL_FSTAB_SAMPLE.as_bytes()
        );
    }

    /// The save side of the marker, and the bug it exists to prevent: after a
    /// successful real write, [`sync_sandbox_after_save`] moves both the draft
    /// and the marker forward, so the *next* launch does not mistake Rustab's
    /// own save for external drift and blow away the draft it just mirrored
    /// into - nor a further unsaved edit made after that save.
    #[test]
    fn a_saves_own_write_is_not_seen_as_external_drift_by_the_next_launch() {
        let dir = TempDir::new("target-sync-after-save");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, "# before the save\n").expect("write");

        let first = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("first process");

        // A T34 save: the elevated writer puts these bytes into the real file
        // (here: the stand-in for it), and the save path then calls this.
        let saved = "# saved by Rustab\nLABEL=x /mnt/x ext4 defaults 0 2\n";
        fs::write(&real_fstab, saved).expect("the elevated write");
        sync_sandbox_after_save(&first.path, saved.as_bytes())
            .expect("the draft sync must succeed");

        assert_eq!(fs::read_to_string(&first.path).expect("read"), saved);
        assert_eq!(
            fs::read_to_string(marker_of(&first.path)).expect("read the marker"),
            saved
        );

        // The user then edits again without saving, and restarts.
        let unsaved = "# edited after the save, not saved yet\n";
        fs::write(&first.path, unsaved).expect("write");
        let second = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("second process");

        assert_eq!(
            fs::read_to_string(&second.path).expect("read"),
            unsaved,
            "a save's own write must not be seen as drift on the next launch"
        );
    }

    /// The marker is strictly best-effort: if it cannot be written, the
    /// refresh itself still counts as done (the draft is current), and the
    /// only consequence is that the next launch refreshes again.
    #[test]
    fn a_marker_that_cannot_be_written_does_not_fail_the_refresh() {
        let dir = TempDir::new("target-marker-unwritable");
        let sandbox_dir = dir.join("data/rustab");
        let sandbox_file = sandbox_dir.join(SANDBOX_FILE_NAME);
        fs::create_dir_all(&sandbox_dir).expect("mkdir");
        // A directory where the marker file wants to be: the rename onto it
        // fails on any Unix, whether or not the suite runs as root.
        fs::create_dir_all(marker_of(&sandbox_file)).expect("mkdir");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        assert!(
            refresh_sandbox_if_real_changed(&sandbox_file, &real_fstab, &DirectFs),
            "an unwritable marker must not make the sync report failure"
        );
        assert_eq!(
            fs::read(&sandbox_file).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes(),
            "the draft itself must still have been refreshed"
        );
        assert!(marker_of(&sandbox_file).is_dir(), "left as it was found");
    }

    /// `sync_sandbox_after_save` reports its failure rather than swallowing
    /// it, because its caller turns that into the non-fatal "your save landed
    /// but the draft did not follow" warning (`crate::commands`).
    #[test]
    fn a_draft_that_cannot_be_published_reports_the_sync_failure() {
        let dir = TempDir::new("target-sync-failure");
        let sandbox_dir = dir.join("data/rustab");
        let sandbox_file = sandbox_dir.join(SANDBOX_FILE_NAME);
        // A directory in the draft's place: the publish rename cannot succeed.
        fs::create_dir_all(&sandbox_file).expect("mkdir");

        let error = sync_sandbox_after_save(&sandbox_file, b"# saved\n")
            .expect_err("an unpublishable draft must be reported");
        assert!(
            !marker_of(&sandbox_file).exists(),
            "and the marker must not claim the draft is current"
        );
        assert!(
            !error.to_string().is_empty(),
            "the failure must carry a message for the warning"
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
            &DirectFs,
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
            &DirectFs,
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

    // --- T40: the real file is read through the injected reader -------------

    /// The T40 property, stated as a test: the refresh takes its bytes from
    /// the injected [`RealFileReader`] and from nowhere else. The real-file
    /// path given here does **not exist on disk at all**, so a draft holding
    /// the stub's bytes can only have come through the seam - which is exactly
    /// the situation a Flatpak sandbox is in, where `/etc/fstab` is reachable
    /// only via `flatpak-spawn --host`.
    #[test]
    fn a_refresh_takes_the_real_files_bytes_from_the_injected_reader() {
        let dir = TempDir::new("target-reader-refresh");
        let sandbox_dir = dir.join("data/rustab");
        // Deliberately never created: a plain `fs::read` of this path fails.
        let unreachable_real = dir.join("not-on-this-disk/fstab");
        let reader = StubReader::serving(REAL_FSTAB_SAMPLE.as_bytes());

        let target = resolve(
            None,
            &sandbox_dir,
            &unreachable_real,
            &reader,
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve");

        assert_eq!(
            fs::read_to_string(&target.path).expect("read the draft"),
            REAL_FSTAB_SAMPLE,
            "the draft must hold what the reader served, byte for byte"
        );
        assert_eq!(
            fs::read_to_string(marker_of(&target.path)).expect("read the marker"),
            REAL_FSTAB_SAMPLE,
            "the marker must record what the reader served"
        );
        assert_eq!(
            reader.asked(),
            vec![unreachable_real],
            "the reader must have been asked for the real file, once"
        );
    }

    /// A reader that cannot reach the real file at all - a sandbox whose host
    /// access is missing or denied - degrades exactly like an unreadable real
    /// file always has: the draft is seeded, nothing fails.
    #[test]
    fn a_reader_that_cannot_reach_the_host_falls_back_to_the_seed() {
        let dir = TempDir::new("target-reader-unreachable");
        let sandbox_dir = dir.join("data/rustab");
        let reader = StubReader::failing(
            io::ErrorKind::Other,
            "Portal call failed: org.freedesktop.DBus.Error.ServiceUnknown",
        );

        let target = resolve(
            None,
            &sandbox_dir,
            Path::new("/etc/fstab"),
            &reader,
            &SandboxRefreshState::new(),
        )
        .expect("a sandbox with no host access must still resolve");

        assert!(target.path.exists(), "the draft must have been seeded");
        assert!(
            fs::read_to_string(&target.path)
                .expect("read")
                .contains("NOT your /etc/fstab"),
            "an unreachable real file must leave the blank seed, not an empty draft"
        );
        // The hardcoded real path is used here purely as the name handed to
        // the stub; it is never read, listed or written by this test.
        assert_eq!(reader.asked(), vec![PathBuf::from("/etc/fstab")]);
    }

    /// The external-change check's unprivileged form reads through the seam
    /// too - again proven by a real-file path that does not exist locally.
    #[test]
    fn the_unprivileged_external_change_check_reads_through_the_seam() {
        let dir = TempDir::new("target-reader-guard");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# unsaved edits\n").expect("seed the draft");
        fs::write(marker_of(&draft), "# what rustab knew\n").expect("seed the marker");
        let unreachable_real = dir.join("not-on-this-disk/fstab");
        let guard = ExternalChangeGuard::from_marker(&draft, false);

        let changed = StubReader::serving(b"# somebody else wrote this\n");
        let detected = guard
            .detect_with(&unreachable_real, &changed)
            .expect("the reader's content differs from the marker");
        assert_eq!(detected.last_known, "# what rustab knew\n");
        assert_eq!(detected.current, "# somebody else wrote this\n");

        let unchanged = StubReader::serving(b"# what rustab knew\n");
        assert_eq!(
            guard.detect_with(&unreachable_real, &unchanged),
            None,
            "bytes equal to the marker's are no external change"
        );

        // And the reader-less form - the one the elevated host-side helper
        // uses - still reads the filesystem directly, which for this
        // nonexistent path means "nothing can be established".
        assert_eq!(guard.detect(&unreachable_real), None);
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

    /// The two file names a synced sandbox directory holds, sorted the way
    /// [`entry_names`] returns them: the draft and its marker, and nothing
    /// else.
    fn synced_dir_entries() -> Vec<String> {
        vec![
            SANDBOX_FILE_NAME.to_owned(),
            format!("{SANDBOX_FILE_NAME}{LAST_KNOWN_REAL_SUFFIX}"),
        ]
    }

    /// Both publishes stage their copy in a sibling temp file and `rename` it
    /// into place; a successful refresh must leave no trace of that staging
    /// behind, or the sandbox directory would accumulate stray files per
    /// launch. What it *does* leave is exactly two files: the draft and its
    /// marker.
    #[test]
    fn a_successful_refresh_leaves_no_temp_file_behind() {
        let dir = TempDir::new("target-refresh-no-temp");
        let sandbox_dir = dir.join("data/rustab");
        let real_fstab = dir.join("real-fstab");
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let target = resolve(
            None,
            &sandbox_dir,
            &real_fstab,
            &DirectFs,
            &SandboxRefreshState::new(),
        )
        .expect("the sandbox must resolve");

        assert_eq!(entry_names(&sandbox_dir), synced_dir_entries());
        assert_eq!(
            fs::read(&target.path).expect("read"),
            REAL_FSTAB_SAMPLE.as_bytes()
        );
    }

    /// The failure path of the same staging: when the swap cannot happen the
    /// temp file must be cleaned up, `refresh_sandbox_if_real_changed` must
    /// report `false` (so the caller falls back to seed/leave-alone), and the
    /// sandbox path itself must be untouched - marker included, since a
    /// marker written after a failed copy would be a lie.
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
            !refresh_sandbox_if_real_changed(&sandbox_path, &real_fstab, &DirectFs),
            "a refresh that cannot be swapped into place must report failure"
        );

        assert_eq!(
            entry_names(&sandbox_dir),
            vec![SANDBOX_FILE_NAME.to_owned()],
            "neither the staging temp file nor a marker may survive a failed refresh"
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

    /// The marker is a sibling of the draft and must not be mistakable for
    /// one of `fstab_core::writer`'s backups - if it were, `list_backups`
    /// would offer it as a restore point and `prune_backups` could delete it.
    #[test]
    fn the_marker_is_a_sibling_that_is_not_a_backup_name() {
        let sandbox_path = Path::new("/data/rustab").join(SANDBOX_FILE_NAME);
        let marker = marker_of(&sandbox_path);

        assert_eq!(marker.parent(), sandbox_path.parent());
        let name = marker
            .file_name()
            .expect("the marker has a file name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, format!("{SANDBOX_FILE_NAME}{LAST_KNOWN_REAL_SUFFIX}"));
        assert!(
            !name.contains(".rustab-backup."),
            "the marker must not look like a backup: {name}"
        );
        assert!(
            !name.contains(".rustab.tmp.") && !name.contains(".rustab-refresh.tmp."),
            "nor like a write in flight: {name}"
        );
        // And `fstab-core`, which owns the backup grammar, agrees: a real
        // marker sitting beside a real draft is not listed as a backup of it.
        let dir = TempDir::new("target-marker-not-a-backup");
        let draft = dir.join(SANDBOX_FILE_NAME);
        fs::write(&draft, REAL_FSTAB_SAMPLE).expect("write");
        fs::write(marker_of(&draft), REAL_FSTAB_SAMPLE).expect("write");
        assert!(
            fstab_core::writer::list_backups(&draft)
                .expect("the directory must be readable")
                .is_empty(),
            "the marker must never be reported as a backup of the draft"
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

    /// The regression test shaped like the actual bug: `get_target_info`
    /// and `list_entries` resolve the target concurrently, each reading
    /// the resolved path separately. Without the barrier, the resolution
    /// that didn't refresh could return while the other was still
    /// mid-copy, handing the frontend stale pre-refresh content.
    ///
    /// Not deterministic on its own (scheduling-dependent); see
    /// `every_caller_waits_until_the_refresh_has_finished` for the
    /// deterministic proof - this one is the readable statement of the bug.
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
                    let target =
                        resolve(None, &sandbox_dir, &real_fstab, &DirectFs, &refresh_state)
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
            synced_dir_entries(),
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

    // -- T36: external-change detection -------------------------------------
    //
    // Unit-level coverage of the one comparison both sides of a write perform
    // (the unprivileged courtesy check in `crate::commands` and the
    // authoritative one in `crate::privileged`), against a stand-in for
    // `/etc/fstab` - never the machine's own file.

    /// The headline: the comparison is the **marker's** recorded content
    /// against the real file's *current* content, and a difference is reported
    /// with both sides, in the right order (old = what Rustab knew, new = what
    /// is there now).
    #[test]
    fn a_real_file_that_no_longer_matches_the_marker_is_an_external_change() {
        let dir = TempDir::new("guard-drift");
        let draft = dir.join("fstab.sandbox");
        let real = dir.join("etc-fstab-stand-in");
        fs::write(marker_of(&draft), "# what rustab knew\n").expect("seed the marker");
        fs::write(&real, "# what someone else wrote\n").expect("seed the real stand-in");

        let change = ExternalChangeGuard::from_marker(&draft, false)
            .detect(&real)
            .expect("a differing real file is an external change");

        assert_eq!(change.last_known, "# what rustab knew\n");
        assert_eq!(change.current, "# what someone else wrote\n");
    }

    /// The ordinary case, and the one that must never produce a warning: the
    /// real file still holds exactly what the marker recorded. The draft is
    /// deliberately *different* from both - that is what unsaved edits look
    /// like, and it says nothing about anything outside Rustab.
    #[test]
    fn an_unchanged_real_file_is_no_external_change_even_with_a_drifted_draft() {
        let dir = TempDir::new("guard-no-drift");
        let draft = dir.join("fstab.sandbox");
        let real = dir.join("etc-fstab-stand-in");
        fs::write(&draft, "# the user's unsaved edits\n").expect("seed the draft");
        fs::write(marker_of(&draft), "# agreed reality\n").expect("seed the marker");
        fs::write(&real, "# agreed reality\n").expect("seed the real stand-in");

        assert_eq!(
            ExternalChangeGuard::from_marker(&draft, false).detect(&real),
            None,
            "a draft differing from the marker is unsaved work, not external drift"
        );
    }

    /// The three ways the check is skipped rather than failed, each of which
    /// leaves the pre-T36 behaviour in place: the user said "write anyway",
    /// there is no marker to compare against, and the real file cannot be read
    /// at all.
    #[test]
    fn a_check_with_nothing_to_compare_reports_no_external_change() {
        let dir = TempDir::new("guard-skips");
        let draft = dir.join("fstab.sandbox");
        let real = dir.join("etc-fstab-stand-in");
        fs::write(&real, "# someone else's content\n").expect("seed the real stand-in");

        // No marker at all: nothing is known, so nothing can be claimed.
        assert_eq!(
            ExternalChangeGuard::from_marker(&draft, false).detect(&real),
            None
        );
        assert_eq!(
            ExternalChangeGuard::unchecked().detect(&real),
            None,
            "the no-marker target kinds behave exactly as they did before T36"
        );

        // A marker that differs, but the user has already seen the diff.
        fs::write(marker_of(&draft), "# what rustab knew\n").expect("seed the marker");
        assert!(ExternalChangeGuard::from_marker(&draft, false)
            .detect(&real)
            .is_some());
        assert_eq!(
            ExternalChangeGuard::from_marker(&draft, true).detect(&real),
            None,
            "an explicit write-anyway must silence the check for this one call"
        );

        // An unreadable (here: missing) real file establishes nothing.
        assert_eq!(
            ExternalChangeGuard::from_marker(&draft, false).detect(&no_real_fstab(&dir)),
            None
        );
    }

    /// The comparison is on bytes, and only the *reported* current content is
    /// lossy: a real file whose bytes are not UTF-8 cannot match a recorded
    /// expectation that is, so it is reported as a change rather than silently
    /// passing a round trip through `from_utf8_lossy`.
    #[test]
    fn a_non_utf8_real_file_is_reported_as_a_change_with_lossy_text() {
        let dir = TempDir::new("guard-non-utf8");
        let draft = dir.join("fstab.sandbox");
        let real = dir.join("etc-fstab-stand-in");
        fs::write(marker_of(&draft), "# plain text\n").expect("seed the marker");
        fs::write(&real, [b'#', 0xff, b'\n']).expect("seed non-UTF-8 bytes");

        let change = ExternalChangeGuard::from_marker(&draft, false)
            .detect(&real)
            .expect("non-UTF-8 bytes cannot match a UTF-8 expectation");
        assert_eq!(change.last_known, "# plain text\n");
        assert!(
            change.current.contains('\u{fffd}'),
            "the reported content is lossy by design: {:?}",
            change.current
        );

        // And the marker itself being non-UTF-8 means "no usable record",
        // rather than a comparison nothing could satisfy.
        fs::write(marker_of(&draft), [0xff, 0xfe]).expect("seed a non-UTF-8 marker");
        assert_eq!(
            ExternalChangeGuard::from_marker(&draft, false).last_known_real_content,
            None
        );
    }

    // -- T38: legacy `fstab.sandbox` -> `fstab.draft` file name migration ---

    /// A full legacy installation - draft, marker, an automatic backup and a
    /// manual backup, all still under the pre-T38 name - migrates onto the
    /// new name byte-exact, with the automatic backup's timestamp and the
    /// manual backup's `.manual` suffix both surviving verbatim. The renamed
    /// backups are also re-verified through [`fstab_core::writer::list_backups`]
    /// itself, so the proof is not just "a file with the right name exists"
    /// but "the backup grammar recognizes it under its new name".
    #[test]
    fn migration_renames_draft_marker_and_both_backup_kinds_byte_exact() {
        let dir = TempDir::new("target-migrate-full");
        let new_path = dir.join(SANDBOX_FILE_NAME);
        let legacy_path = dir.join(LEGACY_SANDBOX_FILE_NAME);
        fs::write(&legacy_path, "# legacy draft\n").expect("write legacy draft");
        let legacy_marker = marker_of(&legacy_path);
        fs::write(&legacy_marker, "# legacy marker\n").expect("write legacy marker");
        let legacy_auto_backup = dir.join(&format!(
            "{LEGACY_SANDBOX_FILE_NAME}.rustab-backup.0000000001.000000002"
        ));
        fs::write(&legacy_auto_backup, "# automatic backup\n").expect("write automatic backup");
        let legacy_manual_backup = dir.join(&format!(
            "{LEGACY_SANDBOX_FILE_NAME}.rustab-backup.0000000003.000000004.manual"
        ));
        fs::write(&legacy_manual_backup, "# manual backup\n").expect("write manual backup");

        migrate_legacy_sandbox_file_name(&new_path);

        assert!(
            !legacy_path.exists(),
            "the legacy draft must have been renamed away"
        );
        assert_eq!(
            fs::read_to_string(&new_path).expect("read"),
            "# legacy draft\n"
        );

        assert!(!legacy_marker.exists());
        assert_eq!(
            fs::read_to_string(marker_of(&new_path)).expect("read"),
            "# legacy marker\n"
        );

        let new_auto_backup = dir.join(&format!(
            "{SANDBOX_FILE_NAME}.rustab-backup.0000000001.000000002"
        ));
        assert!(!legacy_auto_backup.exists());
        assert_eq!(
            fs::read_to_string(&new_auto_backup).expect("read"),
            "# automatic backup\n"
        );

        let new_manual_backup = dir.join(&format!(
            "{SANDBOX_FILE_NAME}.rustab-backup.0000000003.000000004.manual"
        ));
        assert!(!legacy_manual_backup.exists());
        assert_eq!(
            fs::read_to_string(&new_manual_backup).expect("read"),
            "# manual backup\n"
        );

        let listed = fstab_core::writer::list_backups(&new_path).expect("listing");
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|backup| backup.path == new_auto_backup && !backup.manual));
        assert!(listed
            .iter()
            .any(|backup| backup.path == new_manual_backup && backup.manual));
    }

    /// Running the migration a second time - simulating a second launch - is
    /// a true no-op: the one-time gate is `new_path.exists()`, and once that
    /// is true nothing further is ever touched, even a legacy file that
    /// reappears afterwards (e.g. restored by hand).
    #[test]
    fn a_second_migration_call_is_a_true_no_op() {
        let dir = TempDir::new("target-migrate-twice");
        let new_path = dir.join(SANDBOX_FILE_NAME);
        let legacy_path = dir.join(LEGACY_SANDBOX_FILE_NAME);
        fs::write(&legacy_path, "# legacy draft\n").expect("write");

        migrate_legacy_sandbox_file_name(&new_path);
        assert!(new_path.exists(), "the first call must have migrated it");

        // A legacy file reappearing after migration (e.g. a manual restore)
        // must still be left alone, because `new_path` already exists.
        fs::write(&legacy_path, "# a legacy file that reappeared somehow\n").expect("write");
        migrate_legacy_sandbox_file_name(&new_path);

        assert_eq!(
            fs::read_to_string(&new_path).expect("read"),
            "# legacy draft\n",
            "the new path must not be touched again"
        );
        assert_eq!(
            fs::read_to_string(&legacy_path).expect("read"),
            "# a legacy file that reappeared somehow\n",
            "a legacy file present once new_path exists must be left exactly alone"
        );
    }

    /// A fresh install - neither the legacy nor the new file exists - is
    /// completely unaffected: no panic, and no file created by the migration
    /// step itself (the ordinary `seed_sandbox_file_if_missing` path is what
    /// creates the seeded file, untouched by this change).
    #[test]
    fn a_fresh_install_with_neither_file_is_unaffected_by_migration() {
        let dir = TempDir::new("target-migrate-fresh");
        let new_path = dir.join(SANDBOX_FILE_NAME);

        migrate_legacy_sandbox_file_name(&new_path);

        assert!(
            !new_path.exists(),
            "migration itself must not create anything"
        );
        assert!(!dir.join(LEGACY_SANDBOX_FILE_NAME).exists());
    }

    /// An unlikely but possible manual-intervention state: `new_path` already
    /// exists *and* a legacy file also happens to exist. Migration must not
    /// overwrite or touch either - the legacy file (and its marker) are left
    /// exactly alone.
    #[test]
    fn an_existing_new_path_leaves_a_coexisting_legacy_file_untouched() {
        let dir = TempDir::new("target-migrate-coexist");
        let new_path = dir.join(SANDBOX_FILE_NAME);
        let legacy_path = dir.join(LEGACY_SANDBOX_FILE_NAME);
        fs::write(&new_path, "# the current draft\n").expect("write");
        fs::write(&legacy_path, "# an old file that happens to coexist\n").expect("write");
        let legacy_marker = marker_of(&legacy_path);
        fs::write(&legacy_marker, "# an old marker\n").expect("write");

        migrate_legacy_sandbox_file_name(&new_path);

        assert_eq!(
            fs::read_to_string(&new_path).expect("read"),
            "# the current draft\n",
            "the new path must not be overwritten"
        );
        assert_eq!(
            fs::read_to_string(&legacy_path).expect("read"),
            "# an old file that happens to coexist\n",
            "the legacy file must be left exactly alone"
        );
        assert_eq!(
            fs::read_to_string(&legacy_marker).expect("read"),
            "# an old marker\n"
        );
    }

    /// A legacy marker and a legacy backup existing *without* a legacy draft
    /// file does not panic, and both still migrate independently - the main
    /// draft rename is skipped (there is nothing to rename), but that must
    /// not short-circuit the marker/backup migrations.
    #[test]
    fn markers_and_backups_migrate_independently_of_a_missing_legacy_draft() {
        let dir = TempDir::new("target-migrate-partial");
        let new_path = dir.join(SANDBOX_FILE_NAME);
        let legacy_path = dir.join(LEGACY_SANDBOX_FILE_NAME);
        // No legacy draft file at all - only a marker and a backup survive.
        let legacy_marker = marker_of(&legacy_path);
        fs::write(&legacy_marker, "# an orphaned legacy marker\n").expect("write");
        let legacy_backup = dir.join(&format!(
            "{LEGACY_SANDBOX_FILE_NAME}.rustab-backup.0000000005.000000006"
        ));
        fs::write(&legacy_backup, "# an orphaned legacy backup\n").expect("write");

        migrate_legacy_sandbox_file_name(&new_path);

        assert!(
            !new_path.exists(),
            "no draft existed to migrate, so none is created"
        );
        assert!(!legacy_marker.exists());
        assert_eq!(
            fs::read_to_string(marker_of(&new_path)).expect("read"),
            "# an orphaned legacy marker\n"
        );
        let new_backup = dir.join(&format!(
            "{SANDBOX_FILE_NAME}.rustab-backup.0000000005.000000006"
        ));
        assert!(!legacy_backup.exists());
        assert_eq!(
            fs::read_to_string(&new_backup).expect("read"),
            "# an orphaned legacy backup\n"
        );
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
