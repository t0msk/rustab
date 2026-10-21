//! Reaching the **real host** `/etc/` from inside a Flatpak sandbox - the one
//! place that knows this process might not be looking at the machine it is
//! running on (T40).
//!
//! ## The problem, and the empirically confirmed answer
//!
//! Flatpak reserves the whole `/etc` path space. The `/etc` a sandboxed
//! process sees is the runtime's own, and `--filesystem=/etc/fstab:rw` is
//! **not** honoured at all - the sandbox refuses the grant outright
//! (`F: Not sharing "/etc/fstab" with sandbox: Path "/etc" is reserved by
//! Flatpak`, confirmed by running two throwaway diagnostic Flatpak apps).
//! Nothing reaches the real file through it: not a read, not a write, and not
//! a write made by an elevated child either, because `pkexec` changes the
//! **UID** of a process, not its **mount namespace** - the elevated child
//! inherits the sandbox it was started in and so sees the same synthetic
//! `/etc`. T39's module docs in [`crate::backup_location`] assumed otherwise;
//! that assumption is what this module corrects.
//!
//! What does work, confirmed end-to-end with a manifest granting only
//! `--talk-name=org.freedesktop.Flatpak` and no `--filesystem=` at all:
//!
//! - `flatpak-spawn --host cat /etc/fstab` prints the real host file verbatim;
//! - `flatpak-spawn --host id` reports the real host user, which is the proof
//!   that the spawned process is genuinely outside the sandbox's mount
//!   namespace rather than merely outside its cgroup;
//! - `flatpak-spawn --host sh -c 'touch /etc/.test'` fails with
//!   `Permission denied`, exactly as an unprivileged host process should;
//! - `flatpak-spawn --host pkexec sh -c 'touch /etc/.test && rm -f /etc/.test'`
//!   **succeeds** - genuine, unrestricted root access to the real `/etc/`.
//!
//! `flatpak-spawn` is part of the `org.gnome.Platform`/`org.freedesktop.*`
//! runtimes this app already builds against, so this is not a new runtime
//! dependency. It is the standard mechanism Flatpak'd applications that have
//! to touch host system files use, and it is what this module wraps.
//!
//! ## What this module provides, and what it deliberately does not
//!
//! Two things, and nothing else:
//!
//! 1. [`RealFileReader`] - an **unprivileged read** of a host path, injected
//!    into the three production call sites that read the real `/etc/fstab`
//!    (`crate::target`'s draft refresh and [`crate::target::ExternalChangeGuard`]'s
//!    check, [`crate::commands`]'s re-sync command, and
//!    [`crate::backup_location::refresh_mirror`]). A one-method trait with a
//!    production impl and a test double, deliberately in the shape
//!    `fstab_core::writer`'s own `FileSystemOps` seam already established for
//!    this codebase.
//! 2. [`elevated_self_plan`] - *how* the elevated helper is spawned, which is
//!    the only thing T40 changes in [`crate::privileged`]: under Flatpak the
//!    `pkexec` call has to be made **on the host** (`flatpak-spawn --host
//!    pkexec …`), and it has to name a **host** path for this binary, because
//!    `std::env::current_exe()` inside the sandbox yields `/app/bin/rustab`,
//!    a path that does not exist outside it.
//!
//! What it does *not* do is hide the sandbox from the rest of the program.
//! Every function here is explicit about which side of the boundary it acts
//! on, every seam is injected rather than resolved globally inside the code
//! that uses it, and nothing in this module ever writes a file: the single
//! write path in Rustab is still `fstab_core::writer::write_document`, called
//! from exactly one elevated process (see [`crate::privileged`]).
//!
//! ## Why `cargo test` can never reach any of this
//!
//! The production reader is chosen by [`real_file_reader`], which is called
//! only from the handful of production wrappers ([`crate::target::active_target`],
//! the `#[tauri::command]` functions) that also fill in the real `/etc/fstab`
//! path. Every testable core function takes its reader as a parameter and the
//! tests pass [`DirectFs`] or a double of their own, exactly as
//! `target::resolve` and `backup_location::detect` already take the paths they
//! would otherwise have read from constants. The two pieces that genuinely
//! have to spawn a process ([`HostSpawn::read`] and the elevated spawn) are
//! split into a pure "what would be run" half ([`Spawn`], asserted literally
//! by the tests below) and a pure "what did it mean" half
//! ([`interpret_read_output`]), the same split
//! `privileged::interpret_write_output` already uses for the elevated call.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
///
/// Lives here rather than in [`crate::backup_location`] (where T39 first
/// introduced it) because it is now the input to *two* unrelated decisions -
/// "how do I read the real `/etc/fstab`?" and "where do this file's manual
/// backups live?" - and a second copy of the string is a future disagreement
/// waiting to happen. `backup_location` reads it from here.
///
/// Note the one property that makes the whole T40 design hold together: the
/// process `flatpak-spawn --host` starts is **outside** the sandbox, so it has
/// no `/.flatpak-info`. The elevated helper therefore selects [`DirectFs`] on
/// its own, with no flag and no argument to pass it, and
/// `privileged::handle_write`'s own `fs::read` of the target is a read of the
/// real host file - which is exactly what that check needs and why nothing in
/// `privileged`'s elevated half had to change.
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
/// - **Parse `cat`'s stderr** (`cat: /etc/fstab: No such file or directory`):
///   locale-dependent and implementation-dependent. Rejected outright.
/// - **A preceding `flatpak-spawn --host test -e <path>` probe**: `test`
///   exits 1 for "does not exist", but `flatpak-spawn` *itself* also exits 1
///   when it cannot reach the portal at all (confirmed: a sandbox without
///   `--talk-name=org.freedesktop.Flatpak` prints `Portal call failed` and
///   fails). The two cases are indistinguishable from the exit code, which is
///   precisely the confusion that must not happen - and it costs a second
///   process per read.
/// - **This script**: the only way to see [`MISSING_EXIT`] is for a host
///   shell to have run and found the path absent. `flatpak-spawn` failing,
///   `sh` or `cat` missing, the portal being denied, a permission error, a
///   signal - none of them can produce that number, so all of them land on
///   "some other read error", which is the safe direction to be wrong in.
///
/// The path is passed as a positional argument (`$1`) and never interpolated
/// into the script text, so there is no quoting or injection question to get
/// wrong - even though in production the only path this is ever called with is
/// the hardcoded [`crate::target::REAL_FSTAB_PATH`].
const READ_SCRIPT: &str = r#"if [ -e "$1" ]; then exec cat -- "$1"; else exit 44; fi"#;

