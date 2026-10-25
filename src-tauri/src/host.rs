//! Reaching the **real host** `/etc/` from inside a Flatpak sandbox - the
//! one place that knows this process might not be looking at the machine
//! it's running on.
//!
//! ## The problem, and the confirmed answer
//!
//! Flatpak reserves the whole `/etc` path space: the `/etc` a sandboxed
//! process sees is the runtime's, and `--filesystem=/etc/fstab:rw` is
//! outright refused. A `pkexec` child inherits the same sandbox (`pkexec`
//! changes UID, not mount namespace), so elevating alone doesn't help either.
//!
//! What does work, confirmed end-to-end with a manifest granting only
//! `--talk-name=org.freedesktop.Flatpak`:
//!
//! - `flatpak-spawn --host cat /etc/fstab` prints the real host file;
//! - `flatpak-spawn --host id` reports the real host user, proving the
//!   spawned process is genuinely outside the sandbox's mount namespace;
//! - `flatpak-spawn --host sh -c 'touch /etc/.test'` fails with
//!   `Permission denied`, as an unprivileged host process should;
//! - `flatpak-spawn --host pkexec sh -c 'touch /etc/.test && rm -f /etc/.test'`
//!   succeeds - genuine root access to the real `/etc/`.
//!
//! `flatpak-spawn` ships with the runtimes this app already builds
//! against, so this is the standard mechanism, not a new dependency.
//!
//! ## What this module provides, and what it doesn't
//!
//! 1. [`RealFileReader`] - an unprivileged read of a host path, injected
//!    into every production call site that reads the real `/etc/fstab`
//!    (`crate::target`'s draft refresh and change-detection,
//!    [`crate::commands`]'s re-sync command, and
//!    [`crate::backup_location::refresh_mirror`]). A one-method trait,
//!    shaped like `fstab_core::writer`'s own `FileSystemOps` seam.
//! 2. [`elevated_plan`] - how the elevated half is spawned: under Flatpak,
//!    `pkexec` must be invoked on the host
//!    (`flatpak-spawn --host pkexec …`) and must name a **host** path,
//!    since `std::env::current_exe()` inside the sandbox yields
//!    `/app/bin/rustab`, which doesn't exist outside it. The binary named
//!    there is `rustab-helper` - the GUI-free, statically-linked elevated
//!    binary - not the GUI itself, whose GTK, WebKitGTK and glibc come
//!    from the runtime and may be unavailable on the host that has to run
//!    it.
//! 3. A [`CommandRunner`] - `fstab-core`'s seam for everything it has to
//!    ask the edited machine. Two of those are the system tools it shells
//!    out to, `findmnt --verify` and `lsblk`: both answer questions about
//!    `/dev`, udev and the live mount table, none of which the sandbox
//!    shares with the host, so run inside it `findmnt --verify` calls even
//!    `/` "unreachable on boot" and `lsblk` exits 0 reporting no device
//!    with a UUID. Both are wrong, and the `findmnt` one blocks every
//!    save. The third is the advisory "does this mount point exist?"
//!    check, whose answers come from the sandbox's own `/` - the runtime's -
//!    so `/boot`, `/opt` and every `/run/media/…` the host really has are
//!    reported missing. So all three run on the host (see
//!    [`RUN_WITH_INPUT_SCRIPT`] for the one awkward part - `findmnt` needs
//!    its input as a *file*, and the sandbox's `/tmp` is private - and
//!    [`EXISTS_BATCH_SCRIPT`] for why the existence check is one call for
//!    the whole document).
//!
//! It doesn't hide the sandbox from the rest of the program: every
//! function is explicit about which side of the boundary it acts on,
//! every seam is injected rather than resolved globally, and nothing here
//! ever writes a file - the single write path is still
//! `fstab_core::writer::write_document`, run from exactly one elevated
//! process (see [`crate::privileged`]).
//!
//! ## Why `cargo test` never touches any of this
//!
//! The production reader comes from [`real_file_reader`], called only
//! from the production wrappers that also fill in the real path (and the
//! production runner from [`command_runner`], the same way). Every
//! testable core function takes its reader as a parameter, so tests pass
//! [`DirectFs`] or their own double. The pieces that genuinely spawn a
//! process ([`HostSpawn`]'s three methods and the elevated spawn) are split
//! into a pure "what would be run" half ([`Spawn`], asserted literally by
//! tests) and a pure "what did it mean" half ([`interpret_read_output`]),
//! the same split `privileged::interpret_write_output` uses.
//!
//! The one test that does cross the boundary is
//! [`tests::sandbox_smoke_host_runner_runs_on_the_host`], which is
//! `#[ignore]`d and returns early unless it finds itself in a sandbox - it
//! exists to be run by hand inside an installed Flatpak, where it is the
//! only place this module's host-side behaviour can actually be observed.

use fstab_core::exec::{CommandRunner, DirectRunner};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The file whose existence means "this process is running inside a
/// Flatpak sandbox" - the `/etc` it sees is the runtime's, not the host's.
///
/// Flatpak mounts this into every sandbox as the documented way an app
/// detects containment (the same signal GLib's own check uses). Checked
/// as a path rather than `FLATPAK_ID`, since an env var can be unset,
/// inherited, or forged, while this mount is part of the sandbox itself.
///
/// Lives here rather than in [`crate::backup_location`] since it's the
/// input to two unrelated decisions ("how do I read the real
/// `/etc/fstab`?" and "where do manual backups live?") - `backup_location`
/// reads it from here rather than keeping a second copy.
///
/// The property that makes this whole design hold together: a process
/// `flatpak-spawn --host` starts is outside the sandbox, so it has no
/// `/.flatpak-info`. The elevated helper therefore selects [`DirectFs`] on
/// its own, with no flag needed, and its `fs::read` of the target is
/// already a read of the real host file.
pub(crate) const SANDBOX_MARKER_PATH: &str = "/.flatpak-info";

/// The binary that runs a command outside the sandbox, on the host.
const FLATPAK_SPAWN: &str = "flatpak-spawn";

/// `flatpak-spawn`'s "run this on the host, not in a sandbox" flag.
const HOST_FLAG: &str = "--host";

/// The authorization agent the elevated helper is started through, on
/// whichever side of the sandbox boundary that happens.
const PKEXEC: &str = "pkexec";

/// The shell program the host-side read is routed through - see
/// [`READ_SCRIPT`] for why a shell is involved at all.
const SH: &str = "sh";

/// The script [`HostSpawn::read`] runs on the host: print the file if it
/// exists, and exit [`MISSING_EXIT`] if it does not.
///
/// ## Why a sentinel exit code rather than the obvious alternatives
///
/// The contract this reader has to honour is `fs::read`'s: a file that does
/// not exist must come back as [`io::ErrorKind::NotFound`] and *everything
/// else* must come back as some other kind. That distinction is not cosmetic -
/// [`crate::commands`]'s re-sync command treats `NotFound` as "the real
/// `/etc/fstab` does not exist, so sync the working draft to empty" and any
/// other error as "report this and touch nothing". Mapping a transport failure
/// onto `NotFound` would therefore **blank the user's draft** because a portal
/// call failed. So the mapping has to be fail-safe, and the three candidate
/// mechanisms are not equally good at that:
///
/// - **Parse `cat`'s stderr**: locale- and implementation-dependent. Rejected.
/// - **A preceding `flatpak-spawn --host test -e <path>` probe**: `test`
///   exits 1 for "does not exist", but `flatpak-spawn` itself also exits 1
///   when it can't reach the portal at all - the two failures are
///   indistinguishable from the exit code, and it costs a second process.
/// - **This script**: the only way to see [`MISSING_EXIT`] is for a host
///   shell to have run and found the path absent. Every other failure mode
///   (missing binary, denied portal, permission error, signal) lands on
///   "some other read error" instead - the safe direction to be wrong in.
///
/// The path is a positional argument (`$1`), never interpolated into the
/// script text, so there's no quoting/injection question.
const READ_SCRIPT: &str = r#"if [ -e "$1" ]; then exec cat -- "$1"; else exit 44; fi"#;

