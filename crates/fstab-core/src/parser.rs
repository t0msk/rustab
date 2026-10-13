//! Text-to-`Document` parser and round-trip serializer — see `docs/SPEC.md` §5.
//!
//! This module owns the only two operations that convert between fstab text
//! and the [`crate::model`] types: [`parse`] (text → [`Document`]) and the
//! [`fmt::Display`] implementation on [`Document`] (`Document` → text).
//!
//! ## Parsing contract
//!
//! [`parse`] **never panics and never fails**: it always returns a
//! [`Document`], even for completely garbage input. Any line that cannot be
//! understood as a valid entry, comment, or blank line becomes a
//! [`Line::Malformed`] carrying the original line text verbatim, per
//! `docs/SPEC.md` §5 ("surface malformed lines ... rather than crashing or
//! silently corrupting the rest of the file"). A fallible `Result`-returning
//! signature was deliberately rejected: there is no input for which this
//! parser needs to report a hard error rather than a degraded-but-preserved
//! line, and a `Result<Document, _>` would invite callers to `?`-propagate
//! away a document that should instead be shown to the user with malformed
//! lines flagged (see [`Document::has_malformed_lines`]).
//!
//! Per the escaped-whitespace policy documented in [`crate::model`], this
//! parser never decodes `\040`/`\011`-style octal escapes — raw field text is
//! captured exactly as written. Field splitting itself is whitespace-based
//! (consecutive runs of spaces/tabs are delimiters); this is safe with
//! respect to escape sequences without any special-casing because an escape
//! sequence's raw text (`\`, then octal digits) never itself contains a
//! literal whitespace byte — only the *decoded* character would, and we
//! never decode. So naive `str::split_whitespace` cannot split in the middle
//! of a `\NNN` sequence.
//!
//! ## Serialization contract
//!
//! [`fmt::Display`] for [`Document`] renders each line back to text, one
//! line per [`Line`], each terminated with `\n` (including the last line):
//!
//! - [`Line::Blank`] → an empty line.
//! - [`Line::Comment(text)`](Line::Comment) → `#` followed by `text` verbatim.
//! - [`Line::Malformed(raw)`](Line::Malformed) → `raw` verbatim, unchanged.
//! - [`Line::Entry`] → the six fields joined by a single ASCII space, in
//!   field order (source, mount point, fs type, options, dump, pass),
//!   followed by ` #` and the trailing comment text if present. The source
//!   field is reconstructed with its `KEY=` prefix re-added (e.g.
//!   [`SourceId::Uuid`] serializes as `UUID=<value>`); the options field
//!   reuses [`Options`]'s own `Display` impl (comma-joined).
//!
//! This is a deliberately **canonical** rendering: it normalizes whatever
//! original inter-field whitespace a line had down to single spaces. Per the
//! module-level task brief, byte-for-byte round-tripping
//! (`parse(text).to_string() == text`) is therefore only guaranteed for text
//! that was already in this canonical form (single-space-separated fields,
//! no leading whitespace before comments, entries with all six fields
//! present, a trailing newline at end of file) — which is exactly the shape
//! this crate's own fixtures are written in. Non-canonical input (e.g. real
//! hand-edited fstabs with tab-aligned columns) still parses correctly and
//! still reaches a *stable fixed point*
//! (`parse(serialize(parse(text))) == parse(text)`, and
//! `serialize(parse(serialize(parse(text)))) == serialize(parse(text))`),
//! it just won't be byte-identical to the original on the very first pass.

use crate::model::{Document, FstabEntry, FstabOption, Line, Options, SourceId};
use std::fmt;

/// Parses fstab-style text into a [`Document`].
///
/// Always succeeds and never panics — see the module-level "Parsing
/// contract" docs. Each input line (split on `\n`, with any trailing `\r`
/// from CRLF input tolerated by [`str::lines`]) becomes exactly one
/// [`Line`] in the returned document, in original order, so the number of
/// lines in is always the number of lines out.
///
/// Classification per line:
/// - Whitespace-only (or empty) → [`Line::Blank`].
/// - Starts with `#` after optional leading whitespace → [`Line::Comment`],
///   storing everything after that `#` verbatim.
/// - Otherwise, an attempt is made to parse it as an [`FstabEntry`] (see
///   below); on failure, the entire original line text is preserved as
///   [`Line::Malformed`].
///
/// Entry parsing: the line is first split at the first `#` that begins a
/// new whitespace-delimited token (i.e. is preceded by whitespace or is the
/// first character) — everything from there to end-of-line becomes
/// [`FstabEntry::trailing_comment`] (minus the `#` itself), and everything
/// before it is split on whitespace into fields. At least four fields
/// (source, mount point, fs type, options) are required; fewer than that
/// makes the whole line [`Line::Malformed`] (this is what covers the
/// `docs/SPEC.md` §5 "bare source with nothing else" case). The optional
/// trailing `dump`/`pass` fields are handled as follows:
/// - Both absent (exactly 4 fields before any comment) → both default to
///   `0`, per fstab convention for files that omit them entirely.
/// - Only `dump` present (5 fields) → `dump` is parsed from field 5, `pass`
///   defaults to `0`.
/// - Both present (6 fields) → both are parsed from fields 5 and 6.
/// - More than 6 fields, or a present dump/pass field that fails to parse
///   as `u32` → the whole line is [`Line::Malformed`] rather than silently
///   coercing or dropping data, per the explicit instruction to never
///   silently coerce an invalid integer field.
pub fn parse(text: &str) -> Document {
    Document::from_lines(text.lines().map(parse_line).collect())
}

