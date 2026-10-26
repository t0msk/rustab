//! The one seam through which this crate asks the machine it is editing a
//! question.
//!
//! Three of this crate's checks cannot be answered from its own address
//! space. Two are delegated to util-linux rather than reimplemented:
//! `findmnt --verify --tab-file <path>` (semantic validation,
//! [`crate::validate`]) and `lsblk -J -O` (device discovery,
//! [`crate::discovery`]); the third is the advisory "does this mount point
//! exist on disk?" check ([`crate::validate`] again), which is a plain
//! `stat` rather than a program. All three only mean anything when they
//! look at *the machine whose fstab is being edited* - they read `/dev`,
//! udev, the live mount table and the real directory tree, none of which a
//! container necessarily shares with its host.
//!
//! That makes "which process answers this" a decision this crate must
//! not make on its own: a GUI inside a Flatpak sandbox has to route the
//! question out to the host, while a CLI, the elevated helper and the unit
//! tests all just answer it locally. So the choice is injected, exactly as
//! [`crate::writer`]'s `FileSystemOps` injects the filesystem: every
//! externally-observable entry point has a `*_with`/`*_with_runner`
//! variant taking a [`CommandRunner`], and the plain one passes
//! [`DirectRunner`]. Nothing here knows what a sandbox is, and nothing here
//! spawns anything but the program it was asked for.
//!
//! ## Why the existence check is batched
//!
//! [`CommandRunner::paths_exist`] takes *all* the paths a validation needs
//! answers for, not one. For [`DirectRunner`] that is irrelevant - it is a
//! `stat` per path either way - but an out-of-process runner pays a real
//! per-call cost (a portal round trip and a process spawn), and a document
//! has one mount point per entry. One question about N paths keeps that
//! cost independent of the file's size.
//!
//! ## Why `input` is a slice and not a path
//!
//! `findmnt --verify` has no stdin mode: it insists on a *seekable*
//! `--tab-file`, so a candidate document has to exist as a file somewhere
//! the `findmnt` process can open it. A caller that passed a path would be
//! deciding which filesystem that file lands on - and for an
//! out-of-process runner that is precisely the thing it cannot know (a
//! sandbox's `/tmp` is private; the host cannot open a path in it). So
//! callers hand over *bytes* and the runner stages them wherever its own
//! program will be able to read them, appending the resulting path as the
//! final argument.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// Asks the machine whose fstab is being edited a question - the whole of
/// this crate's access to anything outside its own address space.
///
/// Two methods, because there are two kinds of question: "run this program
/// and let me read its exit status and output streams myself"
/// ([`Self::run`]) and "do these paths exist?" ([`Self::paths_exist`]).
/// The second is not phrased as a program because it does not have to be
/// one - in this process it is a `stat`, and only an implementation that
/// has to cross a boundary needs a process for it.
///
/// Shaped after [`crate::writer`]'s own `FileSystemOps` seam: a production
/// implementation ([`DirectRunner`]) plus whatever doubles a test needs.
pub trait CommandRunner {
    /// Runs `program` with `args` and returns what it produced.
    ///
    /// When `input` is `Some`, the implementation must place those bytes
    /// in a file the program is able to read, append that file's path as
    /// the **final** argument (after everything in `args`), and remove the
    /// file once the program has finished - best effort, a removal failure
    /// is not an error. When `input` is `None`, `args` is passed through
    /// unchanged and no file is created.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] only when the program could not be run at all, or
    /// its input could not be staged. A program that ran and exited
    /// non-zero is `Ok`: deciding what its status, stdout and stderr mean
    /// is the caller's job, not the runner's.
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> io::Result<Output>;

    /// Answers, for every path in `paths`, whether it exists on the machine
    /// being inspected.
    ///
    /// Returns one `bool` per input path, in the same order, `true` iff the
    /// path exists there - [`std::path::Path::exists`]'s semantics,
    /// symlinks followed, no distinction between a file and a directory.
    /// Implementations must answer all of them in one round trip (see the
    /// module docs).
    ///
    /// # Errors
    ///
    /// An [`io::Error`] only when the question could not be answered *at
    /// all* - the machine could not be reached, or its answer was
    /// unintelligible. A path that simply is not there is `Ok(false)`, so
    /// there is no partial-success case to represent.
    fn paths_exist(&self, paths: &[&str]) -> io::Result<Vec<bool>>;
}

