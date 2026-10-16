//! The in-memory "candidate document" — T11's central state design.
//!
//! `AppShell` owns one `CandidateDocument`, seeded from `list_entries`'s
//! `DocumentDto` on load. Every staged add/edit/delete mutates it directly
//! in memory; nothing here ever calls a write command (`save` is T12's
//! job). `EntriesTable` renders from it, `EntryDetailDrawer` stages into
//! it, and `validate_candidate` is called against its full
//! `to_document()` so duplicate-mount-point-style checks see the whole
//! file, not just the one entry being edited.
//!
//! Each line remembers the index it had in the *original* `list_entries`
//! response (`original_index`), or `None` if it was added this session.
//! That is deliberately enough information for T12 to compute the
//! Added/Modified/Removed grouping its diff view needs — by comparing each
//! candidate line's `original_index`/content against the original
//! document's lines — without this task having to build that comparison
//! itself:
//! - `original_index: None` → Added.
//! - `original_index: Some(i)` and `line != original.lines[i]` → Modified.
//! - an original index present in `original.lines` but absent from every
//!   candidate line's `original_index` → Removed.
//!
//! Comments and blank lines round-trip untouched (never surfaced as table
//! rows, never dropped — `docs/UI_DESIGN.md` §3) because they are seeded
//! the same way as entry/malformed lines and nothing in this task's UI
//! offers a way to touch them.

use crate::dto::{DocumentDto, EntryDto, LineDto};

/// One line of the candidate document, tagged with where (if anywhere) it
/// came from in the originally loaded document.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateLine {
    pub original_index: Option<usize>,
    pub line: LineDto,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandidateDocument {
    pub lines: Vec<CandidateLine>,
}

impl CandidateDocument {
    /// Seeds a candidate from a freshly loaded `DocumentDto`: every line
    /// maps 1:1 to its own index, in order.
    pub fn from_document(document: &DocumentDto) -> Self {
        Self {
            lines: document
                .lines
                .iter()
                .enumerate()
                .map(|(i, line)| CandidateLine {
                    original_index: Some(i),
                    line: line.clone(),
                })
                .collect(),
        }
    }

    /// Renders the candidate back to a `DocumentDto`, in its current
    /// order — used by T12's `save` and `render_candidate` calls.
    pub fn to_document(&self) -> DocumentDto {
        DocumentDto {
            lines: self.lines.iter().map(|cl| cl.line.clone()).collect(),
        }
    }

    /// Every candidate line except the one at `lines[index]`, as a plain
    /// line list — used to build the "rest of the document" context a
    /// drawer edit/add is validated against (see `EntryDetailDrawer`'s
    /// `other_lines` prop).
    pub fn lines_excluding(&self, index: Option<usize>) -> Vec<LineDto> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != index)
            .map(|(_, cl)| cl.line.clone())
            .collect()
    }

    /// Stages a new entry, appended at the end of the document.
    pub fn stage_add(&mut self, entry: EntryDto) {
        self.lines.push(CandidateLine {
            original_index: None,
            line: LineDto::Entry { entry },
        });
    }

    /// Stages an edit of the structured entry at `index`, in place.
    pub fn stage_edit_entry(&mut self, index: usize, entry: EntryDto) {
        if let Some(candidate_line) = self.lines.get_mut(index) {
            candidate_line.line = LineDto::Entry { entry };
        }
    }

    /// Stages an edit of the raw text of a malformed line at `index`, in
    /// place. The line stays `Malformed` — the frontend has no parser of
    /// its own to promote it to a structured entry (API_CONTRACT.md §3.2).
    pub fn stage_edit_raw(&mut self, index: usize, raw: String) {
        if let Some(candidate_line) = self.lines.get_mut(index) {
            candidate_line.line = LineDto::Malformed { raw };
        }
    }

    /// Stages a removal of the line at `index`.
    pub fn stage_delete(&mut self, index: usize) {
        if index < self.lines.len() {
            self.lines.remove(index);
        }
    }

    /// Row-kind (entry/malformed) candidate lines only, each paired with
    /// its index into `self.lines` — the identity `EntriesTable` reports
    /// back on edit/delete. Comments/blanks are preserved in `lines` but
    /// intentionally excluded from this view (`docs/UI_DESIGN.md` §3).
    pub fn rows(&self) -> Vec<(usize, &CandidateLine)> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, cl)| matches!(cl.line, LineDto::Entry { .. } | LineDto::Malformed { .. }))
            .collect()
    }

    /// Count of staged changes, for `PendingChangesBar`'s "N entries
    /// changed" (`docs/UI_DESIGN.md` §6): added + modified + removed
    /// row-kind lines, compared against the originally loaded document.
    pub fn pending_count(&self, original: &DocumentDto) -> usize {
        let is_row =
            |line: &LineDto| matches!(line, LineDto::Entry { .. } | LineDto::Malformed { .. });

        let added_or_modified = self
            .lines
            .iter()
            .filter(|cl| {
                if !is_row(&cl.line) {
                    return false;
                }
                match cl.original_index {
                    None => true,
                    Some(i) => original.lines.get(i) != Some(&cl.line),
                }
            })
            .count();

        let still_present: Vec<usize> = self
            .lines
            .iter()
            .filter_map(|cl| cl.original_index)
            .collect();
        let removed = original
            .lines
            .iter()
            .enumerate()
            .filter(|(i, line)| is_row(line) && !still_present.contains(i))
            .count();

        added_or_modified + removed
    }
}
