//! Domain model for fstab entries and documents - see `docs/SPEC.md` §5.
//!
//! This module is pure data modeling: it defines the types that represent a
//! parsed `/etc/fstab`-style file and small, pure accessor/mutator helpers on
//! them. It deliberately contains **no** text parsing or serialization logic
//! (that belongs to `parser.rs`, a later task) and **no** file I/O. The types
//! here are designed so a future parser can build them from text, and a
//! future serializer can turn them back into text, without needing to lose
//! or reinterpret any information along the way.
//!
//! ## Escaped-whitespace policy (read this before touching `mount_point` or
//! `SourceId::Path`)
//!
//! `/etc/fstab` requires literal whitespace inside a field (most commonly a
//! space in a mount point or device path) to be written as an octal escape
//! sequence, e.g. `\040` for a space or `\011` for a tab. This module stores
//! such text **exactly as it appears in the file, escape sequences and all** -
//! it never decodes `\040` into a real space character, and it never
//! re-encodes a literal space into `\040`. A raw string field in this module
//! is therefore always "the literal bytes between field separators", full
//! stop. Rationale: decoding would require the model to track "was this
//! field escaped or not" out-of-band to re-encode correctly on write, which
//! is exactly the kind of lossy round-trip SPEC §5 forbids. Keeping the raw
//! text is simple, unambiguous, and correct by construction.
//!
//! This crate still never *re-encodes* a literal space back into `\040` -
//! nothing in this crate writes decoded text back into a model field, so
//! that half of a decode/encode pair genuinely is not needed. Decoding
//! *is* needed in more than one place that treats a field as a real
//! filesystem path rather than model text - e.g. checking whether a mount
//! point exists on disk ([`crate::validate`]), or a frontend's "open this
//! path in the file manager" action - so this module exposes
//! [`decode_escaped_whitespace`] as that one narrow, one-way translation.
//! Every other consumer of `mount_point`/`SourceId::Path` keeps using the
//! raw text untouched.

use std::fmt;

