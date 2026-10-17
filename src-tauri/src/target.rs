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
//!    target, verbatim, in both debug and release builds. Such a target is
//!    written **outright**: whatever path it names is the only file a save
//!    touches (elevated if it happens to *be* `/etc/fstab`, in-process
//!    otherwise), with no second write anywhere - see
//!    `crate::commands::save_document_with`. This is the one way to run
//!    Rustab without it writing the real `/etc/fstab`.
//! 2. Otherwise the target is the per-user **working draft**,
//!    `$XDG_DATA_HOME/rustab/fstab.sandbox` (falling back to
//!    `$HOME/.local/share/rustab/fstab.sandbox`, and to a temp-dir path if
//!    neither variable is set). This is the file the GUI reads and edits, but
//!    since T34 it is *not* where a save ends up: a save writes the real
//!    `/etc/fstab` and the draft is then updated to match it. The draft's
//!    purpose is to keep in-progress, not-yet-saved edits across a restart or
//!    crash (`docs/SPEC.md` §7), not to withhold writes from the real file.
//!    If `/etc/fstab` cannot be read at all, a missing draft falls back to a
//!    harmless comment-only seed, so a fresh checkout still has something
//!    safe and valid to load.
//!
//! ## Draft refresh: conditional, not once-per-launch (T34)
//!
//! The draft is re-synced from `/etc/fstab` at most **once per process
//! launch** (see [`SandboxRefreshState`]), and only **if the real file's
//! current content differs from the content Rustab last knew it to have**.
//! "Last knew" is persisted in a marker file beside the draft (see
//! [`last_known_real_marker`]) and is updated at both events that mean
//! "Rustab now knows this is reality": a refresh, and a successful save of
//! the real file ([`sync_sandbox_after_save`]).
//!
//! That conditionality is the whole point: refreshing unconditionally - what
//! this module did before T34 - would discard an unsaved draft on every
//! single restart, which is exactly the work the draft exists to protect. So:
//!
//! - real content == marker content, and the draft file exists: do nothing at
//!   all, the draft (unsaved edits included) is left byte-for-byte as it was.
//! - real content != marker content, or there is no marker (fresh install, or
//!   a pre-T34 draft), or the draft file is gone: copy `/etc/fstab` over the
//!   draft and record the copied bytes in the marker. A missing marker is
//!   deliberately treated like a differing one - the same shape as the older
//!   "seed if missing" fallback, and the safe direction to be wrong in (the
//!   draft is replaced by current reality rather than kept stale forever).
//! - `/etc/fstab` unreadable: nothing is compared; a missing draft is seeded,
//!   an existing one is left alone.
//!
//! Within one run, every later resolution - and [`active_target`] is called
//! fresh by every command - skips the whole check, so the user's own edits
//! and saves survive the session that made them. Because two commands
//! resolve the target concurrently on every launch,
//! [`SandboxRefreshState`] is a completion *barrier*: the resolutions that do
//! not perform the one-time work wait for the one that does, so no command
//! can read the draft while a refresh is still in flight.
//!
//! The refresh is a plain copy of bytes, never a parse and re-serialize
//! round trip: a copy cannot drift from its source, a round trip can
//! (`docs/SPEC.md` §5). It is published with the same same-directory
//! temp-file-plus-`rename` swap `fstab_core::writer` uses for real saves, so
//! that any reader outside this process's barrier - a second Rustab launching
//! at the same moment, say - still sees whole content rather than a
//! mid-truncate file (see [`publish_bytes`]). It is also strictly
//! best-effort - an unreadable `/etc/fstab` (missing, or no permission)
//! leaves target resolution working on the seed/previous content rather than
//! failing, the same soft-failure stance this project takes towards absent
//! `lsblk`/`findmnt`.
//!
//! ## The marker's second job: external-change detection (T36)
//!
//! The same marker answers a second question, at the other end of a session:
//! "has the real `/etc/fstab` changed outside Rustab since we last synced with
//! it, right now, as a write is about to commit?" That is
//! [`ExternalChangeGuard`], and it compares the marker's recorded bytes
//! against `/etc/fstab`'s current bytes - never the draft against the real
//! file, since a draft differing from the marker is just unsaved edits. A
//! detected difference blocks the write and is reported to the frontend with
//! both sides of the diff (`crate::dto::WriteErrorKindDto::ExternalChange`);
//! the user can cancel, or ask for the write anyway, which threads
//! `allow_external_change` through and skips the check for that one request.
//!
//! Rule 2 deliberately applies to **release builds too**, and the real file
//! is still only ever *read* here, never written: the elevated writer
//! (`crate::privileged`) is the one and only thing that writes `/etc/fstab`.
//! See `docs/API_CONTRACT.md` §2 for the full reasoning and for the
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
//!
//! Since T34 the flag no longer answers "does a save reach `/etc/fstab`?" -
//! a [`TargetSource::SandboxDefault`] target has `is_real == false` and its
//! saves go to the real file anyway. What it still answers exactly is "is the
//! path in [`ActiveTarget::path`] itself the real file?", which is what the
//! [`TargetSource::Environment`] branch of a save needs in order to know
//! whether writing *that path* requires elevation.

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
const SANDBOX_FILE_NAME: &str = "fstab.sandbox";

