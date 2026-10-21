//! Where the real `/etc/fstab`'s **manually managed** backups live for this
//! process - beside the file itself, as they always have, or in a fallback
//! directory under the user's own data directory when this process cannot
//! manage files in `/etc/` at all.
//!
//! ## The problem this exists for (T39)
//!
//! Flathub's `flatpak-builder-lint` hard-rejects `--filesystem=/etc:rw` as a
//! reserved path, so Rustab's Flatpak manifest grants only
//! `--filesystem=/etc/fstab:rw` - a **single-file** bind mount. That is enough
//! for everything that reads or writes the file itself:
//!
//! - reading `/etc/fstab` (the draft refresh, [`crate::commands::list_entries`],
//!   the external-change check) still works, because the file is exposed;
//! - *writing* it still works, because every write goes through the elevated
//!   `pkexec` re-exec ([`crate::privileged`]), which runs fully unsandboxed
//!   with real root access to all of `/etc/` - including the automatic backup
//!   [`fstab_core::writer::write_document`] takes beside the file on every
//!   save, which therefore keeps landing in `/etc/`, unchanged.
//!
//! What it is *not* enough for is the three **manual-management** operations
//! the Backups panel offers - `list_backups`, `create_backup`,
//! `delete_backup`. Those need the real file's *directory*, and the directory
//! this process sees at `/etc` inside the sandbox is the runtime's own `/etc`,
//! not the host's: enumerating it would never find the host's backups, and
//! nothing this process creates in it would be the file the elevated writer's
//! backups sit next to. So for the real target those three operations move, as
//! a set, into [`fallback_dir`] - a directory this process genuinely owns.
//!
//! ## What this module deliberately does *not* change
//!
//! Nothing about writing. [`fstab_core::writer`] and [`crate::privileged`] are
//! untouched by T39: a save still writes the real `/etc/fstab` through the
//! elevated helper and still leaves its automatic backup beside it in `/etc/`.
//! That is the whole point of doing it this way - the rescue-environment
//! colocation property every *automatic* backup has is preserved, and the
//! privilege-escalation path is not weakened or even edited.
//!
//! **The accepted consequence, stated plainly** (it is a real trade-off, and
//! the frontend says so too - `docs/UI_DESIGN.md` §7): in fallback mode the
//! automatic backups taken during ordinary saves are still in `/etc/`, and are
//! simply **not visible** in `list_backups`'s result, because this process
//! still cannot enumerate that directory. Only backups explicitly created via
//! `create_backup` land in - and are visible from - the fallback directory.
//! The invisible ones are still perfectly recoverable from a rescue
//! environment (which is the property the manifest's comment cares about); the
//! in-app list just cannot prove they exist.
//!
//! ## How "can this process manage files in `/etc/`?" is decided
//!
//! See [`detect`] for the probe itself and - since the obvious probe is the
//! wrong one - for why it asks what it asks.

use crate::target;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory, relative to Rustab's own data directory
/// ([`target::rustab_data_dir`]), holding the real `/etc/fstab`'s backups in
/// fallback mode.
///
/// Named after what it holds rather than why it exists: someone finding this
/// directory in `~/.local/share/rustab/` months later needs to know *whose*
/// backups these are, which `etc-fstab-backups` says and `fallback` would not.
const FALLBACK_DIR_NAME: &str = "etc-fstab-backups";

/// File name of the mirror file inside [`fallback_dir`] - see
/// [`refresh_mirror`] for what it is for.
///
/// Deliberately `/etc/fstab`'s own final component, so that every backup taken
/// beside it carries byte-for-byte the name it would have carried in `/etc/`
/// (`fstab.rustab-backup.<secs>.<nanos>`, optionally `.manual`). A user who
/// copies one of these files back into `/etc/` by hand therefore gets a file
/// `fstab_core::writer::list_backups` still recognizes as a backup of the real
/// `/etc/fstab`. The relationship is pinned by
/// [`tests::the_mirror_file_is_named_exactly_like_the_real_fstab`] rather than
/// by computing it from [`target::REAL_FSTAB_PATH`] at runtime, so the name
/// stays a plain constant and the agreement is still enforced.
const MIRROR_FILE_NAME: &str = "fstab";

