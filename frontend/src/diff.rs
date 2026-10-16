//! Pure diff-computation logic for `DiffView` (`docs/UI_DESIGN.md` §6).
//!
//! Two independent pieces, each feeding one of the view's two tabs:
//!
//! - [`compute_summary_diff`] groups the candidate document against the
//!   originally loaded one into Added/Modified/Removed, using *exactly* the
//!   algorithm `candidate.rs`'s module doc comment specifies (same
//!   `original_index` comparison `CandidateDocument::pending_count` already
//!   uses to produce a count - this produces the actual grouped lines).
//! - [`compute_raw_diff`] is a classic line-level unified diff between two
//!   whole-file texts (the original file's raw text and `render_candidate`'s
//!   output), via the `similar` crate.
//!
//! Kept free of any Yew/Tauri dependency so it can be unit-tested directly.

use std::collections::HashSet;

use similar::{ChangeTag, TextDiff};

use crate::candidate::CandidateDocument;
use crate::dto::{DocumentDto, EntryDto, LineDto, OptionDto, SourceDto, SourceKind};

/// One field that differs between the original and candidate version of a
/// modified line, as the Summary tab's `label: ~~old~~ → **new**` rows need
/// (`docs/UI_DESIGN.md` §6: "listing only the fields that actually
/// changed").
#[derive(Clone, Debug, PartialEq)]
pub struct ChangedField {
    pub label: String,
    pub old: String,
    pub new: String,
}

/// A line present in both documents at the same `original_index`, but with
/// different content.
#[derive(Clone, Debug, PartialEq)]
pub struct ModifiedLine {
    /// One-line summary of the line's *new* content, shown as the card's
    /// heading.
    pub summary: String,
    pub changed_fields: Vec<ChangedField>,
}

/// The Summary tab's Added/Modified/Removed grouping.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryDiff {
    pub added: Vec<String>,
    pub modified: Vec<ModifiedLine>,
    pub removed: Vec<String>,
}

fn is_row(line: &LineDto) -> bool {
    matches!(line, LineDto::Entry { .. } | LineDto::Malformed { .. })
}

fn format_source(source: &SourceDto) -> String {
    match source.kind {
        SourceKind::Path | SourceKind::Network | SourceKind::Other => source.value.clone(),
        SourceKind::None => "none".to_string(),
        SourceKind::Uuid => format!("UUID={}", source.value),
        SourceKind::Label => format!("LABEL={}", source.value),
        SourceKind::PartUuid => format!("PARTUUID={}", source.value),
        SourceKind::PartLabel => format!("PARTLABEL={}", source.value),
    }
}

fn format_option(option: &OptionDto) -> String {
    match &option.value {
        Some(value) => format!("{}={}", option.key, value),
        None => option.key.clone(),
    }
}

fn format_options(options: &[OptionDto]) -> String {
    options
        .iter()
        .map(format_option)
        .collect::<Vec<_>>()
        .join(",")
}

fn format_entry_summary(entry: &EntryDto) -> String {
    format!(
        "{}  {}  {}  {}  {}  {}",
        format_source(&entry.source),
        entry.mount_point,
        entry.fs_type,
        format_options(&entry.options),
        entry.dump,
        entry.pass
    )
}

/// One-line, human-readable rendering of a row-kind line, for the
/// Added/Removed whole-row summary cards (`docs/UI_DESIGN.md` §6).
pub fn format_line_summary(line: &LineDto) -> String {
    match line {
        LineDto::Entry { entry } => format_entry_summary(entry),
        LineDto::Malformed { raw } => raw.clone(),
        LineDto::Comment { text } => format!("#{text}"),
        LineDto::Blank => String::new(),
    }
}

