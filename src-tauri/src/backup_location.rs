//! Where the real `/etc/fstab`'s **manually managed** backups live for this
//! process - beside the file itself, as they always have, or in a fallback
//! directory under the user's own data directory when this process cannot
//! manage files in `/etc/` at all.
//!
//! ## The problem this exists for
//!
//! Flatpak reserves the whole `/etc` path space: the `/etc` a sandboxed
//! process sees is the **runtime's**, and no `--filesystem=` grant under
//! `/etc` is honoured at all - not `/etc:rw` (which Flathub's
//! `flatpak-builder-lint` rejects outright) and not even the single-file
//! `/etc/fstab:rw` (which the sandbox refuses: `Path "/etc" is reserved by
//! Flatpak`). So for the real `/etc/fstab`, a sandboxed Rustab cannot
//! enumerate the directory its backups live in, and anything it created at
//! `/etc/…` itself would land in the runtime's throwaway `/etc`, not beside
//! the file the user cares about.
//!
//! That is what the three **manual-management** operations the Backups panel
//! offers - `list_backups`, `create_backup`, `delete_backup` - run into, and
//! why for the real target they move, as a set, into [`fallback_dir`]: a
//! directory this process genuinely owns.
//!
//! ## What T40 corrected in T39's reasoning, and what it did not
//!
//! T39 (the first version of this module) justified the fallback with "a
//! sandboxed process cannot reach the real `/etc/` at all, by any means",
//! and believed writing was unaffected because "the elevated `pkexec` process
//! runs fully unsandboxed". **Both halves of that were wrong**, and in
//! opposite directions:
//!
//! - `pkexec` changes a process's UID, not its mount namespace, so a `pkexec`
//!   child started from inside the sandbox was root *inside the sandbox* - it
//!   could not write the real `/etc/fstab` either. The Flatpak build's saves
//!   never worked.
//! - A sandboxed process *can* reach the real `/etc/`, through
//!   `flatpak-spawn --host` (and, with `pkexec` behind it, as root). T40
//!   implements exactly that: see [`crate::host`] for the mechanism and the
//!   empirical confirmation.
//!
//! So as of T40, for a normal Flatpak install:
//!
//! - **reads of the real `/etc/fstab` work** - the working draft shows the
//!   user's actual fstab, and the external-change check compares against it;
//! - **writes work** - a save goes through `flatpak-spawn --host pkexec`, so
//!   [`fstab_core::writer::write_document`] runs as a genuine host root
//!   process and its **automatic backup really does land beside the real
//!   `/etc/fstab`**, with the rescue-environment colocation property that
//!   backup exists for;
//! - **`create_backup`'s mirror refresh works** - it reads the real file
//!   through the same host seam ([`refresh_mirror`]), which is what makes a
//!   fallback-mode manual backup a snapshot of reality rather than an error.
//!
//! What T40 did **not** change is the thing this module decides. See
//! [`detect`] for the question it now asks, and the "Deviation" note below for
//! why the fallback stayed rather than being replaced by the colocated
//! behaviour now that two thirds of the problem are solved.
//!
//! ## Deviation from T40's suggested design, and the reasoning (the central
//! T40 decision)
//!
//! T40 asked whether the three manual-management operations should move back
//! into `/etc/` for a sandboxed install, since `create_backup` and
//! `delete_backup` for the real target are already routed through the elevated
//! helper ([`crate::commands::backups_are_elevated`]) and that helper is now a
//! real host root process. They **did not**, and this is the reasoning,
//! because the decision is not obvious and the alternative was seriously
//! considered.
//!
//! The binding constraint is `list_backups`. It is deliberately
//! **unprivileged** - `/etc/` is world-readable, and asking for a password to
//! read a directory would be absurd - so it runs in the sandboxed GUI process
//! and calls [`fstab_core::writer::list_backups`], which does its own
//! `read_dir` inside that process. There is no seam to inject there, by
//! design: `fstab-core` is pure logic over paths and bytes with no idea that
//! a sandbox exists, and T40 (like T39) deliberately does not touch it.
//!
//! That makes the three operations a package deal. Moving *creation* and
//! *deletion* into `/etc/` while listing stayed in the fallback directory
//! would mean a backup the user just created never appearing in the list it
//! was created from - not a cosmetic gap but a trust-destroying one in a
//! feature whose whole purpose is "I can get my fstab back".
//!
//! The one way to move all three was to give the GUI a second internal
//! re-exec flag - a **list-only**, never-elevated one - that host-spawns this
//! same binary (`flatpak-spawn --host <host path> --internal-…`) so that
//! `fstab_core::writer::list_backups` runs *on the host* and reports its rows
//! back as JSON. It was rejected, for four reasons, in descending order of
//! weight:
//!
//! 1. **It is not one flag's worth of work, it is the whole backup read
//!    surface.** `list_backups` is only the visible half. `read_backup`
//!    (the backup-diff view) reads a file in `/etc/`;
//!    [`crate::backups::resolve_requested`] - the check that makes a
//!    frontend-supplied `backup_path` safe (`restore_backup`, `read_backup`,
//!    `delete_backup`) - needs the host's listing *and* `fs::canonicalize` of
//!    paths in `/etc/`, neither of which a sandboxed process can do;
//!    `create_backup` needs [`crate::backups::describe_one`] afterwards, same
//!    problem. Each would have to cross the host boundary, which means a
//!    second request/response protocol mirroring [`crate::privileged`]'s and a
//!    second place where "this path is one of this target's backups" is
//!    decided. That is a large new security-relevant surface to buy a
//!    different storage directory.
//! 2. **It would put the most fragile mechanism Rustab has on a routine read
//!    path.** Running this binary on the host means running a program linked
//!    against the Flatpak runtime's libraries (GTK, WebKitGTK, the runtime's
//!    glibc) with the *host's* loader and libraries - which is exactly why
//!    [`crate::host`]'s elevated spawn is confined to the one operation that
//!    has no alternative. A host that lacks `libwebkit2gtk-4.1.so.0`, or whose
//!    glibc is older than the runtime's, cannot exec it. Having the Backups
//!    panel depend on that every time it opens would turn a host-library
//!    mismatch into "your backups vanished"; having only *saving* depend on it
//!    keeps the failure where it is both unavoidable and loud.
//! 3. **It would make a manual backup cost a password prompt.** In `/etc/`,
//!    `create_backup` and `delete_backup` must elevate. Pressing "Create
//!    backup" in a Flatpak install would then open a polkit dialog, where
//!    today it is a silent, instant copy into the user's own directory. That
//!    is defensible (it is what a native install does), but it is a real cost,
//!    not a neutral change.
//! 4. **[`crate::privileged`]'s stated design philosophy is one hidden flag.**
//!    A list-only flag never gains root, so the module's security *property*
//!    ("exactly one string can turn this process into a root process") would
//!    survive intact - but its auditability argument is explicitly about
//!    keeping the number of hidden entry points at one, and this would make it
//!    two.
//!
//! **The accepted consequence, stated plainly** (and the frontend says it
//! too, `docs/UI_DESIGN.md` §7): in fallback mode the automatic backups taken
//! during ordinary saves sit beside the real `/etc/fstab`, where they are
//! exactly as useful from a rescue environment as they are for a native
//! install, but they are **not visible** in `list_backups`'s result, because
//! this process cannot enumerate that directory. Only backups explicitly
//! created via `create_backup` land in - and are visible and restorable from -
//! the fallback directory. Unlike T39, this is now a *verified* statement
//! rather than an assumption: the elevated writer really does run on the host,
//! so those automatic backups really are being written (see
//! `PROJECT_PLAN.md` §T40's verification log).
//!
//! ## The fallback's second job (T40)
//!
//! It is also now the **defense-in-depth** answer for a sandbox whose host
//! access is unavailable - no `flatpak-spawn` in the runtime, the
//! `org.freedesktop.Flatpak` talk-name missing from the manifest or revoked by
//! an override, or the portal otherwise denied. Such a session cannot read or
//! save the real `/etc/fstab` at all, but it can still take, list and keep
//! manual backups in a directory it owns, which is a strictly better place to
//! be than having no backup surface whatsoever. That is why this mechanism
//! survives T40 instead of being deleted along with T39's reasoning for it.

