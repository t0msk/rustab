//! Text-to-`Document` parser and round-trip serializer - see `docs/SPEC.md` §5.
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
//! parser never decodes `\040`/`\011`-style octal escapes - raw field text is
//! captured exactly as written. Field splitting itself is whitespace-based
//! (consecutive runs of spaces/tabs are delimiters); this is safe with
//! respect to escape sequences without any special-casing because an escape
//! sequence's raw text (`\`, then octal digits) never itself contains a
//! literal whitespace byte - only the *decoded* character would, and we
//! never decode. So naive `str::split_whitespace` cannot split in the middle
//! of a `\NNN` sequence.
//!
//! ### Description marker (`# // <text>`) absorption
//!
//! On top of per-line classification, [`parse`] performs one post-processing
//! pass that "absorbs" an entry's optional
//! [`FstabEntry::description`] - a human-readable note stored in the file as a
//! comment line of its own, immediately above the entry it describes (see that
//! field's docs for how it differs from
//! [`FstabEntry::trailing_comment`], which is an inline comment on the
//! entry's *own* line and is handled by the per-line splitting rule below).
//!
//! A comment line is recognized as a description marker when the text after
//! its leading `#`, with leading whitespace trimmed, starts with the literal
//! two characters `//`, **those two slashes are immediately followed by at
//! least one whitespace character**, and at least one non-whitespace
//! character remains after that. The stored description is the remainder
//! after the slashes, trimmed at both ends; internal whitespace is preserved
//! verbatim. The whitespace between the `#` and the `//` carries no meaning -
//! the `#` is fstab's required comment syntax and the `//` is Rustab's own
//! app-level marker - so `# // note` and `#// note` are recognized
//! identically. A marker line whose payload is empty after trimming (`# //`,
//! `# //   `) is deliberately **not** recognized: an empty description
//! carries no information, and absorbing it would make the marker line
//! silently vanish on the very next save.
//!
//! The whitespace required *directly after the slashes* is what keeps the
//! marker from colliding with real fstab content. Every CIFS/SMB source field
//! is spelled `//server/share` (see [`SourceId::Network`]), so a line a user
//! commented out by hand - `#//nas/media /mnt/media cifs guest,noauto 0 0` -
//! is an ordinary comment that happens to begin with `//`. Absorbing it as
//! the description of whatever entry sits below it would both corrupt that
//! line's text on re-serialization and risk deleting it outright on a later
//! save. Requiring whitespace after the slashes rules out that entire
//! collision class rather than just the one CIFS spelling: this parser's own
//! field splitting is whitespace-delimited, so no single real fstab field can
//! ever contain an internal whitespace character, and therefore a
//! commented-out line's first field can never have whitespace directly after
//! a leading `//` that is genuinely part of the field's own text. The cost is
//! that `#//note`, with no whitespace anywhere between the slashes and the
//! text, is no longer a marker - it is treated as an ordinary, unmarked
//! comment (never an error). Both the canonical marker form `# // <text>` and
//! every natural way a person types it by hand still work.
//!
//! Absorption additionally requires *direct* adjacency: the very next line
//! must itself be a [`Line::Entry`], with nothing at all in between (not even
//! a [`Line::Blank`]). A marked comment that is the last line of the file, or
//! is separated from the entry below it by any other line, stays an ordinary
//! [`Line::Comment`] - it is never an error. When two marked comments sit
//! directly above one entry, only the one immediately adjacent to the entry is
//! absorbed; the one above it stays an ordinary [`Line::Comment`], unchanged,
//! even though its own text also starts with `//`. This is decided purely by
//! looking at each comment's own index and the one after it, never at the one
//! before it, so the outcome does not depend on evaluation order. An ordinary
//! comment without the `//` marker is never absorbed, whatever it sits next
//! to.
//!
//! Absorption is the one and only reason the number of [`Line`]s in a parsed
//! [`Document`] can be smaller than the number of physical lines in the input;
//! the absorbed text is not lost, it is re-emitted as its own comment line by
//! the serializer (see below).
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
//!   reuses [`Options`]'s own `Display` impl (comma-joined). If the entry has
//!   a [`FstabEntry::description`], it is written first, on its own preceding
//!   physical line, in the exact canonical marker form `# ` + `// ` + the
//!   description text trimmed at both ends (one space after the `#`, one
//!   after the `//`) - the inverse of the absorption rule above, so a
//!   description read from a file is written back byte-identically if its
//!   marker line was already canonical. An entry with no description renders
//!   exactly as it did before descriptions existed.
//!
//! This is a deliberately **canonical** rendering: it normalizes whatever
//! original inter-field whitespace a line had down to single spaces. Per the
//! module-level task brief, byte-for-byte round-tripping
//! (`parse(text).to_string() == text`) is therefore only guaranteed for text
//! that was already in this canonical form (single-space-separated fields,
//! no leading whitespace before comments, entries with all six fields
//! present, a trailing newline at end of file) - which is exactly the shape
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
/// Always succeeds and never panics - see the module-level "Parsing
/// contract" docs. Each input line (split on `\n`, with any trailing `\r`
/// from CRLF input tolerated by [`str::lines`]) is first classified into
/// exactly one [`Line`], in original order. The single exception to "one
/// input line, one output line" is description-marker absorption (see
/// [`absorb_description_comments`] and the module docs): a `# // <text>`
/// comment line directly above an entry is folded into that entry's
/// [`FstabEntry::description`] and so does not occupy a [`Line`] of its own.
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
/// first character) - everything from there to end-of-line becomes
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
    let classified: Vec<Line> = text.lines().map(parse_line).collect();
    Document::from_lines(absorb_description_comments(classified))
}

