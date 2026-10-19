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
//! ### Description block absorption
//!
//! On top of per-line classification, [`parse`] performs one post-processing
//! pass that "absorbs" an entry's optional [`FstabEntry::description`] - a
//! human-readable note stored in the file as the ordinary comment line (or
//! lines) sitting directly above the entry it describes (see that field's docs
//! for how it differs from [`FstabEntry::trailing_comment`], which is an
//! inline comment on the entry's *own* line and is handled by the per-line
//! splitting rule below).
//!
//! **No special marker is involved.** Any ordinary `#` comment adjacent to an
//! entry in the sense defined below is that entry's description. This
//! deliberately replaced an earlier `# // <text>` marker convention, and with
//! it that convention's "an unmarked comment is inert and is never absorbed"
//! invariant: that guarantee is gone on purpose, so a comment line's own text
//! no longer has any bearing on whether it is absorbed - only its position
//! does.
//!
//! Walking upward from a [`Line::Entry`] to find its attached block:
//!
//! 1. Skip **exactly one** immediately-preceding [`Line::Blank`], if present.
//!    That blank is a pure separator: it belongs to neither neighbor, is never
//!    part of the description, and is never absorbed - it survives as its own
//!    literal [`Line::Blank`] at its original relative position. If the line
//!    directly above the entry is not blank, there is nothing to skip.
//! 2. From there, keep walking upward across a contiguous run of
//!    [`Line::Comment`] and [`Line::Blank`] lines, in any mixture, until
//!    hitting a [`Line::Entry`], a [`Line::Malformed`], or the start of the
//!    file. That whole run is the entry's attached block. Note that a comment
//!    line does **not** stop the walk: two comment blocks separated only by
//!    blanks are one single run, and merge into one description.
//! 3. If the run contains **zero** comment lines, the entry has no
//!    description and - importantly - **nothing in that span is touched**.
//!    Absorption only ever rewrites a span it actually extracts a description
//!    from; a span that yields nothing is left exactly as it was (so, e.g.,
//!    two blank lines between two entries stay two blank lines). This keeps
//!    absorption-with-something-to-show-for-it the one and only way the
//!    number of [`Line`]s can shrink.
//! 4. Otherwise the description is the run's **comment-line texts only**, in
//!    original top-to-bottom order, each trimmed, joined with `\n` - so a
//!    description may legitimately be multi-line. The run's [`Line::Blank`]
//!    lines contribute **nothing to the value**: they are pure formatting
//!    noise inside the block, and the re-rendered description block never
//!    contains a blank line (see the serialization contract below). They are
//!    not *deleted from the file*, though - only the run's comment lines stop
//!    being [`Line`]s of their own. Each blank in the run survives as its own
//!    literal [`Line::Blank`], keeping its relative order, which - because
//!    the description block is re-rendered hugging its entry - means the
//!    run's blanks end up directly above that block.
//!
//! One deliberate guard on step 4: if every comment line in the run is empty
//! or whitespace-only (a run of bare `#` lines), the joined text carries no
//! information, so step 3's "nothing is touched" outcome applies instead.
//! Otherwise those lines would be absorbed into a description that
//! [`crate::model`] documents as never being stored (empty/whitespace-only is
//! normalized to `None`), and would then vanish from the file on the next
//! save.
//!
//! ### Why the run's blank lines are kept as blank lines
//!
//! Step 4 keeping each blank in the run is not a softening of "blanks are not
//! part of the description" - the value and the rendered block really do
//! contain no blanks. It is what makes absorption **idempotent**, which
//! `docs/SPEC.md` §5/§6 and this crate's own fixed-point tests require:
//! re-parsing this parser's own output must reproduce the same document, and
//! saving twice must not keep changing the file.
//!
//! Deleting the run's blanks outright fails that, and the counterexample is
//! the plan's own `/boot` case. Given `entry, blank, comment, blank, blank,
//! entry`, step 1 skips the last blank and the run is `blank, comment,
//! blank`. If those two blanks were deleted, the first save would emit
//! `entry, blank, comment, entry` (the one skipped separator, then the block).
//! Re-parsing *that* finds a comment directly above the entry, so step 1 skips
//! nothing, the run becomes `blank, comment` - and the surviving separator
//! blank is deleted by the second save. Blank lines would keep disappearing,
//! one save at a time, with nothing to show for it; the number of physical
//! lines would shrink on a save that changed nothing. Keeping the run's
//! blanks converges immediately instead: the first save is already a fixed
//! point, and the physical line count never changes just because a document
//! was parsed and written back.
//!
//! Two further consequences worth stating plainly, both of which follow from
//! the rules above rather than from any extra special-casing:
//!
//! - The rule is scoped to [`Line::Entry`] lines. A comment block above a
//!   [`Line::Malformed`] line, or one at the end of the file with no entry
//!   below it at all, is never absorbed and stays an ordinary
//!   [`Line::Comment`] run. A [`Line::Malformed`] line also *stops* an
//!   upward walk, so it shields everything above it from the entry below it.
//! - Byte-for-byte round-tripping holds for the shape real fstab files use -
//!   an entry's comment block directly above it, optionally separated from
//!   whatever is above by blank lines - because the block is re-rendered in
//!   place. It does *not* hold when a blank line sits *between* two comment
//!   lines of one run, or between a run's comments and the entry: such a
//!   blank is in the run, so it comes back above the block rather than inside
//!   it. That is a one-time re-flow on the first save, and the result is a
//!   fixed point.
//!
//! Absorption is the one and only reason the number of [`Line`]s in a parsed
//! [`Document`] can be smaller than the number of physical lines in the input;
//! the absorbed comment text is not lost, it is re-emitted as its own comment
//! line (or lines) by the serializer (see below), so the number of *physical*
//! lines is unchanged by a parse-and-write-back.
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
//!   a [`FstabEntry::description`], it is written first, as its own block of
//!   physical lines directly above the entry: the description is split on
//!   `\n` and each piece is trimmed and written as `# <piece>` (or a bare `#`
//!   for an empty piece, with no trailing space). The block is contiguous -
//!   no blank line is ever inserted inside it, nor between its last line and
//!   the entry's own fields line. This is the inverse of the absorption rule
//!   above, so a description read from a file is written back byte-identically
//!   as long as its comment lines were already canonical (`# ` then the text,
//!   with the block hugging the entry). An entry with no description renders
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
/// input line, one output line" is description-block absorption (see
/// [`absorb_description_comments`] and the module docs): the ordinary comment
/// line(s) directly above an entry are folded into that entry's
/// [`FstabEntry::description`] and so do not occupy [`Line`]s of their own.
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