/// Decodes `/etc/fstab`'s `\NNN` octal-escape convention (three octal
/// digits, e.g. `\040` for a space, `\011` for a tab) into the literal bytes
/// they represent. See this module's "Escaped-whitespace policy" docs above
/// for why the model itself never does this, and when callers should.
///
/// A backslash not followed by exactly three octal digits (`0`-`7`) is left
/// untouched, verbatim - fstab only ever uses this escape for whitespace and
/// the backslash itself (see util-linux's `mangle()`/`unmangle_to_buffer()`
/// in `lib/mangle.c`, the reference implementation this matches), so an
/// unrecognized sequence is far more likely to be a literal backslash in the
/// path than a malformed escape.
pub fn decode_escaped_whitespace(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        let is_octal_escape = bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..=i + 3]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b));
        if is_octal_escape {
            let value =
                (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(value as char);
            i += 4;
        } else {
            // Always lands on a UTF-8 char boundary: every byte skipped by
            // this branch was just checked and is not part of a multi-byte
            // sequence consumed above.
            let ch = raw[i..].chars().next().expect("i < bytes.len()");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod decode_escaped_whitespace_tests {
    use super::decode_escaped_whitespace as decode;

    #[test]
    fn decodes_a_space() {
        assert_eq!(decode("/mnt/My\\040Backup"), "/mnt/My Backup");
    }

    #[test]
    fn decodes_every_escape_fstab_itself_uses() {
        // space, tab, newline, backslash - the four fstab(5) documents.
        assert_eq!(decode("\\040"), " ");
        assert_eq!(decode("\\011"), "\t");
        assert_eq!(decode("\\012"), "\n");
        assert_eq!(decode("\\134"), "\\");
    }

    #[test]
    fn leaves_a_path_with_no_escapes_untouched() {
        assert_eq!(decode("/mnt/data"), "/mnt/data");
    }

    #[test]
    fn leaves_a_lone_backslash_untouched() {
        // Not followed by three octal digits, so not a recognized escape.
        assert_eq!(decode("/mnt/a\\b"), "/mnt/a\\b");
        assert_eq!(decode("/mnt/a\\99"), "/mnt/a\\99");
    }

    #[test]
    fn handles_consecutive_escapes() {
        assert_eq!(
            decode("Local\\040Extension\\040Settings"),
            "Local Extension Settings"
        );
    }
}

/// The source identifier of an fstab entry - the first whitespace-separated
/// field on a line (traditionally called "device" even though it is not
/// always a device).
///
/// Each variant stores the *original raw text* of the field it was built
/// from (including any octal-escaped whitespace, untouched - see the module
/// docs), so that re-serializing a [`SourceId`] produces byte-identical text
/// to what it was parsed from. The variant itself only records which
/// `fstab`-recognized *prefix* (`UUID=`, `LABEL=`, etc.) was present, if any;
/// it does not attempt to validate the value after the prefix (e.g. it does
/// not check that a `UUID=` value is a syntactically valid UUID) - that is
/// `validate.rs`'s job, not the model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceId {
    /// A raw filesystem path used as the source, e.g. `/dev/sda1` for a
    /// block device or an arbitrary directory path for a bind mount.
    /// Stores the path exactly as written, including any octal-escaped
    /// whitespace.
    Path(String),

    /// A `UUID=<value>` source. Stores only the `<value>` portion (without
    /// the `UUID=` prefix); the prefix is re-added by the serializer.
    Uuid(String),

    /// A `LABEL=<value>` source. Stores only the `<value>` portion (without
    /// the `LABEL=` prefix).
    Label(String),

    /// A `PARTUUID=<value>` source. Stores only the `<value>` portion
    /// (without the `PARTUUID=` prefix).
    PartUuid(String),

    /// A `PARTLABEL=<value>` source. Stores only the `<value>` portion
    /// (without the `PARTLABEL=` prefix).
    PartLabel(String),

    /// The literal `none` source, used for swap files declared without a
    /// backing device reference, bind mounts, and various pseudo/virtual
    /// filesystems (`tmpfs`, `proc`, etc.).
    None,

    /// A network filesystem source that doesn't fit the device/UUID/LABEL
    /// styles above, e.g. `//server/share` (cifs) or `server:/export` (nfs).
    /// Stored as the full raw text verbatim; this crate does not parse out
    /// the server/share components, since `fstab`/`mount.cifs`/`mount.nfs`
    /// accept a wide variety of address forms and guessing wrong would risk
    /// losing fidelity. A later task may add structured accessors on top of
    /// this raw text if a concrete UI need arises.
    Network(String),

    /// Any other source text that does not match a recognized prefix and is
    /// not a plain path (as best-effort distinguished by the parser) -
    /// a catch-all that preserves the raw text verbatim so no input is ever
    /// rejected or silently dropped at the model layer.
    Other(String),
}

impl SourceId {
    /// Returns the raw value stored in this source, **without** re-adding
    /// any `KEY=` prefix (e.g. for [`SourceId::Uuid`] this returns the UUID
    /// text alone, not `UUID=<text>`). Useful for callers that already know
    /// the variant and just want the payload. To reconstruct the exact
    /// original field text (prefix included), use a future serializer
    /// (`parser.rs`), not this method.
    pub fn raw_value(&self) -> &str {
        match self {
            SourceId::Path(s)
            | SourceId::Uuid(s)
            | SourceId::Label(s)
            | SourceId::PartUuid(s)
            | SourceId::PartLabel(s)
            | SourceId::Network(s)
            | SourceId::Other(s) => s,
            SourceId::None => "none",
        }
    }
}

/// A single fstab option: either a bare flag (`noauto`) or a `key=value`
/// pair (`x-systemd.idle-timeout=30`).
///
/// This is intentionally not a closed enum of "known" options (per
/// `docs/SPEC.md` §5) - recognizing well-known options and offering toggles
/// for them is a frontend concern for a later task. This type only
/// distinguishes the two syntactic shapes fstab options can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FstabOption {
    /// A bare flag with no `=value` part, e.g. `noatime`, `ro`, `bind`.
    Flag(String),
    /// A `key=value` pair, e.g. `fmask=0022` or `x-systemd.device-timeout=10`.
    /// The key and value are stored separately (both raw text, unescaped
    /// beyond what the comma-separated option list itself required) so
    /// callers can query/mutate by key without re-splitting on `=`.
    KeyValue(String, String),
}