/// Recognizes the app-level description marker on a full-line comment's text
/// (already has its leading `#` stripped, per [`Line::Comment`]'s own docs).
/// Returns `Some(trimmed_text)` iff `text.trim_start()` starts with the
/// literal two characters `//`, those two slashes are **immediately followed
/// by at least one whitespace character**, AND there is at least one
/// non-whitespace character remaining after trimming both ends.
/// Returns `None` otherwise - including when the `//` prefix is present but
/// nothing but whitespace follows it (deliberately NOT treated as a marked
/// description: an empty description carries no information, and absorbing
/// it would make the marker line vanish on the very next save - see
/// `model.rs`'s [`FstabEntry::description`] field docs), and when the `//` is
/// followed directly by a non-whitespace character (`#//text`), which is what
/// keeps a commented-out fstab line whose own first field starts with `//`
/// (every CIFS/SMB source, see [`SourceId::Network`]) from being mistaken for
/// a marker. See the module-level docs for why that whitespace requirement is
/// a complete fix for that collision class rather than a heuristic.
fn extract_description_marker(text: &str) -> Option<String> {
    let after_slashes = text.trim_start().strip_prefix("//")?;
    if !after_slashes.starts_with(char::is_whitespace) {
        return None;
    }
    let payload = after_slashes.trim();
    if payload.is_empty() {
        None
    } else {
        Some(payload.to_string())
    }
}