/// The script [`host_path_exists`] runs on the host: succeed if the path is
/// there, exit [`MISSING_EXIT`] if it is not.
///
/// A separate script from [`READ_SCRIPT`] rather than a `-e`-only mode of it,
/// because the two answer different questions ("what does this file hold?" vs.
/// "is this path there?") and the second one's whole value is that it cannot
/// be confused with the first one's failure. Same positional-argument and
/// sentinel conventions; see [`READ_SCRIPT`] for the reasoning behind both.
const EXISTS_SCRIPT: &str = r#"if [ -e "$1" ]; then exit 0; else exit 44; fi"#;

/// The exit code [`READ_SCRIPT`] and [`EXISTS_SCRIPT`] use to say "that path
/// does not exist on the host" - the only thing [`interpret_read_output`] ever
/// maps to [`io::ErrorKind::NotFound`], and the only thing
/// [`host_path_exists`] ever reports as a clean "no".
///
/// Deliberately not 1 or 2 (which `sh`, `cat` and `flatpak-spawn` all use for
/// their own failures) and well outside the 126/127/128+n range a shell uses
/// for "could not execute" and "died on a signal". The number is duplicated
/// literally inside both scripts because a shell script cannot interpolate a
/// Rust constant; [`tests::the_host_scripts_exit_with_the_sentinel_this_module_looks_for`]
/// pins the three together.
const MISSING_EXIT: i32 = 44;

/// How much of a failed host command's stderr is quoted back in the
/// `io::Error` it becomes.
///
/// The same bound, for the same reason, as `privileged`'s own
/// `STDERR_EXCERPT_LIMIT`: enough to carry a portal error or a shell
/// diagnostic into a bug report, not enough to paste a runaway program's whole
/// output into a UI string. Kept as a separate constant rather than shared
/// because the two modules' excerpts travel to different places and neither
/// should silently change the other's.
const STDERR_EXCERPT_LIMIT: usize = 500;

/// Rustab's Flatpak application id - `tauri.conf.json`'s `identifier`.
const APP_ID: &str = "dev.tomsk.Rustab";