impl FstabOption {
    /// Returns this option's key: the flag name for [`FstabOption::Flag`],
    /// or the part before `=` for [`FstabOption::KeyValue`].
    pub fn key(&self) -> &str {
        match self {
            FstabOption::Flag(k) => k,
            FstabOption::KeyValue(k, _) => k,
        }
    }

    /// Returns this option's value, if it has one. Always `None` for
    /// [`FstabOption::Flag`]; always `Some` (possibly an empty string, for a
    /// literal `key=` with nothing after the `=`) for
    /// [`FstabOption::KeyValue`].
    pub fn value(&self) -> Option<&str> {
        match self {
            FstabOption::Flag(_) => None,
            FstabOption::KeyValue(_, v) => Some(v),
        }
    }
}

impl fmt::Display for FstabOption {
    /// Renders the option back to its fstab text form (`key` or
    /// `key=value`). This is a small, pure text-formatting helper (not a
    /// parser), consistent with the rest of this module exposing no
    /// text-to-model parsing logic.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FstabOption::Flag(k) => write!(f, "{k}"),
            FstabOption::KeyValue(k, v) => write!(f, "{k}={v}"),
        }
    }
}

/// An ordered list of fstab options (the fourth, comma-separated field of an
/// entry).
///
/// Order and exact spelling of each option are preserved as given -
/// `Options` never sorts, deduplicates, or normalizes casing on its own.
/// Mutation methods (`set`/`unset`) describe exactly what they do to order
/// when an option already exists; see their docs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options(Vec<FstabOption>);

impl Options {
    /// Creates an empty option list (equivalent to an entry whose options
    /// field is empty, which the serializer may choose to render as
    /// `defaults` or leave empty - that choice belongs to `parser.rs`, not
    /// this type).
    pub fn new() -> Self {
        Options(Vec::new())
    }

    /// Builds an `Options` list from an already-split sequence of options,
    /// preserving the given order exactly.
    pub fn from_vec(options: Vec<FstabOption>) -> Self {
        Options(options)
    }

    /// Returns the options in their current order as a slice.
    pub fn as_slice(&self) -> &[FstabOption] {
        &self.0
    }

    /// Returns `true` if no options are present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of options in the list.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if an option with this exact key exists, regardless
    /// of whether it is a bare flag or a `key=value` pair. Key comparison is
    /// case-sensitive and exact, matching fstab's own lack of case-folding
    /// for option names.
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|opt| opt.key() == key)
    }

    /// Returns the value of the first `key=value` option with this key, or
    /// `None` if no option with this key exists *or* it exists only as a
    /// bare flag. Use [`Options::contains`] first if you need to
    /// distinguish "absent" from "present but flag-only".
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|opt| opt.key() == key)?.value()
    }

    /// Inserts or replaces a `key=value` option.
    ///
    /// If an option with this key already exists, its value is replaced
    /// **in place** (preserving its original position in the list) and it
    /// is converted to a `key=value` option if it was previously a bare
    /// flag. If no option with this key exists, a new `key=value` option is
    /// appended at the end.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        if let Some(opt) = self.0.iter_mut().find(|opt| opt.key() == key) {
            *opt = FstabOption::KeyValue(key.to_string(), value.into());
        } else {
            self.0
                .push(FstabOption::KeyValue(key.to_string(), value.into()));
        }
    }

    /// Inserts a bare flag option (e.g. `noauto`) if not already present by
    /// key. If an option with this key already exists (flag or
    /// `key=value`), this is a no-op - it does not downgrade an existing
    /// `key=value` option to a flag, since that would silently discard its
    /// value.
    pub fn set_flag(&mut self, key: impl Into<String>) {
        let key = key.into();
        if !self.contains(&key) {
            self.0.push(FstabOption::Flag(key));
        }
    }

    /// Removes any option with this key (flag or `key=value`), returning
    /// `true` if one was present and removed, `false` if the key was not
    /// found. Preserves the relative order of the remaining options.
    pub fn unset(&mut self, key: &str) -> bool {
        let len_before = self.0.len();
        self.0.retain(|opt| opt.key() != key);
        self.0.len() != len_before
    }
}