/// The file whose existence means "this process is running inside a Flatpak
/// sandbox", i.e. that the `/etc` it can see is the runtime's and not the
/// host's.
///
/// Flatpak mounts this file into every sandbox it starts and documents it as
/// the way an application detects that it is sandboxed; it is the same signal
/// GLib's own containment check uses. Checked as a path rather than through
/// the `FLATPAK_ID` environment variable because an environment variable can
/// be unset, inherited or forged by whatever launched the process, while this
/// mount is part of the sandbox itself.
const SANDBOX_MARKER_PATH: &str = "/.flatpak-info";

/// Where this process's manual backups of the real `/etc/fstab` live.
///
/// Only ever decided for the **real** target: an explicit
/// `RUSTAB_TARGET_PATH` naming some other file keeps its backups in its own
/// directory and never consults this type at all (see
/// `crate::commands::backup_fallback_dir`, the one place that applies this
/// value to a target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackupLocation {
    /// Beside the file itself, in `/etc/` - the long-standing behaviour, and
    /// still what every non-sandboxed launch gets. Creating and deleting
    /// backups there needs the elevated helper, because `/etc/` is root-owned.
    Colocated,
    /// In `dir`, because this process cannot manage files in the real file's
    /// own directory. Creating and deleting backups there needs **no**
    /// elevation at all: the directory belongs to the user running Rustab.
    Fallback {
        /// The directory the backups live in - [`fallback_dir`] in production.
        dir: PathBuf,
    },
}

impl BackupLocation {
    /// The fallback directory, or `None` when backups are colocated.
    ///
    /// The one accessor callers need: "is this fallback mode, and if so,
    /// where?" is a single question, and answering it with an `Option` keeps
    /// the two halves from being asked separately and then disagreeing.
    pub(crate) fn fallback_dir(&self) -> Option<&Path> {
        match self {
            BackupLocation::Colocated => None,
            BackupLocation::Fallback { dir } => Some(dir),
        }
    }
}

/// The real `/etc/fstab`'s backup location for this process, decided once and
/// then reused for the rest of the process's life.
///
/// The thin production wrapper around [`detect`], in the same shape
/// [`target::active_target`] wraps `target::resolve`: every input [`detect`]
/// needs is a parameter of it, and this function is the only place that fills
/// those parameters in with the real machine's paths. That is what lets
/// `cargo test` exercise the whole decision against temp directories and never
/// read, list or write the machine's own `/etc/`.
///
/// ## Why a plain `OnceLock` and no barrier
///
/// Unlike [`target`]'s `SandboxRefreshState`, this needs no completion
/// *barrier*. The one-time work here has no side effect a later caller depends
/// on having finished - it answers a question about the filesystem and
/// (best-effort) creates a directory. `OnceLock::get_or_init` already blocks
/// concurrent callers until the first initialization returns and hands them
/// all the same value, which is all that is wanted: one probe per process, one
/// answer per process, so the Backups panel cannot describe one location while
/// a delete acts on another.
pub(crate) fn real_target_location() -> &'static BackupLocation {
    static LOCATION: OnceLock<BackupLocation> = OnceLock::new();
    LOCATION.get_or_init(|| {
        detect(
            &crate::backups::containing_dir(Path::new(target::REAL_FSTAB_PATH)),
            Path::new(SANDBOX_MARKER_PATH),
            &fallback_dir(),
        )
    })
}