/// Classifies and parses a single line (without its trailing newline) into
/// exactly one [`Line`]. See [`parse`]'s docs for the classification rules.
fn parse_line(raw: &str) -> Line {
    let trimmed_start = raw.trim_start();

    if trimmed_start.is_empty() {
        return Line::Blank;
    }

    if let Some(comment_text) = trimmed_start.strip_prefix('#') {
        return Line::Comment(comment_text.to_string());
    }

    try_parse_entry(raw).unwrap_or_else(|| Line::Malformed(raw.to_string()))
}

/// Attempts to parse `raw` (a line already known not to be blank or a
/// full-line comment) as an [`FstabEntry`]. Returns `None` on any structural
/// problem (too few fields, too many fields, or an unparseable dump/pass),
/// leaving it to the caller to fall back to [`Line::Malformed`].
fn try_parse_entry(raw: &str) -> Option<Line> {
    let (fields_part, trailing_comment) = split_trailing_comment(raw);
    let tokens: Vec<&str> = fields_part.split_whitespace().collect();

    if tokens.len() < 4 {
        return None;
    }

    let source = parse_source(tokens[0]);
    let mount_point = tokens[1].to_string();
    let fs_type = tokens[2].to_string();
    let options = parse_options(tokens[3]);

    let (dump, pass) = match tokens.len() {
        4 => (0, 0),
        5 => (tokens[4].parse::<u32>().ok()?, 0),
        6 => (
            tokens[4].parse::<u32>().ok()?,
            tokens[5].parse::<u32>().ok()?,
        ),
        _ => return None,
    };

    Some(Line::Entry(FstabEntry {
        source,
        mount_point,
        fs_type,
        options,
        dump,
        pass,
        trailing_comment,
    }))
}

/// Splits `raw` at the first `#` that starts a new whitespace-delimited
/// token (preceded by whitespace, or the very first character), returning
/// `(fields_part, trailing_comment)`. `trailing_comment` is the text after
/// that `#` (not including it) if one was found. A `#` embedded inside a
/// token with no preceding whitespace (e.g. part of an option value) is
/// left untouched, since it cannot be the start of a trailing comment.
fn split_trailing_comment(raw: &str) -> (&str, Option<String>) {
    let mut prev_is_space = true;
    for (i, c) in raw.char_indices() {
        if c == '#' && prev_is_space {
            return (&raw[..i], Some(raw[i + 1..].to_string()));
        }
        prev_is_space = c.is_whitespace();
    }
    (raw, None)
}

/// Classifies a single raw source-field token into a [`SourceId`] variant.
/// Never rejects input — anything not matching a recognized prefix, `none`,
/// a path, or a network-style address falls back to [`SourceId::Other`].
fn parse_source(token: &str) -> SourceId {
    if let Some(rest) = token.strip_prefix("UUID=") {
        return SourceId::Uuid(rest.to_string());
    }
    if let Some(rest) = token.strip_prefix("LABEL=") {
        return SourceId::Label(rest.to_string());
    }
    if let Some(rest) = token.strip_prefix("PARTUUID=") {
        return SourceId::PartUuid(rest.to_string());
    }
    if let Some(rest) = token.strip_prefix("PARTLABEL=") {
        return SourceId::PartLabel(rest.to_string());
    }
    if token == "none" {
        return SourceId::None;
    }
    // `//server/share`-style UNC/cifs addresses.
    if token.starts_with("//") {
        return SourceId::Network(token.to_string());
    }
    // Any other absolute path: block device, bind-mount source directory,
    // etc.
    if token.starts_with('/') {
        return SourceId::Path(token.to_string());
    }
    // `host:/path`-style nfs addresses: a colon with a non-empty, slash-free
    // host part before it.
    if let Some(colon_idx) = token.find(':') {
        let host = &token[..colon_idx];
        if !host.is_empty() && !host.contains('/') {
            return SourceId::Network(token.to_string());
        }
    }
    SourceId::Other(token.to_string())
}