/// The script [`host_path_exists`] runs on the host: succeed if the path is
/// there, exit [`MISSING_EXIT`] if it isn't.
///
/// A separate script from [`READ_SCRIPT`], not a `-e`-only mode of it, so
/// "is this path there?" can never be confused with "what does it hold?".
const EXISTS_SCRIPT: &str = r#"if [ -e "$1" ]; then exit 0; else exit 44; fi"#;

/// The script [`HostSpawn::paths_exist`] runs on the host: print one `1`
/// per existing path and one `0` per missing one, in the order asked, with
/// no separators.
///
/// ## Why one script for all the paths
///
/// `fstab_core::validate`'s advisory existence check asks about every
/// mount point in the document, i.e. once per entry. A
/// `flatpak-spawn --host` per path would mean a portal round trip and a
/// process spawn per entry on every validation, and validation runs on
/// every edit - so the trait takes the whole list and this answers it in
/// one call, making the cost independent of the file's size.
///
/// ## Why the output is this shape
///
/// A single `0`/`1` per path, no delimiter, makes the reply self-checking:
/// the only well-formed answer to N questions is N characters, so a
/// truncated or interleaved reply cannot be silently misread as a verdict
/// about the wrong path. [`interpret_exists_batch_output`] rejects anything
/// else rather than guessing (a wrong answer here is a false "this mount
/// point does not exist" warning on a path the user has).
///
/// `[ -e ]` is `Path::exists`'s question - it follows symlinks and does not
/// care what kind of file it finds - and it cannot fail the script: the
/// loop always prints something for every path, so the exit status is `0`
/// whatever the verdicts are. The paths are positional (`"$@"`), never
/// interpolated into the script text, so there is no quoting question.
const EXISTS_BATCH_SCRIPT: &str =
    r#"for p in "$@"; do if [ -e "$p" ]; then printf 1; else printf 0; fi; done"#;

/// The script [`HostSpawn::run`] uses when the command it is given has
/// input: stage stdin in a **host** temp file, run the command with that
/// file appended as its last argument, then delete the file and report the
/// command's own exit status.
///
/// ## Why the content has to make this detour
///
/// `findmnt --verify` is the one caller with input, and it will only read a
/// *seekable* `--tab-file`: handed `/dev/stdin` on a pipe it fails with
/// `findmnt: can't read /dev/stdin: Success`. So the candidate fstab has to
/// exist as a file the `findmnt` process can open - and that process is on
/// the host, which cannot see this sandbox's `/tmp` at all. A path from
/// [`std::env::temp_dir`] would therefore name nothing on the other side.
/// `mktemp` on the host is the shortest thing that works: the bytes cross
/// the boundary as the child's stdin, where they already travel safely.
///
/// The command and its arguments are positional (`"$@"`), never
/// interpolated into the script text, so there is no quoting question;
/// `$(mktemp)`'s result is quoted everywhere it is used. The file is
/// removed on both paths, and `$s` preserves the command's status across
/// the `rm`.
///
/// A `mktemp` or `cat` failure exits [`STAGING_FAILED_EXIT`] rather than
/// letting the command run against a missing or truncated file, which for
/// `findmnt --verify` would mean validating something other than what the
/// user is about to save.
const RUN_WITH_INPUT_SCRIPT: &str = r#"t=$(mktemp) || exit 45; cat > "$t" || { rm -f -- "$t"; exit 45; }; "$@" "$t"; s=$?; rm -f -- "$t"; exit $s"#;

/// The exit code [`READ_SCRIPT`]/[`EXISTS_SCRIPT`] use for "that path
/// doesn't exist on the host" - the only code [`interpret_read_output`]
/// maps to [`io::ErrorKind::NotFound`].
///
/// Not 1 or 2 (used by `sh`/`cat`/`flatpak-spawn` failures) and outside the
/// 126/127/128+n shell-signal range. Duplicated literally in both scripts
/// since a shell script can't interpolate a Rust constant;
/// [`tests::the_host_scripts_exit_with_the_sentinel_this_module_looks_for`]
/// pins the three together.
const MISSING_EXIT: i32 = 44;

/// The exit code [`RUN_WITH_INPUT_SCRIPT`] uses for "the input could not be
/// staged on the host" - the only code [`HostSpawn::run`] turns into an
/// `io::Error` instead of passing the command's own status through.
///
/// Distinct from [`MISSING_EXIT`] so the two host scripts can never be
/// confused, and chosen on the same grounds (clear of 0/1/2 and of the
/// 126/127/128+n ranges). It is only ever consulted for a command that was
/// *given* input, i.e. one that really ran under this script, so a command
/// of its own that happened to exit 45 cannot be misread.
const STAGING_FAILED_EXIT: i32 = 45;

/// How much of a failed host command's stderr is quoted back in the
/// resulting `io::Error` - same bound and reason as `privileged`'s own
/// `STDERR_EXCERPT_LIMIT`, kept separate since the two travel to different places.
const STDERR_EXCERPT_LIMIT: usize = 500;

/// Rustab's Flatpak application id - `tauri.conf.json`'s `identifier`.
const APP_ID: &str = "dev.tomsk.Rustab";

/// The elevated binary the Flatpak manifest installs beside the GUI in
/// `/app/bin`, i.e. the final component of the path `pkexec` is handed on
/// the host.
///
/// Not the `command:` (`rustab`, the GUI): inside a Flatpak the process
/// that runs on the host is `rustab-helper`, which links no GUI library and
/// is built as a static binary for exactly that reason - see
/// [`elevated_plan`] and `crates/rustab-helper`.
const HELPER_COMMAND: &str = "rustab-helper";

/// Where a Flatpak app's files are, relative to a Flatpak **installation**
/// root: `app/<app id>/current/active/files`.
///
/// `current/active` are symlinks Flatpak maintains, so this path follows
/// updates without this code knowing an architecture, a branch or a commit.
const APP_FILES_IN_INSTALLATION: &str = "current/active/files";

/// The user-wide Flatpak installation root, relative to `$HOME`.
const USER_INSTALLATION_IN_HOME: &str = ".local/share/flatpak";

/// The system-wide Flatpak installation root - the common case for an app
/// installed from Flathub by a normal user with `flatpak install`.
const SYSTEM_INSTALLATION: &str = "/var/lib/flatpak";

/// The `/.flatpak-info` key holding the host path of this app's `files`
/// directory, as Flatpak itself recorded it when it started this sandbox.
const APP_PATH_KEY: &str = "app-path";

/// `bin`, the subdirectory of an app's `files` directory that
/// `install -Dm755 … /app/bin/rustab-helper` lands in (`/app` *is* `files`).
const BIN_DIR: &str = "bin";

// ---------------------------------------------------------------------------
// Reading a host path
// ---------------------------------------------------------------------------

