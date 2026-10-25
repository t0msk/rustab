//! Which file is "the real `/etc/fstab`", and what a write has to know
//! about it before it starts.
//!
//! The slice of target handling the **elevated** side needs, and nothing
//! else. Choosing the active target, the working draft, its refresh and its
//! last-known-content marker all stay in the GUI crate (`rustab`'s
//! `target` module), which re-exports everything here so its callers see
//! one module.
//!
//! Both pieces are here because both are re-checked *inside* the elevated
//! process, on data it received over a pipe:
//!
//! - [`is_same_file`] is the identity test [`crate::protocol`] applies to
//!   the `target` of every request, so one implementation decides both "is
//!   this the real file, escalate" on the unprivileged side and "am I
//!   allowed to touch this" on the elevated side - they can never disagree.
//! - [`ExternalChangeGuard`] carries what the unprivileged side believed
//!   `/etc/fstab` held, so the elevated process can re-read the file and
//!   compare immediately before writing. `pkexec` blocks on a human typing
//!   a password, so a check made only before the prompt would leave the
//!   largest part of the window uncovered.

use std::fs;
use std::io;
use std::path::Path;

/// The one path that counts as "the real thing": the only file Rustab ever
/// escalates for, and the only one the elevated side will act on.
pub const REAL_FSTAB_PATH: &str = "/etc/fstab";

/// Returns `true` if `path` and `other` name the same file: same literal
/// path, or both canonicalize to one destination (so a symlink in either
/// direction still counts). A path that can't be canonicalized is only
/// matched literally.
///
/// Used by the GUI side to decide whether the active target *is* the real
/// `/etc/fstab` (and so whether a save escalates), and by the elevated side
/// to decide whether a target that arrived as data is the one file it may
/// touch (see [`crate::protocol`]). One implementation, so the two checks
/// cannot drift apart.
pub fn is_same_file(path: &Path, other: &Path) -> bool {
    if path == other {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(other)) {
        (Ok(resolved), Ok(resolved_other)) => resolved == resolved_other,
        _ => false,
    }
}

/// What a real-file write knows about `/etc/fstab`'s content before it
/// starts, and whether the user has already chosen to overwrite an
/// external change.
///
/// The whole input of the "did the real file change under us?" check,
/// travelling as two flat fields inside `protocol::PrivilegedRequest::Write`
/// so the check can run *again*, inside the elevated process, immediately
/// before `write_document` runs - the authoritative check, since `pkexec`'s
/// password wait leaves a window for the file to change that a
/// pre-prompt-only check would miss.
///
/// The comparison is the marker's recorded bytes vs. `/etc/fstab`'s
/// current bytes, never the draft against the real file - a draft
/// differing from the marker is just unsaved edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChangeGuard {
    /// The content Rustab last knew the real `/etc/fstab` to hold - the
    /// last-known-real marker's content, as recorded by the last refresh
    /// or the last successful save.
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
    /// The guard for a write that has no marker to compare against at all:
    /// case 3 of the GUI's save dispatch (an explicit
    /// `RUSTAB_TARGET_PATH=/etc/fstab`), which deliberately keeps no
    /// last-known-content marker.
    ///
    /// Carries no record, so [`Self::detect`] reports nothing and that path
    /// behaves exactly as it did before the check existed.
    /// `allow_external_change` is `false` because nothing was confirmed -
    /// the check is skipped for want of an expectation, not for want of a
    /// warning.
    pub fn unchecked() -> Self {
        ExternalChangeGuard {
            last_known_real_content: None,
            allow_external_change: false,
        }
    }

    /// Compares what Rustab last knew against what `real_fstab` holds right
    /// now, returning both sides when they differ.
    ///
    /// This is the check as the **elevated helper** performs it
    /// ([`crate::protocol`]), which is always looking at the real
    /// filesystem (started by `pkexec` directly, or by `flatpak-spawn
    /// --host pkexec` outside any sandbox). So a plain `fs::read` is right
    /// here, and this is Rustab's one authoritative read - see
    /// [`Self::detect_read_with`] for the unprivileged side's courtesy
    /// check, which may have a sandbox boundary in the way.
    pub fn detect(&self, real_fstab: &Path) -> Option<ExternalChange> {
        self.detect_read_with(real_fstab, |path| fs::read(path))
    }

    /// [`Self::detect`] with the read of `real_fstab` injected, for the
    /// unprivileged GUI process, which can't read that file with
    /// `fs::read` inside a Flatpak sandbox.
    ///
    /// Only *where the bytes come from* differs; the verdict is computed
    /// identically for both callers, here, once. `None` - "no external
    /// change to report" - for each of:
    ///
    /// - the user already confirmed the overwrite
    ///   ([`Self::allow_external_change`]);
    /// - there is no usable record to compare against
    ///   ([`Self::last_known_real_content`]);
    /// - `real_fstab` cannot be read at all (missing, or no permission, or
    ///   broken host access). Nothing can be established in that case, and
    ///   a file that is *gone* is not an edit to warn about: the write
    ///   recreates it, the same first-ever-save state the draft refresh
    ///   already treats as "nothing to compare";
    /// - the bytes match.
    ///
    /// The comparison is byte-exact against the recorded bytes. Only the
    /// *reported* current content is lossy ([`String::from_utf8_lossy`]),
    /// since it exists to be rendered as a diff.
    pub fn detect_read_with<F>(&self, real_fstab: &Path, read: F) -> Option<ExternalChange>
    where
        F: FnOnce(&Path) -> io::Result<Vec<u8>>,
    {
        if self.allow_external_change {
            return None;
        }
        let last_known = self.last_known_real_content.as_deref()?;
        let current = read(real_fstab).ok()?;
        if current == last_known.as_bytes() {
            return None;
        }
        Some(ExternalChange {
            last_known: last_known.to_string(),
            current: String::from_utf8_lossy(&current).into_owned(),
        })
    }
}