/// Splits a comma-separated options field into an [`Options`] list,
/// preserving order. Each comma-delimited chunk becomes [`FstabOption::Flag`]
/// if it has no `=`, or [`FstabOption::KeyValue`] split at the first `=`
/// otherwise (so a value may itself contain `=`, e.g. in some
/// `credentials=`-style paths). Empty chunks (from a stray double comma)
/// are silently skipped rather than producing a malformed empty option.
fn parse_options(field: &str) -> Options {
    let mut opts = Vec::new();
    for chunk in field.split(',') {
        if chunk.is_empty() {
            continue;
        }
        match chunk.split_once('=') {
            Some((key, value)) => {
                opts.push(FstabOption::KeyValue(key.to_string(), value.to_string()))
            }
            None => opts.push(FstabOption::Flag(chunk.to_string())),
        }
    }
    Options::from_vec(opts)
}

/// Reconstructs a source field's full original text, including its `KEY=`
/// prefix where applicable (the model stores the prefix and payload
/// separately; this is the inverse of [`parse_source`]).
fn source_field_text(source: &SourceId) -> String {
    match source {
        SourceId::Path(s) | SourceId::Network(s) | SourceId::Other(s) => s.clone(),
        SourceId::Uuid(s) => format!("UUID={s}"),
        SourceId::Label(s) => format!("LABEL={s}"),
        SourceId::PartUuid(s) => format!("PARTUUID={s}"),
        SourceId::PartLabel(s) => format!("PARTLABEL={s}"),
        SourceId::None => "none".to_string(),
    }
}

impl fmt::Display for Document {
    /// Renders the document back to fstab text. See the module-level
    /// "Serialization contract" docs for the exact per-`Line` rendering
    /// rules and the scope of the byte-for-byte round-trip guarantee.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.lines {
            match line {
                Line::Blank => writeln!(f)?,
                Line::Comment(text) => writeln!(f, "#{text}")?,
                Line::Malformed(raw) => writeln!(f, "{raw}")?,
                Line::Entry(entry) => {
                    write!(
                        f,
                        "{} {} {} {} {} {}",
                        source_field_text(&entry.source),
                        entry.mount_point,
                        entry.fs_type,
                        entry.options,
                        entry.dump,
                        entry.pass
                    )?;
                    if let Some(comment) = &entry.trailing_comment {
                        write!(f, " #{comment}")?;
                    }
                    writeln!(f)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blank_lines() {
        let doc = parse("\n   \n\t\n");
        assert_eq!(doc.lines, vec![Line::Blank, Line::Blank, Line::Blank]);
    }

    #[test]
    fn parses_full_line_comment_with_and_without_leading_whitespace() {
        let doc = parse("# hello\n   # indented\n#no-space");
        assert_eq!(
            doc.lines,
            vec![
                Line::Comment(" hello".to_string()),
                Line::Comment(" indented".to_string()),
                Line::Comment("no-space".to_string()),
            ]
        );
    }

    #[test]
    fn parses_basic_entry_with_all_six_fields() {
        let doc = parse("UUID=aaaa-bbbb / ext4 defaults,noatime 0 1");
        assert_eq!(doc.lines.len(), 1);
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.source, SourceId::Uuid("aaaa-bbbb".to_string()));
        assert_eq!(entry.mount_point, "/");
        assert_eq!(entry.fs_type, "ext4");
        assert!(entry.options.contains("defaults"));
        assert!(entry.options.contains("noatime"));
        assert_eq!(entry.dump, 0);
        assert_eq!(entry.pass, 1);
        assert_eq!(entry.trailing_comment, None);
    }

    #[test]
    fn parses_trailing_comment_on_entry_line() {
        let doc = parse("LABEL=home /home ext4 defaults 0 2 # keep this mount last");
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(
            entry.trailing_comment,
            Some(" keep this mount last".to_string())
        );
        assert_eq!(entry.dump, 0);
        assert_eq!(entry.pass, 2);
    }

    #[test]
    fn arbitrary_whitespace_between_fields_is_accepted() {
        let doc = parse("/dev/sda1\t\t/boot   vfat\tdefaults 0\t2");
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.source, SourceId::Path("/dev/sda1".to_string()));
        assert_eq!(entry.mount_point, "/boot");
        assert_eq!(entry.fs_type, "vfat");
    }