impl fmt::Display for Options {
    /// Renders the options back to fstab's comma-separated text form.
    /// This is a small, pure text-formatting helper (not a parser).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, opt) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ",")?;
            }
            write!(f, "{opt}")?;
        }
        Ok(())
    }
}

/// One logical, successfully-parsed fstab entry - i.e. a non-comment,
/// non-blank line with all six whitespace-separated fields present and
/// structurally well-formed enough to split.
///
/// `mount_point` and `fs_type` are kept as free-form raw text (no closed
/// enum) per `docs/SPEC.md` §5: fstab accepts arbitrary filesystem type
/// strings, and rejecting uncommon-but-valid ones would make this crate
/// less correct than hand-editing the file. `mount_point` follows the same
/// escaped-whitespace policy as [`SourceId::Path`] - see the module-level
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FstabEntry {
    /// The entry's source identifier (first field).
    pub source: SourceId,
    /// The mount point (second field), raw text - may contain
    /// octal-escaped whitespace, stored undecoded (see module docs).
    pub mount_point: String,
    /// The filesystem type (third field), free text. No closed enum: any
    /// string the file contains is preserved as-is.
    pub fs_type: String,
    /// The options list (fourth field).
    pub options: Options,
    /// The dump field (fifth field) - historically used by the `dump(8)`
    /// backup utility. Conventionally `0` or `1`; stored as a plain integer
    /// since fstab itself imposes no tighter range.
    pub dump: u32,
    /// The pass field (sixth field) - fsck ordering. Conventionally `0`
    /// (skip), `1` (check first, root filesystem), or `2` (check after
    /// pass 1); stored as a plain integer since fstab itself imposes no
    /// tighter range.
    pub pass: u32,
    /// An optional trailing inline comment: the raw text appearing after
    /// the six fields on the same physical line, following a `#`, *not*
    /// including the `#` itself. `None` if the line has no trailing
    /// comment.
    pub trailing_comment: Option<String>,
    /// An optional human-readable note about this entry - what the mount
    /// point is *for*, in the user's own words (e.g. "photo archive, only
    /// mounted when backing up").
    ///
    /// This is deliberately **not** the same mechanism as
    /// [`Self::trailing_comment`], and the two are stored separately. A
    /// trailing comment lives on the *same physical line*, after the six
    /// fields. A description instead round-trips to and from the **ordinary
    /// `#` comment line(s) sitting directly above** this entry - no marker of
    /// any kind. Adjacency alone decides it: a comment line's own text has no
    /// bearing on whether it is this entry's description, so e.g. a
    /// hand-commented-out CIFS mount (`#//server/share ...`, see
    /// [`SourceId::Network`]) directly above an entry is that entry's
    /// description like any other comment. The exact rule - how far up the
    /// walk goes, which blank lines it crosses, and how the value is written
    /// back out - is documented in `parser.rs`'s "Description block
    /// absorption" and "Serialization contract" sections; it is not restated
    /// here.
    ///
    /// The value **may contain embedded `\n`**: a description is a
    /// genuinely multi-line note, stored with one `\n` per comment line it
    /// came from (no trailing newline, and no `\r` - the parser never
    /// produces one, and the IPC boundary folds any it is handed). It is
    /// written back out as one comment line per `\n`-separated piece.
    ///
    /// `None` means the entry has no description. An empty or
    /// whitespace-only value is never stored here - it is normalized to
    /// `None` by whoever constructs the entry. This field performs no
    /// normalization itself, consistent with the rest of this module doing
    /// no validation.
    pub description: Option<String>,
}

impl FstabEntry {
    /// Constructs a new entry with no trailing comment and no description.
    /// All fields are taken verbatim; no validation, normalization, or
    /// escaping is performed (that is `validate.rs`'s and `parser.rs`'s job
    /// respectively).
    pub fn new(
        source: SourceId,
        mount_point: impl Into<String>,
        fs_type: impl Into<String>,
        options: Options,
        dump: u32,
        pass: u32,
    ) -> Self {
        FstabEntry {
            source,
            mount_point: mount_point.into(),
            fs_type: fs_type.into(),
            options,
            dump,
            pass,
            trailing_comment: None,
            description: None,
        }
    }