/// The `command:` the Flatpak manifest installs into `/app/bin`, i.e. the
/// final component of this binary's path inside an installed Flatpak.
const APP_COMMAND: &str = "rustab";

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
/// `install -Dm755 … /app/bin/rustab` lands in (`/app` *is* `files`).
const BIN_DIR: &str = "bin";

// ---------------------------------------------------------------------------
// Reading a host path
// ---------------------------------------------------------------------------

/// Reads a file that lives on the **host**, whichever side of a sandbox
/// boundary this process is on.
///
/// One method, because one operation is all the real `/etc/fstab`'s readers
/// need: every one of them wants the file's whole current content as bytes and
/// nothing else (no metadata, no directory listing, no partial read). Shaped
/// after `fstab_core::writer`'s `FileSystemOps` trait - a method per syscall,
/// a production impl, a test double - because that is this codebase's
/// established way of making filesystem-touching code testable, and a second
/// convention for the same job would be worse than a slightly small trait.
///
/// Implementations must honour `fs::read`'s error contract:
/// [`io::ErrorKind::NotFound`] **only** for a file that is genuinely not
/// there, and some other kind for every other failure, including a failure of
/// the transport itself. See [`READ_SCRIPT`] for why that distinction is
/// load-bearing rather than tidy.
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
/// Stateless and uncached on purpose. Every caller of this reader is asking
/// "what does the real `/etc/fstab` hold **right now**" - the draft refresh at
/// startup, the external-change check immediately before a write, the mirror
/// refresh immediately before a manual backup - and a cache is exactly the
/// thing that would make those answers stale. There are at most a handful of
/// these reads in a session, each of a file measured in kilobytes, so the
/// process spawn is not worth optimizing away.
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
/// Pure, and asserted literally by
/// [`tests::the_host_read_plan_is_a_flatpak_spawn_of_the_read_script`], so the
/// exact command line is reviewable and regression-tested without a sandbox or
/// a process.
///
/// The second `sh` is the script's `$0`: `sh -c '<script>' <argv0> <argv1…>`
/// is how a POSIX shell assigns positional parameters, so the path has to be
/// the *second* operand to end up as `$1`. Getting this wrong would make the
/// script test and `cat` an empty path, which is why it has a test of its own.
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
/// Pure, for the same reason `privileged::interpret_write_output` is: the
/// interesting cases (a missing file, a denied portal, a signal) are
/// classified by a function a unit test can call directly, so none of them
/// needs a sandbox to be covered.
///
/// `stdout` is taken by value because the success path hands those exact bytes
/// to the caller - there is no transformation, no trimming and no UTF-8
/// assumption anywhere in this path, so a file whose content is not text
/// survives a host read byte-for-byte, exactly as `fs::read` would have left
/// it.
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
// Spawning the elevated helper
// ---------------------------------------------------------------------------

/// A command to run, as the program plus the arguments it would be given -
/// everything about a spawn except the spawning.
///
/// Exists so that "which command does Rustab run to become root?" is a value a
/// test can assert on and an error message can quote, rather than something
/// only observable by actually running it. [`crate::privileged`]'s one
/// remaining spawn site turns one of these into a `Command` and adds its own
/// stdio configuration; nothing here has an opinion about pipes.
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
    /// assertions. Lossy for non-UTF-8 arguments, which is acceptable for a
    /// diagnostic string - and never reached in production, where every
    /// argument is a hardcoded constant or an absolute path under
    /// `/var/lib/flatpak`, `$HOME` or `/etc`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program.to_string_lossy())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

/// How this process re-executes itself as root with `flag`, on whichever side
/// of a sandbox boundary it has to happen.
///
/// The production entry point for [`crate::privileged`]'s one spawn, and the
/// only part of that module T40 changes. Two shapes:
///
/// - **not sandboxed**: `pkexec <current_exe> <flag>`, byte-for-byte what
///   `privileged::run_elevated` did before T40;
/// - **sandboxed**: `flatpak-spawn --host pkexec <host path of this binary>
///   <flag>`.
///
/// Both halves of the sandboxed form are necessary and neither is sufficient:
/// `pkexec` alone would authorize a root process *inside* the sandbox, which
/// cannot see the real `/etc/` (`pkexec` changes UID, not mount namespace);
/// `flatpak-spawn --host` alone would reach the real `/etc/` as an
/// unprivileged user, which cannot write it. Confirmed empirically in both
/// directions - see the module docs.
///
/// `current_exe()` is not usable in the sandboxed form: it resolves to
/// `/app/bin/rustab`, which exists only inside the sandbox, so a host process
/// handed that path would simply fail to execute. [`host_binary`] finds the
/// path the same binary has *on the host* instead.
///
/// # Errors
///
/// An [`io::Error`] if the path of the binary to run could not be determined:
/// `current_exe()`'s own error outside a sandbox, or [`host_binary`]'s
/// "nowhere to be found" error inside one. In both cases nothing has been
/// spawned and the caller has not acted.
pub(crate) fn elevated_self_plan(flag: &str) -> io::Result<Spawn> {
    elevated_self_plan_with(
        Path::new(SANDBOX_MARKER_PATH).exists(),
        std::env::current_exe,
        || host_binary(sandbox_info(), std::env::var_os("HOME"), &host_path_exists),
        flag,
    )
}