/// Suffix of the marker file that records the real `/etc/fstab`'s content as
/// of the last time Rustab synced with it (see [`last_known_real_marker`]).
///
/// Deliberately distinct from every other naming scheme in play around these
/// files: `fstab_core::writer`'s backup grammar
/// (`<name>.rustab-backup.<secs>.<nanos>`, which `list_backups` scans for and
/// `prune_backups` deletes from), that module's save temp file
/// (`.<name>.rustab.tmp.<pid>`) and this module's publish temp file
/// (`.<name>.rustab-refresh.tmp.<pid>`). A marker can therefore never be
/// mistaken for a backup the user might restore or Rustab might prune.
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

/// Completion barrier for the once-per-process draft sync: it both elects the
/// one resolution that performs the sync *and* holds every concurrent
/// resolution back until that sync has actually finished.
///
/// "Sync" rather than "refresh" since T34: the elected resolution compares the
/// real `/etc/fstab` against the marker holding what Rustab last knew its
/// content to be and *then decides* whether a refresh is needed at all (see
/// [`refresh_sandbox_if_real_changed`]). The mechanism below is unchanged -
/// it is the T33 race fix and is deliberately kept exactly as it was; only
/// what the one-time closure decides to do changed.
///
/// Exactly one instance lives for the lifetime of the app process (inside
/// [`active_target`]), which is what makes "at most one sync per launch" mean
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
/// The first call in a process refreshes the draft from the real
/// `/etc/fstab` *if the real file has changed since Rustab last looked*;
/// later calls in the same process do nothing to it at all (see the module
/// docs). The refresh source is deliberately the hardcoded
/// [`REAL_FSTAB_PATH`] and not configurable - [`TARGET_PATH_ENV`] chooses the
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