fn diff_entry_fields(old: &EntryDto, new: &EntryDto) -> Vec<ChangedField> {
    let mut fields = Vec::new();

    let old_source = format_source(&old.source);
    let new_source = format_source(&new.source);
    if old_source != new_source {
        fields.push(ChangedField {
            label: "source".to_string(),
            old: old_source,
            new: new_source,
        });
    }
    if old.mount_point != new.mount_point {
        fields.push(ChangedField {
            label: "mount point".to_string(),
            old: old.mount_point.clone(),
            new: new.mount_point.clone(),
        });
    }
    if old.fs_type != new.fs_type {
        fields.push(ChangedField {
            label: "fs type".to_string(),
            old: old.fs_type.clone(),
            new: new.fs_type.clone(),
        });
    }
    let old_options = format_options(&old.options);
    let new_options = format_options(&new.options);
    if old_options != new_options {
        fields.push(ChangedField {
            label: "options".to_string(),
            old: old_options,
            new: new_options,
        });
    }
    if old.dump != new.dump {
        fields.push(ChangedField {
            label: "dump".to_string(),
            old: old.dump.to_string(),
            new: new.dump.to_string(),
        });
    }
    if old.pass != new.pass {
        fields.push(ChangedField {
            label: "pass".to_string(),
            old: old.pass.to_string(),
            new: new.pass.to_string(),
        });
    }
    let old_comment = old.trailing_comment.clone().unwrap_or_default();
    let new_comment = new.trailing_comment.clone().unwrap_or_default();
    if old_comment != new_comment {
        fields.push(ChangedField {
            label: "comment".to_string(),
            old: old_comment,
            new: new_comment,
        });
    }

    fields
}

fn build_modified_line(original_line: &LineDto, new_line: &LineDto) -> ModifiedLine {
    let changed_fields = match (original_line, new_line) {
        (LineDto::Entry { entry: old }, LineDto::Entry { entry: new }) => {
            diff_entry_fields(old, new)
        }
        (LineDto::Malformed { raw: old }, LineDto::Malformed { raw: new }) if old != new => {
            vec![ChangedField {
                label: "raw text".to_string(),
                old: old.clone(),
                new: new.clone(),
            }]
        }
        (LineDto::Malformed { .. }, LineDto::Malformed { .. }) => Vec::new(),
        // Not expected in practice (the drawer never changes a line's
        // kind - see `app_shell.rs`'s `on_edit_entry`), but handled rather
        // than panicking: show the whole line as one changed field.
        _ => vec![ChangedField {
            label: "line".to_string(),
            old: format_line_summary(original_line),
            new: format_line_summary(new_line),
        }],
    };
    ModifiedLine {
        summary: format_line_summary(new_line),
        changed_fields,
    }
}

/// Groups `candidate`'s lines against `original` into Added/Modified/
/// Removed, per `candidate.rs`'s module doc comment:
/// - `original_index: None` → Added.
/// - `original_index: Some(i)` and `line != original.lines[i]` → Modified.
/// - an original index present in `original.lines` but absent from every
///   candidate line's `original_index` → Removed.
///
/// Comments and blank lines are excluded from all three groups, matching
/// `CandidateDocument::pending_count` and `docs/UI_DESIGN.md` §3 (they are
/// never given their own row in any line-oriented view).
pub fn compute_summary_diff(original: &DocumentDto, candidate: &CandidateDocument) -> SummaryDiff {
    let mut added = Vec::new();
    let mut modified = Vec::new();

    for candidate_line in &candidate.lines {
        if !is_row(&candidate_line.line) {
            continue;
        }
        match candidate_line.original_index {
            None => added.push(format_line_summary(&candidate_line.line)),
            Some(i) => {
                if let Some(original_line) = original.lines.get(i) {
                    if original_line != &candidate_line.line {
                        modified.push(build_modified_line(original_line, &candidate_line.line));
                    }
                }
            }
        }
    }

    let still_present: HashSet<usize> = candidate
        .lines
        .iter()
        .filter_map(|candidate_line| candidate_line.original_index)
        .collect();
    let removed = original
        .lines
        .iter()
        .enumerate()
        .filter(|(i, line)| is_row(line) && !still_present.contains(i))
        .map(|(_, line)| format_line_summary(line))
        .collect();

    SummaryDiff {
        added,
        modified,
        removed,
    }
}

/// One line of a classic unified diff, already tagged for rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct RawDiffLine {
    /// `"-"`, `"+"` or `" "` (context).
    pub marker: &'static str,
    pub text: String,
}

