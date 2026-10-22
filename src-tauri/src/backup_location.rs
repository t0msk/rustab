//! Where the real `/etc/fstab`'s **manually managed** backups live for this
//! process - beside the file itself, as always, or in a fallback directory
//! under the user's own data directory when this process cannot manage
//! files in `/etc/` at all.
//!
//! ## The problem this exists for
//!
//! Flatpak reserves the whole `/etc` path space: the `/etc` a sandboxed
//! process sees is the runtime's, and no `--filesystem=` grant under `/etc`
//! is honoured, not even the single-file `/etc/fstab:rw`. So a sandboxed
//! Rustab can't enumerate the real backup directory, and anything it
//! created at `/etc/…` would land in the runtime's throwaway `/etc`, not
//! beside the file the user cares about. For the real target, the three
//! manual-management operations the Backups panel offers (`list_backups`,
//! `create_backup`, `delete_backup`) therefore move as a set into
//! [`fallback_dir`], a directory this process genuinely owns.
//!
//! Reads and writes of the real `/etc/fstab` itself aren't affected: both
//! go through `flatpak-spawn --host` (see [`crate::host`]), so a save's
//! automatic backup really does land beside the real file, with the usual
//! rescue-environment colocation property.
//!
//! ## Why the three manual-management operations stay in the fallback dir
//!
//! Since the elevated helper now runs as real host root, `create_backup`/
//! `delete_backup` for the real target *could* move back into `/etc/`. They
//! don't, because `list_backups` can't: it's deliberately unprivileged
//! (`/etc/` is world-readable; a password prompt to read a directory would
//! be absurd), so it runs in the sandboxed GUI process via
//! [`fstab_core::writer::list_backups`]'s own `read_dir`, with no seam to
//! route that through the host.
//!
//! That makes the three a package deal: moving creation/deletion into
//! `/etc/` while listing stays in the fallback dir would mean a backup the
//! user just created never appearing in the list it was created from. The
//! alternative - a second, list-only, never-elevated re-exec flag that
//! host-spawns this binary to list/read/resolve backup paths on the host -
//! was rejected: it would duplicate [`crate::privileged`]'s whole security
//! surface and host-library fragility for a routine read, add a password
//! prompt to a currently-instant manual backup, and double the number of
//! hidden re-exec entry points the module's auditability story relies on
//! staying at one.
//!
//! **The accepted consequence** (also stated in `docs/UI_DESIGN.md` §7):
//! in fallback mode, automatic backups from ordinary saves sit beside the
//! real `/etc/fstab` but are **not visible** in `list_backups`'s result,
//! since this process can't enumerate that directory. Only backups
//! explicitly created via `create_backup` land in, and are visible and
//! restorable from, the fallback directory.
//!
//! ## The fallback's second job
//!
//! It also serves as defense-in-depth for a sandbox whose host access is
//! unavailable (no `flatpak-spawn`, the `org.freedesktop.Flatpak` talk-name
//! missing or revoked, or the portal denied). Such a session can't read or
//! save the real `/etc/fstab` at all, but can still take, list, and keep
//! manual backups in a directory it owns - strictly better than no backup
//! surface at all.

use crate::host::{self, RealFileReader};
use crate::target;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory, relative to Rustab's own data directory
/// ([`target::rustab_data_dir`]), holding the real `/etc/fstab`'s backups
/// in fallback mode.
///
/// Named after what it holds, not why it exists, so someone finding it
/// under `~/.local/share/rustab/` later knows whose backups these are.
const FALLBACK_DIR_NAME: &str = "etc-fstab-backups";

/// File name of the mirror file inside [`fallback_dir`] - see
/// [`refresh_mirror`] for what it's for.
///
/// Deliberately `/etc/fstab`'s own final component, so every backup taken
/// beside it carries the exact name it would have carried in `/etc/`
/// (`fstab.rustab-backup.<secs>.<nanos>`, optionally `.manual`) - a user
/// copying one back into `/etc/` by hand still gets a recognized backup
/// name. Pinned by
/// [`tests::the_mirror_file_is_named_exactly_like_the_real_fstab`] rather
/// than computed from [`target::REAL_FSTAB_PATH`] at runtime.
const MIRROR_FILE_NAME: &str = "fstab";