/// Decides whether the real `/etc/fstab`'s backups can be managed in
/// `colocated_dir` (its own directory) by this process, falling back to
/// `fallback_dir` when they cannot.
///
/// Every input is a parameter so that the tests below can drive every branch
/// against their own temp directories - the same reason `target::resolve`
/// takes the sandbox directory and the real file's path rather than reading
/// the constants itself.
///
/// ## What is probed, and why not the obvious thing
///
/// Two conditions, either of which selects fallback mode:
///
/// 1. **`sandbox_marker` exists.** This process is inside a Flatpak sandbox,
///    so the directory it sees at `colocated_dir` is the runtime's `/etc`, not
///    the host's - whatever its permissions say. Backups created there would
///    not be beside the real file, and the host's backups (the ones the
///    elevated writer leaves there on every save) would never appear in a
///    listing of it. This is the condition T39 is actually about.
/// 2. **`colocated_dir` cannot be enumerated at all** (`fs::read_dir` fails).
///    Belt and braces for any other confinement that leaves the file readable
///    but its directory unusable: without a listing there is no backup list,
///    so there is nothing a colocated mode could offer.
///
/// What this deliberately does **not** probe is whether *this* process can
/// **write** to `colocated_dir`, which was the first instinct and is the wrong
/// question - it would answer "no" on every ordinary Linux install, where
/// `/etc` is root-owned `0755` and the unprivileged GUI process cannot create
/// a file in it either. Colocated mode has never needed that permission:
/// creating and deleting a backup in `/etc/` is done by the elevated helper,
/// as root, which can always write there (`crate::commands::backups_are_elevated`).
/// Probing for it would therefore have moved every normal installation into
/// fallback mode and hidden the backups those users can see today - a
/// regression, not a fix.
///
/// ## The one side effect, and why it is here
///
/// Choosing fallback mode creates `fallback_dir` (recursively, best-effort).
/// A probe creating a directory is worth justifying: it is here because
/// selecting that directory *is* the commitment to using it, and the
/// alternative - creating it lazily in each of the three operations - means
/// `list_backups` reporting "could not list the backups in ...: No such file
/// or directory" the first time the panel opens, on a perfectly healthy
/// install. The failure is ignored for the same reason: a directory that could
/// not be created will be reported, with the real `io::Error`, by the first
/// operation that needs it, and failing the probe itself would leave the
/// session with no backup location at all. The side effect is on a
/// caller-supplied path, so this stays usable from tests.
pub(crate) fn detect(
    colocated_dir: &Path,
    sandbox_marker: &Path,
    fallback_dir: &Path,
) -> BackupLocation {
    if !sandbox_marker.exists() && fs::read_dir(colocated_dir).is_ok() {
        return BackupLocation::Colocated;
    }
    let _ = fs::create_dir_all(fallback_dir);
    BackupLocation::Fallback {
        dir: fallback_dir.to_path_buf(),
    }
}

/// The directory fallback-mode backups live in:
/// `<`[`target::rustab_data_dir`]`>/etc-fstab-backups`, i.e.
/// `$XDG_DATA_HOME/rustab/etc-fstab-backups` (else
/// `$HOME/.local/share/rustab/etc-fstab-backups`, else a temp-dir path, which
/// is exactly the chain the working draft already falls back through).
///
/// Built on [`target::rustab_data_dir`] rather than resolving XDG again here:
/// two copies of "where is Rustab's own data directory" is a future
/// disagreement waiting to happen, and the draft and these backups must land
/// under the same root for a user looking for either to find both.
fn fallback_dir() -> PathBuf {
    target::rustab_data_dir().join(FALLBACK_DIR_NAME)
}

/// The mirror file inside `fallback_dir` - the file every backup operation is
/// pointed at in fallback mode.
///
/// ## Why a mirror file at all
///
/// Every function that deals with backups - [`fstab_core::writer::create_backup`],
/// [`fstab_core::writer::list_backups`], [`crate::backups::resolve_requested`],
/// [`crate::backups::containing_dir`] - derives "the backup directory" purely
/// from "the target file's own parent directory". Pointing those functions at
/// a file *inside* the fallback directory therefore makes all of them operate
/// on that directory, with no change to a single signature, no new parameter
/// threaded into `fstab_core::writer`, and no second implementation of the
/// backup naming grammar. The mirror is what makes T39 a redirection rather
/// than a rewrite.
pub(crate) fn mirror_path(fallback_dir: &Path) -> PathBuf {
    fallback_dir.join(MIRROR_FILE_NAME)
}