/// The decision inside [`elevated_self_plan`], with every input injected.
///
/// `current_exe` and `host_binary` are closures rather than resolved values so
/// that neither is evaluated in the branch that does not use it: a test for
/// the sandboxed plan must not depend on this binary's own path, and a test
/// for the ordinary plan must not depend on a Flatpak installation existing.
fn elevated_self_plan_with(
    sandboxed: bool,
    current_exe: impl FnOnce() -> io::Result<PathBuf>,
    host_binary: impl FnOnce() -> io::Result<PathBuf>,
    flag: &str,
) -> io::Result<Spawn> {
    if sandboxed {
        let binary = host_binary()?;
        Ok(Spawn {
            program: OsString::from(FLATPAK_SPAWN),
            args: vec![
                OsString::from(HOST_FLAG),
                OsString::from(PKEXEC),
                binary.into_os_string(),
                OsString::from(flag),
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

/// This binary's path **on the host**, for a process running inside a Flatpak
/// sandbox.
///
/// Three candidates, in order, first one that exists **on the host** wins:
///
/// 1. `<app-path from /.flatpak-info>/bin/rustab`. Flatpak writes the host
///    path of this very app's `files` directory into the sandbox's own
///    `/.flatpak-info` (`[Instance] app-path=…`, confirmed by reading that
///    file inside a running Rustab sandbox), so this candidate is not a guess
///    at all: it is the answer from the thing that built the sandbox, and it
///    is correct for a user install, a system install, a custom
///    `--installation=`, a non-default architecture and a non-`stable` branch
///    alike. The sandbox's own `/app` *is* that directory, bind-mounted, so
///    `<app-path>/bin/rustab` and the in-sandbox `/app/bin/rustab` are the
///    same file.
/// 2. The user-install layout:
///    `$HOME/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab`.
/// 3. The system-install layout, which is what a Flathub user who ran
///    `flatpak install flathub dev.tomsk.Rustab` without `--user` has:
///    `/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab`.
///
/// 2 and 3 are kept even though 1 subsumes them in every case seen so far,
/// because 1 depends on a key in a file this code does not own: if a future
/// Flatpak version renames or drops `app-path`, the two layout guesses are
/// still right for the overwhelming majority of installs.
///
/// ## Why `exists` has to be a *host-side* check, and is therefore fallible
///
/// This is the trap in the obvious implementation, and it is worth spelling
/// out because `Path::exists()` looks like the right tool and is not: none of
/// these three paths exists *as seen from inside the sandbox*. The app's own
/// files are visible there only as `/app`, and `$HOME` is replaced by a
/// per-app directory, so a plain `Path::exists()` would answer `false` for
/// every candidate including the correct one, and this function would always
/// report "no host-side executable found" on exactly the installs it is meant
/// to work on. The predicate therefore asks the **host** (production passes
/// [`host_path_exists`], which is one more `flatpak-spawn --host` call), and
/// it returns `io::Result<bool>` rather than `bool` so that "the host says no"
/// and "the host could not be asked" stay distinguishable - they need
/// different error messages, and conflating them would blame a missing
/// installation for what is really a missing permission.
///
/// Nothing is *guessed past*: if the host says none of the three is there,
/// this fails with a message naming all of them, because the alternative
/// (handing `pkexec` a path that is not there) would surface as a bare
/// authorization failure *after* the user had already typed their password.
///
/// # Errors
///
/// [`io::ErrorKind::NotFound`] if the host reports that none of the candidates
/// exists; the first probe error (kind preserved) if the host could not be
/// asked about any of them at all.
fn host_binary(
    sandbox_info: Option<String>,
    home: Option<OsString>,
    exists: &dyn Fn(&Path) -> io::Result<bool>,
) -> io::Result<PathBuf> {
    let candidates = host_binary_candidates(sandbox_info.as_deref(), home.as_deref());
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
                "Rustab is running inside a Flatpak sandbox, so the elevated helper has to be \
                 started from this app's host-side path, but the host could not be asked \
                 whether that path ({named}) exists: {err}. Saving the real /etc/fstab needs \
                 the Flatpak permission --talk-name=org.freedesktop.Flatpak."
            ),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Rustab is running inside a Flatpak sandbox, so the elevated helper has to be \
                 started from this app's host-side path, but no Rustab executable was found at \
                 {named}. Reinstalling the Flatpak \
                 (`flatpak install --reinstall dev.tomsk.Rustab`) restores it; until then, \
                 saving the real /etc/fstab cannot work from this sandbox."
            ),
        )),
    }
}