/// Prepares the sandbox file - the working draft - (and its parent directory)
/// for use.
///
/// The first resolution of a process performs the one-time draft sync: it
/// replaces the draft with a byte-for-byte copy of `real_fstab_path` **only
/// if** that file's content differs from the marker recording what Rustab
/// last knew it to be (or there is no marker, or the draft file is gone) -
/// see [`refresh_sandbox_if_real_changed`] and the module docs. If the two
/// already agree, the draft is left completely alone, unsaved edits included;
/// that is the T34 behaviour change, and the reason the draft can be trusted
/// to survive a restart. If the real file could not be read or the copy could
/// not be made, it falls back to the long-standing behaviour instead - create
/// the file with [`SANDBOX_SEED`] if it does not exist yet, and leave a file
/// that does exist completely alone, including an empty one.
///
/// Every *other* resolution does not redo any of that, and - this is the
/// point of [`SandboxRefreshState`] being a barrier rather than a latch -
/// does not return until the first one has finished, so no command can read
/// the target mid-refresh.
///
/// "Does not redo any of that" includes the existence check: the one-time
/// work runs exactly once per process, where the pre-T33 latch-based version
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
/// even if the one-time sync already happened.
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
            if refresh_sandbox_if_real_changed(path, real_fstab_path) {
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

/// The once-per-launch draft sync: copies `real_fstab_path` over the draft
/// verbatim **if** it differs from what Rustab last recorded about it, and
/// records the copied bytes as the new "last known real content".
///
/// Returns whether the draft can be considered dealt with - `true` both when
/// a refresh happened and when none was needed - or `false` when the caller
/// should fall back to [`seed_sandbox_file_if_missing`] (the real file is
/// unreadable, or the copy could not be published).
///
/// ## What counts as "unchanged"
///
/// Exactly two things have to hold to skip the refresh: the marker beside the
/// draft holds byte-for-byte what `real_fstab_path` holds *now*, and the
/// draft file itself still exists. A missing or differing marker, or a
/// missing draft, all mean "refresh" - see the module docs for why a missing
/// marker deliberately errs that way.
///
/// ## Marker update ordering
///
/// The draft is published first and the marker only afterwards, and the
/// marker's own failure is swallowed. That order is chosen so the *worse*
/// inconsistency is impossible: a marker that is newer than the draft would
/// make the next launch skip a refresh the draft actually needed (a stale
/// draft, forever). A marker that is older than the draft - what a crash
/// between the two leaves behind - only costs the next launch one redundant
/// copy of bytes it already has, after which the marker is correct again.
fn refresh_sandbox_if_real_changed(sandbox_path: &Path, real_fstab_path: &Path) -> bool {
    let Ok(real_bytes) = fs::read(real_fstab_path) else {
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

/// The marker file recording the real `/etc/fstab`'s content as of Rustab's
/// last sync point: `<sandbox file name>.rustab-lastknown`, in the sandbox's
/// own directory.
///
/// ## Why a verbatim copy rather than a hash
///
/// The marker stores the bytes themselves, not a digest of them. The
/// workspace has no hashing dependency at all (see the root `Cargo.toml`:
/// `fstab-core` and `src-tauri` pull in serde, tauri and nothing
/// cryptographic), and the two alternatives were both worse for this job:
/// adding a crate to compare two ~1 KB files is disproportionate, and
/// hand-rolling a hash means writing - and then having to justify - a
/// collision story and a "this hash must stay stable across releases"
/// invariant that a plain copy simply does not have. `std::hash`'s own
/// hashers are explicitly *not* an option here: `DefaultHasher` is not
/// guaranteed stable across Rust versions or even process runs, and this
/// value has to be comparable across launches, which is the one thing it
/// cannot promise.
///
/// A copy costs the size of an fstab (kilobytes) in the user's data
/// directory, exposes nothing new (it is a copy of a world-readable file, and
/// the draft right next to it already holds the same content), is exactly
/// comparable with no false "unchanged" verdict possible, and is directly
/// inspectable when someone wonders why a refresh did or did not happen.
///
/// `None` only for a path with no final file-name component, which the
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

/// Updates the working draft **and** the last-known-real marker to `bytes`,
/// in that order - the one call `crate::commands`'s save path makes after the
/// elevated writer has successfully put those same bytes into the real
/// `/etc/fstab`.
///
/// Both halves are the same event: "Rustab now knows that this is reality."
/// Doing them together here, rather than leaving the caller to remember both,
/// is what keeps a save from being seen as external drift by the *next*
/// launch - without the marker update, [`refresh_sandbox_if_real_changed`]
/// would find the real file different from its stale marker and overwrite the
/// draft Rustab had just mirrored into.
///
/// Order and atomicity: each file is published with the same
/// temp-file-plus-`rename` swap a refresh uses, so no reader ever sees a
/// partial one, and the draft is published before the marker for the reason
/// spelled out in [`refresh_sandbox_if_real_changed`] - a crash in between
/// leaves the marker merely *stale*, which self-heals into one redundant copy
/// on the next launch, instead of leaving a marker that claims a draft is
/// current when it is not.
///
/// # Errors
///
/// The first `io::Error` of the two publishes. A failure here is **not** a
/// failed save: the real file already holds `bytes`. The caller surfaces it
/// as a non-fatal warning (see `crate::commands::save_document_with`), and
/// the next launch repairs the draft on its own, since the marker is then
/// either absent or stale and the real file wins.
pub(crate) fn sync_sandbox_after_save(sandbox_path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_bytes(sandbox_path, bytes)?;
    record_last_known_real(sandbox_path, bytes)
}

// ---------------------------------------------------------------------------
// External-change detection (T36)
// ---------------------------------------------------------------------------

/// What a real-file write knows about `/etc/fstab`'s content before it starts,
/// and whether the user has already chosen to overwrite an external change
/// (T36).
///
/// This is the whole input of the "did the real file change under us?" check,
/// travelling - as two flat fields - inside `crate::privileged`'s
/// `PrivilegedRequest::Write` so the check can be performed *again*, inside
/// the elevated process, immediately before
/// [`fstab_core::writer::write_document`] runs. That re-check is the
/// authoritative one: `pkexec` blocks on a human typing a password, and the
/// file can change during exactly that wait, so a check made only before the
/// prompt would leave the largest part of the window uncovered (see
/// `crate::commands::save_document_with` and `crate::privileged::handle_write`).
///
/// ## What is compared, and what is deliberately not
///
/// The comparison is "the **marker**'s recorded bytes vs. `/etc/fstab`'s
/// current bytes" - never the working draft against the real file. A draft
/// that differs from the marker is the ordinary state of having unsaved edits
/// and says nothing about anything outside Rustab; only a real file that no
/// longer matches what Rustab last recorded means someone else wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChangeGuard {
    /// The content Rustab last knew the real `/etc/fstab` to hold - the
    /// [`last_known_real_marker`]'s content, as recorded by the last refresh
    /// or the last successful save ([`sync_sandbox_after_save`]).
    ///
    /// `None` means Rustab has **no usable record** (no marker yet, an
    /// unreadable one, non-UTF-8 content, or a target kind that keeps no
    /// marker at all - see [`ExternalChangeGuard::unchecked`]). With nothing
    /// to compare against, no external change can be established and the
    /// write proceeds, which is exactly the pre-T36 behaviour. The
    /// alternative - blocking every write a best-effort marker write had
    /// failed for - would make the file uneditable for a reason the user can
    /// neither see nor fix.
    pub last_known_real_content: Option<String>,
    /// `true` only when the user has been shown the diff of what changed
    /// externally and explicitly asked for the write anyway. It makes
    /// [`ExternalChangeGuard::detect`] report nothing, on both sides, for this
    /// one request.
    ///
    /// Deliberately a separate signal from
    /// [`Self::last_known_real_content`] rather than "send no expectation":
    /// "the user confirmed" and "Rustab never knew" are different states, and
    /// collapsing them would make the confirmed case indistinguishable from a
    /// lost marker in a bug report.
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
    /// The comparison itself is byte-exact against the recorded bytes. Only
    /// the *reported* current content is lossy
    /// ([`String::from_utf8_lossy`]) - it exists to be rendered as a diff, and
    /// a real file whose bytes are not UTF-8 necessarily differs from a
    /// recorded expectation that is, so the verdict never depends on that
    /// conversion.
    pub fn detect(&self, real_fstab: &Path) -> Option<ExternalChange> {
        if self.allow_external_change {
            return None;
        }
        let last_known = self.last_known_real_content.as_deref()?;
        let current = fs::read(real_fstab).ok()?;
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
/// ## Why this is a temp file plus `rename`, not an `fs::write`
///
/// Writing the bytes straight onto `path` would be an open-truncate-write,
/// which is *not* atomic with respect to a concurrent reader of the same
/// path. Within this process [`SandboxRefreshState`] already keeps the other
/// startup resolution from reading until the sync has returned, but that
/// barrier only binds callers that share it: a second Rustab process
/// launching at the same moment, or anything else reading the draft, has no
/// such ordering. With a plain write such a reader can land between the
/// truncate and the completed write and see an empty document.
///
/// So the bytes go to a sibling temp file in the draft's own directory and
/// are then `rename`d onto it, the same way [`fstab_core::writer`] publishes
/// real saves (and for the same reason - `rename(2)` is atomic *within one
/// filesystem*, which is precisely what "a sibling in the same directory"
/// guarantees). A concurrent reader therefore observes either the complete
/// previous content or the complete new content, never a partial file.
///
/// A literal byte copy, never a parse-and-reserialize round trip: the copy is
/// then incapable of differing from its source, which is exactly the property
/// `docs/SPEC.md` §5 cares about.
///
/// This is deliberately *only* the atomicity half of a real save: no
/// validation, no backup, no permission carry-over, no `fsync`. The draft is
/// a convenience copy in a scratch directory, not a durable publish - if a
/// crash mid-publish loses it, the next launch syncs again. It also means a
/// draft path that is somehow a symlink gets replaced by a regular file
/// rather than written *through*, which for a scratch file is the safer of
/// the two.
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
/// `.<destination file name>.rustab-refresh.tmp.<pid>`, in the destination's
/// own directory.
///
/// Same directory, so the `rename` onto the destination cannot cross a
/// filesystem boundary and is therefore atomic. Leading dot so a stray
/// leftover stays out of the way. The `.rustab-refresh.tmp.` infix is
/// deliberately distinct from both naming schemes `fstab_core::writer` owns -
/// the backup grammar (`<name>.rustab-backup.<secs>.<nanos>`, which
/// `list_backups` scans for) and that module's own save temp file
/// (`.<name>.rustab.tmp.<pid>`) - so this file can never be mistaken for a
/// backup the user might restore, nor for a save in flight. The pid keeps two
/// Rustab processes launching at the same instant from staging into the same
/// temp path.
///
/// `None` only for a path with no final file-name component, which neither
/// the constructed sandbox path nor its marker ever is; returning it rather
/// than panicking keeps the whole sync on its one best-effort failure route.
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

        let target = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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

        let first = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("first process");
        fs::write(&first.path, "# left over from the previous session\n").expect("write");

        // Something outside Rustab edited the real file between launches.
        fs::write(&real_fstab, REAL_FSTAB_SAMPLE).expect("write");

        let second = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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

        let first = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("first process");
        // The user edits in that session and does not save.
        let draft = "# an unsaved draft the user wants back after a restart\n";
        fs::write(&first.path, draft).expect("write");

        // A new launch. Nothing touched the real file in the meantime.
        let second = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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

        let target = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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

        let first = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
            .expect("first process");
        fs::remove_file(&first.path).expect("remove the draft");
        assert!(
            marker_of(&first.path).exists(),
            "the marker must have survived"
        );

        let second = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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

        let first = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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
        let second = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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
            refresh_sandbox_if_real_changed(&sandbox_file, &real_fstab),
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

        let target = resolve(None, &sandbox_dir, &real_fstab, &SandboxRefreshState::new())
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
            !refresh_sandbox_if_real_changed(&sandbox_path, &real_fstab),
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