/// Post-processes a freshly-classified line list, folding each
/// [`Line::Entry`]'s attached comment block into its
/// [`FstabEntry::description`] and removing that block's *comment* lines from
/// the output (the block's blank lines stay, per step 4 of the rule).
/// Implements exactly the four-step upward walk documented in the module's
/// "Description block absorption" section; that section, not this comment, is
/// the normative description of the rule.
///
/// Two-phase by design, so that every decision is made against *original*
/// indices and the outcome cannot depend on evaluation order: phase 1 walks
/// upward from each entry and records (a) which indices that entry absorbs and
/// (b) the description text it absorbs them into; phase 2 rebuilds the vector
/// by filtering out the absorbed indices and attaching each description to its
/// entry.
///
/// Idempotence (required by this crate's fixed-point tests) is a property of
/// the pair (this function, [`fmt::Display`] for [`Document`]), and is argued
/// in the module docs' "Why the run's blank lines are kept as blank lines"
/// section. In short: the serializer re-renders the run's comment lines
/// canonically, hugging the entry, and leaves the run's blanks above them, so
/// a second walk over that output finds the same run and the same description.
///
/// The recorded spans provably never overlap: an entry's walk stops at the
/// first [`Line::Entry`]/[`Line::Malformed`] above it, so a span lies strictly
/// between its own entry and the nearest row-kind line above it, and the one
/// blank line step 1 may skip is excluded from the span by construction.
fn absorb_description_comments(lines: Vec<Line>) -> Vec<Line> {
    // Phase 1: decide, by original index alone, which lines are absorbed and
    // what description each entry ends up with.
    let mut absorbed = vec![false; lines.len()];
    let mut descriptions: Vec<Option<String>> = vec![None; lines.len()];

    for (i, line) in lines.iter().enumerate() {
        if !matches!(line, Line::Entry(_)) {
            continue;
        }

        // Step 1: skip exactly one immediately-preceding blank separator. It
        // stays outside the absorbed span and so survives untouched.
        let mut end = i;
        if end > 0 && matches!(lines[end - 1], Line::Blank) {
            end -= 1;
        }

        // Step 2: continue upward across a contiguous comment/blank run,
        // stopping at an entry, a malformed line, or the start of the file.
        let mut start = end;
        while start > 0 && matches!(lines[start - 1], Line::Comment(_) | Line::Blank) {
            start -= 1;
        }

        // Step 4: comment texts only, each trimmed, joined with `\n`. Blanks
        // inside the run contribute nothing to the value.
        let description = lines[start..end]
            .iter()
            .filter_map(|line| match line {
                Line::Comment(text) => Some(text.trim()),
                _ => None,
            })
            .collect::<Vec<&str>>()
            .join("\n");

        // Step 3 (plus the all-blank-comments guard): a span that yields no
        // description text is left completely alone - not absorbed, not
        // rewritten, nothing removed.
        if description.trim().is_empty() {
            continue;
        }

        // Only the run's comment lines stop being lines of their own; its
        // blanks stay exactly where they are (see the module docs).
        for (offset, line) in lines[start..end].iter().enumerate() {
            if matches!(line, Line::Comment(_)) {
                absorbed[start + offset] = true;
            }
        }
        descriptions[i] = Some(description);
    }

    // Phase 2: rebuild the line list, dropping every absorbed comment and
    // attaching each description to the entry that claimed it. Taking the
    // text out of `descriptions` (rather than cloning) is safe because each
    // index is visited exactly once by this in-order loop.
    let mut result = Vec::with_capacity(lines.len());
    for (i, line) in lines.into_iter().enumerate() {
        if absorbed[i] {
            continue;
        }
        match line {
            Line::Entry(mut entry) => {
                // `try_parse_entry` always leaves this `None`, so this both
                // attaches an absorbed description and preserves "no block
                // above me means no description".
                entry.description = descriptions[i].take();
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
        // attached afterwards by `absorb_description_comments` if a comment
        // block is attached to this entry.
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
                    // A description is rendered as a contiguous block of
                    // ordinary comment lines directly above the entry it
                    // belongs to - the inverse of
                    // `absorb_description_comments`. One comment line per
                    // `\n`-separated piece, each trimmed; an empty piece
                    // renders as a bare `#` rather than `"# "`, so a
                    // re-parse sees the same empty piece back.
                    if let Some(description) = &entry.description {
                        for piece in description.split('\n') {
                            let text = piece.trim();
                            if text.is_empty() {
                                writeln!(f, "#")?;
                            } else {
                                writeln!(f, "# {text}")?;
                            }
                        }
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
        // The canonical shape: a separator blank, then the entry's own
        // comment block hugging it. Both come back byte-for-byte.
        let text = "\n# header\nUUID=aaaa / ext4 defaults 0 1\n";
        let doc = parse(text);
        assert_eq!(doc.to_string(), text);
    }

    /// The one shape that re-flows on the first save: a blank line *between*
    /// an entry's comment block and the entry is inside the absorbed run, so
    /// it comes back above the re-rendered block instead of inside it. No
    /// line is gained or lost, and the result is a fixed point.
    #[test]
    fn a_blank_between_a_comment_and_its_entry_reflows_once_then_is_stable() {
        let text = "# header\n\nUUID=aaaa / ext4 defaults 0 1\n";
        let once = parse(text);
        let serialized = once.to_string();

        assert_eq!(serialized, "\n# header\nUUID=aaaa / ext4 defaults 0 1\n");
        assert_eq!(serialized.lines().count(), text.lines().count());

        let twice = parse(&serialized);
        assert_eq!(once, twice);
        assert_eq!(twice.to_string(), serialized);
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

    // --- Description block absorption ---

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

    /// Every entry's description, in document order.
    fn descriptions(doc: &Document) -> Vec<Option<&str>> {
        doc.entries()
            .map(|entry| entry.description.as_deref())
            .collect()
    }

    #[test]
    fn an_ordinary_comment_directly_above_an_entry_is_its_description() {
        // The whole point of T43: no marker of any kind is involved, and the
        // comment line stops being a `Line` of its own.
        let doc = parse("# a plain note\nUUID=aaaa /mnt/a ext4 defaults 0 2\n");

        assert_eq!(doc.lines.len(), 1, "lines: {:?}", doc.lines);
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("a plain note")
        );
    }

    #[test]
    fn the_comment_hash_and_its_following_whitespace_are_not_part_of_the_value() {
        // Each comment line's text is trimmed; internal whitespace is
        // content and stays verbatim.
        let doc = parse("#\t  photo   archive,  2nd  drive   \nUUID=aaaa /mnt/a ext4 d 0 2");
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("photo   archive,  2nd  drive")
        );
    }

    #[test]
    fn a_contiguous_comment_block_becomes_one_multi_line_description() {
        let text = "# first line of the note\n\
                    # second line of the note\n\
                    # third line of the note\n\
                    UUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 1, "lines: {:?}", doc.lines);
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("first line of the note\nsecond line of the note\nthird line of the note")
        );
        // Canonical in, canonical out: the block is re-emitted contiguously,
        // directly above the entry, with no blank line anywhere in it.
        assert_eq!(doc.to_string(), text);
    }

    /// A comment line does not stop the upward walk, so two comment blocks
    /// separated only by a blank line above one entry are one single run and
    /// merge into one description, with the internal blank dropped. This is
    /// lines 15/17 of the plan's worked example in isolation.
    #[test]
    fn two_comment_blocks_separated_by_a_blank_line_merge_into_one_description() {
        let doc = parse(
            "# /dev/sda3\n\
             \n\
             # /dev/sdb1\n\
             UUID=aaaa /mnt/a ext4 defaults 0 2\n",
        );

        assert_eq!(doc.lines.len(), 2, "lines: {:?}", doc.lines);
        assert_eq!(doc.lines[0], Line::Blank);
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("/dev/sda3\n/dev/sdb1")
        );
        // The internal blank is formatting noise: not part of the value and
        // never inside the re-rendered block - it stays a blank line of its
        // own, above the block.
        assert_eq!(
            doc.to_string(),
            "\n# /dev/sda3\n# /dev/sdb1\nUUID=aaaa /mnt/a ext4 defaults 0 2\n"
        );
    }

    /// Step 1 of the rule: exactly one blank line directly above an entry is
    /// a pure separator. Here the run above it has no comments at all, so by
    /// step 3 nothing whatsoever is touched - the blanks stay blanks.
    #[test]
    fn a_run_with_zero_comments_is_left_completely_untouched() {
        let text = "UUID=aaaa /mnt/a ext4 defaults 0 2\n\
                    \n\
                    \n\
                    UUID=bbbb /mnt/b ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 4, "lines: {:?}", doc.lines);
        assert_eq!(doc.lines[1], Line::Blank);
        assert_eq!(doc.lines[2], Line::Blank);
        assert_eq!(descriptions(&doc), vec![None, None]);
        assert_eq!(doc.to_string(), text);
    }

    /// The other resolved sub-case: when a description *is* found, the one
    /// blank line step 1 skipped survives as its own `Line::Blank`, at its
    /// original relative position - which, with the run's comments absorbed,
    /// means above the re-rendered description block, never between the block
    /// and the entry. The run's own blanks survive the same way (see the
    /// module docs on idempotence), so no line is lost.
    #[test]
    fn the_skipped_separator_blank_survives_when_a_description_is_found() {
        // The plan's `/boot` case: entry, blank, comment, blank, blank, entry.
        let original = "UUID=aaaa /mnt/a ext4 defaults 0 2\n\
                        \n\
                        # /dev/sda2\n\
                        \n\
                        \n\
                        UUID=bbbb /boot vfat defaults 0 2\n";
        let doc = parse(original);

        assert_eq!(doc.lines.len(), 5, "lines: {:?}", doc.lines);
        assert!(matches!(doc.lines[0], Line::Entry(_)));
        assert_eq!(doc.lines[1], Line::Blank);
        assert_eq!(doc.lines[2], Line::Blank);
        assert_eq!(doc.lines[3], Line::Blank);
        assert_eq!(descriptions(&doc), vec![None, Some("/dev/sda2")]);

        let serialized = doc.to_string();
        assert_eq!(
            serialized,
            "UUID=aaaa /mnt/a ext4 defaults 0 2\n\
             \n\
             \n\
             \n\
             # /dev/sda2\n\
             UUID=bbbb /boot vfat defaults 0 2\n"
        );
        // Nothing was gained or lost, and this is already a fixed point - the
        // separator blank is not eaten by a second save.
        assert_eq!(serialized.lines().count(), original.lines().count());
        let twice = parse(&serialized);
        assert_eq!(twice, doc);
        assert_eq!(twice.to_string(), serialized);
    }

    /// A comment block with no entry below it at all is freestanding: the
    /// rule only ever walks upward *from an entry*.
    #[test]
    fn a_comment_block_at_end_of_file_stays_a_freestanding_comment() {
        let text = "UUID=aaaa /mnt/a ext4 defaults 0 2\n\
                    \n\
                    #a trailing note\n\
                    #that belongs to nothing\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 4, "lines: {:?}", doc.lines);
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.lines[2], Line::Comment("a trailing note".to_string()));
        assert_eq!(
            doc.lines[3],
            Line::Comment("that belongs to nothing".to_string())
        );
        assert_eq!(doc.to_string(), text);
    }

    /// A malformed line stops the upward walk, so it shields the comment
    /// block above it from the entry below it. That block stays freestanding,
    /// byte-unchanged.
    #[test]
    fn a_malformed_line_shields_the_comment_block_above_it() {
        let text = "#this note describes the broken line below it\n\
                    /dev/fake1\n\
                    # this one describes the entry below it\n\
                    UUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 3, "lines: {:?}", doc.lines);
        assert_eq!(
            doc.lines[0],
            Line::Comment("this note describes the broken line below it".to_string())
        );
        assert_eq!(doc.lines[1], Line::Malformed("/dev/fake1".to_string()));
        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("this one describes the entry below it")
        );
        assert_eq!(doc.to_string(), text);
    }

    /// A run whose comment lines are all empty carries no information, so the
    /// step-3 "nothing is touched" outcome applies rather than storing an
    /// empty description (which `model.rs` documents as never stored) and
    /// losing those lines on the next save.
    #[test]
    fn a_run_of_empty_comment_lines_is_not_absorbed() {
        let text = "#\n#   \nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(doc.lines.len(), 3, "lines: {:?}", doc.lines);
        assert_eq!(only_entry(&doc).description, None);
        assert_eq!(doc.to_string(), text);
    }

    /// An empty line *inside* an otherwise non-empty block is content: it is
    /// kept as an empty piece of the value and re-emitted as a bare `#`.
    #[test]
    fn an_empty_comment_line_inside_a_block_is_kept_as_an_empty_piece() {
        let text = "# first\n#\n# third\nUUID=aaaa /mnt/a ext4 defaults 0 2\n";
        let doc = parse(text);

        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("first\n\nthird")
        );
        assert_eq!(doc.to_string(), text);
    }

    #[test]
    fn two_entries_keep_their_own_independent_descriptions() {
        let original = "# first entry's own note\n\
                        UUID=aaaa /mnt/a ext4 defaults 0 2\n\
                        # second entry's own note\n\
                        UUID=bbbb /mnt/b ext4 defaults 0 2\n";
        let doc = parse(original);

        assert_eq!(
            descriptions(&doc),
            vec![
                Some("first entry's own note"),
                Some("second entry's own note")
            ]
        );
        assert_eq!(doc.to_string(), original);
    }

    #[test]
    fn document_with_a_description_round_trips_to_a_stable_fixed_point() {
        let original = "# scratch space for builds\nLABEL=scratch /mnt/scratch xfs defaults 0 2\n";
        let once = parse(original);
        let serialized_once = once.to_string();
        assert_eq!(serialized_once, original);

        let twice = parse(&serialized_once);
        assert_eq!(once, twice);
        assert_eq!(twice.to_string(), serialized_once);
    }

    /// A commented-out fstab line directly above an entry *is* now that
    /// entry's description - there is no text-based carve-out left (the old
    /// `//server/share`-versus-marker collision class is gone with the marker
    /// itself). What still matters is that its text survives verbatim through
    /// a round trip, which the trimmed-and-re-emitted block gives us.
    #[test]
    fn a_commented_out_cifs_line_above_an_entry_becomes_its_description_verbatim() {
        let original = "# //nas/media /mnt/media cifs guest,noauto 0 0\n\
                        UUID=1111 /mnt/data ext4 defaults 0 2\n";
        let doc = parse(original);

        assert_eq!(
            only_entry(&doc).description.as_deref(),
            Some("//nas/media /mnt/media cifs guest,noauto 0 0")
        );
        assert_eq!(doc.to_string(), original);
    }

    /// The plan's worked example, verbatim, as the rule's test oracle: every
    /// entry's expected description is spelled out in `docs`/PROJECT_PLAN.md
    /// §T43 and reproduced here exactly.
    #[test]
    fn the_worked_example_from_the_plan_maps_to_the_expected_descriptions() {
        let text = "\
# /dev/sda5
UUID=2f9d47f2-0000-0000-0000-000000000000 / ext4 rw,relatime 0 1

# /dev/sda2


UUID=CAF7-75BB /boot vfat rw,relatime 0 2

# /dev/sda4
UUID=dd525299-0000-0000-0000-000000000000 /root ext4 rw,relatime 0 2

# /dev/sda1
UUID=CA9D-4EC5 /efi vfat rw,relatime 0 2

# /dev/sda3

# /dev/sdb1
UUID=5e94cda0-0000-0000-0000-000000000000 /run/media/tomsk/Data ext4 defaults 0 0

/run/media/tomsk/Data/opt/ /opt none bind 0 0
";
        let doc = parse(text);

        assert_eq!(
            descriptions(&doc),
            vec![
                Some("/dev/sda5"),
                Some("/dev/sda2"),
                Some("/dev/sda4"),
                Some("/dev/sda1"),
                Some("/dev/sda3\n/dev/sdb1"),
                None,
            ]
        );

        // The serialization this implies, hand-traced line by line against
        // the rule: every comment line comes back directly above its own
        // entry, and every blank line comes back too - the ones that were
        // inside an absorbed run simply sit above that entry's block now.
        // Note the preserved line count: a parse-and-write-back of this file
        // changes where two blanks sit, and nothing else.
        let serialized = doc.to_string();
        assert_eq!(
            serialized,
            "\
# /dev/sda5
UUID=2f9d47f2-0000-0000-0000-000000000000 / ext4 rw,relatime 0 1



# /dev/sda2
UUID=CAF7-75BB /boot vfat rw,relatime 0 2

# /dev/sda4
UUID=dd525299-0000-0000-0000-000000000000 /root ext4 rw,relatime 0 2

# /dev/sda1
UUID=CA9D-4EC5 /efi vfat rw,relatime 0 2


# /dev/sda3
# /dev/sdb1
UUID=5e94cda0-0000-0000-0000-000000000000 /run/media/tomsk/Data ext4 defaults 0 0

/run/media/tomsk/Data/opt/ /opt none bind 0 0
"
        );
        assert_eq!(serialized.lines().count(), text.lines().count());

        // And it is a fixed point from there on.
        let twice = parse(&serialized);
        assert_eq!(descriptions(&twice), descriptions(&doc));
        assert_eq!(twice.to_string(), serialized);
    }

    #[test]
    fn a_document_whose_comments_are_all_shielded_is_byte_for_byte_unaffected() {
        // Absorption must not fire where the rule says it cannot: a comment
        // block above a malformed line, a trailing block at end of file, and
        // a lone separator blank above an entry are all left alone.
        let original = "UUID=aaaa /mnt/a ext4 defaults 0 2 # inline note\n\
                        \n\
                        #a note about the broken line below\n\
                        /dev/fake1\n\
                        \n\
                        LABEL=home /home ext4 defaults 0 2\n\
                        #trailing note at end of file\n";
        let doc = parse(original);

        assert!(doc.entries().all(|entry| entry.description.is_none()));
        assert_eq!(doc.lines.len(), original.lines().count());
        assert_eq!(doc.to_string(), original);
    }
}