use crate::host::{self, RealFileReader};
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

/// Environment variable that, when set to any non-empty value, forces
/// [`real_target_location`] to report [`BackupLocation::Fallback`] without
/// consulting [`detect`] or touching `/etc` at all.
///
/// A manual testing aid for seeing the fallback UI/behaviour without a real
/// Flatpak sandbox - faking [`host::SANDBOX_MARKER_PATH`] on the host does **not**
/// work for this: GTK's own icon loading checks the same marker and, once it
/// believes it is sandboxed, switches to a `flatpak-spawn --sandbox`-based
/// loading path that only functions inside a real sandbox, aborting the
/// whole process outside one. This variable affects only this module's own
/// decision and is never read by anything else. Not a documented or
/// supported end-user interface - an internal tool for whoever is working on
/// T39, nothing more.
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
/// `fallback_dir` when they cannot.
///
/// Every input is a parameter so that the tests below can drive every branch
/// against their own temp directories - the same reason `target::resolve`
/// takes the sandbox directory and the real file's path rather than reading
/// the constants itself.
///
/// ## The question this asks (restated for T40)
///
/// Not "is this process sandboxed?", which is what T39 thought it was asking,
/// and not "can this process reach the real `/etc/` by any means?", which
/// since T40 is a different question with a different answer (yes, via
/// [`crate::host`]). It is:
///
/// > Can **this process**, using the mechanism those three manual-management
/// > operations actually use, enumerate and manage the real file's own
/// > directory?
///
/// The mechanism is the binding part. `list_backups` is unprivileged by
/// design and calls [`fstab_core::writer::list_backups`], which does a
/// `read_dir` **in this process**; `fstab-core` has no sandbox-crossing seam
/// and deliberately is not given one (see the module docs' "Deviation" note
/// for the full reasoning and the rejected alternative). So the answer is "no"
/// whenever the directory this process would enumerate is not the host's real
/// one - whatever else this process can reach by spawning something on the
/// host.
///
/// Two conditions, either of which selects fallback mode:
///
/// 1. **`sandbox_marker` exists.** Inside a Flatpak sandbox the directory seen
///    at `colocated_dir` is the runtime's `/etc`, not the host's - perfectly
///    listable, and the wrong directory. Enumerating it would never find the
///    real file's backups (including the automatic ones the elevated host-side
///    writer leaves beside it on every save), and anything created in it would
///    not be beside the real file. Note what this condition does **not** imply
///    any more: nothing about whether reads or saves work. Since T40 they do,
///    through a different mechanism entirely.
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
/// Nor does it probe whether host-spawn works, which would be the natural
/// reading of "can the mechanism reach `/etc/` right now". That probe was
/// considered and rejected as *misleading rather than wrong*: its answer could
/// not change this function's result (listing cannot use host-spawn either
/// way), so it would be a process spawn - and a second, differently-scoped
/// notion of "reachable" - whose only effect would be to suggest that this
/// decision depends on it. Whether host access works is observable where it
/// matters instead: a save that cannot reach the host fails with the elevated
/// path's own error ([`crate::host::elevated_self_plan`]), and a draft that
/// could not be refreshed shows the seed.
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
/// ## Why the read of `source` goes through `real_reader` (T40)
///
/// `source` is the real `/etc/fstab` in every production call that reaches
/// here - fallback mode is only ever chosen for that file - and this function
/// runs in the **unprivileged GUI process**, which inside a Flatpak sandbox
/// cannot read that path with `fs::read` at all. Before T40 this was a plain
/// `fs::read`, which means the one thing fallback mode exists to make
/// possible (pressing "Create backup" in a sandboxed install) always failed with
/// "`/etc/fstab` does not exist yet, so there is nothing to back up"
/// ([`crate::commands`]'s `create_backup_with` maps `NotFound` that way). The
/// injected reader is what makes that button do what it says. The *write* half
/// stays a plain `fs::write`: the mirror is in this process's own data
/// directory, which it sees and owns normally.
///
/// # Errors
///
/// The underlying [`io::Error`]: `source` could not be read (including
/// [`io::ErrorKind::NotFound`] for a real file that does not exist yet, which
/// the caller reports as "nothing to back up"), the directory could not be
/// created, or the mirror could not be written.
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