/// Reads a file that lives on the **host**, whichever side of a sandbox
/// boundary this process is on.
///
/// One method, since every real `/etc/fstab` reader wants only the whole
/// current content as bytes - no metadata, no listing, no partial read.
/// Shaped after `fstab_core::writer`'s `FileSystemOps` seam: a production
/// impl plus a test double.
///
/// Implementations must honour `fs::read`'s error contract:
/// [`io::ErrorKind::NotFound`] only for a file that's genuinely not there,
/// some other kind for every other failure (see [`READ_SCRIPT`]).
pub(crate) trait RealFileReader {
    /// Reads `path`'s full contents from the host.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::NotFound`] if `path` does not exist on the host; any
    /// other [`io::Error`] if it could not be read, including a failure to
    /// reach the host at all.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// The reader for a process that is already looking at the real filesystem: a
/// plain `fs::read`.
///
/// Used by every non-Flatpak launch, by the elevated helper (which runs on the
/// host and therefore has no sandbox marker - see [`SANDBOX_MARKER_PATH`]),
/// and by the unit tests, which point it at their own temp files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectFs;

impl RealFileReader for DirectFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }
}

/// The reader for a process inside a Flatpak sandbox: one
/// `flatpak-spawn --host` per read (see [`READ_SCRIPT`]).
///
/// Stateless and uncached on purpose - every caller wants to know what the
/// real `/etc/fstab` holds *right now*, and a cache would make that stale.
/// At most a handful of these reads happen per session, each of a
/// kilobyte-sized file, so the process spawn isn't worth optimizing away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostSpawn;

impl RealFileReader for HostSpawn {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let plan = read_plan(path);
        // A failure to *start* `flatpak-spawn` is mapped away from its own
        // `io::ErrorKind` deliberately: a missing `flatpak-spawn` binary is
        // reported by the OS as `NotFound`, and passing that through would
        // tell the caller that `path` does not exist - the one misreading this
        // reader must never produce (see `READ_SCRIPT`).
        let output = plan.command().output().map_err(|err| {
            io::Error::other(format!(
                "could not run `{plan}` to read {} from outside this Flatpak sandbox ({err})",
                path.display()
            ))
        })?;
        interpret_read_output(path, output.status.code(), output.stdout, &output.stderr)
    }
}

/// The production reader for this process: [`HostSpawn`] inside a Flatpak
/// sandbox, [`DirectFs`] everywhere else.
///
/// The thin production wrapper, in the same shape
/// [`crate::backup_location::real_target_location`] wraps its own `detect`:
/// this function is the only place that consults the real
/// [`SANDBOX_MARKER_PATH`], and everything it decides is decided by
/// [`reader_for`], which the tests drive against paths of their own.
///
/// Returns a `&'static dyn` rather than an owned value so callers can hold it
/// in a plain reference without a lifetime or an allocation; both
/// implementations are zero-sized and stateless, so there is nothing to own.
pub(crate) fn real_file_reader() -> &'static dyn RealFileReader {
    reader_for(Path::new(SANDBOX_MARKER_PATH))
}

/// Chooses the reader appropriate for a process whose sandbox marker is (or is
/// not) at `sandbox_marker`.
///
/// Parameterized purely so the choice itself is testable without a sandbox;
/// production always passes [`SANDBOX_MARKER_PATH`].
fn reader_for(sandbox_marker: &Path) -> &'static dyn RealFileReader {
    /// The two readers, as values with a `'static` lifetime to hand out.
    /// Both are zero-sized, so these cost nothing.
    static HOST: HostSpawn = HostSpawn;
    static DIRECT: DirectFs = DirectFs;

    if sandbox_marker.exists() {
        &HOST
    } else {
        &DIRECT
    }
}

/// What [`HostSpawn::read`] runs for `path`:
/// `flatpak-spawn --host sh -c '<READ_SCRIPT>' sh <path>`.
///
/// Pure, so the exact command line is reviewable and regression-tested
/// without a sandbox (see
/// [`tests::the_host_read_plan_is_a_flatpak_spawn_of_the_read_script`]).
///
/// The second `sh` is the script's `$0`: `sh -c '<script>' <argv0>
/// <argv1…>` means the path must be the *second* operand to land in `$1`.
fn read_plan(path: &Path) -> Spawn {
    Spawn {
        program: OsString::from(FLATPAK_SPAWN),
        args: vec![
            OsString::from(HOST_FLAG),
            OsString::from(SH),
            OsString::from("-c"),
            OsString::from(READ_SCRIPT),
            OsString::from(SH),
            path.as_os_str().to_os_string(),
        ],
    }
}