/// Whether `path` exists **on the host**, asked by running one command there.
///
/// Uses the same sentinel-exit-code shape as [`READ_SCRIPT`], for the same
/// reason: `test -e` on its own exits 1 for "absent", and so does
/// `flatpak-spawn` when the portal is unreachable, which would make a missing
/// permission indistinguishable from a missing file. [`MISSING_EXIT`] can only
/// come from a host shell that ran and found the path absent.
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

/// The candidate host paths [`host_binary`] tries, in order, for the
/// `/.flatpak-info` content and `$HOME` value given.
///
/// Split out from [`host_binary`] so the *list* can be asserted
/// independently of the existence check that picks from it.
fn host_binary_candidates(sandbox_info: Option<&str>, home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(app_files) = sandbox_info.and_then(app_path_in_sandbox_info) {
        candidates.push(app_files.join(BIN_DIR).join(APP_COMMAND));
    }
    if let Some(home) = home.filter(|home| !home.is_empty()) {
        candidates.push(
            Path::new(home)
                .join(USER_INSTALLATION_IN_HOME)
                .join("app")
                .join(APP_ID)
                .join(APP_FILES_IN_INSTALLATION)
                .join(BIN_DIR)
                .join(APP_COMMAND),
        );
    }
    candidates.push(
        Path::new(SYSTEM_INSTALLATION)
            .join("app")
            .join(APP_ID)
            .join(APP_FILES_IN_INSTALLATION)
            .join(BIN_DIR)
            .join(APP_COMMAND),
    );
    candidates
}

/// The `app-path` value in a `/.flatpak-info`'s text, or `None` if there is
/// none.
///
/// A deliberately minimal line scan rather than a real INI parse: this file
/// has exactly one `app-path` key, in one section, written by Flatpak itself,
/// and the only thing a section-aware parser would buy here is the ability to
/// ignore an `app-path` in some *other* section - which does not exist. A
/// value that is not an absolute path is ignored, so a malformed or
/// unexpected file falls through to the layout candidates instead of producing
/// a nonsense relative path.
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

/// This sandbox's own `/.flatpak-info` content, or `None` if it could not be
/// read (which for a sandboxed process would be surprising, and is simply
/// treated as "no `app-path` hint available" rather than as an error).
///
/// Read as UTF-8 text because the only key read from it is an absolute path
/// written by Flatpak; a file that is not UTF-8 at all is not one this code can
/// learn anything from.
fn sandbox_info() -> Option<String> {
    fs::read_to_string(SANDBOX_MARKER_PATH).ok()
}