    /// Returns a copy of this entry with `comment` attached as its trailing
    /// inline comment. Builder-style helper for use alongside [`Self::new`].
    #[must_use]
    pub fn with_trailing_comment(mut self, comment: impl Into<String>) -> Self {
        self.trailing_comment = Some(comment.into());
        self
    }

    /// Returns a copy of this entry with `description` attached as its
    /// human-readable description. Builder-style helper for use alongside
    /// [`Self::new`]; see [`Self::description`] for what the value means and
    /// how it is round-tripped to text.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// One line of a parsed fstab document.
///
/// A `Document` is an ordered sequence of `Line`s rather than just a `Vec`
/// of entries, specifically so that comment lines, blank lines, and
/// malformed lines retain their original position relative to real entries -
/// required for byte-faithful round-tripping per `docs/SPEC.md` §5/§6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A successfully parsed fstab entry.
    Entry(FstabEntry),
    /// A full-line comment: the raw text that followed the line's leading
    /// `#`, not including the `#` itself. Distinct from
    /// [`FstabEntry::trailing_comment`], which is an inline comment
    /// attached to an entry line rather than a comment occupying its own
    /// line. Also distinct from [`FstabEntry::description`]: a comment line
    /// that belongs to the comment block attached to an entry is absorbed
    /// into that entry's `description` by the parser and therefore never
    /// appears as a `Comment` line. What is left as a `Comment` is every
    /// comment line no entry claimed - one above a [`Line::Malformed`] line,
    /// or one with no entry below it at all (see `parser.rs` for the exact
    /// rule).
    Comment(String),
    /// A blank (whitespace-only or empty) line, preserved so that visual
    /// spacing between sections of the original file survives a round
    /// trip.
    Blank,
    /// A line that could not be parsed as a valid entry, comment, or blank
    /// line (e.g. a source-only line with missing fields, or some other
    /// structurally incomplete content) but must still be preserved
    /// verbatim rather than dropped. Stores the entire original raw line
    /// text. Per `docs/SPEC.md` §5, callers should surface this to the user
    /// as a visible warning rather than silently skipping or rejecting the
    /// whole file.
    Malformed(String),
}

/// A whole parsed fstab file: an ordered sequence of [`Line`]s.
///
/// Reconstructing a file from a `Document` (serialization, a later task) is
/// only guaranteed to be byte-faithful if every line of the original file
/// was captured as exactly one `Line` in `lines`, in original order - this
/// type carries no separate "header comment block" or "entries" field
/// precisely so that invariant is trivially true by construction rather
/// than something a parser must carefully keep in sync across two
/// collections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    /// The lines of the document, in original file order.
    pub lines: Vec<Line>,
}

impl Document {
    /// Creates an empty document (no lines).
    pub fn new() -> Self {
        Document { lines: Vec::new() }
    }

    /// Creates a document from an already-ordered sequence of lines, taken
    /// verbatim.
    pub fn from_lines(lines: Vec<Line>) -> Self {
        Document { lines }
    }

    /// Returns an iterator over just the successfully-parsed entries, in
    /// document order, skipping comments, blanks, and malformed lines. This
    /// is a read-only convenience view - it does not let a caller mutate
    /// entries in place while skipping the surrounding lines; for mutation,
    /// index into `lines` directly (e.g. via `iter_mut` and matching on
    /// `Line::Entry`).
    pub fn entries(&self) -> impl Iterator<Item = &FstabEntry> {
        self.lines.iter().filter_map(|line| match line {
            Line::Entry(entry) => Some(entry),
            _ => None,
        })
    }