/// The runner for a process that is already looking at the machine it is
/// meant to inspect: a plain [`Command`], with input staged under
/// [`std::env::temp_dir`].
///
/// Used by every non-sandboxed launch, by the elevated helper, and by the
/// unit tests. Zero-sized and stateless, so it can be handed out as a
/// `&'static dyn CommandRunner` with nothing to own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectRunner;

impl CommandRunner for DirectRunner {
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> io::Result<Output> {
        let mut command = Command::new(program);
        command.args(args);

        let staged = match input {
            None => None,
            Some(bytes) => {
                let path = stage_scratch_file(bytes)?;
                command.arg(&path);
                Some(path)
            }
        };

        let output = command.output();

        // Removed whether or not the program ran: the file is this call's
        // alone, and its only purpose was to be read during it. Any removal
        // error is dropped on purpose so it cannot mask `output`'s.
        if let Some(path) = staged {
            let _ = fs::remove_file(&path);
        }

        output
    }

    /// One `stat` per path, in this process - no program is run, since this
    /// process is already looking at the filesystem being asked about.
    fn paths_exist(&self, paths: &[&str]) -> io::Result<Vec<bool>> {
        Ok(paths.iter().map(|p| Path::new(p).exists()).collect())
    }
}

/// Keeps scratch-file names from concurrent calls (e.g. parallel test
/// threads) from colliding, since [`std::process::id`] alone is constant
/// for the whole process lifetime.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to a fresh file under [`std::env::temp_dir`] and returns
/// its path, for [`DirectRunner`] to hand to the program it runs.
///
/// Only ever used for input a program *reads*, so it needs none of
/// [`crate::writer`]'s same-filesystem rename atomicity: nothing is
/// renamed onto a real target, and the file is removed as soon as the
/// program has exited.
fn stage_scratch_file(bytes: &[u8]) -> io::Result<PathBuf> {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "rustab-scratch-{}-{counter}.tmp",
        std::process::id()
    ));
    fs::write(&path, bytes)?;
    Ok(path)
}

/// One [`CommandRunner::run`] call, as [`RecordingRunner`] saw it.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedRun {
    /// The program the caller asked for.
    pub(crate) program: String,
    /// Its arguments, *excluding* the staged-input path a real runner
    /// would append - a double stages nothing, so what is recorded here is
    /// exactly what the caller passed.
    pub(crate) args: Vec<String>,
    /// The input bytes the caller handed over, if any.
    pub(crate) input: Option<Vec<u8>>,
}

/// How a [`RecordingRunner`] answers [`CommandRunner::paths_exist`].
#[cfg(test)]
enum ExistenceAnswer {
    /// Every path asked about exists - the default, so a test that is not
    /// about the existence check sees no findings from it.
    AllExist,
    /// Only these paths exist; anything else does not.
    Only(Vec<String>),
    /// The question cannot be answered at all, with this message.
    Unanswerable(String),
}

/// A [`CommandRunner`] double for the tests of *other* modules: answers
/// every call with one canned [`Output`] (or one canned existence verdict)
/// and records what it was asked.
///
/// Lives here, beside the trait, since both call sites need the same two
/// things from it - proving the exact command line a check is built from,
/// and feeding that check canned output without the real binary being
/// installed (or being allowed to touch this machine).
#[cfg(test)]
pub(crate) struct RecordingRunner {
    /// The exit code every call reports.
    code: i32,
    /// The stdout every call produces.
    stdout: Vec<u8>,
    /// Every call made, in order.
    runs: std::cell::RefCell<Vec<RecordedRun>>,
    /// What [`CommandRunner::paths_exist`] answers.
    existence: ExistenceAnswer,
    /// Every [`CommandRunner::paths_exist`] call's paths, in order - one
    /// entry per call, so "exactly one batched question" is assertable.
    exists_queries: std::cell::RefCell<Vec<Vec<String>>>,
}