/// A [`RealFileReader`] double for the tests of *other* modules: it answers
/// every read with one canned result and records the paths it was asked for.
///
/// Lives here, beside the trait, rather than being re-written inside each
/// module's `mod tests`, because three modules need the same two things from
/// it - "serve these bytes for a file that is **not** on this machine's disk"
/// (which is what proves a read really goes through the seam rather than
/// through `fs::read`) and "fail with exactly this `io::ErrorKind`" (which is
/// what proves `NotFound` and "could not reach the host" are treated
/// differently where that difference matters).
///
/// Deliberately path-insensitive: every caller of the seam reads exactly one
/// file - the real `/etc/fstab` - so a per-path map would add bookkeeping
/// without adding coverage. The recorded paths are still available so a test
/// can assert *which* file was asked for.
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
        assert_ne!(MISSING_EXIT, 0);
        assert_ne!(MISSING_EXIT, 1, "1 is what every failure already uses");
        assert_ne!(MISSING_EXIT, 2);
        assert!(
            !(126..=165).contains(&MISSING_EXIT),
            "the sentinel must stay clear of the could-not-execute and signal ranges"
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
        let plan = elevated_self_plan_with(
            false,
            || Ok(PathBuf::from("/usr/bin/rustab")),
            || panic!("the host binary must not be looked for outside a sandbox"),
            "--internal-privileged-write",
        )
        .expect("the plan must be buildable");

        assert_eq!(
            plan.to_string(),
            "pkexec /usr/bin/rustab --internal-privileged-write"
        );
    }

    #[test]
    fn the_sandboxed_elevation_plan_host_spawns_pkexec_on_the_host_binary() {
        let plan = elevated_self_plan_with(
            true,
            || panic!("current_exe is meaningless inside the sandbox and must not be consulted"),
            || {
                Ok(PathBuf::from(
                    "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab",
                ))
            },
            "--internal-privileged-write",
        )
        .expect("the plan must be buildable");

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host pkexec \
             /var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab \
             --internal-privileged-write"
        );
    }

    #[test]
    fn an_unfindable_host_binary_fails_the_plan_rather_than_guessing() {
        let error = elevated_self_plan_with(
            true,
            || panic!("not consulted"),
            || host_binary(None, None, &|_| Ok(false)),
            "--internal-privileged-write",
        )
        .expect_err("a plan that cannot name a real binary must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains(SYSTEM_INSTALLATION));
        assert!(
            error.to_string().contains("no Rustab executable was found"),
            "the message must say the installation is missing: {error}"
        );
    }

    /// "The host said no" and "the host could not be asked" are different
    /// failures with different fixes (reinstall the app vs. grant the Flatpak
    /// permission), so they must not collapse into one message - and the
    /// second must not be reported as `NotFound`, which would read as a
    /// missing installation.
    #[test]
    fn a_probe_that_cannot_reach_the_host_is_not_reported_as_a_missing_binary() {
        let error = host_binary(None, None, &|_| {
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
        let candidates = host_binary_candidates(Some(SANDBOX_INFO), Some(&home));
        let system_install = candidates[2].clone();

        let picked = host_binary(Some(SANDBOX_INFO.to_string()), Some(home), &|candidate| {
            if candidate == system_install {
                Ok(true)
            } else {
                Err(io::Error::other("probe blew up"))
            }
        })
        .expect("a cleanly-answered candidate must win");

        assert_eq!(picked, system_install);
    }

    #[test]
    fn the_host_existence_plan_is_a_flatpak_spawn_of_the_existence_script() {
        let plan = exists_plan(Path::new("/var/lib/flatpak/app/x/files/bin/rustab"));

        assert_eq!(
            plan.to_string(),
            "flatpak-spawn --host sh -c ".to_string()
                + EXISTS_SCRIPT
                + " sh /var/lib/flatpak/app/x/files/bin/rustab"
        );
    }

    #[test]
    fn the_app_path_from_flatpak_info_is_the_first_candidate() {
        let candidates = host_binary_candidates(Some(SANDBOX_INFO), Some(OsStr::new("/home/u")));

        assert_eq!(
            candidates[0],
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/x86_64/test/abc/files/bin/rustab"
            )
        );
        assert_eq!(
            candidates[1],
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab"
            )
        );
        assert_eq!(
            candidates[2],
            PathBuf::from("/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab")
        );
    }

    /// No `/.flatpak-info` hint and no `$HOME`: the system install is still
    /// tried, so a system-installed app with a stripped environment works.
    #[test]
    fn the_system_install_is_always_a_candidate() {
        assert_eq!(
            host_binary_candidates(None, None),
            vec![PathBuf::from(
                "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab"
            )]
        );
        assert_eq!(
            host_binary_candidates(Some("[Application]\nname=x\n"), Some(OsStr::new(""))),
            vec![PathBuf::from(
                "/var/lib/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab"
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
        let candidates = host_binary_candidates(Some("app-path=/nowhere/files\n"), Some(&home));
        let user_install = candidates[1].clone();

        let picked = host_binary(
            Some("app-path=/nowhere/files\n".to_string()),
            Some(home),
            &|candidate| Ok(candidate == user_install),
        )
        .expect("the user-install candidate exists");

        assert_eq!(picked, user_install);
        assert_eq!(
            picked,
            PathBuf::from(
                "/home/u/.local/share/flatpak/app/dev.tomsk.Rustab/current/active/files/bin/rustab"
            )
        );
    }
}
