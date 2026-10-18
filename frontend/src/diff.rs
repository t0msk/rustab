//! Pure diff-computation logic for `DiffView` (`docs/UI_DESIGN.md` §6).
//!
//! Two independent pieces, each feeding one of the view's two tabs:
//!
//! - [`compute_summary_diff`] groups the candidate document against the
//!   originally loaded one into Added/Modified/Reordered/Removed, using
//!   *exactly* the algorithm `candidate.rs`'s module doc comment specifies
//!   (same `original_index` comparison `CandidateDocument::pending_count`
//!   already uses to produce a count - this produces the actual grouped
//!   lines - plus `CandidateDocument::position_changes` for the positional
//!   `Reordered` category, again shared with `pending_count`).
//! - [`compute_raw_diff`] is a classic line-level unified diff between two
//!   whole-file texts (the original file's raw text and `render_candidate`'s
//!   output), via the `similar` crate.
//!
//! Kept free of any Yew/Tauri dependency so it can be unit-tested directly.

use std::collections::{HashMap, HashSet};

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
    /// Set when this entry *also* changed position: §13.7's precedence rule
    /// reports such an entry once, here, as one extra note line on its
    /// Modified card - never a second time under `Reordered`.
    pub position_note: Option<String>,
}

/// A line whose content is byte-identical to its original but whose
/// position among the still-present entries changed (`docs/UI_DESIGN.md`
/// §13.7). Positions are 1-based, as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct ReorderedLine {
    /// Whole-row summary of the line, same rendering as Added/Removed -
    /// there are no field-level rows, since nothing about the content
    /// changed.
    pub summary: String,
    pub old_position: usize,
    pub new_position: usize,
}

/// The Summary tab's Added/Modified/Reordered/Removed grouping.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryDiff {
    pub added: Vec<String>,
    pub modified: Vec<ModifiedLine>,
    pub reordered: Vec<ReorderedLine>,
    pub removed: Vec<String>,
}

fn is_row(line: &LineDto) -> bool {
    matches!(line, LineDto::Entry { .. } | LineDto::Malformed { .. })
}

/// The one sentence both a `Reordered` card and a `Modified` card's
/// position note use (`docs/UI_DESIGN.md` §6/§13.7), from 1-based
/// positions - written once here so the two cards can't word it
/// differently.
pub fn position_sentence(old_position: usize, new_position: usize) -> String {
    format!("Moved from position {old_position} to position {new_position}")
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
    let old_description = old.description.clone().unwrap_or_default();
    let new_description = new.description.clone().unwrap_or_default();
    if old_description != new_description {
        fields.push(ChangedField {
            label: "description".to_string(),
            old: old_description,
            new: new_description,
        });
    }

    fields
}

fn build_modified_line(
    original_line: &LineDto,
    new_line: &LineDto,
    position_note: Option<String>,
) -> ModifiedLine {
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
        position_note,
    }
}