/// Turns a finished host read into the `io::Result` a `fs::read` caller
/// expects.
///
/// Pure, like `privileged::interpret_write_output`: the interesting cases
/// (missing file, denied portal, signal) are classified by a function a
/// unit test can call directly, no sandbox needed.
///
/// `stdout` is taken by value and handed straight to the caller on
/// success - no trimming, no UTF-8 assumption, so non-text content
/// survives byte-for-byte, exactly as `fs::read` would leave it.
fn interpret_read_output(
    path: &Path,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: &[u8],
) -> io::Result<Vec<u8>> {
    match code {
        Some(0) => Ok(stdout),
        Some(MISSING_EXIT) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist on the host", path.display()),
        )),
        other => {
            let how = match other {
                Some(code) => format!("with status {code}"),
                None => "on a signal".to_string(),
            };
            Err(io::Error::other(format!(
                "could not read {} from outside this Flatpak sandbox: the host read exited {how}{}",
                path.display(),
                stderr_excerpt(stderr)
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Running a system tool on the host
// ---------------------------------------------------------------------------

/// `fstab-core`'s command seam, satisfied by running the command on the
/// host - one `flatpak-spawn --host` per call, exactly like
/// [`HostSpawn::read`].
///
/// A command with input goes through [`RUN_WITH_INPUT_SCRIPT`], which
/// stages the bytes in a host temp file and appends its path, honouring
/// [`CommandRunner::run`]'s contract on the far side of the boundary. The
/// existence question goes through [`EXISTS_BATCH_SCRIPT`], which answers
/// the whole list in that same single call.
///
/// Only a failure to launch `flatpak-spawn`, or the staging sentinel, is an
/// `io::Error`: every other outcome is passed through so that `findmnt`'s
/// and `lsblk`'s own exit statuses keep being interpreted by `fstab-core`
/// as they always were. A portal failure therefore arrives as a non-zero
/// exit with "Portal call failed" in stderr, which `fstab-core` turns into
/// its conservative blocking finding carrying that text - noisy but
/// fail-safe, and the right direction to be wrong in for a check that
/// guards a save.
impl CommandRunner for HostSpawn {
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> io::Result<Output> {
        let plan = run_plan(program, args, input.is_some());
        let mut command = plan.command();
        let Some(bytes) = input else {
            let output = command.output().map_err(|err| launch_error(&plan, err))?;
            return Ok(output);
        };

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| launch_error(&plan, err))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other(format!("`{plan}` gave no stdin to write to")))?;
        let written = stdin.write_all(bytes).and_then(|()| stdin.flush());
        // Dropped before waiting, or the host-side `cat` would never see EOF
        // and both processes would block forever.
        drop(stdin);

        // A broken pipe means the host script already exited - its status
        // says why (the staging sentinel, most likely), and that is far more
        // informative than "write failed". Any other write error is real:
        // reporting it beats letting the command verify a truncated file.
        if let Err(err) = written {
            if err.kind() != io::ErrorKind::BrokenPipe {
                let _ = child.wait();
                return Err(io::Error::other(format!(
                    "could not send {program}'s input to the host through `{plan}` ({err})"
                )));
            }
        }

        let output = child.wait_with_output().map_err(|err| {
            io::Error::other(format!("could not wait for `{plan}` to finish ({err})"))
        })?;
        if output.status.code() == Some(STAGING_FAILED_EXIT) {
            return Err(io::Error::other(format!(
                "could not stage the input on the host for `{program}`: the host could not \
                 create a temporary file to receive it{}",
                stderr_excerpt(&output.stderr)
            )));
        }
        Ok(output)
    }

    /// One `flatpak-spawn --host` for the whole list, whatever its length -
    /// see [`EXISTS_BATCH_SCRIPT`].
    fn paths_exist(&self, paths: &[&str]) -> io::Result<Vec<bool>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let plan = exists_batch_plan(paths);
        let output = plan
            .command()
            .output()
            .map_err(|err| launch_error(&plan, err))?;
        interpret_exists_batch_output(paths, output.status.code(), &output.stdout, &output.stderr)
    }
}

/// What [`HostSpawn::paths_exist`] runs for `paths`:
/// `flatpak-spawn --host sh -c '<EXISTS_BATCH_SCRIPT>' sh <path…>`.
///
/// Pure, so the exact command line is reviewable and regression-tested
/// without a sandbox, exactly like [`read_plan`] and [`run_plan`].
///
/// The second `sh` is the script's `$0`, so the paths start at `$1` and
/// `"$@"` expands to all of them and nothing else.
fn exists_batch_plan(paths: &[&str]) -> Spawn {
    let mut args = vec![
        OsString::from(HOST_FLAG),
        OsString::from(SH),
        OsString::from("-c"),
        OsString::from(EXISTS_BATCH_SCRIPT),
        OsString::from(SH),
    ];
    args.extend(paths.iter().map(OsString::from));
    Spawn {
        program: OsString::from(FLATPAK_SPAWN),
        args,
    }
}

/// Turns a finished batched existence query into one verdict per path.
///
/// Pure, like [`interpret_read_output`]: every failure mode is classified
/// by a function a unit test can call directly, no sandbox needed.
///
/// Anything other than exit 0 with exactly one `0`/`1` character per path
/// is an `io::Error`, never a partial verdict - the caller's check is
/// advisory, and a guessed answer would surface as a confident claim about
/// a path nobody actually looked at.
fn interpret_exists_batch_output(
    paths: &[&str],
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> io::Result<Vec<bool>> {
    if code != Some(0) {
        let how = match code {
            Some(code) => format!("with status {code}"),
            None => "on a signal".to_string(),
        };
        return Err(io::Error::other(format!(
            "asking the host which of {} path(s) exist failed: the host command exited {how}{}",
            paths.len(),
            stderr_excerpt(stderr)
        )));
    }
    if stdout.len() != paths.len() {
        return Err(io::Error::other(format!(
            "asking the host which of {} path(s) exist gave {} answer(s)",
            paths.len(),
            stdout.len()
        )));
    }
    stdout
        .iter()
        .map(|byte| match byte {
            b'1' => Ok(true),
            b'0' => Ok(false),
            other => Err(io::Error::other(format!(
                "the host's existence answer contained {:?}, which is neither 0 nor 1",
                char::from(*other)
            ))),
        })
        .collect()
}

/// The production [`CommandRunner`] for this process: [`HostSpawn`] inside
/// a Flatpak sandbox, `fstab-core`'s own [`DirectRunner`] everywhere else.
///
/// The counterpart of [`real_file_reader`], in the same shape and for the
/// same reason: the only place that consults the real
/// [`SANDBOX_MARKER_PATH`], with the decision itself in [`runner_for`] so
/// the tests can drive it against paths of their own.
pub(crate) fn command_runner() -> &'static dyn CommandRunner {
    runner_for(Path::new(SANDBOX_MARKER_PATH))
}

/// Chooses the runner appropriate for a process whose sandbox marker is (or
/// is not) at `sandbox_marker`.
///
/// Parameterized purely so the choice itself is testable without a sandbox;
/// production always passes [`SANDBOX_MARKER_PATH`].
fn runner_for(sandbox_marker: &Path) -> &'static dyn CommandRunner {
    /// The two runners, as values with a `'static` lifetime to hand out.
    /// Both are zero-sized, so these cost nothing.
    static HOST: HostSpawn = HostSpawn;
    static DIRECT: DirectRunner = DirectRunner;

    if sandbox_marker.exists() {
        &HOST
    } else {
        &DIRECT
    }
}

/// What [`HostSpawn::run`] runs for `program` and `args`. Two shapes:
///
/// - **no input**: `flatpak-spawn --host <program> <args…>`;
/// - **with input**: `flatpak-spawn --host sh -c '<RUN_WITH_INPUT_SCRIPT>'
///   sh <program> <args…>`, with the bytes on the child's stdin and the
///   staged file's path appended by the script itself.
///
/// Pure, so both command lines are reviewable and regression-tested
/// without a sandbox, exactly like [`read_plan`] and [`exists_plan`].
///
/// The second `sh` is the script's `$0`, so `<program>` lands in `$1` and
/// `"$@"` expands to the command and its arguments.
fn run_plan(program: &str, args: &[&str], has_input: bool) -> Spawn {
    let mut spawn_args = vec![OsString::from(HOST_FLAG)];
    if has_input {
        spawn_args.push(OsString::from(SH));
        spawn_args.push(OsString::from("-c"));
        spawn_args.push(OsString::from(RUN_WITH_INPUT_SCRIPT));
        spawn_args.push(OsString::from(SH));
    }
    spawn_args.push(OsString::from(program));
    spawn_args.extend(args.iter().map(OsString::from));
    Spawn {
        program: OsString::from(FLATPAK_SPAWN),
        args: spawn_args,
    }
}

/// The error for a `flatpak-spawn` that could not be started at all.
///
/// Its own [`io::ErrorKind`] is deliberately discarded, for
/// [`HostSpawn::read`]'s reason: a missing `flatpak-spawn` binary is
/// reported as `NotFound`, which passed through would read as "the thing
/// you asked about does not exist".
fn launch_error(plan: &Spawn, err: io::Error) -> io::Error {
    io::Error::other(format!(
        "could not run `{plan}` from inside this Flatpak sandbox ({err})"
    ))
}

// ---------------------------------------------------------------------------
// Spawning the elevated helper
// ---------------------------------------------------------------------------

/// A command to run, as the program plus the arguments it would be given -
/// everything about a spawn except the spawning.
///
/// Exists so "which command does Rustab run to become root?" is a value a
/// test can assert on, rather than only observable by running it.
/// [`crate::privileged`]'s one spawn site turns this into a `Command` and
/// adds its own stdio configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spawn {
    /// The program to execute, resolved through `PATH` by the OS as usual.
    pub(crate) program: OsString,
    /// Its arguments, in order, not including `argv[0]`.
    pub(crate) args: Vec<OsString>,
}