    #[test]
    fn missing_dump_and_pass_default_to_zero() {
        let doc = parse("/dev/sda1 /boot vfat defaults");
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.dump, 0);
        assert_eq!(entry.pass, 0);
    }

    #[test]
    fn dump_present_without_pass_defaults_pass_to_zero() {
        let doc = parse("/dev/sda1 /boot vfat defaults 1");
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.dump, 1);
        assert_eq!(entry.pass, 0);
    }

    #[test]
    fn invalid_dump_field_makes_whole_line_malformed() {
        let doc = parse("/dev/sda1 /boot vfat defaults notanumber 2");
        assert_eq!(
            doc.lines,
            vec![Line::Malformed(
                "/dev/sda1 /boot vfat defaults notanumber 2".to_string()
            )]
        );
    }

    #[test]
    fn invalid_pass_field_makes_whole_line_malformed() {
        let doc = parse("/dev/sda1 /boot vfat defaults 0 notanumber");
        assert!(matches!(doc.lines[0], Line::Malformed(_)));
    }

    #[test]
    fn too_few_fields_is_malformed_without_panicking() {
        let doc = parse("/dev/fake1");
        assert_eq!(doc.lines, vec![Line::Malformed("/dev/fake1".to_string())]);
    }

    #[test]
    fn too_many_fields_is_malformed() {
        let doc = parse("/dev/sda1 /boot vfat defaults 0 2 extra-field");
        assert!(matches!(doc.lines[0], Line::Malformed(_)));
    }

    #[test]
    fn recognizes_all_source_prefix_styles() {
        assert_eq!(
            parse_source("UUID=1111"),
            SourceId::Uuid("1111".to_string())
        );
        assert_eq!(
            parse_source("LABEL=home"),
            SourceId::Label("home".to_string())
        );
        assert_eq!(
            parse_source("PARTUUID=2222"),
            SourceId::PartUuid("2222".to_string())
        );
        assert_eq!(
            parse_source("PARTLABEL=extra"),
            SourceId::PartLabel("extra".to_string())
        );
        assert_eq!(parse_source("none"), SourceId::None);
        assert_eq!(
            parse_source("/dev/sda1"),
            SourceId::Path("/dev/sda1".to_string())
        );
        assert_eq!(
            parse_source("//srv/share"),
            SourceId::Network("//srv/share".to_string())
        );
        assert_eq!(
            parse_source("srv.example.internal:/export"),
            SourceId::Network("srv.example.internal:/export".to_string())
        );
        assert_eq!(
            parse_source("mysterious-thing"),
            SourceId::Other("mysterious-thing".to_string())
        );
    }

    #[test]
    fn escaped_space_in_mount_point_stays_raw_and_undecoded() {
        let doc = parse("/dev/sda2 /mnt/My\\040Backup\\040Drive ext4 defaults 0 2");
        let Line::Entry(entry) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.mount_point, "/mnt/My\\040Backup\\040Drive");
        assert!(!entry.mount_point.contains(' '));
    }

    #[test]
    fn bind_mount_and_swap_entries_parse() {
        let doc = parse("/srv/data /mnt/data-bind none bind 0 0\nUUID=cccc /none swap sw 0 0");
        let Line::Entry(bind) = &doc.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(bind.fs_type, "none");
        assert!(bind.options.contains("bind"));

        let Line::Entry(swap) = &doc.lines[1] else {
            panic!("expected an entry");
        };
        assert_eq!(swap.fs_type, "swap");
        assert!(swap.options.contains("sw"));
    }

    #[test]
    fn key_value_option_with_empty_value_round_trips() {
        let opts = parse_options("defaults,x-systemd.automount=");
        assert_eq!(opts.get("x-systemd.automount"), Some(""));
    }

    #[test]
    fn display_renders_canonical_single_space_entry() {
        let doc = parse("UUID=aaaa / ext4 defaults,noatime 0 1 # note");
        assert_eq!(
            doc.to_string(),
            "UUID=aaaa / ext4 defaults,noatime 0 1 # note\n"
        );
    }

    #[test]
    fn display_round_trips_blank_and_comment_lines() {
        let text = "# header\n\nUUID=aaaa / ext4 defaults 0 1\n";
        let doc = parse(text);
        assert_eq!(doc.to_string(), text);
    }

    #[test]
    fn parse_never_panics_on_pathological_input() {
        let inputs = [
            "",
            "#",
            "   ",
            "=====",
            "UUID=",
            "::::",
            "\\040\\040",
            "#################",
        ];
        for input in inputs {
            let doc = parse(input);
            // Just asserting this returns without panicking; re-serializing
            // must also never panic.
            let _ = doc.to_string();
        }
    }

    #[test]
    fn fixed_point_after_one_round_trip_for_noncanonical_whitespace() {
        // Non-canonical (tab-aligned) input won't byte-round-trip on the
        // first pass, but parsing its canonical re-serialization must
        // reproduce an identical Document, and serializing that must be
        // stable (a fixed point).
        let original = "/dev/sda1\t/boot\tvfat\tdefaults 0\t2";
        let once = parse(original);
        let serialized_once = once.to_string();
        let twice = parse(&serialized_once);
        assert_eq!(once, twice);
        assert_eq!(twice.to_string(), serialized_once);
    }
}
