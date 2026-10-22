//! Domain model for fstab entries and documents - see `docs/SPEC.md` §5.
//!
//! Pure data modeling: the types representing a parsed `/etc/fstab`-style
//! file, plus small pure accessor/mutator helpers. No text parsing (see
//! `parser.rs`), no serialization (see `writer.rs`), no file I/O.
//!
//! ## Escaped-whitespace policy
//!
//! `/etc/fstab` escapes literal whitespace in a field as octal (`\040` for a
//! space, `\011` for a tab). `mount_point` and `SourceId::Path` store that
//! text **raw, escapes and all** - never decoded, never re-encoded. Decoding
//! up front would require tracking "was this escaped" out-of-band to
//! re-encode on write, a lossy round-trip SPEC §5 forbids.
//!
//! Callers that need the real filesystem path (e.g. [`crate::validate`]
//! checking whether a mount point exists) use [`decode_escaped_whitespace`]
//! for that one-way translation; nothing in this crate re-encodes the other
//! direction.

use std::fmt;

/// Decodes `/etc/fstab`'s `\NNN` octal-escape sequences (e.g. `\040` for a
/// space) into literal bytes. See the module's "Escaped-whitespace policy"
/// docs for why the model itself never does this.
///
/// A backslash not followed by three octal digits is left untouched -
/// matches util-linux's `unmangle_to_buffer()` (`lib/mangle.c`).
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
            // Always on a UTF-8 boundary: bytes consumed above are only octal escapes.
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

/// The source identifier of an fstab entry - the first field on a line
/// (traditionally called "device" even though it isn't always one).
///
/// Each variant stores the raw field text as parsed (escapes untouched), so
/// re-serializing is byte-identical to the input. The prefix (`UUID=` etc.)
/// is only recorded, never validated - that's `validate.rs`'s job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceId {
    /// A raw filesystem path, e.g. `/dev/sda1` or a bind-mount directory.
    Path(String),

    /// A `UUID=<value>` source; stores only `<value>`.
    Uuid(String),

    /// A `LABEL=<value>` source; stores only `<value>`.
    Label(String),

    /// A `PARTUUID=<value>` source; stores only `<value>`.
    PartUuid(String),

    /// A `PARTLABEL=<value>` source; stores only `<value>`.
    PartLabel(String),

    /// The literal `none` source (swap files, bind mounts, pseudo-filesystems).
    None,

    /// A network source not matching the styles above, e.g. `//server/share`
    /// (cifs) or `server:/export` (nfs). Kept as raw text verbatim since this
    /// crate doesn't parse server/share components.
    Network(String),

    /// Catch-all for source text matching no recognized prefix, so no input
    /// is ever rejected at the model layer.
    Other(String),
}

impl SourceId {
    /// Returns the value stored in this source, without any `KEY=` prefix.
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

/// A single fstab option: a bare flag (`noauto`) or a `key=value` pair
/// (`x-systemd.idle-timeout=30`).
///
/// Not a closed enum of "known" options (per `docs/SPEC.md` §5) - that's a
/// frontend concern. This type only distinguishes the two syntactic shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FstabOption {
    /// A bare flag, e.g. `noatime`, `ro`, `bind`.
    Flag(String),
    /// A `key=value` pair, e.g. `fmask=0022`, stored as separate key/value
    /// so callers can query/mutate by key without re-splitting on `=`.
    KeyValue(String, String),
}

impl FstabOption {
    /// This option's key: the flag name, or the part before `=`.
    pub fn key(&self) -> &str {
        match self {
            FstabOption::Flag(k) => k,
            FstabOption::KeyValue(k, _) => k,
        }
    }

    /// This option's value, if any. Always `None` for a flag; always
    /// `Some` (possibly empty) for a `key=value` pair.
    pub fn value(&self) -> Option<&str> {
        match self {
            FstabOption::Flag(_) => None,
            FstabOption::KeyValue(_, v) => Some(v),
        }
    }
}

impl fmt::Display for FstabOption {
    /// Renders back to fstab text form (`key` or `key=value`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FstabOption::Flag(k) => write!(f, "{k}"),
            FstabOption::KeyValue(k, v) => write!(f, "{k}={v}"),
        }
    }
}

/// An ordered list of fstab options (the fourth, comma-separated field).
///
/// Order and spelling are preserved as given - never sorted, deduplicated,
/// or case-normalized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options(Vec<FstabOption>);

impl Options {
    /// Creates an empty option list.
    pub fn new() -> Self {
        Options(Vec::new())
    }

    /// Builds an `Options` list from an already-split sequence, preserving order.
    pub fn from_vec(options: Vec<FstabOption>) -> Self {
        Options(options)
    }

    /// Returns the options in order as a slice.
    pub fn as_slice(&self) -> &[FstabOption] {
        &self.0
    }