/// Line-level unified diff of `old_text` (the original file's exact text)
/// against `new_text` (`render_candidate`'s output) - the Raw diff tab's
/// "exact textual before/after of the real file content"
/// (`docs/UI_DESIGN.md` §6).
pub fn compute_raw_diff(old_text: &str, new_text: &str) -> Vec<RawDiffLine> {
    TextDiff::from_lines(old_text, new_text)
        .iter_all_changes()
        .map(|change| {
            let marker = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            RawDiffLine {
                marker,
                text: change.value().trim_end_matches('\n').to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{LineDto, SourceDto, SourceKind};

    fn entry_line(mount_point: &str, options: &[&str]) -> LineDto {
        LineDto::Entry {
            entry: EntryDto {
                source: SourceDto {
                    kind: SourceKind::Uuid,
                    value: "1111".to_string(),
                },
                mount_point: mount_point.to_string(),
                fs_type: "ext4".to_string(),
                options: options
                    .iter()
                    .map(|o| OptionDto {
                        key: o.to_string(),
                        value: None,
                    })
                    .collect(),
                dump: 0,
                pass: 2,
                trailing_comment: None,
            },
        }
    }

    #[test]
    fn added_line_has_no_original_index() {
        let original = DocumentDto {
            lines: vec![entry_line("/data", &["defaults"])],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_add(match entry_line("/new", &["defaults"]) {
            LineDto::Entry { entry } => entry,
            _ => unreachable!(),
        });

        let diff = compute_summary_diff(&original, &candidate);
        assert_eq!(
            diff.added,
            vec![format_line_summary(&entry_line("/new", &["defaults"]))]
        );
        assert!(diff.modified.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn modified_line_reports_only_changed_fields() {
        let original = DocumentDto {
            lines: vec![entry_line("/data", &["defaults"])],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_edit_entry(
            0,
            match entry_line("/data", &["defaults", "noatime"]) {
                LineDto::Entry { entry } => entry,
                _ => unreachable!(),
            },
        );

        let diff = compute_summary_diff(&original, &candidate);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.modified[0].changed_fields.len(), 1);
        assert_eq!(diff.modified[0].changed_fields[0].label, "options");
        assert_eq!(diff.modified[0].changed_fields[0].old, "defaults");
        assert_eq!(diff.modified[0].changed_fields[0].new, "defaults,noatime");
    }

    #[test]
    fn removed_line_absent_from_every_candidate_original_index() {
        let original = DocumentDto {
            lines: vec![
                entry_line("/data", &["defaults"]),
                entry_line("/mnt/broken", &["defaults"]),
            ],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_delete(1);

        let diff = compute_summary_diff(&original, &candidate);
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert_eq!(
            diff.removed,
            vec![format_line_summary(&entry_line(
                "/mnt/broken",
                &["defaults"]
            ))]
        );
    }

    #[test]
    fn add_then_delete_of_a_fresh_entry_is_not_reported_as_removed() {
        // Mirrors `candidate.rs`'s `pending_count` doc comment: an
        // add-then-delete of the same entry nets to zero, not "removed"
        // (removed only ever applies to lines that existed in `original`).
        let original = DocumentDto { lines: vec![] };
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_add(match entry_line("/new", &["defaults"]) {
            LineDto::Entry { entry } => entry,
            _ => unreachable!(),
        });
        candidate.stage_delete(0);

        let diff = compute_summary_diff(&original, &candidate);
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn comments_and_blanks_are_excluded_from_every_group() {
        let original = DocumentDto {
            lines: vec![
                LineDto::Comment {
                    text: " hi".to_string(),
                },
                LineDto::Blank,
            ],
        };
        let candidate = CandidateDocument::from_document(&original);

        let diff = compute_summary_diff(&original, &candidate);
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn raw_diff_marks_added_and_removed_lines() {
        let old = "a\nb\nc\n";
        let new = "a\nx\nc\n";
        let lines = compute_raw_diff(old, new);
        let markers: Vec<&str> = lines.iter().map(|l| l.marker).collect();
        assert_eq!(markers, vec![" ", "-", "+", " "]);
        assert_eq!(lines[1].text, "b");
        assert_eq!(lines[2].text, "x");
    }
}