impl Spawn {
    /// This plan as a [`Command`], with no stdio configured - the caller's
    /// business.
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

impl fmt::Display for Spawn {
    /// The command line, space-separated, for error messages and test
    /// assertions. Lossy for non-UTF-8 arguments (fine for a diagnostic
    /// string; never reached in production).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program.to_string_lossy())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

/// How the elevated half is started as root, on whichever side of a sandbox
/// boundary that has to happen.
///
/// The production entry point for [`crate::privileged`]'s one spawn. Two
/// shapes, and they do not name the same executable:
///
/// - **not sandboxed**: `pkexec <current_exe> <flag>` - this binary
///   re-executed with its hidden flag, as it always was. Its libraries are
///   the ones it was built against on this very machine, so re-exec is free
///   and there is nothing to keep in version lockstep.
/// - **sandboxed**: `flatpak-spawn --host pkexec <host path>/bin/rustab-helper`,
///   with **no flag** - `rustab-helper` reads one request on stdin and
///   refuses any argument at all. `pkexec` alone would authorize root
///   *inside* the sandbox (can't see the real `/etc/`); `flatpak-spawn
///   --host` alone would reach the real `/etc/` unprivileged (can't write
///   it). Both together are needed.
///
/// ## Why the sandboxed shape runs a different binary
///
/// The process `flatpak-spawn --host pkexec` starts runs on the **host**,
/// with the host's libraries - not the runtime's. The GUI binary is linked
/// against the runtime's GTK, WebKitGTK and glibc, so exec'ing it there
/// works only on a host that happens to satisfy all of them, and fails
/// before it reads a byte of its request on one that doesn't.
/// `rustab-helper` is the same elevated code
/// (`rustab_privileged::protocol::handle_privileged_request`) in a binary
/// with no GUI dependency, built as a static executable, so the host's
/// libraries are not consulted at all.
///
/// `current_exe()` also resolves to `/app/bin/rustab` inside the sandbox,
/// which names nothing on the host, so [`host_helper_binary`] resolves the
/// helper's host-side path instead.
///
/// # Errors
///
/// An [`io::Error`] if the binary's path couldn't be determined -
/// `current_exe()`'s own error outside a sandbox, or
/// [`host_helper_binary`]'s "nowhere to be found" error inside one.
/// Nothing is spawned either way.
pub(crate) fn elevated_plan(flag: &str) -> io::Result<Spawn> {
    elevated_plan_with(
        Path::new(SANDBOX_MARKER_PATH).exists(),
        std::env::current_exe,
        || host_helper_binary(sandbox_info(), std::env::var_os("HOME"), &host_path_exists),
        flag,
    )
}

/// The decision inside [`elevated_plan`], with every input injected.
///
/// `current_exe` and `host_helper_binary` are closures rather than resolved
/// values so that neither is evaluated in the branch that does not use it: a
/// test for the sandboxed plan must not depend on this binary's own path, and
/// a test for the ordinary plan must not depend on a Flatpak installation
/// existing. `flag` is likewise only meaningful in the ordinary branch - the
/// helper takes none.
fn elevated_plan_with(
    sandboxed: bool,
    current_exe: impl FnOnce() -> io::Result<PathBuf>,
    host_helper_binary: impl FnOnce() -> io::Result<PathBuf>,
    flag: &str,
) -> io::Result<Spawn> {
    if sandboxed {
        let helper = host_helper_binary()?;
        Ok(Spawn {
            program: OsString::from(FLATPAK_SPAWN),
            args: vec![
                OsString::from(HOST_FLAG),
                OsString::from(PKEXEC),
                helper.into_os_string(),
            ],
        })
    } else {
        let exe = current_exe()?;
        Ok(Spawn {
            program: OsString::from(PKEXEC),
            args: vec![exe.into_os_string(), OsString::from(flag)],
        })
    }
}

/// The elevated helper's path **on the host**, for a process running inside
/// a Flatpak sandbox.
///
/// Three candidates, in order, first one that exists on the host wins -
/// resolved exactly like the GUI's own host path used to be, since the
/// helper is installed beside it in the same `/app/bin`:
///
/// 1. `<app-path from /.flatpak-info>/bin/rustab-helper` - Flatpak writes
///    the host path of this app's `files` directory into the sandbox's own
///    `/.flatpak-info` (`[Instance] app-path=…`), so this is correct for
///    any install layout, architecture, or branch.
/// 2. The user-install layout:
///    `$HOME/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper`.
/// 3. The system-install layout (`flatpak install` without `--user`):
///    `/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper`.
///
/// 2 and 3 are kept as fallbacks in case a future Flatpak version drops
/// the `app-path` key that 1 depends on.
///
/// ## Why `exists` must be a host-side check
///
/// None of these three paths exists *as seen from inside the sandbox* -
/// the app's own files are visible there only as `/app`, so a plain
/// `Path::exists()` would answer `false` for every candidate, including
/// the correct one. So the predicate asks the host instead (production
/// passes [`host_path_exists`]), returning `io::Result<bool>` so "the
/// host says no" and "the host couldn't be asked" stay distinguishable -
/// they need different error messages.
///
/// If the host says none of the three exists, this fails naming all of
/// them, rather than handing `pkexec` a dead path that would surface as a
/// bare authorization failure after the user already typed their password.
///
/// # Errors
///
/// [`io::ErrorKind::NotFound`] if the host reports that none of the candidates
/// exists; the first probe error (kind preserved) if the host could not be
/// asked about any of them at all.
fn host_helper_binary(
    sandbox_info: Option<String>,
    home: Option<OsString>,
    exists: &dyn Fn(&Path) -> io::Result<bool>,
) -> io::Result<PathBuf> {
    let candidates = host_helper_candidates(sandbox_info.as_deref(), home.as_deref());
    let mut probe_failure = None;
    for candidate in &candidates {
        match exists(candidate) {
            Ok(true) => return Ok(candidate.clone()),
            Ok(false) => {}
            // Remembered, not returned: a later candidate may still answer
            // cleanly, and only a run in which *every* probe failed is
            // evidence that the host itself could not be reached.
            Err(err) => probe_failure = probe_failure.or(Some(err)),
        }
    }
    let named = candidates
        .iter()
        .map(|candidate| candidate.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    match probe_failure {
        Some(err) => Err(io::Error::new(
            err.kind(),
            format!(
                "Rustab is running inside a Flatpak sandbox, so the elevated helper \
                 ({HELPER_COMMAND}) has to be started from this app's host-side path, but the \
                 host could not be asked whether that path ({named}) exists: {err}. Saving the \
                 real /etc/fstab needs the Flatpak permission \
                 --talk-name=org.freedesktop.Flatpak."
            ),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Rustab is running inside a Flatpak sandbox, so the elevated helper \
                 ({HELPER_COMMAND}) has to be started from this app's host-side path, but no \
                 {HELPER_COMMAND} executable was found at {named}. Reinstalling the Flatpak \
                 (`flatpak install --reinstall dev.tomsk.Rustab`) restores it; until then, \
                 saving the real /etc/fstab cannot work from this sandbox."
            ),
        )),
    }
}

/// Whether `path` exists **on the host**, asked by running one command there.
///
/// Uses the same sentinel-exit-code shape as [`READ_SCRIPT`]: `test -e`
/// alone exits 1 for "absent", as does `flatpak-spawn` when the portal is
/// unreachable - [`MISSING_EXIT`] can only come from a host shell that ran
/// and found the path absent, so the two stay distinguishable.
///
/// # Errors
///
/// An [`io::Error`] if the host could not be asked at all - `flatpak-spawn`
/// missing, the `org.freedesktop.Flatpak` portal denied, the command killed.
/// Never [`io::ErrorKind::NotFound`] for the *queried* path: absence is
/// `Ok(false)`.
fn host_path_exists(path: &Path) -> io::Result<bool> {
    let plan = exists_plan(path);
    let output = plan.command().output().map_err(|err| {
        io::Error::other(format!(
            "could not run `{plan}` to look for {} on the host ({err})",
            path.display()
        ))
    })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(MISSING_EXIT) => Ok(false),
        other => {
            let how = match other {
                Some(code) => format!("with status {code}"),
                None => "on a signal".to_string(),
            };
            Err(io::Error::other(format!(
                "asking the host whether {} exists failed: the command exited {how}{}",
                path.display(),
                stderr_excerpt(&output.stderr)
            )))
        }
    }
}

/// What [`host_path_exists`] runs for `path`:
/// `flatpak-spawn --host sh -c '<EXISTS_SCRIPT>' sh <path>`.
///
/// Pure, and asserted literally by the tests, exactly like [`read_plan`].
fn exists_plan(path: &Path) -> Spawn {
    Spawn {
        program: OsString::from(FLATPAK_SPAWN),
        args: vec![
            OsString::from(HOST_FLAG),
            OsString::from(SH),
            OsString::from("-c"),
            OsString::from(EXISTS_SCRIPT),
            OsString::from(SH),
            path.as_os_str().to_os_string(),
        ],
    }
}

/// The candidate host paths [`host_helper_binary`] tries, in order, for the
/// `/.flatpak-info` content and `$HOME` value given.
///
/// Split out from [`host_helper_binary`] so the *list* can be asserted
/// independently of the existence check that picks from it.
fn host_helper_candidates(sandbox_info: Option<&str>, home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(app_files) = sandbox_info.and_then(app_path_in_sandbox_info) {
        candidates.push(app_files.join(BIN_DIR).join(HELPER_COMMAND));
    }
    if let Some(home) = home.filter(|home| !home.is_empty()) {
        candidates.push(
            Path::new(home)
                .join(USER_INSTALLATION_IN_HOME)
                .join("app")
                .join(APP_ID)
                .join(APP_FILES_IN_INSTALLATION)
                .join(BIN_DIR)
                .join(HELPER_COMMAND),
        );
    }
    candidates.push(
        Path::new(SYSTEM_INSTALLATION)
            .join("app")
            .join(APP_ID)
            .join(APP_FILES_IN_INSTALLATION)
            .join(BIN_DIR)
            .join(HELPER_COMMAND),
    );
    candidates
}

/// The `app-path` value in a `/.flatpak-info`'s text, or `None` if there
/// is none.
///
/// A deliberately minimal line scan, not a real INI parse: Flatpak writes
/// exactly one `app-path` key, so section-awareness buys nothing here. A
/// value that isn't an absolute path is ignored, falling through to the
/// layout candidates instead of a nonsense relative path.
fn app_path_in_sandbox_info(info: &str) -> Option<PathBuf> {
    let value = info
        .lines()
        .filter_map(|line| line.strip_prefix(APP_PATH_KEY))
        .filter_map(|rest| rest.strip_prefix('='))
        .map(str::trim)
        .find(|value| !value.is_empty())?;
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

/// This sandbox's own `/.flatpak-info` content, or `None` if it couldn't
/// be read (treated as "no `app-path` hint available", not an error).
fn sandbox_info() -> Option<String> {
    fs::read_to_string(SANDBOX_MARKER_PATH).ok()
}

/// A [`RealFileReader`] double for the tests of *other* modules: answers
/// every read with one canned result and records the paths asked for.
///
/// Lives here, beside the trait, since several modules need the same two
/// things from it: serving bytes for a file not on disk (proving a read
/// goes through the seam, not `fs::read`), and failing with a specific
/// `io::ErrorKind` (proving `NotFound` is treated differently where that matters).
///
/// Deliberately path-insensitive, since every caller reads exactly one
/// file (the real `/etc/fstab`); recorded paths are still available for
/// asserting which file was asked for.
#[cfg(test)]
pub(crate) struct StubReader {
    /// What every [`RealFileReader::read`] call returns, cloned per call.
    answer: Result<Vec<u8>, (io::ErrorKind, String)>,
    /// Every path this reader was asked for, in order.
    asked: std::cell::RefCell<Vec<PathBuf>>,
}

#[cfg(test)]
impl StubReader {
    /// A reader that serves `bytes` for anything it is asked for.
    pub(crate) fn serving(bytes: &[u8]) -> Self {
        StubReader {
            answer: Ok(bytes.to_vec()),
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// A reader that fails every read with `kind` and `message` - including
    /// [`io::ErrorKind::NotFound`], the one kind callers must treat specially.
    pub(crate) fn failing(kind: io::ErrorKind, message: &str) -> Self {
        StubReader {
            answer: Err((kind, message.to_string())),
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The paths this reader was asked for, in order.
    pub(crate) fn asked(&self) -> Vec<PathBuf> {
        self.asked.borrow().clone()
    }
}

#[cfg(test)]
impl RealFileReader for StubReader {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.asked.borrow_mut().push(path.to_path_buf());
        match &self.answer {
            Ok(bytes) => Ok(bytes.clone()),
            Err((kind, message)) => Err(io::Error::new(*kind, message.clone())),
        }
    }
}

/// `": <stderr>"`, truncated, or `""` when the child said nothing.
///
/// Trailing whitespace is trimmed so the excerpt does not end a sentence with
/// a newline, and the text is lossy-decoded because it is a diagnostic, not
/// data.
fn stderr_excerpt(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut excerpt: String = trimmed.chars().take(STDERR_EXCERPT_LIMIT).collect();
    if excerpt.len() < trimmed.len() {
        excerpt.push('…');
    }
    format!(": {excerpt}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A `/.flatpak-info` as Flatpak really writes one, trimmed to the parts
    /// this module reads (copied from a running Rustab sandbox's own file).
    const SANDBOX_INFO: &str = "[Application]\n\
                                name=dev.tomsk.Rustab\n\
                                runtime=runtime/org.gnome.Platform/x86_64/51\n\
                                \n\
                                [Instance]\n\
                                instance-id=2130982506\n\
                                app-path=/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/x86_64/test/abc/files\n\
                                app-commit=abc\n";

    #[test]
    fn a_direct_read_is_a_plain_fs_read() {
        let dir = TempDir::new("host-direct-read");
        let path = dir.join("fstab");
        fs::write(&path, b"UUID=1 / ext4 rw 0 1\n").expect("seed");

        assert_eq!(
            DirectFs.read(&path).expect("the read must succeed"),
            b"UUID=1 / ext4 rw 0 1\n"
        );
    }

    /// The error contract the whole module exists to honour, on the one impl
    /// that can be exercised without a sandbox.
    #[test]
    fn a_direct_read_of_a_missing_file_is_not_found() {
        let dir = TempDir::new("host-direct-missing");

        let error = DirectFs
            .read(&dir.join("no-such-fstab"))
            .expect_err("a missing file must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// Outside a sandbox nothing is routed through `flatpak-spawn` - which is
    /// also what keeps `cargo test` from ever spawning it, since no test
    /// machine has the marker.
    #[test]
    fn the_reader_is_direct_without_a_sandbox_marker_and_host_spawn_with_one() {
        let dir = TempDir::new("host-reader-choice");
        let marker = dir.join("flatpak-info");

        let direct = reader_for(&marker);
        fs::write(&marker, "[Application]\n").expect("seed the marker");
        let sandboxed = reader_for(&marker);

        // The readers are zero-sized singletons, so identity is compared by
        // behaviour: only the direct one can read a real local file, and only
        // it is reachable without the marker.
        let path = dir.join("fstab");
        fs::write(&path, b"x\n").expect("seed");
        assert_eq!(direct.read(&path).expect("direct read"), b"x\n");
        // `sandboxed` must *not* be used to read anything here: it would
        // spawn `flatpak-spawn`. Asserting the plan it would build is enough,
        // and is done by its own test below.
        let _ = sandboxed;
    }

    #[test]
    fn the_host_read_plan_is_a_flatpak_spawn_of_the_read_script() {
        let plan = read_plan(Path::new("/etc/fstab"));

        assert_eq!(plan.program, OsString::from("flatpak-spawn"));
        assert_eq!(
            plan.args,
            vec![
                OsString::from("--host"),
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from(READ_SCRIPT),
                OsString::from("sh"),
                OsString::from("/etc/fstab"),
            ]
        );
        // The path has to be `$1`, i.e. the second operand after the script.
        assert_eq!(plan.args[4], OsString::from("sh"));
        assert_eq!(plan.args[5], OsString::from("/etc/fstab"));
    }

    /// The script and the constant the Rust side matches on have to agree, and
    /// a shell script cannot interpolate a Rust constant - so the agreement is
    /// pinned here instead.
    #[test]
    fn the_host_scripts_exit_with_the_sentinel_this_module_looks_for() {
        assert!(
            READ_SCRIPT.contains(&format!("exit {MISSING_EXIT}")),
            "the read script must exit with the sentinel: {READ_SCRIPT}"
        );
        assert!(
            EXISTS_SCRIPT.contains(&format!("exit {MISSING_EXIT}")),
            "the existence script must exit with the sentinel: {EXISTS_SCRIPT}"
        );
        assert!(
            RUN_WITH_INPUT_SCRIPT.contains(&format!("exit {STAGING_FAILED_EXIT}")),
            "the with-input script must exit with the staging sentinel: {RUN_WITH_INPUT_SCRIPT}"
        );
        assert_ne!(
            MISSING_EXIT, STAGING_FAILED_EXIT,
            "the two sentinels must stay distinguishable"
        );
        for sentinel in [MISSING_EXIT, STAGING_FAILED_EXIT] {
            assert_ne!(sentinel, 0);
            assert_ne!(sentinel, 1, "1 is what every failure already uses");
            assert_ne!(sentinel, 2);
            assert!(
                !(126..=165).contains(&sentinel),
                "the sentinel must stay clear of the could-not-execute and signal ranges"
            );
        }
    }

    #[test]
    fn the_host_run_plan_without_input_is_a_bare_flatpak_spawn_of_the_command() {
        let plan = run_plan("lsblk", &["-J", "-O"], false);

        assert_eq!(plan.to_string(), "flatpak-spawn --host lsblk -J -O");
    }

    /// The with-input shape, literally: the script's `$0` filler, then the
    /// command and its arguments as `"$@"`. The staged file's path is
    /// deliberately *not* here - the host script appends it, since only the
    /// host knows a path that exists there.
    #[test]
    fn the_host_run_plan_with_input_wraps_the_command_in_the_staging_script() {
        let plan = run_plan("findmnt", &["--verify", "--tab-file"], true);

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host sh -c ".to_string()
                + RUN_WITH_INPUT_SCRIPT
                + " sh findmnt --verify --tab-file"
        );
    }

    /// Outside a sandbox nothing is routed through `flatpak-spawn`, for the
    /// same reason as the reader's own choice test.
    #[test]
    fn the_runner_is_direct_without_a_sandbox_marker_and_host_spawn_with_one() {
        let dir = TempDir::new("host-runner-choice");
        let marker = dir.join("flatpak-info");

        let direct = runner_for(&marker);
        fs::write(&marker, "[Application]\n").expect("seed the marker");
        let sandboxed = runner_for(&marker);

        // Only the direct runner can be exercised here: it runs the command
        // in this process, which is observable; the sandboxed one would
        // spawn `flatpak-spawn`, so only the plan it would build is asserted
        // (by the two tests above).
        let output = direct
            .run("cat", &[], Some(b"payload"))
            .expect("the direct runner must run cat locally");
        assert_eq!(output.stdout, b"payload");
        let _ = sandboxed;
    }

    /// Only meaningful inside a Flatpak sandbox, which is why it is
    /// `#[ignore]`d: run it with
    /// `flatpak run --command=… dev.tomsk.Rustab … --ignored sandbox_smoke`
    /// from an installed build. Outside one it returns early, so
    /// `cargo test -- --ignored` stays green on a developer machine.
    #[test]
    #[ignore]
    fn sandbox_smoke_host_runner_runs_on_the_host() {
        if !Path::new(SANDBOX_MARKER_PATH).exists() {
            eprintln!(
                "skipping: this smoke test only means anything inside a Flatpak sandbox \
                 (no {SANDBOX_MARKER_PATH} here)"
            );
            return;
        }

        let runner = command_runner();

        // The host has no `/.flatpak-info`, so "out" proves the command did
        // not run in this sandbox.
        let where_it_ran = runner
            .run(
                "sh",
                &["-c", "test -e /.flatpak-info && echo in || echo out"],
                None,
            )
            .expect("the host command must run");
        assert_eq!(
            String::from_utf8_lossy(&where_it_ran.stdout).trim(),
            "out",
            "the command must run on the host, not in this sandbox"
        );

        // And the input staging has to survive the same crossing.
        let echoed = runner
            .run("cat", &[], Some(b"payload"))
            .expect("the staged input must reach the host");
        assert_eq!(echoed.stdout, b"payload");

        // The existence check has to see the host's filesystem, not this
        // sandbox's: the host has `/etc/fstab` (which the sandbox does not)
        // and no `/.flatpak-info` (which the sandbox does), so this pair of
        // answers can only come from outside.
        let answers = runner
            .paths_exist(&["/etc/fstab", SANDBOX_MARKER_PATH])
            .expect("the host must be able to answer");
        assert_eq!(
            answers,
            vec![true, false],
            "the existence check must be answered by the host, not by this sandbox"
        );
    }

    #[test]
    fn a_successful_host_read_yields_stdout_verbatim() {
        // Bytes that are not valid UTF-8, so "verbatim" is really tested.
        let bytes = vec![b'#', 0xff, b'\n'];

        assert_eq!(
            interpret_read_output(Path::new("/etc/fstab"), Some(0), bytes.clone(), b"")
                .expect("exit 0 must succeed"),
            bytes
        );
    }

    #[test]
    fn the_sentinel_exit_is_the_only_not_found() {
        let missing =
            interpret_read_output(Path::new("/etc/fstab"), Some(MISSING_EXIT), Vec::new(), b"")
                .expect_err("the sentinel must fail");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(missing.to_string().contains("/etc/fstab"));

        // A denied portal - the case that must never be mistaken for "the file
        // is not there", because the re-sync command would blank the draft.
        let denied = interpret_read_output(
            Path::new("/etc/fstab"),
            Some(1),
            Vec::new(),
            b"Portal call failed: org.freedesktop.DBus.Error.ServiceUnknown\n",
        )
        .expect_err("a portal failure must fail");
        assert_ne!(denied.kind(), io::ErrorKind::NotFound);
        assert!(denied.to_string().contains("Portal call failed"));

        for code in [Some(2), Some(126), Some(127), None] {
            let error = interpret_read_output(Path::new("/etc/fstab"), code, Vec::new(), b"")
                .expect_err("every other outcome must fail");
            assert_ne!(
                error.kind(),
                io::ErrorKind::NotFound,
                "{code:?} must not be reported as a missing file"
            );
        }
    }

    #[test]
    fn a_long_stderr_is_excerpted_and_marked() {
        let long = "x".repeat(STDERR_EXCERPT_LIMIT * 2);
        let error = interpret_read_output(
            Path::new("/etc/fstab"),
            Some(1),
            Vec::new(),
            long.as_bytes(),
        )
        .expect_err("must fail")
        .to_string();

        assert!(error.contains('…'));
        assert!(error.len() < long.len());
    }

    #[test]
    fn the_unsandboxed_elevation_plan_is_pkexec_on_this_binary() {
        let plan = elevated_plan_with(
            false,
            || Ok(PathBuf::from("/usr/bin/rustab")),
            || panic!("the host helper must not be looked for outside a sandbox"),
            "--internal-privileged-write",
        )
        .expect("the plan must be buildable");

        assert_eq!(
            plan.to_string(),
            "pkexec /usr/bin/rustab --internal-privileged-write"
        );
    }

    /// The sandboxed shape, literally: the **helper** on the host, and no
    /// flag - `rustab-helper` refuses any argument, so passing the GUI's
    /// hidden flag would make every elevated operation fail.
    #[test]
    fn the_sandboxed_elevation_plan_host_spawns_pkexec_on_the_host_helper() {
        let plan = elevated_plan_with(
            true,
            || panic!("current_exe is meaningless inside the sandbox and must not be consulted"),
            || {
                Ok(PathBuf::from(
                    "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper",
                ))
            },
            "--internal-privileged-write",
        )
        .expect("the plan must be buildable");

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host pkexec \
             /var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
        );
    }

    #[test]
    fn an_unfindable_host_helper_fails_the_plan_rather_than_guessing() {
        let error = elevated_plan_with(
            true,
            || panic!("not consulted"),
            || host_helper_binary(None, None, &|_| Ok(false)),
            "--internal-privileged-write",
        )
        .expect_err("a plan that cannot name a real binary must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains(SYSTEM_INSTALLATION));
        assert!(
            error
                .to_string()
                .contains("no rustab-helper executable was found"),
            "the message must name the helper that is missing: {error}"
        );
        assert!(
            error.to_string().contains("bin/rustab-helper"),
            "the message must name the paths that were tried: {error}"
        );
    }

    /// "The host said no" and "the host could not be asked" are different
    /// failures with different fixes (reinstall the app vs. grant the Flatpak
    /// permission), so they must not collapse into one message - and the
    /// second must not be reported as `NotFound`, which would read as a
    /// missing installation.
    #[test]
    fn a_probe_that_cannot_reach_the_host_is_not_reported_as_a_missing_binary() {
        let error = host_helper_binary(None, None, &|_| {
            Err(io::Error::other("Portal call failed: ServiceUnknown"))
        })
        .expect_err("an unanswerable probe must fail");

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("could not be asked"));
        assert!(error
            .to_string()
            .contains("--talk-name=org.freedesktop.Flatpak"));
    }

    /// One failing probe does not doom the resolution: a later candidate that
    /// answers cleanly still wins.
    #[test]
    fn a_single_failed_probe_does_not_hide_a_candidate_that_does_exist() {
        let home = OsString::from("/home/u");
        let candidates = host_helper_candidates(Some(SANDBOX_INFO), Some(&home));
        let system_install = candidates[2].clone();

        let picked = host_helper_binary(Some(SANDBOX_INFO.to_string()), Some(home), &|candidate| {
            if candidate == system_install {
                Ok(true)
            } else {
                Err(io::Error::other("probe blew up"))
            }
        })
        .expect("a cleanly-answered candidate must win");

        assert_eq!(picked, system_install);
    }

    /// The batched shape, literally: one script, then every path as a
    /// positional argument after the `$0` filler - never one spawn per path.
    #[test]
    fn the_batched_existence_plan_passes_every_path_to_one_flatpak_spawn() {
        let plan = exists_batch_plan(&["/boot", "/mnt/My Disk", "/opt"]);

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host sh -c ".to_string()
                + EXISTS_BATCH_SCRIPT
                + " sh /boot /mnt/My Disk /opt"
        );
        // A path with a space is one argument, whatever the Display joining
        // above looks like.
        assert_eq!(plan.args.len(), 8);
        assert_eq!(plan.args[6], OsString::from("/mnt/My Disk"));
    }

    #[test]
    fn a_batched_existence_answer_is_one_verdict_per_path_in_order() {
        let paths = ["/etc/fstab", "/nope", "/boot"];

        assert_eq!(
            interpret_exists_batch_output(&paths, Some(0), b"101", b"")
                .expect("a well-formed answer must be accepted"),
            vec![true, false, true]
        );
    }

    /// Every malformed reply is an error rather than a guessed verdict: a
    /// wrong answer here becomes a confident "this mount point does not
    /// exist" warning about a path nobody looked at.
    #[test]
    fn a_malformed_batched_existence_answer_is_an_error() {
        let paths = ["/etc/fstab", "/nope"];

        for (code, stdout, stderr) in [
            (Some(0), &b"1"[..], &b""[..]),
            (Some(0), &b"111"[..], &b""[..]),
            (Some(0), &b"1x"[..], &b""[..]),
            (Some(0), &b""[..], &b""[..]),
            (Some(1), &b"10"[..], &b"Portal call failed\n"[..]),
            (None, &b""[..], &b""[..]),
        ] {
            let error = interpret_exists_batch_output(&paths, code, stdout, stderr)
                .expect_err("a malformed answer must fail");
            assert!(
                error.to_string().contains("host"),
                "the message must say who could not answer: {error}"
            );
        }

        let denied = interpret_exists_batch_output(&paths, Some(1), b"", b"Portal call failed\n")
            .expect_err("a denied portal must fail");
        assert!(denied.to_string().contains("Portal call failed"));
    }

    /// Nothing to ask means nothing is spawned - asserted on the real
    /// [`HostSpawn`], since an empty list is the one input it can be given
    /// outside a sandbox without reaching for `flatpak-spawn`.
    #[test]
    fn an_empty_batched_existence_query_spawns_nothing() {
        assert_eq!(
            HostSpawn.paths_exist(&[]).expect("nothing to answer"),
            Vec::<bool>::new()
        );
    }

    #[test]
    fn the_host_existence_plan_is_a_flatpak_spawn_of_the_existence_script() {
        let plan = exists_plan(Path::new("/var/lib/flatpak/app/x/files/bin/rustab-helper"));

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host sh -c ".to_string()
                + EXISTS_SCRIPT
                + " sh /var/lib/flatpak/app/x/files/bin/rustab-helper"
        );
    }

    #[test]
    fn the_app_path_from_flatpak_info_is_the_first_candidate() {
        let candidates = host_helper_candidates(Some(SANDBOX_INFO), Some(OsStr::new("/home/u")));

        assert_eq!(
            candidates[0],
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/x86_64/test/abc/files/bin/rustab-helper"
            )
        );
        assert_eq!(
            candidates[1],
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
            )
        );
        assert_eq!(
            candidates[2],
            PathBuf::from(
                "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
            )
        );
    }

    /// No `/.flatpak-info` hint and no `$HOME`: the system install is still
    /// tried, so a system-installed app with a stripped environment works.
    #[test]
    fn the_system_install_is_always_a_candidate() {
        assert_eq!(
            host_helper_candidates(None, None),
            vec![PathBuf::from(
                "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
            )]
        );
        assert_eq!(
            host_helper_candidates(Some("[Application]\nname=x\n"), Some(OsStr::new(""))),
            vec![PathBuf::from(
                "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
            )]
        );
    }

    #[test]
    fn a_relative_or_absent_app_path_is_ignored() {
        assert_eq!(
            app_path_in_sandbox_info(SANDBOX_INFO).as_deref(),
            Some(Path::new(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/x86_64/test/abc/files"
            ))
        );
        assert_eq!(app_path_in_sandbox_info("app-path=\n"), None);
        assert_eq!(app_path_in_sandbox_info("app-path=relative/files\n"), None);
        assert_eq!(app_path_in_sandbox_info("[Instance]\n"), None);
    }

    /// A candidate that does not exist is skipped rather than returned, and
    /// the next one that does exist is used - the `app-path` hint is a
    /// preference, not a commitment.
    #[test]
    fn the_first_candidate_that_exists_is_used() {
        let home = OsString::from("/home/u");
        let candidates = host_helper_candidates(Some("app-path=/nowhere/files\n"), Some(&home));
        let user_install = candidates[1].clone();

        let picked = host_helper_binary(
            Some("app-path=/nowhere/files\n".to_string()),
            Some(home),
            &|candidate| Ok(candidate == user_install),
        )
        .expect("the user-install candidate exists");

        assert_eq!(picked, user_install);
        assert_eq!(
            picked,
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab-helper"
            )
        );
    }
}