    /// Returns `true` if any line in this document is [`Line::Malformed`].
    /// Callers (e.g. the GUI) should use this to decide whether to show a
    /// "some lines could not be understood" warning.
    pub fn has_malformed_lines(&self) -> bool {
        self.lines
            .iter()
            .any(|line| matches!(line, Line::Malformed(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_raw_value_strips_known_prefixes_conceptually() {
        // raw_value() returns the payload only, never the KEY= prefix.
        assert_eq!(SourceId::Path("/dev/sda1".into()).raw_value(), "/dev/sda1");
        assert_eq!(SourceId::Uuid("1234-5678".into()).raw_value(), "1234-5678");
        assert_eq!(SourceId::Label("home".into()).raw_value(), "home");
        assert_eq!(
            SourceId::PartUuid("abcd-ef01".into()).raw_value(),
            "abcd-ef01"
        );
        assert_eq!(SourceId::PartLabel("boot".into()).raw_value(), "boot");
        assert_eq!(SourceId::None.raw_value(), "none");
        assert_eq!(
            SourceId::Network("//server/share".into()).raw_value(),
            "//server/share"
        );
        assert_eq!(
            SourceId::Other("weird-thing".into()).raw_value(),
            "weird-thing"
        );
    }

    #[test]
    fn source_id_equality_and_clone() {
        let a = SourceId::Uuid("abc-123".to_string());
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, SourceId::Label("abc-123".to_string()));
    }

    #[test]
    fn fstab_option_key_and_value() {
        let flag = FstabOption::Flag("noatime".to_string());
        assert_eq!(flag.key(), "noatime");
        assert_eq!(flag.value(), None);

        let kv = FstabOption::KeyValue("fmask".to_string(), "0022".to_string());
        assert_eq!(kv.key(), "fmask");
        assert_eq!(kv.value(), Some("0022"));
    }

    #[test]
    fn fstab_option_display() {
        assert_eq!(
            FstabOption::Flag("noauto".to_string()).to_string(),
            "noauto"
        );
        assert_eq!(
            FstabOption::KeyValue("x-systemd.idle-timeout".to_string(), "30".to_string())
                .to_string(),
            "x-systemd.idle-timeout=30"
        );
    }

    #[test]
    fn options_contains_get_generic_keys() {
        let mut opts = Options::new();
        opts.set_flag("defaults");
        opts.set("x-systemd.idle-timeout", "30");
        opts.set("fmask", "0022");

        assert!(opts.contains("defaults"));
        assert!(opts.contains("x-systemd.idle-timeout"));
        assert!(opts.contains("fmask"));
        assert!(!opts.contains("dmask"));

        assert_eq!(opts.get("defaults"), None); // flag-only, no value
        assert_eq!(opts.get("x-systemd.idle-timeout"), Some("30"));
        assert_eq!(opts.get("fmask"), Some("0022"));
        assert_eq!(opts.get("dmask"), None);
    }

    #[test]
    fn options_set_replaces_in_place_preserving_order() {
        let mut opts = Options::from_vec(vec![
            FstabOption::Flag("noauto".to_string()),
            FstabOption::KeyValue("fmask".to_string(), "0022".to_string()),
            FstabOption::Flag("nofail".to_string()),
        ]);

        opts.set("fmask", "0077");

        assert_eq!(
            opts.as_slice(),
            &[
                FstabOption::Flag("noauto".to_string()),
                FstabOption::KeyValue("fmask".to_string(), "0077".to_string()),
                FstabOption::Flag("nofail".to_string()),
            ]
        );
    }

    #[test]
    fn options_set_appends_when_absent() {
        let mut opts = Options::new();
        opts.set_flag("noauto");
        opts.set("dmask", "0022");

        assert_eq!(opts.len(), 2);
        assert_eq!(
            opts.as_slice()[1],
            FstabOption::KeyValue("dmask".to_string(), "0022".to_string())
        );
    }

    #[test]
    fn options_set_flag_is_noop_if_key_already_present_as_keyvalue() {
        let mut opts = Options::new();
        opts.set("fmask", "0022");
        opts.set_flag("fmask");

        // Must not downgrade the existing key=value option.
        assert_eq!(opts.len(), 1);
        assert_eq!(opts.get("fmask"), Some("0022"));
    }

    #[test]
    fn options_unset_removes_and_preserves_order() {
        let mut opts = Options::from_vec(vec![
            FstabOption::Flag("a".to_string()),
            FstabOption::Flag("b".to_string()),
            FstabOption::Flag("c".to_string()),
        ]);

        assert!(opts.unset("b"));
        assert_eq!(
            opts.as_slice(),
            &[
                FstabOption::Flag("a".to_string()),
                FstabOption::Flag("c".to_string()),
            ]
        );
        assert!(!opts.unset("b")); // already gone
    }

    #[test]
    fn options_display_renders_comma_separated() {
        let opts = Options::from_vec(vec![
            FstabOption::Flag("defaults".to_string()),
            FstabOption::KeyValue("x-systemd.automount".to_string(), String::new()),
        ]);
        // A key=value option with an empty value still renders the `=`.
        assert_eq!(opts.to_string(), "defaults,x-systemd.automount=");
    }

    #[test]
    fn options_is_empty_and_len() {
        let opts = Options::new();
        assert!(opts.is_empty());
        assert_eq!(opts.len(), 0);
    }

    #[test]
    fn fstab_entry_construction_and_equality() {
        let mut opts = Options::new();
        opts.set_flag("defaults");
        opts.set_flag("noatime");

        let entry = FstabEntry::new(
            SourceId::Uuid("1111-2222".to_string()),
            "/mnt/data",
            "ext4",
            opts.clone(),
            0,
            2,
        );

        let same = FstabEntry::new(
            SourceId::Uuid("1111-2222".to_string()),
            "/mnt/data",
            "ext4",
            opts,
            0,
            2,
        );

        assert_eq!(entry, same);
        assert_eq!(entry.trailing_comment, None);
        assert_eq!(entry.description, None);
    }

    #[test]
    fn fstab_entry_with_trailing_comment_builder() {
        let entry = FstabEntry::new(SourceId::None, "none", "swap", Options::new(), 0, 0)
            .with_trailing_comment("encrypted swap, see notes");

        assert_eq!(
            entry.trailing_comment,
            Some("encrypted swap, see notes".to_string())
        );
    }

    #[test]
    fn fstab_entry_with_description_builder() {
        let entry = FstabEntry::new(SourceId::None, "none", "swap", Options::new(), 0, 0)
            .with_description("swap space on the second disk");

        assert_eq!(
            entry.description,
            Some("swap space on the second disk".to_string())
        );
        // The two comment mechanisms are independent: setting one must not
        // populate the other.
        assert_eq!(entry.trailing_comment, None);
    }

    #[test]
    fn mount_point_preserves_escaped_whitespace_raw() {
        // Per module docs: \040 is NOT decoded into a real space.
        let entry = FstabEntry::new(
            SourceId::Path("/dev/sda2".to_string()),
            "/mnt/My\\040Backup",
            "ext4",
            Options::new(),
            0,
            2,
        );
        assert_eq!(entry.mount_point, "/mnt/My\\040Backup");
        assert!(!entry.mount_point.contains(' '));
    }

    #[test]
    fn document_entries_iterator_skips_non_entry_lines() {
        let doc = Document::from_lines(vec![
            Line::Comment(" leading header comment".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Label("root".to_string()),
                "/",
                "ext4",
                Options::new(),
                0,
                1,
            )),
            Line::Malformed("/dev/sdb1".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::None,
                "none",
                "swap",
                Options::new(),
                0,
                0,
            )),
        ]);

        let entries: Vec<&FstabEntry> = doc.entries().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mount_point, "/");
        assert_eq!(entries[1].fs_type, "swap");
    }

    #[test]
    fn document_has_malformed_lines_detects_presence() {
        let clean = Document::from_lines(vec![Line::Blank, Line::Comment("ok".to_string())]);
        assert!(!clean.has_malformed_lines());

        let dirty = Document::from_lines(vec![Line::Malformed("/dev/sdb1".to_string())]);
        assert!(dirty.has_malformed_lines());
    }

    #[test]
    fn document_new_is_empty() {
        let doc = Document::new();
        assert!(doc.lines.is_empty());
        assert_eq!(doc.entries().count(), 0);
    }

    #[test]
    fn document_preserves_line_order_for_round_trip_shape() {
        // The core guarantee: lines come back out in the exact order given,
        // which is what lets a future serializer reconstruct the file.
        let lines = vec![
            Line::Comment("header".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda1".to_string()),
                "/boot",
                "vfat",
                Options::new(),
                0,
                2,
            )),
            Line::Comment("trailing note".to_string()),
        ];
        let doc = Document::from_lines(lines.clone());
        assert_eq!(doc.lines, lines);
    }
}