/// Groups `candidate`'s lines against `original` into Added/Modified/
/// Reordered/Removed, per `candidate.rs`'s module doc comment:
/// - `original_index: None` → Added.
/// - `original_index: Some(i)` and `line != original.lines[i]` → Modified.
/// - an original index present in `original.lines` but absent from every
///   candidate line's `original_index` → Removed.
/// - a still-present line whose content is unchanged but whose rank among
///   the still-present lines differs → Reordered (`docs/UI_DESIGN.md`
///   §13.7); a line that is *also* Modified is reported only there, with
///   the position appended as a note (that section's precedence rule).
///
/// The positional half comes from `CandidateDocument::position_changes`,
/// the same shared implementation `pending_count` uses, so the diff view
/// and `PendingChangesBar`'s count can never disagree.
///
/// Comments and blank lines are excluded from all four groups, matching
/// `CandidateDocument::pending_count` and `docs/UI_DESIGN.md` §3 (they are
/// never given their own row in any line-oriented view).
pub fn compute_summary_diff(original: &DocumentDto, candidate: &CandidateDocument) -> SummaryDiff {
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut reordered = Vec::new();

    let position_changes: HashMap<usize, (usize, usize)> = candidate
        .position_changes(original)
        .into_iter()
        .map(|change| (change.original_index, (change.old_rank, change.new_rank)))
        .collect();

    for candidate_line in &candidate.lines {
        if !is_row(&candidate_line.line) {
            continue;
        }
        match candidate_line.original_index {
            None => added.push(format_line_summary(&candidate_line.line)),
            Some(i) => {
                if let Some(original_line) = original.lines.get(i) {
                    let moved = position_changes.get(&i).copied();
                    if original_line != &candidate_line.line {
                        let note = moved.map(|(old_rank, new_rank)| {
                            position_sentence(old_rank + 1, new_rank + 1)
                        });
                        modified.push(build_modified_line(
                            original_line,
                            &candidate_line.line,
                            note,
                        ));
                    } else if let Some((old_rank, new_rank)) = moved {
                        reordered.push(ReorderedLine {
                            summary: format_line_summary(&candidate_line.line),
                            old_position: old_rank + 1,
                            new_position: new_rank + 1,
                        });
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
        reordered,
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
                description: None,
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
    fn a_pure_reorder_is_reported_under_reordered_with_one_based_positions() {
        let original = DocumentDto {
            lines: vec![
                entry_line("/data", &["defaults"]),
                entry_line("/srv", &["defaults"]),
            ],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(0, 1);

        let diff = compute_summary_diff(&original, &candidate);
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.reordered.len(), 2);
        // Listed in current-document order: /srv is now first.
        assert_eq!(
            diff.reordered[0].summary,
            format_line_summary(&entry_line("/srv", &["defaults"]))
        );
        assert_eq!(diff.reordered[0].old_position, 2);
        assert_eq!(diff.reordered[0].new_position, 1);
        assert_eq!(diff.reordered[1].old_position, 1);
        assert_eq!(diff.reordered[1].new_position, 2);
    }

    #[test]
    fn a_moved_and_edited_entry_is_reported_once_under_modified_with_a_position_note() {
        // §13.7's precedence rule - never listed under both groups.
        let original = DocumentDto {
            lines: vec![
                entry_line("/data", &["defaults"]),
                entry_line("/srv", &["defaults"]),
            ],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(0, 1);
        candidate.stage_edit_entry(
            1,
            match entry_line("/data", &["defaults", "noatime"]) {
                LineDto::Entry { entry } => entry,
                _ => unreachable!(),
            },
        );

        let diff = compute_summary_diff(&original, &candidate);
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.modified[0].changed_fields.len(), 1);
        assert_eq!(
            diff.modified[0].position_note.as_deref(),
            Some("Moved from position 1 to position 2")
        );
        // The *other* line only moved, so it is the single Reordered card.
        assert_eq!(diff.reordered.len(), 1);
        assert_eq!(
            diff.reordered[0].summary,
            format_line_summary(&entry_line("/srv", &["defaults"]))
        );
    }

    #[test]
    fn a_modified_entry_that_stayed_put_has_no_position_note() {
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
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.modified[0].position_note, None);
        assert!(diff.reordered.is_empty());
    }

    #[test]
    fn a_reorder_alongside_an_addition_and_a_deletion_still_ranks_correctly() {
        // §13.7 step 2: removed lines drop out of both rankings and added
        // lines never enter them, so only the real move is reported.
        let original = DocumentDto {
            lines: vec![
                entry_line("/a", &["defaults"]),
                entry_line("/b", &["defaults"]),
                entry_line("/c", &["defaults"]),
            ],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_delete(0); // /b and /c keep their relative order
        candidate.stage_add(match entry_line("/new", &["defaults"]) {
            LineDto::Entry { entry } => entry,
            _ => unreachable!(),
        });
        let _ = candidate.stage_move(0, 1); // swap the two survivors

        let diff = compute_summary_diff(&original, &candidate);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.modified.is_empty());
        assert_eq!(diff.reordered.len(), 2);
        assert_eq!(
            diff.reordered[0].summary,
            format_line_summary(&entry_line("/c", &["defaults"]))
        );
        assert_eq!(
            (
                diff.reordered[0].old_position,
                diff.reordered[0].new_position
            ),
            (2, 1)
        );
        assert_eq!(
            (
                diff.reordered[1].old_position,
                diff.reordered[1].new_position
            ),
            (1, 2)
        );
    }

    #[test]
    fn comments_and_blanks_keeping_their_place_do_not_shift_entry_ranks() {
        // A freestanding comment sits between the two entries and stays
        // put when they swap (§13.4) - it is not part of the ranking.
        let original = DocumentDto {
            lines: vec![
                entry_line("/a", &["defaults"]),
                LineDto::Comment {
                    text: " Network mounts".to_string(),
                },
                entry_line("/b", &["defaults"]),
            ],
        };
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(2, 0);

        let diff = compute_summary_diff(&original, &candidate);
        assert_eq!(diff.reordered.len(), 2);
        assert!(diff.added.is_empty() && diff.modified.is_empty() && diff.removed.is_empty());
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