/// Environment variable that, when set to any non-empty value, forces
/// [`real_target_location`] to report [`BackupLocation::Fallback`] without
/// consulting [`detect`] or touching `/etc` at all.
///
/// A manual testing aid for seeing fallback behavior without a real
/// Flatpak sandbox - faking [`host::SANDBOX_MARKER_PATH`] doesn't work for
/// this, since GTK's own icon loading checks the same marker and aborts
/// outside a real sandbox. Not a documented end-user interface.
const FORCE_FALLBACK_ENV_VAR: &str = "RUSTAB_FORCE_BACKUP_FALLBACK";

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

/// The real `/etc/fstab`'s backup location for this process, decided once
/// and reused for the rest of the process's life.
///
/// A thin production wrapper around [`detect`]: this is the only place
/// that fills in the real machine's paths, so `cargo test` can exercise
/// the whole decision against temp directories instead.
///
/// A plain `OnceLock`, no completion barrier like [`target`]'s
/// `SandboxRefreshState` needs: the one-time work here has no side effect
/// a later caller depends on having finished. `OnceLock::get_or_init`
/// already gives one probe, one answer per process, so the Backups panel
/// can't describe one location while a delete acts on another.
pub(crate) fn real_target_location() -> &'static BackupLocation {
    static LOCATION: OnceLock<BackupLocation> = OnceLock::new();
    LOCATION.get_or_init(|| {
        if std::env::var_os(FORCE_FALLBACK_ENV_VAR).is_some_and(|value| !value.is_empty()) {
            let dir = fallback_dir();
            let _ = fs::create_dir_all(&dir);
            return BackupLocation::Fallback { dir };
        }
        detect(
            &crate::backups::containing_dir(Path::new(target::REAL_FSTAB_PATH)),
            Path::new(host::SANDBOX_MARKER_PATH),
            &fallback_dir(),
        )
    })
}

/// Decides whether the real `/etc/fstab`'s backups can be managed in
/// `colocated_dir` (its own directory) by this process, falling back to
/// `fallback_dir` when they can't.
///
/// Every input is a parameter so the tests below can drive every branch
/// against their own temp directories.
///
/// ## The question this asks
///
/// Not "is this process sandboxed?" and not "can this process reach the
/// real `/etc/` by any means?" (yes, via [`crate::host`]) - it's "can
/// **this process**, using the mechanism the three manual-management
/// operations actually use, enumerate and manage the real file's own
/// directory?" `list_backups` is unprivileged by design and does a
/// `read_dir` **in this process** via [`fstab_core::writer::list_backups`],
/// which has no sandbox-crossing seam. So the answer is "no" whenever the
/// directory this process would enumerate isn't the host's real one.
///
/// Two conditions select fallback mode:
///
/// 1. **`sandbox_marker` exists.** Inside a Flatpak sandbox,
///    `colocated_dir` is the runtime's `/etc`, not the host's - listable,
///    but the wrong directory, so it would never find the real backups or
///    put new ones beside the real file.
/// 2. **`colocated_dir` can't be enumerated at all** (`fs::read_dir` fails) -
///    belt and braces for any other confinement.
///
/// Deliberately **not** probed: whether this process can *write* to
/// `colocated_dir` (would wrongly answer "no" on every ordinary install,
/// where `/etc` is root-owned `0755` - creating/deleting there is the
/// elevated helper's job, not this process's), and whether host-spawn
/// works right now (can't change this function's result either way, since
/// listing never uses it; observable instead where it matters, via a
/// save's own error or a draft that failed to refresh).
///
/// ## The one side effect
///
/// Choosing fallback mode creates `fallback_dir` (recursively, best-effort)
/// here rather than lazily in each of the three operations, so
/// `list_backups` doesn't report "No such file or directory" the first
/// time the panel opens on a healthy install. Failure to create it is
/// ignored here; the first operation that actually needs it reports the
/// real `io::Error`.
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
/// `<`[`target::rustab_data_dir`]`>/etc-fstab-backups`.
///
/// Built on [`target::rustab_data_dir`] rather than resolving XDG again
/// here, so the draft and these backups always land under the same root.
fn fallback_dir() -> PathBuf {
    target::rustab_data_dir().join(FALLBACK_DIR_NAME)
}