    /// Returns `true` if no options are present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of options.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if an option with this exact key exists (flag or
    /// `key=value`). Case-sensitive, matching fstab itself.
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|opt| opt.key() == key)
    }

    /// Returns the value of the `key=value` option with this key, or `None`
    /// if absent or present only as a bare flag.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|opt| opt.key() == key)?.value()
    }

    /// Inserts or replaces a `key=value` option, in place if the key already
    /// exists (converting a bare flag to `key=value`), else appended.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        if let Some(opt) = self.0.iter_mut().find(|opt| opt.key() == key) {
            *opt = FstabOption::KeyValue(key.to_string(), value.into());
        } else {
            self.0
                .push(FstabOption::KeyValue(key.to_string(), value.into()));
        }
    }

    /// Inserts a bare flag if not already present by key. No-op if the key
    /// already exists, even as `key=value` - never downgrades a value away.
    pub fn set_flag(&mut self, key: impl Into<String>) {
        let key = key.into();
        if !self.contains(&key) {
            self.0.push(FstabOption::Flag(key));
        }
    }

    /// Removes any option with this key, returning whether one was found.
    /// Preserves order of the remaining options.
    pub fn unset(&mut self, key: &str) -> bool {
        let len_before = self.0.len();
        self.0.retain(|opt| opt.key() != key);
        self.0.len() != len_before
    }
}

impl fmt::Display for Options {
    /// Renders back to fstab's comma-separated text form.
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

/// One logical, successfully-parsed fstab entry - a non-comment, non-blank
/// line with all six fields present and well-formed enough to split.
///
/// `mount_point` and `fs_type` are free-form raw text (no closed enum) per
/// `docs/SPEC.md` §5, since fstab accepts arbitrary filesystem type strings.
/// `mount_point` follows the same escaped-whitespace policy as
/// [`SourceId::Path`] - see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FstabEntry {
    /// The source identifier (first field).
    pub source: SourceId,
    /// The mount point (second field), raw text - may contain undecoded
    /// octal-escaped whitespace (see module docs).
    pub mount_point: String,
    /// The filesystem type (third field), free text.
    pub fs_type: String,
    /// The options list (fourth field).
    pub options: Options,
    /// The dump field (fifth field, `dump(8)` backup flag). Conventionally
    /// `0` or `1`; stored as a plain integer since fstab imposes no range.
    pub dump: u32,
    /// The pass field (sixth field, fsck ordering). Conventionally `0`
    /// (skip), `1` (root), or `2` (checked after pass 1); stored as a plain
    /// integer since fstab imposes no range.
    pub pass: u32,
    /// An inline comment on the same line as the entry, after the six
    /// fields, following `#` (not including it). `None` if absent.
    pub trailing_comment: Option<String>,
    /// A human-readable note about what this mount point is for.
    ///
    /// Distinct from [`Self::trailing_comment`]: a description round-trips
    /// to/from the ordinary `#` comment line(s) directly above the entry
    /// (adjacency alone decides it - no marker), while a trailing comment
    /// lives on the entry's own line. See `parser.rs`'s "Description block
    /// absorption" and "Serialization contract" for the exact rule.
    ///
    /// May contain embedded `\n` (one per source comment line, no trailing
    /// newline, no `\r`). `None` means no description; empty/whitespace-only
    /// values are normalized to `None` by the constructor, not this field.
    pub description: Option<String>,
}

impl FstabEntry {
    /// Constructs a new entry with no trailing comment or description.
    /// Fields are taken verbatim - no validation or escaping (see
    /// `validate.rs` and `parser.rs`).
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

    /// Builder helper: attaches a trailing inline comment.
    #[must_use]
    pub fn with_trailing_comment(mut self, comment: impl Into<String>) -> Self {
        self.trailing_comment = Some(comment.into());
        self
    }

    /// Builder helper: attaches a description (see [`Self::description`]).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// One line of a parsed fstab document.
///
/// `Document` stores an ordered sequence of `Line`s rather than a `Vec` of
/// entries so comments, blanks, and malformed lines keep their original
/// position - required for byte-faithful round-tripping per `docs/SPEC.md` §5/§6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A successfully parsed fstab entry.
    Entry(FstabEntry),
    /// A full-line comment, text after the leading `#` (not including it).
    /// Distinct from [`FstabEntry::trailing_comment`] (inline, on an entry's
    /// own line) and [`FstabEntry::description`] (a comment block the
    /// parser absorbed into an entry). What remains `Comment` is any comment
    /// line no entry claimed - see `parser.rs` for the exact rule.
    Comment(String),
    /// A blank line, preserved so spacing survives a round trip.
    Blank,
    /// A line that couldn't be parsed as an entry, comment, or blank line,
    /// preserved verbatim rather than dropped. Per `docs/SPEC.md` §5,
    /// callers should surface this as a visible warning, not skip it.
    Malformed(String),
}

/// A whole parsed fstab file: an ordered sequence of [`Line`]s.
///
/// No separate "header" or "entries" field - everything lives in `lines`,
/// in original order, so byte-faithful reconstruction is guaranteed by
/// construction rather than two collections that could drift apart.
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

    /// Creates a document from an already-ordered sequence of lines.
    pub fn from_lines(lines: Vec<Line>) -> Self {
        Document { lines }
    }

    /// Iterator over just the successfully-parsed entries, in document
    /// order. Read-only; for mutation, index into `lines` directly.
    pub fn entries(&self) -> impl Iterator<Item = &FstabEntry> {
        self.lines.iter().filter_map(|line| match line {
            Line::Entry(entry) => Some(entry),
            _ => None,
        })
    }

    /// Returns `true` if any line is [`Line::Malformed`] - callers (e.g.
    /// the GUI) use this to decide whether to show a warning.
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