/// Refreshes the mirror file in `fallback_dir` so that it holds `source`'s
/// current bytes, creating the directory if needed.
///
/// Called immediately before a fallback-mode `create_backup` and nowhere else:
/// that is the one operation whose result has to *be* the real file's content
/// as of now. Listing and deleting only enumerate or remove files that are
/// already in the directory, so neither needs a fresh mirror and neither pays
/// for one.
///
/// A literal byte copy, never a parse-and-reserialize round trip, for the same
/// reason `target`'s draft refresh is one: a copy cannot drift from its source
/// (`docs/SPEC.md` §5).
///
/// ## Why a plain `fs::write` and not the temp-file-plus-`rename` ceremony
///
/// `target::publish_bytes` goes to a sibling temp file and `rename`s it onto
/// the working draft because that draft has readers outside this process's
/// control (a second Rustab launching at the same moment), and an
/// open-truncate-write is not atomic against them. This file has no such
/// reader: it is internal staging, written and then immediately read by the
/// `create_backup` call that asked for it, in that order, in this process. No
/// `fsync` either - the durable artifact of the operation is the *backup*
/// `fstab_core::writer::create_backup` makes (which does `fsync` its copy),
/// and a mirror lost to a crash is re-created by the next refresh from the
/// real file, which is always the authority.
///
/// # Errors
///
/// The underlying [`io::Error`]: `source` could not be read (including
/// [`io::ErrorKind::NotFound`] for a real file that does not exist yet, which
/// the caller reports as "nothing to back up"), the directory could not be
/// created, or the mirror could not be written.
pub(crate) fn refresh_mirror(fallback_dir: &Path, source: &Path) -> io::Result<()> {
    let bytes = fs::read(source)?;
    fs::create_dir_all(fallback_dir)?;
    fs::write(mirror_path(fallback_dir), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A sandbox marker path that does not exist - i.e. "not sandboxed", the
    /// state every non-Flatpak launch is in.
    fn no_sandbox(dir: &TempDir) -> PathBuf {
        dir.join("not-a-flatpak-info")
    }

    /// The headline for every existing installation: a listable directory and
    /// no sandbox marker means colocated mode, with no fallback directory
    /// created anywhere.
    #[test]
    fn a_listable_directory_outside_a_sandbox_stays_colocated() {
        let dir = TempDir::new("location-colocated");
        let etc = dir.join("etc");
        fs::create_dir_all(&etc).expect("mkdir");
        let fallback = dir.join("data/rustab/etc-fstab-backups");

        let location = detect(&etc, &no_sandbox(&dir), &fallback);

        assert_eq!(location, BackupLocation::Colocated);
        assert_eq!(location.fallback_dir(), None);
        assert!(
            !fallback.exists(),
            "colocated mode must not create a fallback directory"
        );
    }

    /// A root-owned, **unwritable** directory is still colocated: creating and
    /// deleting backups there is the elevated helper's job, not this
    /// process's, so this process's own write permission is not part of the
    /// question (see [`detect`]'s docs - this is the probe that was
    /// deliberately not written).
    #[test]
    fn a_directory_this_process_cannot_write_is_still_colocated() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("location-readonly");
        let etc = dir.join("etc");
        fs::create_dir_all(&etc).expect("mkdir");
        fs::write(etc.join("fstab"), "# like /etc/fstab\n").expect("seed");
        // r-xr-xr-x: listable and readable, not writable - exactly `/etc`.
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).expect("chmod");

        let location = detect(&etc, &no_sandbox(&dir), &dir.join("fallback"));

        // Restore write permission before the `TempDir`'s `Drop` has to clean
        // the directory up, so a passing test leaves nothing behind.
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).expect("chmod back");

        assert_eq!(
            location,
            BackupLocation::Colocated,
            "an unwritable but listable directory is the normal /etc, not a fallback case"
        );
    }

    /// The T39 case: inside a sandbox the directory is perfectly listable -
    /// it is just the wrong directory - so the marker, not the permissions,
    /// is what selects fallback mode.
    #[test]
    fn a_sandbox_marker_selects_the_fallback_directory() {
        let dir = TempDir::new("location-sandboxed");
        let etc = dir.join("etc");
        fs::create_dir_all(&etc).expect("mkdir");
        fs::write(etc.join("passwd"), "# a runtime's own /etc\n").expect("seed");
        let marker = dir.join("flatpak-info");
        fs::write(&marker, "[Application]\n").expect("seed the marker");
        let fallback = dir.join("data/rustab/etc-fstab-backups");

        let location = detect(&etc, &marker, &fallback);

        assert_eq!(
            location,
            BackupLocation::Fallback {
                dir: fallback.clone()
            }
        );
        assert_eq!(location.fallback_dir(), Some(fallback.as_path()));
        assert!(
            fallback.is_dir(),
            "selecting fallback mode must leave the directory ready to use"
        );
    }

    /// The second, belt-and-braces condition: a directory that cannot be
    /// enumerated at all offers no backup list, so there is nothing colocated
    /// mode could do with it.
    #[test]
    fn an_unlistable_directory_selects_the_fallback_directory() {
        let dir = TempDir::new("location-unlistable");
        let missing = dir.join("etc-that-does-not-exist");
        let fallback = dir.join("data/rustab/etc-fstab-backups");

        let location = detect(&missing, &no_sandbox(&dir), &fallback);

        assert_eq!(location.fallback_dir(), Some(fallback.as_path()));
        assert!(fallback.is_dir());
    }

    /// The fallback directory must not be *inside* `/etc` or anywhere else the
    /// sandbox cannot reach, and must sit under Rustab's own data directory -
    /// the same root the working draft lives in. Asserted against the real
    /// production resolution, not an injected one, so a mistake in
    /// [`fallback_dir`] itself is caught too.
    #[test]
    fn the_real_fallback_directory_is_under_rustabs_own_data_dir() {
        let resolved = fallback_dir();
        assert_eq!(resolved.parent(), Some(target::rustab_data_dir().as_path()));
        assert!(resolved.ends_with(FALLBACK_DIR_NAME));
        assert!(
            !resolved.starts_with("/etc"),
            "the fallback must never be in /etc: {}",
            resolved.display()
        );
    }

    /// The mirror file's name is `/etc/fstab`'s own, so backups taken beside
    /// it are named exactly as they would have been in `/etc/` (see
    /// [`MIRROR_FILE_NAME`]).
    #[test]
    fn the_mirror_file_is_named_exactly_like_the_real_fstab() {
        assert_eq!(
            Path::new(target::REAL_FSTAB_PATH).file_name(),
            Some(std::ffi::OsStr::new(MIRROR_FILE_NAME))
        );
        assert_eq!(
            mirror_path(Path::new("/data/etc-fstab-backups")),
            Path::new("/data/etc-fstab-backups/fstab")
        );
    }

    /// A refresh is a byte-for-byte copy of the source as of right now,
    /// creating the directory on the way if it is not there yet, and
    /// overwriting whatever the mirror held before.
    #[test]
    fn a_refresh_copies_the_sources_current_bytes_onto_the_mirror() {
        let dir = TempDir::new("location-refresh");
        let source = dir.join("real-fstab");
        // Content a parse-and-reserialize round trip could plausibly
        // normalize, so byte equality really does prove a literal copy.
        let first = "UUID=1111\t/\text4\trw,relatime\t0 1";
        fs::write(&source, first).expect("seed");
        let fallback = dir.join("data/rustab/etc-fstab-backups");

        refresh_mirror(&fallback, &source).expect("the first refresh must succeed");
        assert_eq!(
            fs::read(mirror_path(&fallback)).expect("read the mirror"),
            first.as_bytes()
        );

        let second = "LABEL=data   /mnt/data   xfs   defaults,nofail   0   2\n";
        fs::write(&source, second).expect("change the source");
        refresh_mirror(&fallback, &source).expect("the second refresh must succeed");
        assert_eq!(
            fs::read(mirror_path(&fallback)).expect("read the mirror"),
            second.as_bytes(),
            "a refresh must publish the source's content as of now"
        );
    }

    /// A source that does not exist is reported as `NotFound` rather than
    /// quietly leaving a stale mirror behind for `create_backup` to copy -
    /// which is what lets the caller turn it into "there is nothing to back
    /// up yet".
    #[test]
    fn a_refresh_from_a_missing_source_reports_not_found() {
        let dir = TempDir::new("location-refresh-missing");
        let fallback = dir.join("fallback");
        let stale = mirror_path(&fallback);
        fs::create_dir_all(&fallback).expect("mkdir");
        fs::write(&stale, "# stale\n").expect("seed a stale mirror");

        let error = refresh_mirror(&fallback, &dir.join("no-such-fstab"))
            .expect_err("a missing source must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            fs::read_to_string(&stale).expect("read"),
            "# stale\n",
            "a failed refresh must not have touched the mirror"
        );
    }

    // There is deliberately no test of [`real_target_location`] itself: it is
    // the one function here that probes the machine's own `/etc`, and
    // `cargo test` must never read, list or write that directory (the same
    // rule `target`'s tests follow for the real `/etc/fstab`). Everything it
    // decides is decided by [`detect`] and [`fallback_dir`], both of which are
    // covered above against temp directories; what is left in it is the two
    // production constants and `OnceLock::get_or_init`.
}