#[cfg(test)]
impl RecordingRunner {
    /// A runner whose every call exits `code` and prints `stdout`, and for
    /// which every path exists.
    pub(crate) fn answering(code: i32, stdout: &str) -> Self {
        RecordingRunner {
            code,
            stdout: stdout.as_bytes().to_vec(),
            runs: std::cell::RefCell::new(Vec::new()),
            existence: ExistenceAnswer::AllExist,
            exists_queries: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The same runner, but reporting only `paths` as existing.
    pub(crate) fn with_existing_paths(mut self, paths: &[&str]) -> Self {
        self.existence =
            ExistenceAnswer::Only(paths.iter().map(|path| (*path).to_string()).collect());
        self
    }

    /// The same runner, but unable to answer the existence question at all.
    pub(crate) fn with_unanswerable_existence(mut self, message: &str) -> Self {
        self.existence = ExistenceAnswer::Unanswerable(message.to_string());
        self
    }

    /// The calls this runner was asked to make, in order.
    pub(crate) fn runs(&self) -> Vec<RecordedRun> {
        self.runs.borrow().clone()
    }

    /// The existence questions this runner was asked, in order.
    pub(crate) fn exists_queries(&self) -> Vec<Vec<String>> {
        self.exists_queries.borrow().clone()
    }
}

#[cfg(test)]
impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> io::Result<Output> {
        self.runs.borrow_mut().push(RecordedRun {
            program: program.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            input: input.map(<[u8]>::to_vec),
        });
        Ok(Output {
            // `from_raw` wants a `wait(2)` status, in which a normal exit's
            // code lives in the second byte.
            status: std::os::unix::process::ExitStatusExt::from_raw(self.code << 8),
            stdout: self.stdout.clone(),
            stderr: Vec::new(),
        })
    }

    fn paths_exist(&self, paths: &[&str]) -> io::Result<Vec<bool>> {
        self.exists_queries
            .borrow_mut()
            .push(paths.iter().map(|path| (*path).to_string()).collect());
        match &self.existence {
            ExistenceAnswer::AllExist => Ok(vec![true; paths.len()]),
            ExistenceAnswer::Only(existing) => Ok(paths
                .iter()
                .map(|path| existing.iter().any(|known| known == path))
                .collect()),
            ExistenceAnswer::Unanswerable(message) => Err(io::Error::other(message.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `cat` with no arguments of its own: the only way it can print
    /// `payload` is if the staged file's path was appended as an argument
    /// and the file held the input bytes.
    #[test]
    fn staged_input_is_appended_as_the_final_argument_and_holds_the_bytes() {
        let output = DirectRunner
            .run("cat", &[], Some(b"payload"))
            .expect("cat must be runnable");

        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        assert_eq!(output.stdout, b"payload");
    }

    /// The path really is *after* the caller's own arguments, and the file
    /// is gone once the program has finished.
    #[test]
    fn the_staged_file_is_removed_after_the_program_exits() {
        // `sh -c <script> sh <path>` puts the appended path in `$1`, so the
        // program reports the path it was given.
        let output = DirectRunner
            .run("sh", &["-c", r#"printf %s "$1""#, "sh"], Some(b"payload"))
            .expect("sh must be runnable");

        let reported = String::from_utf8(output.stdout).expect("a path is UTF-8 here");
        assert!(
            reported.contains("rustab-scratch-"),
            "the appended argument must be the staged file: {reported}"
        );
        assert!(
            !Path::new(&reported).exists(),
            "the staged file must not outlive the call: {reported}"
        );
    }

    #[test]
    fn no_input_means_no_extra_argument() {
        let output = DirectRunner
            .run("sh", &["-c", r#"printf %s "$#""#, "sh"], None)
            .expect("sh must be runnable");

        assert_eq!(output.stdout, b"0");
    }

    /// A non-zero exit is the caller's to interpret, not an `Err` - both
    /// `findmnt --verify` and `lsblk` depend on this.
    #[test]
    fn a_program_that_exits_non_zero_is_still_ok() {
        let output = DirectRunner
            .run("sh", &["-c", "exit 3"], None)
            .expect("a non-zero exit must not be an error");

        assert_eq!(output.status.code(), Some(3));
    }

    /// One answer per question, in order - and `/` is the one path every
    /// machine running this test is guaranteed to have.
    #[test]
    fn the_direct_runner_answers_every_path_in_order() {
        let answers = DirectRunner
            .paths_exist(&["/", "/rustab-no-such-path", "/"])
            .expect("a local stat must be answerable");

        assert_eq!(answers, vec![true, false, true]);
        assert_eq!(
            DirectRunner.paths_exist(&[]).expect("nothing to answer"),
            Vec::<bool>::new()
        );
    }

    #[test]
    fn a_program_that_cannot_be_run_is_an_error() {
        let error = DirectRunner
            .run("rustab-no-such-program", &[], None)
            .expect_err("a missing program must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