/// Post-processes a freshly-classified line list, "absorbing" any
/// [`Line::Comment`] that is recognized as an app-level description marker
/// (see [`extract_description_marker`]) AND is immediately followed (no line
/// of any kind in between - must be literally the next index) by a
/// [`Line::Entry`], into that entry's [`FstabEntry::description`] field -
/// removing the comment as a separate line from the output.
///
/// Two-phase by design: first decide which original indices are absorbed
/// (purely by looking at each [`Line::Comment`]'s *own* index `i` and index
/// `i + 1`, never at `i - 1`), then build the final vector by filtering. This
/// is what makes two consecutive marked comments directly above one entry
/// resolve correctly and deterministically: only the one immediately
/// adjacent to the entry (`i + 1` is the entry) is ever absorbed; anything
/// further up is untouched, even if it also happens to start with `//`.
fn absorb_description_comments(lines: Vec<Line>) -> Vec<Line> {
    // Phase 1: decide, by original index alone, which comment lines are
    // absorbed and what description text each one contributes.
    let mut absorbed: Vec<Option<String>> = vec![None; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        let Line::Comment(text) = line else {
            continue;
        };
        let Some(description) = extract_description_marker(text) else {
            continue;
        };
        if matches!(lines.get(i + 1), Some(Line::Entry(_))) {
            absorbed[i] = Some(description);
        }
    }

    // Phase 2: rebuild the line list, dropping every absorbed comment and
    // attaching its text to the entry that directly followed it. Taking the
    // text out of `absorbed` here (rather than cloning) is safe because
    // index `i - 1` was already passed over by this same in-order loop.
    let mut result = Vec::with_capacity(lines.len());
    for (i, line) in lines.into_iter().enumerate() {
        if absorbed[i].is_some() {
            continue;
        }
        match line {
            Line::Entry(mut entry) if i > 0 && absorbed[i - 1].is_some() => {
                entry.description = absorbed[i - 1].take();
                result.push(Line::Entry(entry));
            }
            other => result.push(other),
        }
    }
    result
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
        // A description never comes from the entry's own line; it is
        // attached afterwards by `absorb_description_comments` if a marked
        // comment line directly precedes this entry.
        description: None,
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
/// Never rejects input - anything not matching a recognized prefix, `none`,
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
                    // A description is rendered as its own canonical marker
                    // line directly above the entry it belongs to - the
                    // inverse of `absorb_description_comments`.
                    if let Some(description) = &entry.description {
                        writeln!(f, "# // {}", description.trim())?;
                    }
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

    // --- Description marker (`# // <text>`) ---

    /// Returns the single entry of a one-entry document, panicking with a
    /// useful message if the document is not shaped that way.
    fn only_entry(doc: &Document) -> &FstabEntry {
        let entries: Vec<&FstabEntry> = doc.entries().collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one entry, got lines: {:?}",
            doc.lines
        );
        entries[0]
    }

    #[test]
    fn extract_description_marker_recognizes_only_the_marked_nonempty_shapes() {
        assert_eq!(
            extract_description_marker(" // a note").as_deref(),
            Some("a note")
        );
        assert_eq!(
            extract_description_marker("// a note").as_deref(),
            Some("a note")
        );
        // The whitespace directly after the slashes is mandatory: without it
        // the text could be the first field of a commented-out fstab line
        // (`//server/share`), so this shape is an ordinary comment, not a
        // marker.
        assert_eq!(extract_description_marker("//a note"), None);
        assert_eq!(
            extract_description_marker("//nas/media /mnt/media cifs"),
            None
        );
        assert_eq!(extract_description_marker(" a plain note"), None);
        assert_eq!(extract_description_marker(" /not a marker"), None);
        assert_eq!(extract_description_marker(" //"), None);
        assert_eq!(extract_description_marker(" //   "), None);
    }

    #[test]
    fn description_marker_is_recognized_with_or_without_a_space_after_the_hash() {
        // The `#` is fstab syntax and the `//` is rustab's marker, so the
        // whitespace between them carries no meaning: both spellings must
        // absorb, and must yield the exact same description text.
        let with_space = parse("# // project archive\nUUID=aaaa /mnt/a ext4 defaults 0 2");
        let without_space = parse("#// project archive\nUUID=aaaa /mnt/a ext4 defaults 0 2");

        assert_eq!(with_space.lines.len(), 1);
        assert_eq!(without_space.lines.len(), 1);
        assert_eq!(
            only_entry(&with_space).description.as_deref(),
            Some("project archive")
        );
        assert_eq!(
            only_entry(&without_space).description.as_deref(),
            Some("project archive")
        );
        assert_eq!(with_space, without_space);
    }

    #[test]
    fn description_text_is_trimmed_at_the_ends_but_not_internally() {
        let doc = parse("#   //\t  photo   archive,  2nd  drive   \nUUID=aaaa /mnt/a ext4 d 0 2");
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("photo   archive,  2nd  drive")
        );
    }

    /// The ambiguous-candidate rule: when two marked comments sit directly
    /// above one entry, only the one immediately adjacent to it is absorbed.
    /// The one above stays an ordinary comment line, byte-unchanged, even
    /// though its own text also starts with `//`.
    #[test]
    fn only_the_marked_comment_immediately_adjacent_to_an_entry_is_absorbed() {
        let text = "# // first marker, not adjacent\n\
                    # // second marker, adjacent\n\
                    UUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(
            doc.lines[0],
            Line::Comment(" // first marker, not adjacent".to_string())
        );
        let Line::Entry(entry) = &doc.lines[1] else {
            panic!("expected the entry at index 1, got {:?}", doc.lines[1]);
        };
        assert_eq!(
            entry.description.as_deref(),
            Some("second marker, adjacent")
        );

        // And the non-absorbed marker survives serialization verbatim, so
        // the whole document still round-trips byte-for-byte.
        assert_eq!(doc.to_string(), text);
    }

    #[test]
    fn marked_comment_as_the_last_line_of_the_file_stays_an_ordinary_comment() {
        let doc = parse("UUID=aaaa /mnt/a ext4 defaults 0 2\n# // dangling marker\n");
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(
            doc.lines[1],
            Line::Comment(" // dangling marker".to_string())
        );
    }

    #[test]
    fn marked_comment_separated_from_its_entry_by_a_blank_line_is_not_absorbed() {
        let text = "# // not adjacent\n\nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.lines[0], Line::Comment(" // not adjacent".to_string()));
        assert_eq!(doc.lines[1], Line::Blank);
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), text);
    }

    #[test]
    fn marked_comment_with_an_empty_payload_is_not_absorbed() {
        // `# //` - marker, nothing after it at all.
        let bare = "# //\nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(bare);
        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(doc.lines[0], Line::Comment(" //".to_string()));
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), bare);

        // `# //   ` - marker followed by whitespace only, which trims away
        // to an empty payload and so is equally not a description.
        let padded = "# //   \nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(padded);
        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(doc.lines[0], Line::Comment(" //   ".to_string()));
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), padded);
    }

    /// The safety-critical regression test: an ordinary, unmarked comment
    /// directly above an entry must never be swallowed into that entry.
    #[test]
    fn ordinary_unmarked_comment_directly_above_an_entry_is_never_absorbed() {
        let text = "# a plain note\nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0], Line::Comment(" a plain note".to_string()));
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), text);
    }

    /// The CIFS collision regression test: `//server/share` is a real fstab
    /// source spelling (see `SourceId::Network`), so a hand-commented-out
    /// network mount sitting directly above another entry must stay an
    /// ordinary comment. Absorbing it would corrupt its whitespace on
    /// re-serialization and risk deleting the line on a later save.
    #[test]
    fn commented_out_cifs_source_line_is_never_mistaken_for_a_description_marker() {
        let original = "#//nas/media /mnt/media cifs guest,noauto 0 0\n\
                        UUID=1111 /mnt/data ext4 defaults 0 2\n";
        let doc = parse(original);

        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(
            doc.lines[0],
            Line::Comment("//nas/media /mnt/media cifs guest,noauto 0 0".to_string())
        );
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), original);
    }

    /// A marker-shaped comment with no whitespace after the slashes is an
    /// ordinary comment, not an error and not a description - the deliberate
    /// narrowing that makes the CIFS case above safe.
    #[test]
    fn marked_comment_without_whitespace_after_the_slashes_is_an_ordinary_comment() {
        let original = "#//note\nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(original);

        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(doc.lines[0], Line::Comment("//note".to_string()));
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), original);
    }

    #[test]
    fn document_with_a_description_round_trips_to_a_stable_fixed_point() {
        let original = "# header\n\n# // scratch space for builds\nLABEL=scratch /mnt/scratch xfs defaults 0 2\n";
        let once = parse(original);
        let serialized_once = once.to_string();
        assert_eq!(serialized_once, original);

        let twice = parse(&serialized_once);
        assert_eq!(once, twice);
        assert_eq!(twice.to_string(), serialized_once);
    }

    #[test]
    fn two_entries_keep_their_own_independent_descriptions() {
        let original = "# // first entry's own note\n\
                        UUID=aaaa /mnt/a ext4 defaults 0 2\n\
                        \n\
                        # // second entry's own note\n\
                        UUID=bbbb /mnt/b ext4 defaults 0 2\n";
        let doc = parse(original);

        let entries: Vec<&FstabEntry> = doc.entries().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].description.as_deref(),
            Some("first entry's own note")
        );
        assert_eq!(
            entries[1].description.as_deref(),
            Some("second entry's own note")
        );
        assert_eq!(doc.to_string(), original);
    }

    #[test]
    fn document_with_no_marked_comments_is_byte_for_byte_unaffected() {
        // Regression proof that absorption never fires spuriously: a
        // document mixing ordinary comments, blanks, a malformed line, a
        // trailing inline comment and several entries must serialize to
        // exactly its original text, with no entry picking up a description.
        let original = "#rustab header comment\n\
                        \n\
                        #ordinary note above an entry\n\
                        UUID=aaaa /mnt/a ext4 defaults 0 2 # inline note\n\
                        /dev/fake1\n\
                        LABEL=home /home ext4 defaults 0 2\n\
                        #trailing note at end of file\n";
        let doc = parse(original);

        assert!(doc.entries().all(|entry| entry.description.is_none()));
        assert_eq!(doc.lines.len(), original.lines().count());
        assert_eq!(doc.to_string(), original);
    }
}