/// The mirror file inside `fallback_dir` - the file every backup operation
/// is pointed at in fallback mode.
///
/// ## Why a mirror file at all
///
/// Every backup-handling function ([`fstab_core::writer::create_backup`],
/// [`fstab_core::writer::list_backups`],
/// [`crate::backups::resolve_requested`], [`crate::backups::containing_dir`])
/// derives "the backup directory" from "the target file's own parent
/// directory". Pointing those functions at a file inside the fallback
/// directory makes them all operate on it, with no signature changes and
/// no second implementation of the backup naming grammar.
pub(crate) fn mirror_path(fallback_dir: &Path) -> PathBuf {
    fallback_dir.join(MIRROR_FILE_NAME)
}

/// Refreshes the mirror file in `fallback_dir` so that it holds `source`'s
/// current bytes, creating the directory if needed.
///
/// Called immediately before a fallback-mode `create_backup` and nowhere
/// else: listing and deleting only act on files already in the directory,
/// so neither needs a fresh mirror.
///
/// A literal byte copy, never parse-and-reserialize, so it can't drift
/// from its source (`docs/SPEC.md` §5). Written with a plain `fs::write`,
/// not the temp-file-plus-rename ceremony [`target::publish_bytes`] uses:
/// this file has no outside reader to be atomic against, and no `fsync` -
/// the durable artifact is the backup `fstab_core::writer::create_backup`
/// makes, and a mirror lost to a crash is just re-created from the real
/// file next time.
///
/// The read of `source` goes through `real_reader` rather than a plain
/// `fs::read`, since `source` is always the real `/etc/fstab` here and
/// this function runs in the unprivileged GUI process, which can't read
/// that path directly inside a Flatpak sandbox. The write stays plain
/// `fs::write`, since the mirror is in this process's own data directory.
///
/// # Errors
///
/// The underlying [`io::Error`]: `source` couldn't be read (including
/// [`io::ErrorKind::NotFound`], which the caller reports as "nothing to
/// back up"), the directory couldn't be created, or the mirror couldn't
/// be written.
pub(crate) fn refresh_mirror(
    fallback_dir: &Path,
    source: &Path,
    real_reader: &dyn RealFileReader,
) -> io::Result<()> {
    let bytes = real_reader.read(source)?;
    fs::create_dir_all(fallback_dir)?;
    fs::write(mirror_path(fallback_dir), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::DirectFs;
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

    /// The Flatpak case: inside a sandbox the directory is perfectly
    /// listable, it is just the wrong directory, so the marker rather than the
    /// permissions is what selects fallback mode.
    ///
    /// Still true after T40, and that is the point of keeping this test as it
    /// is: a sandboxed session whose *reads and saves* now reach the real
    /// `/etc/fstab` through [`crate::host`] still cannot **enumerate** `/etc/`,
    /// so its manual-management set still belongs in the fallback directory
    /// (see [`detect`]'s docs for the question, and the module docs for why
    /// the alternative was rejected).
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

        refresh_mirror(&fallback, &source, &DirectFs).expect("the first refresh must succeed");
        assert_eq!(
            fs::read(mirror_path(&fallback)).expect("read the mirror"),
            first.as_bytes()
        );

        let second = "LABEL=data   /mnt/data   xfs   defaults,nofail   0   2\n";
        fs::write(&source, second).expect("change the source");
        refresh_mirror(&fallback, &source, &DirectFs).expect("the second refresh must succeed");
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

        let error = refresh_mirror(&fallback, &dir.join("no-such-fstab"), &DirectFs)
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
    // production constants, `OnceLock::get_or_init` and the
    // [`FORCE_FALLBACK_ENV_VAR`] branch - the last of which also could not be
    // tested meaningfully here even in isolation, since its `OnceLock` makes
    // the decision once per test *binary*, not once per test.
}
