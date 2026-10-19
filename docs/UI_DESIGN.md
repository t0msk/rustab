# Rustab - UI/UX Design Spec

This document is the implementation contract for the Rustab frontend (T10/T11/T12).
It covers every GUI requirement in SPEC §9 by name and gives concrete, Tailwind-based
values - colors, spacing, typography, component states - so a frontend developer does
not need to invent any structural or visual decision. Where a decision required
inventing something outside this document's remit (brand identity, backend behavior),
it is explicitly flagged rather than guessed.

This is a design spec, not code: no Yew/Rust is written here. Tailwind utility class
names are given as concrete design tokens (per the brief's instruction to be
implementable, not a mood board), not as a literal implementation.

**Priority reminder (SPEC §2):** nothing in this document may obscure, shortcut, or
let the user bypass the diff-before-save step or the "which file does a save actually
write" target indicator. Every flow below treats those two as hard gates, not optional
affordances.

---

## 1. Overall layout

Single-window desktop app, no page routing - view switching is in-memory state
(matches T10's "routing/view-state skeleton" using Yew state, not a router).

```
┌─────────────────────────────────────────────────────────┐
│ TargetBanner  (always on top, full width, never hidden)  │  <- safety-critical, §2
├─────────────────────────────────────────────────────────┤
│ HeaderBar: "Rustab" · [Entries] [Backups] tabs · Add Entry│
│ + Raw view (Entries tab) / Create backup + Open backup     │
│ location (Backups tab) · always-visible About trigger      │
│ (far right)                                                │
├─────────────────────────────────────────────────────────┤
│ PendingChangesBar (only visible once ≥1 staged change)   │
├─────────────────────────────────────────────────────────┤
│                                                           │
│   Main content area:                                     │
│   - "Entries" tab  → EntriesTable                        │
│   - "Backups" tab  → BackupPanel                         │
│                                                           │
└─────────────────────────────────────────────────────────┘
        ↕ (overlay, slides in from the right)
   EntryDetailDrawer (add/edit)
        ↕ (centered modal overlay, blocking)
   DiffView / DeleteConfirmDialog / BackupRestoreConfirmDialog
        ↕ (centered modal overlay, dismissible by backdrop click)
   BackupDiffView / AboutDialog / RawViewDialog - read-only, so unlike the
   overlays above these three are dismissible by clicking the backdrop
```

**Layout decisions and justification:**

- **TargetBanner is its own fixed region above the header**, not folded into the
  header bar. It must survive scrolling, tab switching, and drawer/modal overlays.
  Nothing else in the layout is allowed to render above it in z-order.
- **Add/edit uses a right-side slide-in drawer, not a centered modal.** Justification:
  the entry form has many fields (source type + device picker, mount point + directory
  picker, fstype, an options tag-input that can grow tall, dump/pass) and benefits from
  real vertical room; a drawer keeps the EntriesTable and TargetBanner visible behind a
  dimmed backdrop so the user retains context of what they're editing relative to the
  rest of the file, and matches the drawer pattern already implied by T10's "add/edit
  modal/panel shell" wording. Delete, by contrast, is a single irreversible action with
  no fields to show, so it uses a small centered **DeleteConfirmDialog**, not the drawer.
- **The diff/confirm step is a centered modal (DiffView)**, deliberately *not* a drawer
  and *not* dismissible by clicking the backdrop - it is the final safety gate before a
  write (SPEC §9 "a clear pending changes / diff view before committing a save"), so it
  behaves like a blocking confirmation dialog, not a casual panel.
- **Backups get a top-level tab, not a side panel**, because restoring a backup is a
  distinct, infrequent workflow that deserves full screen width for its own list and
  confirm flow, not a cramped sidebar.
- Minimum recommended window size: 960×640px. Below ~760px width, the EntriesTable
  collapses secondary columns (Dump/Pass fold into a "..." overflow per row) and the
  EntryDetailDrawer becomes full-width instead of a fixed 520px panel.

---

## 2. Target-file indicator (SPEC §9: "a visible indicator of which file ... is
currently targeted - this must never be ambiguous to the user")

Component: **TargetBanner**. This is the single most safety-critical visual element in
the app and is designed to be *impossible to miss*, not a subtle label.

- **Position**: full-width, fixed at the very top of the window, above the HeaderBar.
  Height 40px. Never scrolls away, never covered by any overlay (highest z-index in the
  app, drawers/modals render below it or the banner re-renders on top).
- **Content**: an icon + bold lead clause + the literal resolved file path, e.g.
  `⚠ REAL SYSTEM FILE - /etc/fstab` or
  `⚠ SAVING WRITES THE REAL /ETC/FSTAB - editing working draft /home/user/.local/share/rustab/fstab.draft`.
  The full path is always shown in full (truncated with an ellipsis + tooltip only if
  genuinely too long for the window), never abbreviated to just a filename - ambiguity
  between two similarly-named files is exactly what this banner exists to prevent.
- **Three states since T34, only one of them alarming (revised in T35)** - before T34
  there were two states, because the only way to reach `/etc/fstab` was to *be* pointed
  at `/etc/fstab`. T34 broke that equivalence: a default, unconfigured launch edits a
  local working draft but every save from it writes the real `/etc/fstab` and then
  mirrors back into the draft (`docs/SPEC.md` §7,
  `src-tauri/src/commands.rs::save_document_with`). T34 initially answered that by making
  the middle column amber/hazard-striped/alarmed, on the theory that the banner should
  also signal *what a future save will do*. T35 reverted that call, at the repo owner's
  explicit request (`PROJECT_PLAN.md`'s T35 section): the ambient banner now reports only
  *what file you are currently looking at* - a calm fact, true for the whole time you are
  editing - not *what saving will do*, which is a separate, future, one-time event with
  its own dedicated risk signal elsewhere (see below the table). Nothing here is the
  pre-T34 "Sandbox target" lie, though: the copy for the middle column still names the
  real file by path and does not claim it is untouched.

  | | **Real file** (`is_real`) | **Working draft, saves through to the real file** (`source == SandboxDefault`) | **Isolated test target** (`source == Environment`, not real) |
  |---|---|---|---|
  | When | explicit `RUSTAB_TARGET_PATH=/etc/fstab` | the default launch, no env var set | explicit `RUSTAB_TARGET_PATH=/some/other/file` |
  | Does Save write `/etc/fstab`? | yes | **yes** (then updates the draft to match) | no, never |
  | Background | `bg-red-600` (`#dc2626`) | `bg-emerald-600` (`#059669`) | `bg-emerald-600` (`#059669`) |
  | Text | `text-white`, `font-bold`, `uppercase`, `tracking-wide` | `text-white`, `font-semibold`, normal case | `text-white`, `font-semibold`, normal case |
  | Icon | triangle-exclamation (hazard), filled | flask/beaker (sandbox/test), outline | flask/beaker (sandbox/test), outline |
  | Extra texture | subtle 8px diagonal hazard-stripe pattern at ~8% white opacity across the banner background (`repeating-linear-gradient(135deg, rgba(255,255,255,.08) 0 8px, transparent 8px 16px)`) | none - flat color, calm | none - flat color, calm |
  | Border | `border-b-2 border-red-800` | `border-b-2 border-emerald-800` | `border-b-2 border-emerald-800` |
  | Copy | `"REAL SYSTEM FILE - {path}"` | `"Working draft - {path}"` | `"Test target - {path} · your real /etc/fstab is never written"` |
  | ARIA role | `alert` | `status` | `status` |

  Why the ambient banner is deliberately calm for the middle column, rather than keeping
  it visually distinct from the third column's genuinely-safe state: the two signals in
  play are *"what am I looking at"* and *"what happens if I save"*, and T34 made this
  banner try to carry both at once, permanently, for a state most users sit in most of
  the time. That duplicated a risk signal this app already has, placed at a better
  moment: `DiffView`'s Review Changes step and `BackupRestoreConfirmDialog`/
  `BackupDeleteConfirmDialog`'s confirmation dialogs each require an explicit
  acknowledgement checkbox - gated on the same `writes_real_file` predicate described
  below - at the one point a real write is actually about to happen, not for the entire
  session beforehand. An always-on, uppercase, striped alarm for a state that, by itself,
  has written nothing yet trains users to tune the banner out, which is worse for the
  state that still deserves full alarm (`RealFile`, unchanged by this revision). So the
  middle and third columns now share the same calm visual language; the path string
  itself, always shown in full, is what still lets a user tell the working draft apart
  from an isolated override by its text, same as before.

  The red/striped/uppercase/bold treatment for the real-file mode is still a dedicated
  design decision, not reused anywhere else in the app at this intensity - nowhere else
  should a full-width striped red banner appear, so the user's pattern-match of "this
  specific look = I am looking at the real system file" stays reliable.

  The third column remains the *only* state in the app allowed to say "your real
  `/etc/fstab` is never written", because it is the only state where that is true
  (`README.md`'s Safety Notes, `CONTRIBUTING.md`'s "Safety-first contribution notes").
  The middle column's copy must not say or imply that, and must not use the word
  "sandbox": that word now names a *file* (`fstab.draft`, the persistent working
  draft), not a safe *mode*, and reusing it as reassurance here is what made the
  pre-T34 copy wrong in the first place - "Working draft" names the file being edited
  without claiming anything about where a save ends up.
- **Data need (not a backend decision)**: TargetBanner needs `{ path: String, is_real:
  bool, source: "environment" | "sandbox_default" }` from the "get active target" call
  (`TargetInfoDto`, `docs/API_CONTRACT.md` §6). `source` is not redundant with
  `is_real`: the two non-real cases above are opposites in consequence and `is_real`
  alone cannot tell them apart. It re-renders live if those values ever change during
  the session (e.g. a future settings change) - never cached at startup only.
- The derived predicate **`writes_real_file = is_real || source == SandboxDefault`**
  ("will confirming write the real `/etc/fstab`?") is computed **once**, in `AppShell`,
  and is what color-codes and gates the confirm button in DiffView and
  BackupRestoreConfirmDialog (§6, §7) - so the "danger" signal is consistent end-to-end
  through the save flow, not just on the banner, and the formula cannot drift between
  call sites. Before T34 this role was played by raw `is_real`; anything still gating on
  `is_real` alone would silently skip the confirmation checkbox on the default launch,
  which is the one regression T34 could not ship with
  (`PROJECT_PLAN.md`'s T34 non-negotiables).

---

## 3. Entries table/list view (SPEC §9: "Table/list view of all current entries
... including ones Rustab doesn't 'understand' perfectly - show raw text with a
warning rather than dropping them")

Component: **EntriesTable**, rows are **EntryRow** (parsed) or **UnparsedEntryRow**
(malformed). Comments and blank lines are preserved for serialization but are not
rendered as table rows - the table only ever lists `Entry` and `Malformed` lines
(this is a UI display decision, not a change to backend round-trip behavior: comments
and blanks are never dropped, just not given their own row in this list view).

**Columns (EntryRow):**

| Column | Content |
|---|---|
| *(leading, narrow)* | A chevron (`ChevronRightIcon`), rotated 90° when the row is expanded - the discoverability hint for row expansion below. Header cell is `sr-only`-labeled, not blank-looking-blank. |
| Source | Small type badge (`UUID`, `LABEL`, `PARTUUID`, `PARTLABEL`, `PATH`, `NET`, `NONE`) + the value, `font-mono text-sm` |
| Mount point | `font-mono text-sm`, truncated with a hover tooltip; the full, untruncated value - plus copy/open-in-file-manager actions - lives in the row's own expanded detail panel below, not here |
| FS type | plain text badge, `text-sm` |
| Options | first ~3 options as small chips + `+N more` overflow; the full ordered list is visible on hover tooltip, in the expanded detail panel below, and always fully editable in the drawer |
| Dump | single digit |
| Pass | single digit |
| Actions | Move up / Move down (`ChevronUpIcon`/`ChevronDownIcon` buttons, T40 - full spec in §13) · Edit (`EditIcon` button) · Delete (`DeleteIcon` button, `text-red-600` on hover) |

Row styling: `bg-white dark:bg-slate-800`, alternating/zebra optional (`odd:bg-slate-50
dark:odd:bg-slate-800/60`), hover `hover:bg-sky-50 dark:hover:bg-slate-700/60`, row
height ~44px, `border-b border-slate-200 dark:border-slate-700`, `cursor-pointer` (see
click-to-expand below).

**Click-to-expand row detail:** clicking anywhere on an `EntryRow`'s own `<tr>` (except
the Edit/Delete buttons, which stop the click from propagating) toggles an inline detail
panel for that row, replacing the only-visible-in-the-drawer description gap called out
above - this is the deliberate, permanent way to see an entry's full data, including its
description, without opening Edit. Any number of rows can be expanded at once (not an
accordion); the chevron in the leading column rotates 90° to reflect each row's own
state.

The expanded panel is a full-width row, structurally like `UnparsedEntryRow`'s own
colspan pattern (a single `colspan={8}` `<td>`) but with its own, distinct visual
treatment so it reads as "an open detail view," not a warning: `bg-slate-50
dark:bg-slate-900/40` background, `border-l-4 border-l-sky-400 dark:border-l-sky-500`
left accent (the app's `accent` token, §8), laid out as a labeled two-column grid
(`text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide` labels
above each value, matching this table's own header typography). It shows, fully
untruncated: Source (value + type badge), Mount point (value + the two icon buttons
below), FS type, Dump/Pass, the complete Options chip list (no `+N more` truncation),
Description (only rendered if `Some`) and Trailing comment (only rendered if `Some`).

Immediately after the Mount point value in the expanded panel sit two icon buttons:

- **Copy to clipboard** (`CopyIcon`, swapping to `CheckIcon` for ~1.5s after a
  successful copy) - uses the Web Clipboard API (`navigator.clipboard.write_text`)
  directly, not a Tauri plugin. On failure, an error toast reads `"Could not copy the
  mount point to the clipboard."`.
- **Open in file manager** (`FolderOpenIcon`) - calls the `open_path_in_file_manager`
  command (`docs/API_CONTRACT.md` §3.14) with the entry's mount point. On failure, the
  command's own message is shown as an error toast; on success, nothing (the file
  manager window appearing is its own confirmation, the same pattern
  `open_backup_location` already uses).

**Document-wide warning indicator**: `AppShell` runs `validate_candidate` against the
*whole* candidate document independently of whether any drawer is open (debounced only
by how often the document itself changes - a staged add/edit/delete - not per
keystroke, since nothing is being typed at this level). Any row with ≥1 Warning-severity
finding tagged to its own `line_index` gets a left border accent, `border-l-4
border-l-amber-400 dark:border-l-amber-500` (same token `UnparsedEntryRow` already uses
for a different reason below), plus the full warning text as a native `title` tooltip -
a background tint was considered and deliberately rejected in favor of the border alone:
a full amber background reads as more alarming than a non-blocking warning deserves, and
would look noisy across several flagged rows at once; the border is just as scannable
without overstating severity. This is the same mechanism that powers the drawer's own
inline mount-point warning and flat warning stack (§5) - both read from the same
`validate_candidate` response, just scoped differently (the table scopes by row, the
drawer by "about this draft specifically").

**Reordering (T40)**: entries can be moved up/down within this table, staged into the
same in-memory candidate document as any other edit and feeding the exact same
document-wide validation described above. Full spec, including the Move up/down button
design, the freestanding-comment/blank-line landing rule, and a required correctness fix
to the diff/pending-count logic this introduces, is in §13 - not duplicated here.

**UnparsedEntryRow (malformed line):**

- Full-width single row (spans all columns), background `bg-amber-50 dark:bg-amber-900/20`,
  left border accent `border-l-4 border-amber-500`.
- Content: a warning icon (amber triangle) + label `"Couldn't fully parse this line -
  shown as raw text"` + the raw original line text verbatim in `font-mono text-sm`
  inside a `bg-slate-900/5 dark:bg-slate-950/40` inset block so it visually reads as
  "quoted raw text," not as a normal structured row.
- Still has Move up/Move down, Edit and Delete actions (§13). Edit opens the
  EntryDetailDrawer in a "raw text" sub-mode (a single multi-line text field containing
  the raw line, since structured fields cannot be offered for a line the parser couldn't
  decompose) rather than the normal structured form. Delete removes the line like any
  other row. The line is never silently hidden or auto-corrected - this satisfies
  SPEC's explicit "never dropped or hidden" requirement.
- `EmptyState` variant: if the table has zero entries (e.g. a truly empty fstab), show
  a centered placeholder: icon + `"No entries yet"` + a primary `Add Entry` button.

---

## 4. Add/edit/delete form (SPEC §9 structured inputs)

Component: **EntryDetailDrawer**, slides in from the right, fixed width 520px (full
width below the 760px breakpoint), `bg-white dark:bg-slate-800`, `shadow-lg`, backdrop
`bg-black/40` behind it. **Not** dismissible by clicking the backdrop (changed from the
original design, which allowed it as "non-destructive" - a misclick outside the drawer
was silently discarding real unsaved edits with no undo, per direct user report).
Closing is explicit only: the header's × button or FormActionBar's Cancel.

Field order, top to bottom, each with a `text-sm font-medium` label above it and
`space-y-4` (16px) between fields:

1. **SourceTypeSelect** - dropdown. Options, per SPEC §9's literal list (path / UUID /
   LABEL / PARTUUID / none) **extended** to also cover PARTLABEL and Network, since
   `fstab-core`'s `SourceId` model (T2) already defines those variants and the dropdown
   must be able to represent every value the backend can round-trip:
   `Device path` · `UUID` · `LABEL` · `PARTUUID` · `PARTLABEL` · `Network (NFS/CIFS)` ·
   `None`. *(Flagging this as a transparent completeness extension of the §9 wording,
   not a visual/brand judgment call.)*
2. **Source value field** - changes shape based on the selected type:
   - `Device path` / `Network`: free-text input + a `Browse…` button that opens
     **DevicePicker**.
   - `UUID` / `LABEL` / `PARTUUID` / `PARTLABEL`: a combobox pre-populated from
     device-discovery matches of that identifier type, with manual free-text entry
     always available as a fallback (SPEC §8: discovery tool absence must never block
     manual entry).
   - `None`: the field is disabled, shows literal placeholder text `none`, with helper
     text `"Used for pseudo-filesystems (tmpfs, proc) or swap entries without a
     discrete backing device."`
   - **DevicePicker** data need: a list of discovered devices, each conceptually
     `{ path, uuid, label, partuuid, partlabel, fstype }` - sourced from whatever
     discovery call T7/T8 exposes. If the list is empty (tools unavailable), DevicePicker
     shows `"No devices discovered - enter a value manually"` and the manual field stays
     usable; it is never a blocking state.
3. **Mount point** - text input (`font-mono`) + a `Browse…` button opening a native
   directory picker via `tauri-plugin-dialog`'s `open` command with `directory: true`,
   setting this field to the chosen path on success and leaving it untouched on
   cancel. While the source type is `None` and this field is still empty, it shows a
   `"none"` placeholder hint
   (the conventional real-fstab value for a swap entry's mount point, e.g.
   `/dev/sda2 none swap sw 0 0`) - a visual hint only, never written into the value
   unless the user actually types it.
4. **Filesystem type** - free-text combobox: typing shows a suggestion dropdown of
   common types (`ext4, btrfs, xfs, vfat, ntfs3, swap, none, nfs, nfs4, cifs, overlay,
   tmpfs, proc`) but any value can be typed and accepted - no closed enum, matching
   SPEC §5. No conditional placeholder here (unlike Mount point above) - `swap` is
   already one of the suggestion-dropdown values, so a placeholder hint would be
   redundant with (and visually confusable as) an already-entered value.
5. **OptionsTagInput** - the SPEC §9 "checklist/tag-input for options":
   - A row of toggle chips for the well-known options named in SPEC §5:
     `defaults · noatime · relatime · ro · rw · user · nofail · auto · noauto · bind · sw`.
   - A separate, visually grouped "systemd" row of toggle/value chips for the
     `x-systemd.*` family: `x-systemd.automount` (plain toggle), `x-systemd.idle-timeout=`,
     `x-systemd.requires-mounts-for=`, `x-systemd.device-timeout=` (each a toggle that,
     once on, reveals a small inline value input for its `=value` suffix).
   - A collapsed-by-default "vfat/filesystem-specific" group (expandable) for
     `fmask= · dmask= · codepage= · iocharset= · shortname= · errors=remount-ro`, same
     toggle+value pattern.
   - Below those: a free-text tag input (type a value, press Enter or `,` to commit) for
     anything not covered above.
   - All of the above - toggled well-known chips, systemd/vfat value chips, and free
     typed tags - render as one single flowing, removable-chip list in the exact order
     they'll be serialized (SPEC's "ordered key/key=value list"), so the user always sees
     the real resulting option order, not three disconnected UI groups.
   - Chip styling: active/selected `bg-sky-100 text-sky-700 dark:bg-sky-900/40
     dark:text-sky-300`, inactive/available-but-off `bg-slate-100 text-slate-600
     dark:bg-slate-800 dark:text-slate-400`, each chip `rounded-full px-3 py-1 text-xs
     font-medium`, with an `×` to remove.
6. **Dump** and **Pass** - two small numeric inputs side by side (`w-16` each), default
   `0`, helper text under each: `"0 = don't dump"` / `"0 = skip fsck, 1 = root
   filesystem, 2 = checked after root"`.
7. **DescriptionField** - a three-row `<textarea>` (`rows="3"`), labeled `"Description
   (optional)"`. The value is genuinely multi-line (T43): embedded `\n` is a normal,
   expected shape, not something this field guards against, so there is no client-side
   stripping and no backend newline-rejection check (both existed pre-T43 and are gone).
   The textarea is vertically resizable - a user drags its bottom-right corner to grow
   it for a longer multi-line note - rather than auto-growing via JS/WASM measurement;
   this matches the project's existing (if incidental) textarea behavior elsewhere (the
   drawer's raw-line-edit `<textarea>` for `UnparsedEntryRow`'s Edit action, §3, never
   opts out of Tailwind Preflight's default `resize: vertical` either), so it introduces
   no new interaction pattern, just makes an existing one deliberate here too. Placed
   last in the field list, directly
   above `ValidationSummary`, so it sits apart
   from the six functional fstab fields above it rather than between them (a deliberate
   repositioning - it was originally grouped right below Mount point, moved here per
   direct user request). The value is round-tripped to/from one or more ordinary `#`
   comment lines placed immediately above the entry in the real fstab file - see
   `docs/API_CONTRACT.md` §5 for the full round-trip detail. Helper text below the
   input: `"Shown as one or more ordinary '#' comment lines directly above this entry in
   the fstab file. Drag the bottom-right corner to resize."`
8. **TrailingCommentField** - a single-line `<input type="text">`, labeled
   `"Trailing comment (optional)"`. Placed directly below Description (item
   7 above) - grouped with it, not between the six functional fstab fields,
   since both are optional per-entry metadata rather than one of those six
   fields. Unlike Description, the value is appended *inline*, on the same
   physical line as the entry's own six fields, after a `#` - not as a
   separate comment line above it. It must stay single line: an embedded
   newline would split one physical fstab line into two, corrupting the
   file - the second half would then be misparsed as a `Malformed` line or
   a bogus new entry of its own - so, unlike Description (T43 made that
   field genuinely multi-line), this input strips any `\n`/`\r` a paste
   might carry as a client-side defense-in-depth guard, backed by the
   authoritative `trailing_comment_contains_newline` server-side check. See
   `docs/API_CONTRACT.md` §5 for the full DTO-level round-trip detail,
   including this field's verbatim (non-trimming) preservation contract and
   its newline validation. Helper text below the input: `"Appended inline
   after this entry's own fields, on the same line, after a '#'. Must be a
   single line - newlines are not allowed here."`

**FormActionBar** (bottom of drawer, sticky): `Cancel` (secondary, `bg-slate-100
dark:bg-slate-700`), `Verify` (secondary/outline, `bg-white dark:bg-slate-800 border
border-slate-300 dark:border-slate-600 text-slate-700 dark:text-slate-200`), and `Save
entry` (primary, `bg-sky-600 hover:bg-sky-700 text-white`, disabled while blocking
validation errors exist - see §5). Saving here only stages the change in memory as a
pending edit; it does **not** write to disk. Disk writes only ever happen through the
DiffView confirm step (§6).

`Verify`, between Cancel and Save, is on-demand reassurance distinct from the passive
debounced check already running while typing (§5): it forces an immediate,
non-debounced `validate_candidate` re-check against the same candidate document, and
confirms the outcome with a toast - a success toast `"No validation issues found."`
when the report has neither errors nor warnings, or an error-styled toast stating the
error and warning counts otherwise (deliberately error-styled even when only warnings
are present, so a non-blocking warning never goes unnoticed just because the user
didn't wait for the debounce). It does **not** change how findings render inline, in
the flat warning stack, or in `ValidationSummary` - that result is the exact same
`validate_candidate` response already driving those displays, just fetched immediately
instead of after the 400ms debounce, so clicking Verify also refreshes them right away.
Hidden entirely in raw-text edit mode (§3), since there is no structured document to
validate there. Disabled while a Save or a Verify call is already in flight, labeled
`"Verifying…"` with a spinner while its own call is in flight.

**DeleteConfirmDialog**: small centered modal, `"Remove this entry from {mount
point}?"` + the entry's source/mount-point/fstype summarized read-only, `Cancel` /
`Remove` (`bg-red-600 hover:bg-red-700 text-white`) buttons. Like edit, this only stages
a pending removal - the actual file is untouched until DiffView is confirmed.

---

## 5. Inline validation feedback (SPEC §9: "duplicate mount point, mount point
doesn't exist on disk, malformed UUID" before allowing save)

Component: **ValidationMessage**, a generic renderer that consumes
`{ field: FieldId, message: String, severity: "error" | "warning" }` per field - the
severity classification itself (which checks are hard-blocking vs. advisory) is a
backend/validator decision (T5), not invented here; the UI simply renders whatever
severity the validation response assigns, so e.g. duplicate mount point and malformed
UUID are expected to arrive as `error`, while something like "mount point doesn't exist
yet" *could* legitimately arrive as either `error` or `warning` depending on context the
validator has and the UI doesn't need to know - this document only fixes how each
severity looks and behaves:

- **error** (blocking): field border becomes `ring-2 ring-red-500`, message text
  directly below the field in `text-red-700 dark:text-red-400 text-xs`, plain text,
  no icon. **The drawer's `Save entry` button is disabled** (`bg-slate-300
  text-slate-500 dark:bg-slate-700 dark:text-slate-500 cursor-not-allowed`) with a
  tooltip `"Fix validation errors before saving"` - this is an actual disabled state,
  not just a styling warning the user could click through.
- **warning** (non-blocking): field border `ring-1 ring-amber-400`, message in
  `text-amber-600 dark:text-amber-400 text-xs`, plain text, no icon (an earlier
  revision prefixed each message with a small ⚠/ℹ glyph - removed per direct
  request as visual clutter the color/weight already conveys). Does not disable
  Save.
- A warning that is specifically about the mount point of the entry currently being
  edited (the backend's `field`/`line_index` tagging on the finding, matched against
  this draft's own position in the document) renders inline, directly below the
  mount point input, using the same `ValidationMessage` styling. The flat warning
  stack described below is deliberately scoped to **document-level findings only**
  (no `line_index` at all - e.g. a duplicate mount point spanning two lines, or a
  `findmnt --verify` dump) plus any other non-mount-point warning about this same
  draft's own line. A warning tagged to some *other* line entirely (e.g. "mount
  point X does not exist" for an unrelated, already-saved entry elsewhere in the
  document) is real, and still surfaces inline the next time *that* entry's own
  drawer is opened - but is not shown inside a drawer editing a different entry,
  where it isn't actionable and was confusingly reading as if it were about the
  entry currently open (an earlier revision showed it here unconditionally -
  corrected per direct user report after real use surfaced the confusion). Nothing
  in this app currently aggregates warnings across every entry into one combined
  view outside a drawer - DiffView (§6) shows the diff only, not validation
  findings.
- If ≥1 error exists anywhere in the form, a **ValidationSummary** banner appears at
  the bottom of the drawer body, immediately above the Save/Cancel bar: `bg-red-50
  dark:bg-red-900/20 border border-red-300 dark:border-red-800 text-red-800
  dark:text-red-300`, text `"N issues must be fixed before saving"` listing each
  error with a jump-to-field link.
- Validation runs on field blur and on every attempted Save click (not only at the
  end), so errors surface as early as possible, never only after a failed save attempt.

---

## 6. Pending-changes / diff view (SPEC §9: "a clear pending changes / diff view
before committing a save")

**Flow placement**: staging an add/edit/delete (via EntryDetailDrawer or
DeleteConfirmDialog, §4) only updates an in-memory candidate document and makes the
**PendingChangesBar** appear - a slim bar pinned just below HeaderBar, `bg-amber-50
dark:bg-amber-900/20 border-b border-amber-300 dark:border-amber-800`, text `"N entries
changed"` + a primary button `Review & Save…`. This bar is the only way to reach the
diff/write step; there is no other Save action that skips it.

Clicking `Review & Save…` opens **DiffView**: a centered modal (`max-w-4xl`, up to
~85vh tall with internal scroll), `bg-white dark:bg-slate-800 shadow-lg rounded-lg`,
backdrop `bg-black/50`. **Not** dismissible by clicking the backdrop (Escape still
cancels) - this is the last gate before an actual write, same non-dismissible-by-
backdrop treatment EntryDetailDrawer itself now also has (§4), for the analogous
reason of not silently discarding unsaved work on a stray click.

**Contents, top to bottom:**

1. Header: `"Review changes before saving"`, the resolved target path, and a small
   inline reuse of the TargetBanner color coding (a compact badge, the same
   three-state red/amber/emerald logic from §2) showing **which file will actually be
   written** - the danger signal must carry through to this final screen, not just
   live on the top banner. The badge describes the *destination*, which since T34 is
   not always the path printed next to it:

   | State (§2) | Badge | Why |
   |---|---|---|
   | Real file (`is_real`) | `REAL`, `bg-red-600` | the path beside it already is `/etc/fstab` |
   | Working draft (`SandboxDefault`) | `WRITES /etc/fstab`, `bg-amber-600` | names the destination explicitly, *because* it differs from the draft path shown beside it - a bare `REAL` here would read as "this path is the real file" |
   | Isolated test target | `TEST FILE`, `bg-emerald-600` | the only state where the save stops at the path shown |

   The pre-T34 `SANDBOX` badge is gone for the same reason the green banner is: it
   told the user the save was contained when it no longer is.
2. Two tabs, **Summary** (default) and **Raw diff** - both are required, satisfying
   both halves of the brief ("structured per-field change list" and "exact textual
   diff"):
   - **DiffSummaryList** (Summary tab): entries grouped under `Added (N)` / `Modified
     (N)` / `Reordered (N)` / `Removed (N)` headings. The `Reordered` group is new in
     T40 (§13.7) - see that section for why it exists and exactly when an entry lands
     there instead of `Modified`.
     - Added entries: whole-row summary on a `bg-emerald-50 dark:bg-emerald-900/20`
       tinted card.
     - Removed entries: whole-row summary on `bg-red-50 dark:bg-red-900/20`, text
       `line-through text-slate-500`.
     - Modified entries: one card per entry, listing only the fields that actually
       changed as `label: ~~old value~~ → **new value**` rows - old value muted
       strikethrough, new value bold, unchanged fields not repeated here at all. If
       the same entry also changed position, that is appended as one extra note line
       rather than also being listed under `Reordered` (§13.7's precedence rule) - no
       entry is ever listed twice.
     - Reordered entries (T40, §13.7): whole-row summary on a `bg-sky-50
       dark:bg-sky-900/20` tinted card (sky, not emerald/red/amber - this is neither
       additive, destructive, nor itself a warning), with one extra line stating
       `"Moved from position {old} to position {new}"` (1-based rank among entries
       still present in both versions - see §13.7 for exactly how that rank is
       computed). No field-level diff rows, since content is byte-identical to the
       original - position is the only thing that changed.
   - **DiffRawText** (Raw diff tab): classic unified-diff rendering of the full
     candidate file text against the current target file text, `font-mono text-sm`,
     line-numbered gutter, `-` (removed) lines `bg-red-50 text-red-700 dark:bg-red-950/40
     dark:text-red-300`, `+` (added) lines `bg-emerald-50 text-emerald-700
     dark:bg-emerald-950/40 dark:text-emerald-300`, unchanged context lines neutral
     `text-slate-600 dark:text-slate-400`. This is the exact textual before/after of
     the real file content, including comments/blank-line positions, so a careful user
     can verify byte-level correctness, not just Rustab's interpretation of the change.
     A pure reorder already renders correctly here with no changes needed (§13.7) -
     the two now-swapped lines simply show as a `-`/`+` pair at their respective
     positions, same as any other line-level change.
3. Footer action bar:
   - `Cancel` (secondary) - closes the modal, pending changes remain staged (nothing is
     lost, nothing is written).
   - Confirm button, styled and gated by the single `writes_real_file` predicate from
     §2 - **two** states, not the badge's three, deliberately: a button's appearance
     should describe the consequence of pressing it, and pressing it writes
     `/etc/fstab` in both of the first two states above. Whether the file on screen is
     `/etc/fstab` itself or the draft it is saved through is a *"what am I looking at"*
     distinction, which the banner and the badge already carry.
     - **Isolated test target** (`writes_real_file == false`): `bg-sky-600
       hover:bg-sky-700 text-white`, label `"Save changes to test file"`, enabled as
       soon as the modal opens.
     - **Any save that reaches `/etc/fstab`** (`writes_real_file == true`, i.e. an
       explicit real target *or* the default working-draft launch): `bg-red-600
       hover:bg-red-700 text-white`, label `"Write to /etc/fstab"`, **disabled until**
       an explicit checkbox directly above the footer is checked: `"I understand this
       modifies my system's real fstab and takes effect on next mount/boot."` - that
       sentence is accurate in both cases, since both do modify the real fstab. The
       checkbox is not rendered at all in the isolated-target state, where it would be
       false. This extra friction step is what §2's asymmetric alarm treatment buys
       the user at the point of no return, and `confirm_disabled` in
       `real_target_confirm_checkbox.rs` is the one implementation of the rule, shared
       with §7's restore dialog.
4. On confirm: button becomes a loading state (`"Writing changes…"` + spinner). On
   success: toast/StatusMessage success notice (`"Changes saved."`), modal closes,
   EntriesTable refreshes from the new saved state. On failure: the error is shown
   **inside the still-open modal** (not only as a transient toast), since a failed save
   is safety-relevant and the user must not lose the context of what failed;
   `Cancel`/retry remain available.
5. One in-between outcome, T34-specific: a save can succeed *and* carry
   `draft_sync_warning` (`docs/API_CONTRACT.md` §8) - `/etc/fstab` was written, but the
   local working draft could not be updated to match it. The modal still closes (the
   write the user asked for did happen), and the backend's ready-to-show message
   replaces the plain success line in the toast, as `StatusToast`'s **`Warning`** kind:
   amber + `⚠`, wrapped at `max-w-md` since it is a sentence rather than a label. It is
   deliberately *not* the `Error` kind - a red `✕` on a save that actually wrote
   `/etc/fstab` would be read as "my fstab write failed", prompting a retry or a panic
   over a cosmetic, self-repairing problem (the next launch refreshes the draft from the
   real file on its own) - and deliberately not silent either, per `PROJECT_PLAN.md`'s
   T34 non-negotiable "a toast, not a silent swallow". The same handling applies to
   `restore_backup` (§7), which goes through the identical save dispatch.

---

## 7. Backup/restore panel (SPEC §9: "list recent auto-backups ... and let the
user restore one")

Component: **BackupPanel**, reached via the `Backups` tab in HeaderBar (§1).

- **"Create backup" button**, living in `HeaderBar`'s top-right contextual action
  slot - the same slot `"+ Add Entry"` occupies while the `Entries` tab is active.
  It renders only while the `Backups` tab is active, and `"+ Add Entry"` is absent
  from the DOM during that time (not a row action - it is not tied to an existing
  backup, it acts on the active target's current content, `docs/API_CONTRACT.md`
  §3.11). Primary-ish but not alarming styling
  (`bg-sky-600 hover:bg-sky-700 text-white`, the same non-destructive-primary treatment
  used elsewhere in the app), label `"Create backup"` while idle, `"Creating…"` with the
  same inline spinner treatment as `BackupDeleteConfirmDialog`'s `"Deleting…"` state
  while in flight, disabled while in flight. Deliberately **no confirm dialog**: this
  action is safe/additive/reversible - it only ever adds a new file next to the existing
  backups, and the existing `Delete` action can always clean it up afterward - so a
  loading state and an inline error are the whole of its UI. On failure, the message is
  shown as a small red error line directly beneath the button (no dialog, no toast); on
  success, a success toast (`"Backup created."`) and the list reloads.
- **"Open backup location" button** (T25, `docs/API_CONTRACT.md` §3.12), now living in
  `HeaderBar`'s contextual action slot, next to `"Create backup"` (relocated there by
  T26 per direct user request - superseding T25's own placement choice of a small bar
  inside `BackupPanel`'s content area). Rendered **once, not once per row** - every
  backup lives in the same directory as the active target (`backups.rs`'s own module
  docs: "in that directory the name *is* the identity"), so a per-row button would just
  duplicate the same action. The relocation changes only where the button lives; its
  behavior is otherwise unchanged: secondary styling, identical treatment to
  `BackupListItem`'s `Restore`/`View diff` buttons, label `"Open backup location"`,
  deliberately **no confirm dialog** (opening a directory for viewing is not a write or a
  privileged action) and **no loading/disabled state** (the call is a near-instant
  `spawn()` of the host file manager, not a slow operation, unlike every other backup
  action in this panel). On failure, a small red error line directly beneath the button
  (no dialog, no toast), the same treatment as the "Create backup" button's own
  `create_error`. On success, **no toast and no list reload**: unlike
  `create_backup`/`restore_backup`/`delete_backup`, this action does not change the
  backup list or the target, so there is nothing to refresh - the file manager window
  appearing is its own visible confirmation.
- **BackupListItem** rows, newest first: human-readable relative timestamp (e.g. `"2
  hours ago"`, exact ISO timestamp on hover tooltip), a pill badge showing whether the
  backup is automatic or manual (same rounded-pill shape as `EntriesTable`'s source-kind
  badge: `"Automatic"` in the same neutral slate styling, `"Manual"` in the sky tones
  used elsewhere for non-destructive emphasis - distinct but not alarming), file size,
  which target path it was backed up from (prefixed with a literal `"Target: "` label,
  e.g. `"Target: /home/tomsk/.local/share/rustab/fstab.draft · 1.2 KB"`), and three
  row-level actions: `Restore`
  (secondary styling, not primary - restoring is not the app's main action), `View diff`
  (secondary styling, identical treatment to `Restore`), and `Delete` (destructive but
  still a row-level secondary button - red text, `text-red-600 hover:bg-red-50
  dark:text-red-400 dark:hover:bg-red-900/20` - the giant red confirm button lives in
  the dialog below, not in the row).
- `EmptyState` if no backups exist yet: `"No backups yet - one will be created
  automatically the first time you save."`
- **BackupRestoreConfirmDialog**: centered modal, same non-click-outside-dismiss
  treatment as DiffView, since this is also a real write:
  - Copy: `"Restore the backup from {timestamp}? This will overwrite the current
    contents of {target path}."` followed by a reassuring-but-accurate safety note:
    `"Restoring itself creates a fresh backup of the current file first, following the
    same atomic write guarantee used everywhere else - but any unsaved pending edits in
    this session will be discarded."`
  - When `writes_real_file` (§2), one extra line sits between those two, in the
    `warning` amber: `"Like a save, this restore writes the real /etc/fstab and will
    ask for administrator access."` It is there because `{target path}` above is the
    *backup's* target - the local working draft, on a default launch - which on its own
    would read as if the real file were out of scope here. `restore_backup` delegates to
    the very same save dispatch (`src-tauri/src/commands.rs`), so it has exactly the
    three cases §2 lists, not a safer set of its own.
  - Confirm button follows the identical `writes_real_file` color + checkbox-gate
    pattern from §6 (red + required understanding checkbox for any restore that reaches
    `/etc/fstab`, including the default working-draft launch; plain sky blue with no
    extra gate and the label `"Restore to test file"` only for an isolated test target).
  - On confirm: same loading → success/error/`draft_sync_warning` handling pattern as
    DiffView's save (§6 points 4-5).
- **BackupDeleteConfirmDialog**: centered modal, triggered by a row's `Delete` button.
  Deliberately **non-dismissible by backdrop click**, the same as
  `BackupRestoreConfirmDialog` and for the same reason: unlike `DeleteConfirmDialog`'s
  staged, undoable entry removal, deleting a backup is an immediate, irreversible
  action that - for the real target - goes through the same `pkexec` prompt as a
  save/restore, so it must not be dismissible by an accidental click.
  - Copy: `"Delete the backup from {timestamp}? This cannot be undone - there is no
    backup of a backup."`
  - For a real target, the confirmation checkbox's copy is deliberately **not** the
    restore/save dialog's sentence (that sentence talks about modifying `/etc/fstab`
    on next mount/boot, which is false for a backup deletion - a backup delete never
    touches the live target file at all). Instead: `"I understand this permanently
    deletes a backup of /etc/fstab and requires administrator privileges."`
  - The confirm button is styled destructively (`bg-red-600 hover:bg-red-700
    text-white`) **regardless of `is_real`** - unlike restore/save, which are sky-blue
    for an isolated test target, a backup deletion is irreversible either way, so there
    is no "safe" variant to signal with a calmer color. Label `"Delete backup"` while
    idle, `"Deleting…"` + spinner while in flight.
  - This dialog is no longer gated on raw `is_real` - it now takes `writes_real_file`
    (computed once in `AppShell`, same as `DiffView` and `BackupRestoreConfirmDialog`),
    because T35 made `delete_backup` and `create_backup` dispatch on the same
    `is_real || source == sandbox_default` predicate as a save does (§2). On a default
    launch, backup deletion is now also `pkexec`-elevated, and the checkbox's
    `"...requires administrator privileges"` copy is accurate for that case too.
  - On confirm: same loading → success/error handling pattern as
    BackupRestoreConfirmDialog; on success, only the backup list is reloaded (a backup
    deletion never touches the live target, so there is nothing else to refresh).
- **BackupDiffView**: a read-only overlay opened by a row's `View diff` button, showing
  `crate::diff::compute_raw_diff` between the live target's current text (old) and the
  backup's raw text fetched via `read_backup` (new) - reusing `DiffRawText` directly,
  raw tab only (there is no `render_candidate` equivalent for arbitrary backup text, so
  a parsed Summary tab isn't buildable here, and adding one is out of scope). Old/new
  are deliberately current-then-backup, not the reverse: this answers "what would
  restoring this backup change", which is what someone opening this dialog actually
  wants to know before clicking Restore (changed from an earlier backup-then-current
  ordering per direct user report - the add/removed lines read backwards from what the
  Restore action is about to do). Unlike the two dialogs above, this performs no write
  and no deletion, so - judgment call, mirroring `DeleteConfirmDialog`'s reasoning - it
  **is dismissible by clicking the backdrop**: there is nothing a stray click could
  lose. Header names both sides, e.g.
  `"Diff: current {target path} vs. backup from {timestamp}"`; a single `Close` button.
  If the `read_backup` call rejected, the error is shown in place of the diff.

---

## 8. Visual design tokens

Tailwind v4 (originally locked as v3; migrated to v4 after the UI was built - the
tokens below are unchanged by that, see the note at the end of this section).
Dark mode uses Tailwind's default media strategy (`prefers-color-scheme`): the app
has no class toggling anywhere, it follows the OS colour scheme. The tokens below are
written as light-value / dark-value pairs, so a class-based strategy would apply them
identically if one were ever adopted.

**Brand flag**: SPEC/PROJECT_PLAN define no existing brand colors, logo, or voice for
Rustab. The palette below (slate neutrals + sky primary) is a reasonable
dark-mode-friendly default, not a brand decision - **flagging this explicitly**: if the
user wants a specific primary/brand color or wordmark treatment later, that's a
decision for them, not something to infer from this spec.

### Color tokens

| Token | Light | Dark | Used for |
|---|---|---|---|
| `bg-app` | `slate-50` `#f8fafc` | `slate-900` `#0f172a` | window background |
| `bg-surface` | `white` `#ffffff` | `slate-800` `#1e293b` | panels, drawer, modal, table |
| `bg-surface-sunken` | `slate-100` `#f1f5f9` | `slate-950` `#020617` | raw-text/diff code blocks |
| `border-default` | `slate-200` `#e2e8f0` | `slate-700` `#334155` | all borders/dividers |
| `text-primary` | `slate-900` `#0f172a` | `slate-100` `#f1f5f9` | body/heading text |
| `text-muted` | `slate-500` `#64748b` | `slate-400` `#94a3b8` | secondary/meta text |
| `accent` (primary action/focus) | `sky-600` `#0284c7` | `sky-500` `#0ea5e9` | primary buttons, active tab, links |
| `accent-hover` | `sky-700` `#0369a1` | `sky-400` `#38bdf8` | hover state of the above |
| `focus-ring` | `sky-500` `#0ea5e9` | `sky-500` `#0ea5e9` | `:focus-visible` ring, 2px + 2px offset |
| `danger` | `red-600` `#dc2626` | `red-600` `#dc2626` | destructive buttons (Delete, real-target Write/Restore) |
| `danger-hover` | `red-700` `#b91c1c` | `red-500` `#ef4444` | hover of the above |
| `target-real` | `red-600` `#dc2626` bg / `red-800` `#991b1b` border | same | TargetBanner real-file mode only (§2) |
| `target-draft` | `amber-600` `#d97706` bg / `amber-800` `#92400e` border | same | TargetBanner working-draft mode, and the matching DiffView badge (§2, T34) |
| `target-isolated` | `emerald-600` `#059669` bg / `emerald-800` `#065f46` border | same | TargetBanner isolated-test-target mode only (§2) |
| `warning` | `amber-500`/`amber-600` | `amber-400` | unparsed-row accents, non-blocking validation, the working-draft banner/badge (§2), `StatusToast`'s `Warning` kind (§6 point 5) |
| `success` | `emerald-600` | `emerald-400` | save/restore success state |

Reused consistently: `danger` and `target-real` are deliberately the **same** red
(`#dc2626`) - the brief asked for "a distinct danger/real-target accent color used
consistently," and using one red for both "this action is destructive" and "this is the
real system file" keeps the semantic meaning ("real consequences") a single color
teaches once, rather than splitting it into two reds the user has to learn separately.
The *treatment* (banner stripes/uppercase vs. a plain button) is what makes the
TargetBanner read as more alarming, not a different hue.

### Spacing scale

Standard Tailwind 4px-base scale; only these increments are used anywhere in the app
(kept small deliberately so a dev isn't guessing between near-identical values):

`1` = 4px · `2` = 8px · `3` = 12px · `4` = 16px · `5` = 20px · `6` = 24px · `8` = 32px ·
`10` = 40px · `12` = 48px · `16` = 64px

Conventions: page gutter `px-6`; table cell padding `px-4 py-2`; drawer/modal body
padding `p-6`; vertical gap between stacked form fields `space-y-4`; standard button
`px-4 py-2`; compact/chip button `px-3 py-1.5`; icon-only button hit target minimum
32×32px (`p-2` around a 16–20px icon).

### Typography scale

Font stack: `font-sans` = `Inter, ui-sans-serif, system-ui, -apple-system, sans-serif`
(no custom brand typeface specified - system/Inter default, same brand-flag caveat as
color above). `font-mono` = `"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo,
monospace`, used for every raw/technical value: source paths/UUIDs, mount points, the
unparsed-row raw text, and the DiffRawText block.

| Token | Size / line-height | Weight | Used for |
|---|---|---|---|
| `text-xs` | 12px / 16px | normal | timestamps, helper text, chip labels |
| `text-sm` | 14px / 20px | normal/medium | default body, table cells, form inputs & labels - the base UI density |
| `text-base` | 16px / 24px | medium | drawer/modal section sub-headings |
| `text-lg` | 18px / 28px | semibold | drawer/modal titles ("Review changes before saving") |
| `text-xl` | 20px / 28px | bold | app title in HeaderBar ("Rustab") |

`font-bold` is reserved for the TargetBanner's real-mode text and nothing else, so its
visual weight stays a reliable signal (§2).

### Elevation, radius, motion

- Radius: `rounded-md` (6px) default on inputs/buttons/table cells; `rounded-lg` (8px)
  on panels/drawer/modal; `rounded-full` on chips/badges/icon buttons.
- Shadow: `shadow-lg` on drawer and modal surfaces; `shadow-sm` on hovered table rows
  only if a lift effect is wanted - optional, not required.
- Motion: drawer slides in/out over 150–200ms ease-out; modal fades+scales in over
  150ms; respect `prefers-reduced-motion` by disabling the transform/scale and keeping
  only an opacity fade.

### Accessibility baseline

- All text/background pairs above meet WCAG AA (4.5:1 body text, 3:1 large text) in
  both themes - verify `red-600`/white (≈4.8:1), `emerald-600`/white (≈3.9:1, used only
  for large bold banner text at 18px+, which passes the 3:1 large-text threshold; do not
  reuse `emerald-600`-on-white for small body text).
- Every interactive element has a visible `:focus-visible` ring using `focus-ring`
  (§ above) - never remove the outline without replacing it.
- All icon-only buttons (Edit/Delete/Browse/remove-chip/Move up/Move down) carry an
  accessible label (`aria-label` equivalent in Yew) in addition to the icon.
- Disabled buttons (e.g. Save while validation errors exist, or a boundary row's Move
  up/down - §13.2) are real `disabled` elements, not just visually dimmed clickable
  ones.

### Responsive breakpoints

- `< 760px` window width: EntriesTable drops Dump/Pass into a per-row overflow menu;
  EntryDetailDrawer becomes full-width instead of fixed 520px; DiffView modal becomes
  full-width/full-height instead of `max-w-4xl` centered.
- `≥ 760px`: full layout as specified above. No further breakpoints are defined - this
  is a desktop app, not a target for phone-width layouts.

### What the Tailwind v3 → v4 migration changed about these tokens

The token *names* above are all still valid in v4, and the migration was verified to
generate exactly the same set of utility class names v3 did (250 classes, no additions
and no omissions). Two token *values* differ, both by Tailwind's own design rather
than by a project decision:

- **The default colour palette moved from sRGB hex to OKLCH.** The slate neutrals that
  carry most of this UI are visually unchanged (≤ 4/255 per channel); the saturated
  accents are deliberately more vivid (up to ≈ 39/255 on a single channel for
  `red-600`, `pink-600`, `amber-400`), because v4 pushes saturated colours toward the
  P3 gamut. Expect accent colours to read slightly stronger than the hex values quoted
  in the colour-token tables above, especially on a wide-gamut display.
- **The shadow scale was renamed**, so `shadow-sm` in v4 is what `shadow` was in v3,
  and v3's `shadow-sm` is now `shadow-xs`. The three card surfaces that use
  `shadow-sm` therefore render a slightly stronger shadow than they did under v3;
  switch them to `shadow-xs` if the original hairline shadow is wanted back.

---

## 9. Component inventory

Flat checklist for T10 (shell/mock data), T11 (table + form data wiring), and T12
(diff + backup + target indicator wiring):

1. `AppShell` - top-level layout: banner + header + tab content area
2. `TargetBanner` - three-state "which file does a save write" indicator (§2)
3. `HeaderBar` - app title, `Entries`/`Backups` tab nav, contextual action button
   (`Add Entry` on the `Entries` tab, `Create backup` + `Open backup location` on the
   `Backups` tab, T26), plus an always-visible `About` trigger (both tabs, T26)
4. `PendingChangesBar` - "N entries changed" + `Review & Save…` trigger (§6)
5. `EntriesTable` - main entry list (§3)
6. `EntryRow` - one parsed entry's row (§3); click-to-expand reveals every field,
   untruncated, including the description, in an inline detail panel (no separate
   inline description indicator in the collapsed row itself). Does get a left-border
   accent + title tooltip when a document-wide warning is tagged to this row, plus
   Move up/Move down buttons in its Actions cell (T40, §13)
7. `UnparsedEntryRow` - malformed-line row, raw text + warning (§3) - also gets
   Move up/Move down (T40, §13)
8. `EmptyState` - generic empty-list placeholder (entries or backups)
9. `EntryDetailDrawer` - add/edit form container (§4)
10. `SourceTypeSelect` - source-type dropdown
11. `DevicePicker` - discovered-device list/combobox for source value
12. `MountPointField` - text input + directory-picker trigger
13. `DirectoryPickerButton` - native directory chooser trigger
14. `FsTypeField` - free-text combobox with common-type suggestions
15. `OptionsTagInput` - well-known toggle chips + systemd/vfat value chips + free tags,
    one ordered chip list (§4)
16. `DumpPassFields` - the two small numeric inputs
17. `ValidationMessage` - generic per-field error/warning renderer (§5)
18. `ValidationSummary` - drawer-top aggregate error banner (§5)
19. `FormActionBar` - Cancel/Verify/Save buttons inside the drawer
20. `DeleteConfirmDialog` - single-entry delete confirmation (§4)
21. `DiffView` - the pending-changes/diff confirmation modal (§6)
22. `DiffSummaryList` - structured per-field Added/Modified/Reordered/Removed list (§6,
    §13.7)
23. `DiffRawText` - unified-diff monospace rendering (§6)
24. `RealTargetConfirmCheckbox` - the extra "I understand..." gate for any write that
    reaches the real `/etc/fstab` (§2's `writes_real_file`, not raw `is_real`), reused
    by DiffView and BackupRestoreConfirmDialog
25. `BackupPanel` - backup list view (§7)
26. `BackupListItem` - one backup's row + Restore/View diff/Delete buttons
27. `BackupRestoreConfirmDialog` - restore confirmation modal (§7)
28. `StatusToast` - transient success/warning/error notification (`Warning` carries a
    successful save's `draft_sync_warning`, §6 point 5)
29. `LoadingSpinner` - inline loading indicator (buttons, panel loads)
30. `BackupDeleteConfirmDialog` - backup delete confirmation modal (§7)
31. `BackupDiffView` - read-only backup-vs-current-target diff overlay (§7)
32. `AboutDialog` - static "About Rustab" overlay: name, logo, version, external
    links, author/contact line (§10, T26)
33. `icons` module (`EditIcon`/`DeleteIcon`/`ChevronRightIcon`/`CopyIcon`/`CheckIcon`/
    `FolderOpenIcon`) - small reusable inline-SVG icon components (Lucide-derived path
    data, ISC license, no crate dependency - see the module's own doc comment for why
    a whole icon-set crate was rejected), replacing the `✎`/`🗑` Unicode glyphs
    `EntryRow`/`UnparsedEntryRow`'s Edit/Delete buttons used before, plus the
    expand-chevron and the expanded Mount point row's copy/open-in-file-manager
    buttons (§3). `stroke="currentColor"` so hover/dark-mode color changes already
    applied to the button apply to the icon too, same as `AboutDialog`'s own icons.
    T40 (§13) adds two more to this module: `ChevronUpIcon`/`ChevronDownIcon`, for the
    Move up/Move down buttons - dedicated Lucide glyphs, not a CSS-rotated reuse of
    `ChevronRightIcon`, consistent with every other icon in this module getting its
    own path rather than a transform. T48 (§13.11) added one more, `GripVerticalIcon`,
    for its drag handle; **T52 removed both the handle and the icon** - the whole row
    is the grab surface now, so there is no handle left to hang a glyph on (§13.11).
34. `RawViewDialog` - read-only overlay showing the active target's exact current
    raw text, no diff (§11)
35. `ExternalChangeDialog` - external-change/drift warning modal shown after a
    `save`/`restore_backup` rejection (§12, T36)

---

## 10. About dialog (T26)

Component: **AboutDialog**, triggered by an always-visible `"About"` text button at
the far right of `HeaderBar`, present on both the `Entries` and `Backups` tabs (unlike
every other button in that component, which is tab-conditional).

- **Trigger placement**: a plain text button, not an icon glyph - this avoids any
  font-rendering ambiguity around which glyph should mean "about" (ⓘ vs. ⋯ vs. a
  question mark all have different conventions across platforms), and this app has no
  footer today to put it in instead, so the header's always-visible far-right corner is
  the next most discoverable, always-on location.
- **Content**, stacked and centered in a small modal panel: the app logo, the name
  `"Rustab"`, the live app version (read from the frontend crate's own `Cargo.toml` at
  compile time via `env!("CARGO_PKG_VERSION")` - never a hardcoded string, so it can
  never drift from the shipped build), two links that call the `open_external_link`
  command (`docs/API_CONTRACT.md` §3.13) rather than a plain `<a href>` - a GitHub
  repository link and a GitHub Sponsors link - and an author/contact line matching this
  project's own existing public-facing convention (`CONTRIBUTORS.md` /
  README.md's "Contact & Support" section).
- **Dismiss behavior**: like `BackupDiffView`, this dialog is purely read-only and
  informational, so - the same judgment call as that component - it **is dismissible by
  clicking the backdrop**, unlike the non-dismissible write/delete confirm dialogs. A
  `Close` button is also present, styled identically to `BackupDiffView`'s.

---

## 11. Raw view dialog

Component: **RawViewDialog**, triggered by a `"Raw view"` button in `HeaderBar`,
**Entries tab only** (unlike `About`) - on `Backups`, a per-row `View diff` already shows
the same live target content as one side of a comparison, so a second, separate
always-visible trigger there was judged redundant clutter (changed from an initial
both-tabs placement per direct follow-up request, after the user tried it live). Added
per direct user request after a live real-target session surfaced a real need: there
was no way to see the active target's exact current raw content without either opening
`DiffView` (which always computes a diff against a pending edit - meaningless/
unavailable with zero staged changes) or `BackupDiffView` (which is about a *backup*,
not the live target). This fills that gap: no diff, no comparison, just "what does this
file actually contain right now."

- **Content**: the target's current raw text - the exact same `original_text` state
  `AppShell` already holds for `DiffView`'s own raw-text tab (`docs/API_CONTRACT.md`
  §5), so this needs no new fetch - rendered verbatim in a `font-mono text-sm
  whitespace-pre-wrap` block, the same raw-text treatment `UnparsedEntryRow` already
  uses for a malformed line's text (§3). Header names the target path, e.g.
  `"Raw view: /etc/fstab"`.
- **Dismiss behavior**: purely read-only, so - same reasoning as `BackupDiffView`/
  `AboutDialog` - it **is dismissible by clicking the backdrop**. A `Close` button is
  also present, styled identically to those two.
- Reuses the identical modal shell (`max-w-4xl max-h-[85vh]`, header/body/footer
  structure) `BackupDiffView` already established, rather than inventing a new one.

---

## 12. External-change warning dialog (T36)

Component: **ExternalChangeDialog**, shown when a `save` or `restore_backup` call is
rejected with `WriteErrorDto{ kind: "external_change", ... }` (`docs/API_CONTRACT.md` §8)
- the real `/etc/fstab` changed outside Rustab since this session last synced with it,
so nothing was written.

- **Trigger and flow placement**: Shared by both the save flow (via DiffView, §6) and
  the restore flow (via BackupRestoreConfirmDialog, §7). When the rejection is detected,
  `AppShell` diverts into this dialog instead of showing a plain error banner - the
  prior dialog is hidden and the new dialog is shown, since this is a real-write gate
  that requires user review and explicit re-confirmation. Documented choice: `Cancel`
  closes the whole flow back to the pending-changes state (save) or Backups panel
  (restore), not back to the intermediate dialog - simpler state management and leaves
  the user exactly where they would be if they had cancelled from DiffView/
  BackupRestoreConfirmDialog themselves.

- **Modal shell**: Centered, `max-w-4xl max-h-[85vh]`, same non-dismissible-by-backdrop
  treatment as DiffView/BackupRestoreConfirmDialog (this is also a real-write gate).
  `bg-white dark:bg-slate-800 shadow-lg rounded-lg`, backdrop `bg-black/50`.

- **Header and body copy**: Header `"This file changed outside Rustab"`. Body names the
  target path and explains: `"{path} changed outside Rustab since this session last
  synced with it. Nothing was written. Review what changed below before deciding."`.

- **Diff rendering**: Raw diff only (no Summary tab - just two raw file bodies), computed
  via `compute_raw_diff(last_known_content, current_content)` from the error's
  `ExternalChangeDto`, rendered via the reused `DiffRawText` component (same treatment
  as BackupDiffView, §7).

- **Confirmation gate**: A second `RealTargetConfirmCheckbox` - `"I understand this
  overwrites a change that happened outside Rustab, discarding what is on disk right
  now."` - gates the "Write anyway" button. This is **layered on top** of whichever
  first-level `RealTargetConfirmCheckbox` (from DiffView/BackupRestoreConfirmDialog,
  §6/§7) the user already passed to reach this point, not a replacement for it.
  `RealTargetConfirmCheckbox` is always required here (the dialog only appears after
  a save/restore that would write the real `/etc/fstab`), so the "requires confirmation"
  flag is always true.

- **Action buttons**: three, in order `Cancel` / `"Reload and re-sync"` / `"Write
  anyway"`. `Cancel` (secondary, `bg-slate-100 dark:bg-slate-700`) closes the whole
  flow back to the state before DiffView/BackupRestoreConfirmDialog opened - nothing
  is written, pending changes remain staged (save) or nothing happens (restore).
  `"Write anyway"` (destructive, `bg-red-600 hover:bg-red-700 text-white`) retries the
  original call with `allow_external_change: true`, disabled until the checkbox is
  checked. Loading state: `"Writing changes…"` + spinner, identical to DiffView/
  BackupRestoreConfirmDialog. **(T37) `"Reload and re-sync"`** (non-destructive,
  `bg-sky-600 hover:bg-sky-700 text-white`, the same "safe/additive" treatment
  BackupPanel's "Create backup" button already uses, §7) calls `resync_external_change`
  (`docs/API_CONTRACT.md` §3.15), which adopts the real file's *current* content as the
  new baseline - and re-syncs the last-known-real marker to match - without writing to
  the real file at all. It needs no `RealTargetConfirmCheckbox` gate: nothing it does
  can ever touch the real target. Loading state: `"Reloading…"` + the same spinner, via
  its own `resyncing` flag, kept deliberately separate from `saving` so neither button's
  label is misrepresented by the other call being in flight. All three buttons are
  disabled while *either* `saving` or `resyncing` is `true`, so the user can never fire
  two of these calls at once.

- **Outcomes**: On success, same success/`draft_sync_warning` handling as DiffView/
  BackupRestoreConfirmDialog (§6 points 4-5). On re-drift (file changed again mid-retry):
  the dialog stays open with a refreshed diff and the checkbox re-required, recursively
  if external changes keep drifting. On any other failure: shown inline in the still-open
  dialog's error slot, with `Cancel`/retry remaining available. **(T37)** On "Reload and
  re-sync" success: the dialog closes and the flow returns to its own normal confirm step
  with a freshly computed diff - DiffView reopened (save flow, pending edits untouched) or
  BackupRestoreConfirmDialog revealed again with its checkbox re-armed (restore flow) -
  rather than any new state of its own; no toast, since nothing was written and the
  dialog's own transition is feedback enough. On failure: shown inline in this dialog's
  error slot, same treatment as any other retry failure here.

---

## 13. Entry reordering (T40)

**The problem**: `findmnt --verify` (the validation backend already calls this, see
`PROJECT_PLAN.md`'s T39 section) requires a parent mount point's line to appear before
any nested/child mount point beneath it. Before this task, Rustab had no in-app way to
change an entry's position at all - the only way to fix a genuine "wrong order" finding
was delete-and-re-add, or editing the raw file outside the app entirely.

**Decision: Move up / Move down buttons added to `EntryRow`/`UnparsedEntryRow`'s
existing Actions cell** - candidate #1 of the five the repo owner named, not
drag-and-drop, not an order-number field, and not a separate reorder view. This is not
a close call the repo owner needs to break a tie on: §13.1 below gives the specific
reasoning for this app. Two refinements beyond the plain button idea as literally
named turned out to be necessary, not optional nice-to-haves, and are specified in full
here: an entry moving together with its own attached comment block (§13.4, revised by
T41, see below), and a correctness fix to the diff/pending-count logic so a pure
reorder is never silently invisible at the one screen (DiffView) this app treats as a
hard safety gate (§13.7).

**Correction (T41).** The repo owner reviewed T40 after it shipped (`4be7d2a`, not
amended - see `PROJECT_PLAN.md`'s T41 section for the full history) and found §13.4's
original rule - stated unconditionally - wrong in a specific, common case: an ordinary
`#` comment written directly above an entry (the everyday fstab convention of
describing the entry below it - at the time, distinct from this app's own `# //`
description marker; as of T43 the backend absorbs *any* such comment block directly
above an `Entry` line into that entry's own `description` field at parse time, so it is
never a standalone comment line to begin with and this walk never sees it - see §13.10
case 6 and `candidate.rs`'s `attached_block_len` doc comment)
was left behind by a move, ending up describing the wrong entry afterward. §13.4, §13.5
and §13.8 below are rewritten to fix this - a comment block that is actually *attached*
to a moving entry, by the precise rule §13.4 now defines, now moves with it; truly
freestanding content still never moves, exactly as T40 originally specified. §13.7 is
confirmed unaffected (not just left alone - see its own closing paragraph) since it only
ever ranks row-kind lines. This note documents the correction in place, the same way §2
narrates T34 → T35.

### 13.1 Why Move up/down, and why not the other four

**Against drag-and-drop (#2)**: this app's entire UI today is zero drag interactions -
confirmed by reading `docs/UI_DESIGN.md` itself (this document) end to end before this
section existed. Adding one here would be a genuinely new interaction paradigm, not an
extension of an existing pattern, and the cost is not just implementation effort:

- A fully keyboard-operable drag reorder (WAI-ARIA's own grid/listbox reorder pattern)
  ends up needing something functionally equivalent to Move up/down buttons *underneath*
  the drag anyway, to satisfy the accessibility baseline this document already commits
  to (§8: "every interactive element has a visible focus state", disabled real buttons,
  not just visual dimming). Shipping drag-and-drop would mean shipping both, with drag
  as pure added surface area for the mouse-only case, not a replacement for anything.
- This app's whole visual language is deliberately calm and explicit everywhere else:
  named icon buttons with `aria-label`s, native `disabled` states, no hover-reveal
  "mystery meat" controls. A drag handle, a ghost row, and a drop-target indicator would
  be the first fluid, physics-feeling interaction in an otherwise entirely static,
  click-and-see-the-result UI - a style mismatch on top of the engineering cost, not
  independent of it.
- Precision matters more here than in, say, a kanban board: this app's own audience
  (people editing a system file, described in this task's own brief as skewing toward
  caution and precision) benefits more from "I clicked this exact button and the row
  visibly swapped by exactly one slot" than from "I dropped it somewhere that was
  probably where I meant."

**Against an order-number field (#3)**: it raises exactly the sub-questions the brief
itself names - what happens on a duplicate number while typing, whether gaps are
allowed, when renumbering of the other rows actually commits - none of which has an
established, discoverable answer in this app today, so choosing this option would mean
*first* resolving a second, harder design problem for the same underlying capability
Move up/down solves with zero new sub-questions. It is also a worse fit for the common
case: nudging one entry by one or two positions needs no arithmetic with Move up/down,
but typing a correct destination number requires the user to already know (or count)
the exact target rank. It does not resolve the freestanding-comment ambiguity (§13.4)
any more cleanly either - that ambiguity exists regardless of how "move" is expressed -
so nothing is gained for the extra complexity.

**Against a dedicated reorder view (#4)**: the existing table already shows exactly
what a user needs to decide how to reorder - source, mount point, fstype, and (critically)
the amber warning border this document already specifies in §3 for an ordering problem.
A separate view would have to either duplicate that context or strip it down to
something less informative. It also splits "see the current order" from "see whether
reordering fixed the validation problem" into two screens the user has to switch
between, which actively works against this task's own live-validation-feedback
requirement (§13.6) rather than helping it. Nothing about this task's actual complexity
- which turned out to be data-model correctness (§13.7), not UI surface - is made
easier by a second screen.

**For Move up/down (#1)**, honestly weighed: its own named downside - "tedious over
long distances" - is real but minor for this app's actual data. A real `/etc/fstab`
file realistically holds on the order of single digits to a couple of dozen entries,
not hundreds, so even a worst-case top-to-bottom move is a small, bounded number of
clicks. And because every click re-runs validation immediately and visibly (§13.6), the
one-step-at-a-time nature of this interaction is arguably the right fit for this app's
existing character, not just an accepted cost: nothing else in Rustab lets one click
cascade an effect the user can't immediately see and verify (the same "a toast, not a
silent swallow" standard `PROJECT_PLAN.md`'s T34 section holds the rest of this app to).

### 13.2 Row layout and new icons

Two new icon-only buttons, **Move up** and **Move down**, are added to the Actions
cell of both `EntryRow` and `UnparsedEntryRow`, to the *left* of the existing Edit
button, in this order: Move up, Move down, Edit, Delete. A small visual gap (`ml-2` on
the Edit button, or an explicit `border-l border-slate-200 dark:border-slate-700 pl-2`
divider before it) separates the Move pair from Edit/Delete, so the cell reads as two
groups - "change this entry's position" and "change this entry's content/existence" -
without inventing any new chrome (same spacing-only grouping technique already used
elsewhere in this document, e.g. the drawer's option-chip groups in §4).

- Icons: two new dedicated Lucide-derived SVGs added to the `icons` module (§9 item 33)
  - `ChevronUpIcon` (path `m18 15-6-6-6 6`) and `ChevronDownIcon` (path `m6 9 6 6 6-6`) -
  not a CSS-rotated reuse of `ChevronRightIcon`. `ChevronRightIcon`'s own rotation is
  appropriate because it represents one glyph's two states (collapsed/expanded, §3); Up
  and Down are two distinct, permanently-meaningful actions, so - matching how
  `EditIcon`/`DeleteIcon` are each their own dedicated path rather than a shared shape
  transformed - they get their own paths too.
- Styling: identical treatment to the existing Edit button - `p-2 rounded-md
  hover:bg-slate-100 dark:hover:bg-slate-700 focus-visible:ring-2
  focus-visible:ring-sky-500`, neutral slate on hover, **not** the accent/sky color
  (reserved for primary CTAs) and **not** red (reserved for Delete/real-target writes):
  moving an entry is reversible by clicking the opposite button and is neither the
  app's primary action nor destructive.
- `aria-label`: `"Move entry up"` / `"Move entry down"` (or the raw-row equivalent
  wording for `UnparsedEntryRow`).
- **Disabled state, not hidden**: the top row's Move up and the bottom row's Move down
  are real `disabled` buttons (same rule as the Save button in §5 - "an actual disabled
  state, not just a styling warning the user could click through"), `opacity-40
  cursor-not-allowed`, no hover background, with a `title` tooltip explaining why
  (`"Already the first entry"` / `"Already the last entry"`). A 1-row table disables
  both. Buttons are never removed from the DOM - their position in the Actions cell
  stays constant across every row, so nothing shifts around as a row becomes/stops
  being a boundary row.
- The `<th>` header label for this column stays `"Actions"` (unchanged) - it already
  covers "any icon button that acts on this specific row," which Move up/down are.

### 13.3 Keyboard accessibility and focus behavior

Move up/down are plain `<button type="button">` elements, so they are reachable by Tab
and operable with Enter/Space with zero extra code, consistent with every other
icon-only button in this table.

**Deliberately not adding a "sticky focus" mechanism** to let a keyboard user blast
through several positions by holding Enter without looking: `EntriesTable`'s rows are
keyed by `candidate_index`, which is a *position* in the candidate document (the same
index `on_edit`/`on_delete` already use as the row's identity - see
`frontend/src/candidate.rs`), not a stable per-entry id. After a swap, Yew's keyed diff
correctly re-renders the row at a given key with its new content, but that means a DOM
node that had keyboard focus keeps the focus while the *entry* under it changes - so a
second blind "press Enter again" would act on whichever entry now occupies that slot,
not the one the user originally meant to keep moving. Building a stable, moveable
per-entry focus target would require giving every row a position-independent identity
throughout the table (edit/delete included), which is a materially larger change than
this task's own scope boundary allows ("reordering ... must not require touching"
anything beyond producing a new candidate document order). The practical answer is
simpler and, for this app's audience, arguably more appropriate anyway: each click
immediately re-renders the table with the new order and the focus ring (§8) stays
visibly on a real `<button>`, so a user who wants to move an entry several steps just
looks at the row before each subsequent click - exactly the "look before you act on a
system file" posture this app already expects of its users everywhere else (e.g. the
non-dismissible-by-backdrop drawer/modals in §4/§6, there for the identical reason).

### 13.4 Attached comment blocks and the move-span algorithm (revised, T41)

**This subsection corrects and replaces T40's original §13.4.** T40 said: "moving an
entry up/down is a plain positional swap of exactly the two row-kind lines involved...
[freestanding content] is anchored to its position in the document, never to either
entry." The repo owner found this wrong for a case that rule did not distinguish: an
ordinary `#` comment (or a contiguous block of them) written directly above an entry is
a common, real-world fstab convention for describing that entry - a genuinely different
thing from a truly freestanding comment (a section header, a standalone note) that
describes nothing in particular. T40 left both kinds behind a moved entry equally; this
subsection draws the distinction T40 was missing and fixes the first kind, without
touching T40's correct handling of the second.

**Unchanged from T40, generalized by T43**: any ordinary `#` comment block sitting
directly above an `Entry` line, and a same-line trailing comment, are both baked into
that entry's own `description`/`trailing_comment` fields at parse time (`parser.rs`'s
`absorb_description_comments`/`split_trailing_comment`) - never standalone
`LineDto::Comment` values, so they always travel with their entry automatically. (T43
retired the old `# //`-marker requirement: it is no longer *only* a specially-marked
comment that gets absorbed this way, it is any ordinary comment block directly above an
`Entry`.) Nothing below concerns them. Everything below is about comment blocks the
backend's rule does *not* reach - directly above a `Malformed` line, or at true
end-of-file - which the parser leaves as their own standalone `LineDto::Comment` values
in `CandidateDocument.lines` - and `Blank` lines.

**The attached-comment-block walk.** For any row-kind (`Entry`/`Malformed`) candidate
line `E`, its **attached comment block** is the (possibly empty) run of comment lines
that moves with `E` whenever `E` moves. It is found by walking backward from `E`
through `CandidateDocument.lines`:

1. Skip zero or more consecutive `Blank` lines immediately above `E` - any number, not
   just zero or one. A comment separated from its entry by several blank lines is still
   reached by this step; blank lines alone never break attachment.
2. Look at the next line up (the first non-blank line reached, after step 1's skip, if
   any).
   - If it is **not** a `Comment` - another row-kind line, or nothing further (start of
     document) - `E` has **no** attached comment block. Stop.
   - If it **is** a `Comment`, this is the nearest comment block below, and it attaches
     to `E` in full. Find that block's own upper boundary by continuing backward from
     here *without* skipping any more blanks: as long as the immediately preceding line
     is also a `Comment` (strictly contiguous - a blank line inside the run would end
     it), it belongs to the same block. The walk stops at the first line that is a
     `Blank`, a row-kind line, or the start of the document. Everything from that
     stopping point (exclusive) down to `E` (exclusive of `E` itself) is `E`'s attached
     comment block - a single indivisible unit, however many lines long. It moves with
     `E` in full whenever `E` moves: never split, never partially left behind.
3. This walk cannot double-claim a comment block. A second, separate comment block
   sitting further back (above the one step 2 just found) is never reached by `E`'s
   walk, because step 2 stops at the very first comment block it meets going downward -
   attachment requires that no other comment block sit between the candidate block and
   `E`. That further-back block is evaluated independently, by running this same walk
   from its own position: the nearest thing below it, once blanks are skipped, is the
   block step 2 already found (not a row-kind line), so it has no owning entry and is
   **freestanding**.
4. A comment block at true end-of-file, with no row-kind line below it anywhere, is
   freestanding by construction: this walk only ever runs backward *from* a row-kind
   line, so a block with nothing below it is never visited by any walk, and keeps T40's
   original pinned-in-place behavior automatically.

**Attached vs. freestanding - what actually supersedes T40.** A comment block is
**attached** (and moves with its entry) exactly when some entry's walk above reaches
it; every other comment block is **freestanding** (cases 3 and 4 above), and a `Blank`
line not consumed by step 1 of some entry's walk is also freestanding (step 2 only ever
matches a `Comment` going backward, never a `Blank`, so a blank line is never itself
part of an attached block). T40's "freestanding content never moves, anchored to its
position" still holds, unchanged, for every genuinely freestanding line - it was only
wrong to call *every* comment freestanding.

**The move itself: swapping full spans.** Define an entry's (or malformed line's)
**move span** as its attached comment block (if any) immediately followed by its own
row-kind line - the whole unit that relocates together. Given the two row-kind indices
`EntriesTable` resolves for a Move click (§13.8, unchanged from T40 - the clicked entry
and its adjacent row-kind neighbor), call whichever one sits earlier in the document
`upper` and the other `lower` (by document position, not by which button was clicked -
a Move-up and a Move-down between the same two entries name `upper`/`lower`
identically). Let `upper_len`/`lower_len` be the length (possibly 0) of each one's own
attached comment block.

1. The lines strictly between `upper` (exclusive) and `lower`'s attached block's start,
   `lower - lower_len` (exclusive), are the **freestanding middle** - whatever already
   sat between the upper entry's own line and the lower entry's attached block,
   belonging to neither span. It never moves, by the same "freestanding content keeps
   its position" principle T40 established, now applied between the two full spans
   rather than the two single lines.
2. The whole affected range - from `upper`'s own block start, `upper - upper_len`,
   through `lower` inclusive - is rearranged in place to: `lower`'s full span, then the
   unchanged freestanding middle, then `upper`'s full span. Its total length is
   unchanged (lines are reordered within it, not added or removed), so **nothing
   outside this range shifts by even one index** - the same guarantee T40's plain
   two-element swap gave, now stated for a range instead of two slots.
3. Two facts fall out of step 2 and matter to the caller: `upper`'s own row-kind line
   always lands at index `lower`, regardless of either block's length (it is the last
   line of the last span placed in the range, and the range still ends at `lower`).
   `lower`'s own row-kind line lands at `upper - upper_len + lower_len` - *not* simply
   `upper`'s old index, whenever the two attached blocks differ in length. This pair of
   formulas is named `resolve_moved_indices(upper, upper_len, lower, lower_len) ->
   (new_upper, new_lower)` below, since both `stage_move` and `EntriesTable`'s own
   `expanded`-tracking need it and must use the exact same one (the same "one place
   decides" standard §13.7 already holds the diff/pending-count logic to).

**Contract.** `CandidateDocument` gains `attached_block_len(&self, row_index: usize) ->
usize` (the pure query behind the walk above - the length of `row_index`'s own attached
comment block, 0 if none; §13.8 reuses it for `expanded`) and a free function
`resolve_moved_indices(upper: usize, upper_len: usize, lower: usize,
lower_len: usize) -> (usize, usize)` implementing step 3's two formulas - the single
place either index fact is ever computed. `stage_move(&mut self, from: usize, to:
usize) -> Option<MoveOutcome>` replaces T40's plain `Vec::swap`:

> No-op (`None`, document untouched) under the same conditions as T40 - either index
> out of range, or `from == to`. Otherwise: computes `upper`/`lower` and their lengths
> via `attached_block_len`, performs the rearrangement in steps 1-2 above, calls
> `resolve_moved_indices` for the two new indices, and returns `Some(MoveOutcome {
> from_new_index, to_new_index })` - the *actual* new indices of the lines that were at
> `from`/`to` (mapped from whichever of `upper`/`lower` each one was), not assumed to be
> `to`/`from` the way T40's bare swap let the caller assume.

This is the index-shift subtlety's resolution: once the two entries' attached blocks
can differ in length, their final indices are no longer simply "swap `from` and `to`,"
so the computation that already has to happen to build the new line order also reports
its own answer, rather than leaving a caller to assume or re-derive it with a second,
possibly-drifting implementation. §13.8 covers exactly how `EntriesTable` uses this (via
`resolve_moved_indices` directly, not a round trip through `stage_move`'s return value -
see there for why).

### 13.5 (removed in T42)

The between-rows gap indicator previously specified here - a dashed-border row shown
between two table rows whenever a freestanding comment/blank line sat between them -
was removed entirely in T42, a repo owner's own decision, not a design question left
open: no reworded tooltip, no conditional suppression for any particular case, full
removal uniformly across every case it covered. The underlying attached-comment-block
move logic (§13.4) it sat on top of is unaffected and still ships; see
`PROJECT_PLAN.md`'s T42 section for the full rationale. This subsection number is left
unused rather than renumbering §13.6 onward, since other parts of this file and of
`PROJECT_PLAN.md` cite those by number.

### 13.6 Live validation feedback

No new plumbing is needed here - `stage_move` is staged into the exact same
`candidate` state every other mutation already goes through
(`frontend/src/components/app_shell.rs`'s `on_delete_confirm`/`on_drawer_save` are the
precedent: `let mut next = (*candidate).clone(); next.stage_move(from, to);
candidate.set(next);`), and that state already drives the `use_effect_with(candidate.
clone(), ...)` effect documented in §3 ("Document-wide warning indicator") that
re-runs `validate_candidate` against the whole document on every staged change. A move
is staged exactly like an add/edit/delete, so it participates in that same, already-
specified revalidation automatically. The result is shown exactly the same way §3
already specifies: the amber left-border + tooltip on whichever row(s) `line_index` the
finding is tagged to. If a move actually resolves a `findmnt`-ordering warning, that
row's amber border simply disappears on the next render once the async
`validate_candidate` call resolves - no new success state, modal, or toast is
introduced for this, matching this app's existing minimal, non-celebratory feedback
style for a warning going away anywhere else in the table.

The drawer's own `Verify` button and inline validation (§4, §5) are untouched - per
this task's own scope boundary, reordering is a table-level operation kept cleanly
separate from the drawer, which only ever concerns one entry's content.

### 13.7 Diff and pending-count correctness - the Reordered category (required)

**This is the one genuinely load-bearing correctness requirement this task
introduces, not an optional polish item** - it directly affects the diff-before-save
step this document's own priority reminder (top of this file) calls a hard gate that
"nothing in this document may obscure, shortcut, or let the user bypass."

`frontend/src/diff.rs::compute_summary_diff` and
`frontend/src/candidate.rs::CandidateDocument::pending_count` both currently classify a
line purely by comparing its *content* at its own `original_index` - they have no
notion of position at all. A plain `stage_move` swap exchanges two `CandidateLine`
values (content **and** `original_index` together), so each entry's content-at-its-
own-original-index comparison is unchanged by a swap: **a pure reorder with no content
edit currently produces zero Added/Modified/Removed entries and does not increment
`pending_count`**, verified directly against both functions' existing logic (confirmed
by reading `frontend/src/diff.rs` and `frontend/src/candidate.rs` while designing this
section). Concretely, this means `PendingChangesBar` could read "0 entries changed"
and `DiffView`'s Summary tab could show nothing at all for a session where the user did
nothing but reorder entries - exactly the kind of silent gap this app's whole diff-gate
philosophy exists to prevent. (The Raw diff tab is **not** affected - it is a textual
diff of full rendered file content, so a reorder already shows there correctly as a
line-level change with no fix needed.)

**Required fix - a fourth category, `Reordered`,** added to `SummaryDiff` (alongside
`added`/`modified`/`removed`) and to `pending_count`'s total. Detection, in prose (this
document's own convention - no Rust/Yew code is written here):

1. Take every row-kind line still present in the candidate document (i.e. its
   `original_index` is `Some` and that original index wasn't dropped by a delete - the
   same `still_present` set `compute_summary_diff`/`pending_count` already build today).
2. Rank those still-present lines twice: once by their order in the *original* document
   (ignoring any lines that were since removed, so ranks are comparable like-for-like),
   and once by their order in the *current* candidate document (ignoring any added
   lines, which have no original rank to compare against). Both ranks are plain
   0-based positions among only the lines in that comparison set.
3. Any still-present line whose original rank differs from its current rank has
   changed position - that is the `Reordered` set, keyed by `original_index`.
4. **Precedence**: a line whose *content* also changed is reported once, under
   `Modified`, never under both - `Reordered` only ever covers a line whose content is
   byte-identical to its original and whose rank alone differs. If a `Modified` entry's
   rank also changed, append one extra note to its existing card (§6: "If the same
   entry also changed position...") rather than listing it a second time.

This algorithm is correct for any combination of moves, edits, additions and deletions
made in any order before Review & Save is opened, not just a single isolated swap -
it compares final state to original state directly, the same way every other category
already does, so it can't drift out of sync with however many individual moves the
user actually made. `DiffSummaryList`'s new `Reordered (N)` group and its card styling
(`bg-sky-50 dark:bg-sky-900/20`, "Moved from position {old} to position {new}", both
1-based) are specified in §6. Where exactly the shared rank-comparison logic lives
(a method on `CandidateDocument` that both `pending_count` and `diff.rs` call, vs. two
independent call sites) is left to the implementer's judgment, with one hard
requirement carried over from T39's own closing lesson in `PROJECT_PLAN.md` - both
call sites must use the *same* logic, not two independently-written versions that can
quietly disagree.

**Confirmed unaffected by T41.** This category ranks only row-kind (`Entry`/
`Malformed`) lines (`is_row`, step 1's filter) - comment lines were never part of the
comparable set, before or after §13.4's fix. An attached comment block now physically
relocating with its entry changes no row-kind line's rank relative to any other
row-kind line beyond what the move itself already changed under T40 - confirmed by
re-reading this algorithm against §13.4's move semantics, not assumed.
`position_changes`, `pending_count`, and `compute_summary_diff` need no code change for
this fix.

### 13.8 Data model and component-contract summary (revised, T41)

For the frontend-developer implementing this (prose, matching this document's
no-code convention):

- `CandidateDocument` gains `attached_block_len(&self, row_index: usize) -> usize` and
  a revised `stage_move(&mut self, from: usize, to: usize) -> Option<MoveOutcome>`
  (both §13.4), plus the shared `resolve_moved_indices` free function. The
  rank-comparison helper behind the `Reordered` category (§13.7) is unchanged -
  confirmed, not just left alone (§13.7's closing paragraph). All of these stay pure,
  synchronous, in-memory operations on `CandidateDocument`, exactly like the existing
  `stage_add`/`stage_edit_entry`/`stage_edit_raw`/`stage_delete` - no backend, writer,
  or IPC surface is touched by this fix, matching the scope boundary.
- `EntriesTableProps`'s `on_move: Callback<(usize, usize)>` is unchanged in shape and
  meaning - `EntriesTable` still resolves `to` itself from the adjacent element in its
  own `rows` list, exactly as T40 specified, with no knowledge of comment blocks needed
  to do that. `AppShell`'s handler barely changes: clone `candidate`, call
  `stage_move(from, to)`, `candidate.set(next)` - the same structural match to
  `on_delete_confirm` T40 specified; the `Option<MoveOutcome>` return value can be
  ignored here (a no-op move leaves `next` equal to the clone, so setting it is
  harmless either way, same as T40's version already tolerated for a self-move).
- **`expanded` tracking, revised.** T40's `expanded: HashSet<usize>` mirroring assumed
  a plain swap, moving membership directly between `from` and `to`. That assumption
  breaks once the two entries' attached blocks can differ in length (§13.4 step 3) -
  the entry that was at `from` does not, in general, end up at `to`. The fix does
  **not** route anything back through `on_move`'s one-way callback (`AppShell` never
  reports back to `EntriesTable` - there is no such channel today, and inventing one
  would be a bigger structural change than this fix needs). Instead, `EntriesTable`
  computes the real outcome itself, synchronously, in the same click handler that
  already updates `expanded` and emits `on_move` (T40's existing structure) - using the
  **same** `resolve_moved_indices` function `stage_move` uses internally, fed by the
  new `attached_block_len` prop it already receives:
  `upper = min(from, to)`, `lower = max(from, to)`, `upper_len`/`lower_len` read from
  the prop map, then `resolve_moved_indices(upper, upper_len, lower, lower_len)` gives
  the real new indices, mapped back to whichever of `from`/`to` was `upper`/`lower`.
  `expanded`'s membership is then moved from `from`/`to` to these real new indices (if
  `from` was in the set, it moves to its real new index; same for `to`) instead of to
  the old, possibly-wrong `to`/`from`. Because `attached_block_len` is recomputed every
  render from the same `CandidateDocument` `EntriesTable` is currently showing, and the
  click handler reads it synchronously at click time, this prediction is always
  consistent with what `stage_move` is about to do - calling the *same* function both
  places is what makes that a guarantee, not a coincidence (the same "one place
  decides" standard §13.7 already holds the diff/pending-count logic to, now also
  covering this calculation). In the common case where both attached blocks are the
  same length (including the usual zero/zero case), this reduces exactly to T40's
  original swap - the fix only changes behavior where T40 would otherwise have been
  wrong.
- `EntriesTableProps` gains the one new prop named above, `attached_block_len:
  HashMap<usize, usize>` (§13.4), computed by `AppShell` once per render from
  `CandidateDocument::attached_block_len`, the same whole-document cadence as the
  existing `row_warnings` map. No other new prop is needed - this single map is enough
  for the `expanded`-tracking fix above. (T42 removed the gap-divider UI that also
  once consumed this prop; the prop itself stays, since `remap_expanded` above still
  needs it.)

### 13.9 Visual tokens used

Nothing new was invented for this task - every value above is a direct reuse: the
existing icon-button hover/focus/disabled treatment (§8's spacing/accessibility
conventions, and §5's "real `disabled` element" rule) for Move up/down; `bg-sky-50
dark:bg-sky-900/20` (the existing `accent` family, already used at this same tint
elsewhere, e.g. the row-expand panel's accent border in §3) for the new `Reordered`
diff card, chosen specifically because sky is this app's only "notable, non-alarming"
color - every other tinted card in DiffView is emerald (added/safe) or red
(removed/destructive), and a reorder is neither.

**T41 confirms this list is still complete.** The comment-attachment fix is
logic-only (§13.4/§13.8) - no new color, spacing, or icon token was needed.

**T42 note.** The gap divider that previously used
`text-slate-400`/`dark:text-slate-500` and `border-slate-200`/`dark:border-slate-700`
here was removed (§13.5); no replacement token is needed since nothing renders in its
place.

### 13.10 Required test coverage (T41)

Combining the repo owner's original list and both of their corrections
(`PROJECT_PLAN.md`'s T41 section), plus the index-shift fix's own needs (§13.4/§13.8).
Prose descriptions only - implementation writes the actual Rust, matching this
document's convention.

**Attachment walk (§13.4), exercised through `attached_block_len`/`stage_move`:**

1. A plain single `#` comment line directly (no blank) above an entry has an attached
   block of length 1, and moves attached to that entry when it is moved.
2. A multi-line (3+) contiguous comment block directly above an entry has an attached
   block of that exact length, and moves as one intact, unsplit unit attached to that
   entry - never split, never partially left behind.
3. A comment separated from its owning entry by **multiple** (2+) blank lines still has
   an attached block reaching that comment, and still moves attached to that entry when
   it is moved - this must not regress to "a blank line breaks attachment" (the
   orchestrator's own first, corrected-away draft of this rule).
4. A comment block that has **another, separate** comment block between it and the
   nearest entry below is **not** attached to that entry - and does not move when that
   nearby entry is reordered; it remains freestanding and pinned in place.
5. A comment block at true end-of-file, with no entry below it at all, has no owning
   entry (freestanding) and stays pinned in place regardless of any move elsewhere in
   the document.
6. The existing description-absorption behavior (parse-time absorption of any ordinary
   comment block directly above an `Entry` into `EntryDto.description`, pre-T40,
   generalized by T43 from the original `# //`-marker-only scheme) is unchanged from
   this walk's point of view - an absorbed description comment never becomes a
   standalone `LineDto::Comment` at all, so it never enters this logic's attached-block
   computation in the first place; a regression test confirms a document containing one
   round-trips and moves (via its entry's `description` field, not a `LineDto::Comment`)
   exactly as before.

**Move-span and index-shift (§13.4 steps 1-3, §13.8):**

7. Moving two entries whose attached comment blocks differ in length (e.g. one has a
   3-line block, the other none) relocates each one's own full span correctly, leaves
   the freestanding middle (if any) exactly in place, and `stage_move`'s returned
   `MoveOutcome` reports each entry's real new index - not the naive `to`/`from` a plain
   swap would assume.
8. `resolve_moved_indices`, tested directly with asymmetric block lengths, agrees with
   the indices `stage_move` actually produces for the equivalent scenario in test 7 -
   confirming the one shared function, not two independently-written computations that
   could disagree.
9. `EntriesTable`'s `expanded`-remap logic (consuming `attached_block_len` and calling
   `resolve_moved_indices` itself, per §13.8), given the same asymmetric-block scenario,
   moves an expanded row's membership to the entry's real new index, not to the old
   `to`/`from` T40's version assumed - the direct regression test for the bug this
   subtlety exists to prevent.

**Gap divider (§13.5): removed in T42**, along with its four dedicated tests
(`adjacent_entries_have_no_gap_between_them`,
`one_freestanding_line_between_two_entries_is_one_gap_line`,
`several_freestanding_lines_between_two_entries_are_counted_and_pluralized`,
`a_comment_block_attached_to_the_lower_row_is_excluded_from_the_gap_count`) - no
replacement coverage is needed since the UI surface they tested no longer exists.

### 13.11 Drag-and-drop reordering (T48; grab surface revised by T52)

**The problem**: T40/T41 gave every row Move up/Move down buttons - a precise,
keyboard-operable, fully accessible way to change an entry's position, and the only one
this app had until now. The repo owner asked for a second, additive way to do the same
thing: real pointer-based drag-and-drop, a mouse/touch enhancement, not a replacement -
the Move buttons stay exactly as specified in §13.2/§13.3/§13.8, unchanged, forever the
accessible baseline. Native HTML5 Drag and Drop is ruled out (clunky default ghost,
inconsistent cross-engine behavior - another WebKitGTK quirk, per `README.md`/
`docs/FLATPAK.md` §1.5); the mandated technique is `pointerdown`/`pointermove`/
`pointerup` plus a CSS `transition: transform` (FLIP-style: measure before, measure
after, animate the delta). Every discrete step a drag commits is the exact same
`stage_move`/`on_move((from, to))` call a Move-button click already makes - never a new
reorder algorithm, never more than one `on_move` emission per pointer event - so §13.4's
attached-comment-block semantics and §13.7's `Reordered` diff category keep working
completely unchanged under a drag; see §13.11's closing subsection below for the one
place this is worth spelling out anyway, even though nothing there requires a code or
doc change.

**Drag handle vs. whole-row grab.** T48 shipped a dedicated grip handle (in the Actions
cell, leftmost of the Move up/Move down group, with its own `GripVerticalIcon`);
**T52 replaced it with a whole-row grab**, on the repo owner's own direct instruction
after using T48's drag live - "drag should start from anywhere on the row, not just that
small icon." Everything else about the gesture was explicitly kept: the threshold
stepping, the `translateY` animation of displaced rows, the insertion line, the
one-`on_move`-per-pointer-event rule. Only the grab surface changed. The handle and its
icon are gone (§9 item 33).

T48's own reasoning for a handle was not wrong, it is outranked: the handle existed
precisely to dodge a collision with T32's row-expand click, and with the row as the grab
surface that collision has to be solved for real instead of avoided. It is solved
structurally, as follows.

**Two-phase gesture: pending press → committed drag.** `EntryRow`'s `<tr>` now carries
both T32's expand `onclick` and the drag's `pointerdown`, so the two gestures are told
apart by pointer *travel*, not by which element was hit:

- A `pointerdown` on the row records a **pending** gesture only - pointer id, the row's
  render position, the press's client x/y - and captures the pointer. It deliberately
  does *not* measure row geometry, does not touch any state that renders, and calls
  neither `preventDefault` nor `stopPropagation`. A press that never travels therefore
  remains, as far as the browser is concerned, exactly the plain click it was before
  this feature existed, and T32's expand/collapse toggle runs unchanged on the `click`
  the browser synthesises after `pointerup`.
- **Threshold: 5px of Euclidean travel** (`sqrt(dx² + dy²)`, both axes) from the press
  position. The first `pointermove` that clears it promotes the pending gesture: rows
  are measured, the layout freezes, a real drag state exists, and only then is
  `preventDefault` called. A distance over both axes rather than a per-axis test,
  because a drag is a drag however diagonally it starts (two 4px components are ~5.7px
  of real travel, which a per-axis 5px test would not notice). Small enough that a
  deliberate drag never feels like it has to be "unstuck," comfortably above the hand
  jitter an ordinary click carries. The promoted drag is anchored to the **original
  press position**, not to the point the threshold happened to be crossed, so the
  stepping geometry sees the gesture's true start and the dragged row does not jump by
  the threshold distance at the moment of promotion.
- **Click suppression is structurally outside the framework's render cycle, not a timing
  hope.** A promoted gesture still ends in a `click` the browser synthesises from the
  same mouse interaction, dispatched synchronously right after `pointerup` - before
  Yew's scheduler is guaranteed to have re-rendered with the drag state cleared. A
  handler built on the current render's `use_state` snapshot therefore cannot be trusted
  to see "this press just finished dragging"; that is a race, and this project's own
  "verify, don't assume" standard does not allow shipping one. Instead, promotion raises
  a plain shared mutable cell (a `use_mut_ref` flag, living outside the render cycle
  entirely), and the row's toggle handler checks *and clears* that flag synchronously as
  its very first act. One flag per table, not one per row per render: several renders
  happen between promotion and the synthetic click, so a cell re-allocated on each
  render would not be the same cell the click reads - the shared identity is the whole
  point. Any subsequent `pointerdown` on a row also clears the flag, so a promoted
  gesture whose synthetic click never arrived (released off the table, cancelled) cannot
  swallow a later, unrelated click.
- **Excluded descendants bail out before anything happens at all.** A `pointerdown`
  whose target is inside `button, pre` (resolved with `closest()`) returns immediately -
  no pointer capture, no pending gesture, no `preventDefault`, no `stopPropagation` - so
  those elements' native behaviour is left completely untouched rather than fought with
  `stopPropagation` after the fact. `button` covers Move up/Move down, Edit and Delete
  (and, for defence in depth, the expanded detail panel's copy/open-in-file-manager
  buttons, which sit on a row that carries no pointer wiring anyway). `pre` is
  `UnparsedEntryRow`'s raw-line block, and it is the load-bearing exclusion: that row has
  **no copy button for its raw text**, so selecting the text with a click-drag is the
  only way to copy the line at all, and a literal "anywhere on the row" grab would
  hijack every such selection into a reorder the instant it passed 5px - a real
  regression, in exactly the one place in this table where manual text selection is the
  only available affordance (`EntryRow`'s Source/Mount point have copy buttons in the
  detail panel; this row has nothing). Excluding `pre` preserves that selection and
  still leaves the rest of the row (its padding, the warning icon and message text, the
  amber background) fully grabbable.
- **Scope: the main `<tr>` only.** The pointer handlers go exactly where T32's `onclick`
  already lived - `EntryRow`'s main row, and `UnparsedEntryRow`'s single row - and
  deliberately **not** on `EntryRow`'s expanded detail `<tr>`, which is a separate
  element. That is the same surface T32 used, not a narrowing, and it has a useful
  side effect: the detail panel's plain-text fields that have no copy button
  (Description, Trailing comment) stay normally selectable for free, by the same
  reasoning as the `pre` exclusion and without needing a selector of their own. Both
  row kinds get the whole-row grab, the same parity T48 had.

**Cursor at rest: unchanged, and deliberately no new resting grab affordance.**
`EntryRow` keeps `cursor-pointer` (it is still, far more often, a click-to-expand row);
`UnparsedEntryRow` keeps the default cursor. T48's handle showed `cursor-grab` at rest
because the handle *was* this gesture's entire discoverable surface; with the whole row
grabbable, showing "grab" everywhere at rest would compete with - and, on a dense table,
visually drown out - the click-to-expand affordance that remains the much more common
interaction. The `cursor-grabbing` treatment stays exactly as T48 shipped it: applied at
the row level and at the document level once a drag is actually in progress, never at
rest. This is a settled decision rather than an oversight - the repo owner's live
feedback settled "whole row grab," not a new discoverability design, and Move up/Move
down remain the fully discoverable, keyboard-operable baseline (see **Accessibility**
below), so the drag gesture needs no hover affordance to be justified.

**Text selection during a drag (T52 fix to a T48 bug).** T48 blocked text selection with
`user-select: none` on `<body>`, applied from an effect keyed on "is a drag in progress."
That lost a race it could not win: an effect cannot run until the framework has processed
a state update and re-rendered, whereas the browser's own "press, then move, therefore
select text" behavior is gated on nothing - so dragging a row visibly highlighted its
text (reported from live use; present since T48, and T52's pending phase, which exists
before any drag state at all, only widened the window). The fix is to stop routing a
latency-critical style change through the render cycle: a non-excluded `pointerdown` sets
`user-select: none` on `<body>` **synchronously, inside the handler itself**, and the
gesture's `pointerup`/`pointercancel` handler clears it again, also synchronously, inside
the same pointer-id gate the rest of the teardown uses. A press that stays a plain click
therefore sets and clears it within one synchronous handler pair, before the synthetic
`click` is even dispatched; a real drag holds it for exactly the gesture's duration.
`button`/`pre` presses are unaffected, since this sits after the exclusion bail-out -
`UnparsedEntryRow`'s raw-line selection keeps working. The effect is kept as-is for
`cursor: grabbing` (not latency-critical) and is now merely an idempotent backstop for
`user-select` plus the unmount-mid-gesture cleanup path.

`preventDefault()` on `pointerdown` was considered as the alternative fix and
**deliberately rejected**: per the Pointer Events spec it suppresses that interaction's
compatibility mouse events, which for mouse input plausibly includes the native
`mousedown`/`mouseup`/`click` sequence - i.e. the exact click-to-expand path this
two-phase gesture exists to preserve. Whether WebKitGTK does that cannot be settled
without a live session, and `user-select` is orthogonal to click synthesis, so the CSS
route sidesteps the question instead of assuming an answer. The `preventDefault()` call
at the moment of *promotion* (once the gesture is already committed to being a drag) is
unchanged.

**What is and is not verified about this fix.** There is no DOM/pointer test harness in
the frontend crate (§13.12), so "WebKitGTK no longer starts a selection" is not
unit-tested and was not visually confirmed in a browser by the implementation. What is
guaranteed by code trace is the structural property the fix rests on: the style mutation
happens synchronously inside the `pointerdown` handler, which runs to completion before
the browser dispatches any subsequent native pointer event for the same input stream, so
there is no window of any size between "the press began" and "selection is disabled."

**Touch (T52 note).** T48's handle carried `touch-action: none`, which a whole-row grab
cannot inherit: putting `touch-action: none` on every row would make the table
unscrollable by touch, a far worse regression than losing touch-drag. So a touch drag on
a row is preempted by the browser's own scrolling - the gesture receives
`pointercancel`, ends cleanly, and keeps whatever steps it had already committed - and
touch users reorder with Move up/Move down, which remain the baseline path for every
input modality. Mouse/trackpad drag is unaffected.

**Visual treatment, drop indicator, animation.** For the one row following the pointer
(the orchestrator's own note that other rows must visibly slide to make room is assumed
as the required visual result; the mechanism behind that slide is the implementer's
job, not specified here):

- Elevation: `shadow-lg` - the existing drawer/modal elevation token (§8), reused rather
  than inventing a new shadow value, strong enough to read as "lifted off the table"
  against the row's own optional `shadow-sm` hover lift and the table's own `shadow-sm`
  container.
- Opacity: `opacity-90`, not a stronger fade. Deliberately subtle: the repo owner's own
  objection to native HTML5 drag-and-drop was its "clunky default ghost" - a heavily
  faded drag image feeling disconnected from what is actually being moved. Staying close
  to fully opaque keeps the dragged row reading as "the real row, just lifted," not a
  ghost.
- Cursor: `cursor-grabbing` on the rows and, for the duration of the gesture, at the
  document level (so it does not flicker back to default as the pointer crosses other
  rows). T48 also had `cursor-grab` on the handle at rest; T52 removed that along with
  the handle and adds no resting replacement - see "Cursor at rest" above.
- Elevated above sibling rows in paint order for the duration of the gesture only
  (whatever `z-index`/portal mechanism the implementer's FLIP approach supports), since
  it visually overlaps neighbors as it translates past them.
- No `transition` on the dragged row's own transform - it must track the pointer's Y
  position 1:1, every `pointermove`, with zero easing delay. Only the *other*, displaced
  rows get an animated transition (next point); applying the same transition to the
  dragged row itself would make it visibly lag behind the cursor, breaking the "glued to
  the pointer" feel a drag interaction depends on - a usability defect, not a style
  nitpick.

For the other, displaced rows: `transition: transform 150ms ease-out` - reusing the
drawer's own existing motion duration/easing (§8 "Motion": "drawer slides in/out over
150-200ms ease-out"), the closest existing precedent to a "something moves into view"
animation this app already specifies, rather than inventing a new arbitrary duration.
Per §8's existing `prefers-reduced-motion` rule ("disabling the transform/scale and
keeping only an opacity fade"): an opacity fade does not make sense for a pure position
change, so the reduced-motion fallback here is to skip the animated transform outright
and let every affected row snap directly to its post-move position with no transition at
all - the cleanest equivalent of that rule's intent for a case it was not originally
written for.

Drop-target indicator: the live, animated slide itself is the primary indicator - every
displaced row genuinely slides toward the gap that is about to open at the dragged
entry's prospective new position, so the visible gap, not a separate marker, is what
tells the user where it will land (matching the task brief's own framing: "other rows
visibly slide to make room"). On top of that, add one explicit insertion line -
`border-t-2 border-sky-500 dark:border-sky-400`, full row width, drawn at the exact
boundary the dragged row would land on if released now, visible only while a drag is in
progress. Not purely decorative: the dragged row itself is elevated and can visually sit
over more than one candidate gap as it moves, so an explicit line removes any remaining
ambiguity about exactly which boundary is current - the same "precision matters more
here than in a kanban board" standard §13.1 already used to prefer Move buttons'
exactness over drag-and-drop's fuzziness; this line gives the drag gesture that same
precision. Sky, not a new color: §13.9 already names sky as "this app's only notable,
non-alarming color," reused here at full `sky-500`/`sky-400` - a stronger weight than
the `sky-50`/`sky-900/20` tint used for the Reordered diff card and the row-expand
accent border, since this is a transient, actively-watched indicator rather than a
resting background tint, but still the same hue family, not a new token.

**Icon: none (T52).** T48 added a dedicated `GripVerticalIcon` (Lucide's
`grip-vertical`) to the `icons` module for the handle. T52 removed the handle, so the
icon has no call site left and was deleted along with it (§9 item 33) rather than kept
as dead code - the module's own stated convention is one dedicated path per
*permanently-distinct action*, and this one no longer names anything the UI renders.
`ChevronUpIcon`/`ChevronDownIcon` (Move up/Move down) are untouched.

**Accessibility.** Confirmed explicitly: the drag gesture is never the *only* way to
reorder. Move up/Move down (§13.2/§13.3) already satisfy this app's accessibility
baseline - keyboard-operable, visible-focus, real `disabled` boundary states - entirely
on their own, before this task existed; nothing here changes that, and nothing here is
allowed to regress it, per the brief's own "Move buttons must stay exactly as they are"
instruction.

T48's decision was that the handle must not be keyboard-focusable, because a focusable
control implies an activation the keyboard can trigger and there is no sensible keyboard
activation for "start a continuous pointer drag" - Enter/Space on it could at best do
nothing (a control that takes focus but does nothing is worse than one that was never
reachable, the same "don't fake an affordance" standard this app applies via real
`disabled` buttons rather than fake dimming), or at worst invite a future "fix" bolting
on a redundant keyboard-reorder mode that duplicates Move up/Move down. **T52 preserves
that outcome by construction, and more cleanly:** there is no handle, nothing new is
focusable, nothing new is in the tab order, and the rows themselves are no more
focusable than they were before (they were never focusable; the pointer gesture adds no
`tabindex`, no `role` and no ARIA attribute to them). The handle's `title` tooltips
("Drag to reorder" / "Drag to reorder raw entry") are gone with the handle; the gesture
is now an undiscoverable-by-tooltip mouse enhancement on top of two permanently visible,
labelled buttons that do the same job - the deliberate tradeoff recorded under "Cursor at
rest" above. Nothing about the keyboard path changed: Tab order, focus rings and the
Move buttons' `aria-label`s/`disabled` states are exactly as §13.2 specifies.

**Interaction with §13.6 and §13.7 (documentation-only notes).** Both are structurally
unaffected, confirmed independently here as well, not just taken on the orchestrator's
own word - with two things worth writing down explicitly so a future reader is not
surprised by live behavior that is correct but easy to misread as a bug:

1. **§13.6 (live validation feedback) may visibly flicker during a multi-row drag, not
   just once at drop.** Because each row-boundary crossed during a drag commits its own
   `stage_move` the moment the pointer crosses the threshold (per the orchestrator's own
   binding "at most one `on_move` emission per discrete pointer event" rule), a drag
   spanning several rows re-runs the whole-document `validate_candidate` effect once per
   boundary crossed while the pointer is still moving - exactly as if the user had
   clicked Move several times in a row in quick succession. A row's amber warning border
   can therefore appear and disappear more than once during a single drag gesture,
   settling on its final state only once the pointer stops (or the drag ends). This is
   the same behavior §13.6 already specifies for repeated Move clicks, not a new code
   path - stated here only so it is not mistaken for a bug during manual testing.
2. **§13.7's `Reordered` category is computed from final-vs-original document state, not
   from a count of steps**, so a drag that crosses five row boundaries produces exactly
   the same `Reordered` set as five individual Move-button clicks that reach the same
   final arrangement - never a drag-specific entry, never a different count than the
   equivalent button clicks would have produced. The "one click can report two
   `Reordered` lines" behavior T40's own final write-up already called out as deliberate
   (ranking individual lines, not operations) carries over unchanged: a drag is just a
   sequence of those same operations.

No code, test, or copy change is implied by either note - they exist here purely so a
reviewer reading a live drag does not mistake either behavior for new, task-specific
logic that needs its own spec.

### 13.12 Required test coverage addendum - drag-and-drop (T48, extended by T52)

Matching §13.10's own split between what a pure function can cover and what needs a
live DOM, prose descriptions only. There is deliberately **no DOM/pointer test harness
in the frontend crate** (no `wasm-bindgen-test`), which is exactly why the split below
exists and why every decision worth testing is extracted into a pure function first.

**Unit-testable (pure functions, no DOM):**

0. **Click vs. drag (T52):** given the pointer's travel from where it went down, whether
   that press has become a drag - the 5px Euclidean threshold predicate, extracted as
   its own pure function in the same spirit as items 1-4 below. Covers: zero travel is
   still a click; sub-pixel jitter is still a click; just-under/just-over the threshold
   in both directions on both axes; purely horizontal travel counts as a drag too; a
   diagonal combination where neither axis clears 5px alone but the real distance does
   (the case a per-axis test would get wrong); and that exactly-on-the-threshold counts
   as travel, so there is no gap between "click" and "drag".
1. Given a pointer Y position, a row's own top/height, and the direction of travel,
   which side of the row's midpoint (or whatever exact threshold the implementer picks -
   e.g. 50% of row height) the pointer currently sits on, and therefore whether a given
   row-boundary has been "crossed" yet - the pure geometry decision behind "should this
   `pointermove` trigger a step" at all.
2. Given a list of row heights/positions and a currently-dragged row's index, which
   other rows need a transform applied for the "slide to make room" effect, and by how
   much (the FLIP delta computation) - independent of whether that transform is actually
   animated in a live browser.
3. The "at most one `on_move` emission per discrete pointer event" rule itself: given a
   sequence of pointer Y positions that collectively cross more than one row boundary,
   confirm the chosen step logic calls out at most one boundary-crossing per event
   (whatever data structure records "pending steps" or "already-committed-for-this-
   event" state), never a synchronous loop of several moves derived from one event.
4. Reuse of the existing move mechanism: a drag's single committed step, given the same
   `from`/`to` candidate indices a Move-button click would have produced for the same
   adjacent pair, calls the same `on_move`/`stage_move` path with the same arguments -
   i.e. this needs no new move-resolution logic of its own, only new pointer-position-
   to-`(from, to)` translation (verifiable as a direct equality/trace assertion against
   §13.8's existing `on_move` contract, not a new algorithm to test from scratch).

**Manual/code-review only (needs a real DOM, pointer events, and visual judgment - no
meaningful unit test exists for these):**

5. **The whole-row `pointerdown`/`pointermove`/`pointerup` wiring (T52)**, i.e. the
   collision §13.11's two-phase gesture exists to resolve, in three scenarios that all
   need real pointer events on a real DOM and so are verified by code trace plus manual
   use, never by a unit test:
   - A plain press-and-release anywhere on a row's own surface (no travel, or under 5px)
     still toggles the expansion panel exactly as before T52 - nothing captured the
     click, nothing prevented the default, the toggle handler's suppression flag is
     clear.
   - A press that travels past 5px reorders the row and does **not** toggle expansion:
     the synthetic `click` the browser fires after `pointerup` is swallowed by the
     promotion flag (§13.11), and the flag is cleared by that same click so the next one
     toggles normally.
   - A press that lands on an excluded descendant (`button, pre`) behaves natively and is
     wholly unaffected: the Move/Edit/Delete buttons still activate on click, and
     `UnparsedEntryRow`'s raw-line text can still be selected by click-dragging across
     it without the row being picked up.
6. That the dragged row visually tracks the pointer with no lag while other rows animate
   with the specified 150ms ease-out transition, that the drop-indicator line renders at
   the correct boundary as the pointer moves, and that releasing the pointer commits
   whatever the indicator was last showing.
7. `prefers-reduced-motion` actually suppresses the row-slide transition (rows snap
   instantly) without breaking the drag itself.
8. Keyboard-only operation: confirm the drag gesture adds nothing to the tab order (T48:
   the handle was unreachable by Tab; T52: there is no handle, and the rows are not
   focusable) and that Move up/Move down remain fully operable and unaffected by any of
   this task's changes - the direct regression check for "Move buttons must stay exactly
   as they are."
9. Both `EntryRow` and `UnparsedEntryRow` are grabbable from their own main `<tr>` (T52),
   with `UnparsedEntryRow`'s raw-line `<pre>` excluded; `EntryRow`'s expanded detail
   `<tr>` carries no pointer wiring at all, so a press inside the open detail panel never
   starts a drag.

---

## SPEC §9 checklist (for reviewer sign-off)

- [x] Table/list view of all current entries, including unparsed ones shown as raw
  text + warning, never dropped - §3
- [x] Add/edit/delete form with structured source-type dropdown + device picker,
  mount point + directory picker, filesystem type, options checklist/tag-input with
  toggles for common options, dump, pass - §4
- [x] Inline validation feedback (duplicate mount point, mount point missing on disk,
  malformed UUID) before save is allowed, and it blocks save - §5
- [x] Clear pending-changes/diff view before committing a save - §6
- [x] Visible, unambiguous indicator of which file a save writes - real `/etc/fstab`,
  the working draft that is saved through to it, or an isolated test target - §2
- [x] Backup/restore UI: list of recent auto-backups with restore action - §7
- [x] Tailwind styling, dark-mode friendly, modern/clean - §8
