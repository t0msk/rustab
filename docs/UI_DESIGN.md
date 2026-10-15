# Rustab — UI/UX Design Spec

This document is the implementation contract for the Rustab frontend (T10/T11/T12).
It covers every GUI requirement in SPEC §9 by name and gives concrete, Tailwind-based
values — colors, spacing, typography, component states — so a frontend developer does
not need to invent any structural or visual decision. Where a decision required
inventing something outside this document's remit (brand identity, backend behavior),
it is explicitly flagged rather than guessed.

This is a design spec, not code: no Yew/Rust is written here. Tailwind utility class
names are given as concrete design tokens (per the brief's instruction to be
implementable, not a mood board), not as a literal implementation.

**Priority reminder (SPEC §2):** nothing in this document may obscure, shortcut, or
let the user bypass the diff-before-save step or the real-vs-sandbox target
indicator. Every flow below treats those two as hard gates, not optional
affordances.

---

## 1. Overall layout

Single-window desktop app, no page routing — view switching is in-memory state
(matches T10's "routing/view-state skeleton" using Yew state, not a router).

```
┌─────────────────────────────────────────────────────────┐
│ TargetBanner  (always on top, full width, never hidden)  │  <- safety-critical, §2
├─────────────────────────────────────────────────────────┤
│ HeaderBar: "Rustab" · [Entries] [Backups] tabs · Add Entry│
│ (Entries tab) / Create backup (Backups tab)               │
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
  and *not* dismissible by clicking the backdrop — it is the final safety gate before a
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
currently targeted — this must never be ambiguous to the user")

Component: **TargetBanner**. This is the single most safety-critical visual element in
the app and is designed to be *impossible to miss*, not a subtle label.

- **Position**: full-width, fixed at the very top of the window, above the HeaderBar.
  Height 40px. Never scrolls away, never covered by any overlay (highest z-index in the
  app, drawers/modals render below it or the banner re-renders on top).
- **Content**: an icon + bold uppercase lead word + the literal resolved file path, e.g.
  `⚠ REAL SYSTEM FILE — /etc/fstab` or `🧪 SANDBOX — /home/user/rustab/dev-data/fstab.sandbox`.
  The full path is always shown in full (truncated with an ellipsis + tooltip only if
  genuinely too long for the window), never abbreviated to just a filename — ambiguity
  between two similarly-named files is exactly what this banner exists to prevent.
- **Two states, deliberately asymmetric in alarm level** — this is the explicit
  requirement that real-target mode must look more "alarming" than sandbox mode, not
  just differently colored:

  | | **Real target** | **Sandbox / dev target** |
  |---|---|---|
  | Background | `bg-red-600` (`#dc2626`) | `bg-emerald-600` (`#059669`) |
  | Text | `text-white`, `font-bold`, `uppercase`, `tracking-wide` | `text-white`, `font-semibold`, normal case |
  | Icon | triangle-exclamation (hazard), filled | flask/beaker (sandbox/test), outline |
  | Extra texture | subtle 8px diagonal hazard-stripe pattern at ~8% white opacity across the banner background (`repeating-linear-gradient(135deg, rgba(255,255,255,.08) 0 8px, transparent 8px 16px)`) | none — flat color, calm |
  | Border | `border-b-2 border-red-800` | `border-b-2 border-emerald-800` |
  | Copy | `"REAL SYSTEM FILE — {path}"` | `"Sandbox target — {path}"` |

  The red/striped/uppercase/bold treatment for real-target mode is a dedicated design
  decision, not reused anywhere else in the app at this intensity — it exists only here
  and nowhere else should a full-width striped red banner appear, so the user's
  pattern-match of "this specific look = I am touching the real system" stays reliable.
- **Data need (not a backend decision)**: TargetBanner needs `{ path: String, is_real:
  bool }` from whatever "get active target" call T8 exposes. It re-renders live if that
  value ever changes during the session (e.g. a future settings change) — never cached
  at startup only.
- The same `is_real` flag is reused (not re-invented) to color-code the confirm button
  in DiffView and BackupRestoreConfirmDialog (§6, §7), so the "danger" signal is
  consistent end-to-end through the save flow, not just on the banner.

---

## 3. Entries table/list view (SPEC §9: "Table/list view of all current entries
... including ones Rustab doesn't 'understand' perfectly — show raw text with a
warning rather than dropping them")

Component: **EntriesTable**, rows are **EntryRow** (parsed) or **UnparsedEntryRow**
(malformed). Comments and blank lines are preserved for serialization but are not
rendered as table rows — the table only ever lists `Entry` and `Malformed` lines
(this is a UI display decision, not a change to backend round-trip behavior: comments
and blanks are never dropped, just not given their own row in this list view).

**Columns (EntryRow):**

| Column | Content |
|---|---|
| Source | Small type badge (`UUID`, `LABEL`, `PARTUUID`, `PARTLABEL`, `PATH`, `NET`, `NONE`) + the value, `font-mono text-sm` |
| Mount point | `font-mono text-sm` |
| FS type | plain text badge, `text-sm` |
| Options | first ~3 options as small chips + `+N more` overflow; full ordered list visible on hover tooltip and always fully editable in the drawer |
| Dump | single digit |
| Pass | single digit |
| Actions | Edit (pencil icon button) · Delete (trash icon button, `text-red-600` on hover) |

Row styling: `bg-white dark:bg-slate-800`, alternating/zebra optional (`odd:bg-slate-50
dark:odd:bg-slate-800/60`), hover `hover:bg-sky-50 dark:hover:bg-slate-700/60`, row
height ~44px, `border-b border-slate-200 dark:border-slate-700`.

**UnparsedEntryRow (malformed line):**

- Full-width single row (spans all columns), background `bg-amber-50 dark:bg-amber-900/20`,
  left border accent `border-l-4 border-amber-500`.
- Content: a warning icon (amber triangle) + label `"Couldn't fully parse this line —
  shown as raw text"` + the raw original line text verbatim in `font-mono text-sm`
  inside a `bg-slate-900/5 dark:bg-slate-950/40` inset block so it visually reads as
  "quoted raw text," not as a normal structured row.
- Still has Edit and Delete actions. Edit opens the EntryDetailDrawer in a "raw text"
  sub-mode (a single multi-line text field containing the raw line, since structured
  fields cannot be offered for a line the parser couldn't decompose) rather than the
  normal structured form. Delete removes the line like any other row. The line is never
  silently hidden or auto-corrected — this satisfies SPEC's explicit "never dropped or
  hidden" requirement.
- `EmptyState` variant: if the table has zero entries (e.g. a truly empty fstab), show
  a centered placeholder: icon + `"No entries yet"` + a primary `Add Entry` button.

---

## 4. Add/edit/delete form (SPEC §9 structured inputs)

Component: **EntryDetailDrawer**, slides in from the right, fixed width 520px (full
width below the 760px breakpoint), `bg-white dark:bg-slate-800`, `shadow-lg`, backdrop
`bg-black/40` behind it (click-outside *does* close this one, unlike DiffView, since
it's a non-destructive in-progress edit, not a final write).

Field order, top to bottom, each with a `text-sm font-medium` label above it and
`space-y-4` (16px) between fields:

1. **SourceTypeSelect** — dropdown. Options, per SPEC §9's literal list (path / UUID /
   LABEL / PARTUUID / none) **extended** to also cover PARTLABEL and Network, since
   `fstab-core`'s `SourceId` model (T2) already defines those variants and the dropdown
   must be able to represent every value the backend can round-trip:
   `Device path` · `UUID` · `LABEL` · `PARTUUID` · `PARTLABEL` · `Network (NFS/CIFS)` ·
   `None`. *(Flagging this as a transparent completeness extension of the §9 wording,
   not a visual/brand judgment call.)*
2. **Source value field** — changes shape based on the selected type:
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
     `{ path, uuid, label, partuuid, partlabel, fstype }` — sourced from whatever
     discovery call T7/T8 exposes. If the list is empty (tools unavailable), DevicePicker
     shows `"No devices discovered — enter a value manually"` and the manual field stays
     usable; it is never a blocking state.
3. **Mount point** — text input (`font-mono`) + a `Browse…` button opening a native
   directory picker (conceptually: open a directory chooser, receive a path string
   back — the concrete Tauri API is not this document's decision).
4. **Filesystem type** — free-text combobox: typing shows a suggestion dropdown of
   common types (`ext4, btrfs, xfs, vfat, ntfs3, swap, none, nfs, nfs4, cifs, overlay,
   tmpfs, proc`) but any value can be typed and accepted — no closed enum, matching
   SPEC §5.
5. **OptionsTagInput** — the SPEC §9 "checklist/tag-input for options":
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
   - All of the above — toggled well-known chips, systemd/vfat value chips, and free
     typed tags — render as one single flowing, removable-chip list in the exact order
     they'll be serialized (SPEC's "ordered key/key=value list"), so the user always sees
     the real resulting option order, not three disconnected UI groups.
   - Chip styling: active/selected `bg-sky-100 text-sky-700 dark:bg-sky-900/40
     dark:text-sky-300`, inactive/available-but-off `bg-slate-100 text-slate-600
     dark:bg-slate-800 dark:text-slate-400`, each chip `rounded-full px-3 py-1 text-xs
     font-medium`, with an `×` to remove.
6. **Dump** and **Pass** — two small numeric inputs side by side (`w-16` each), default
   `0`, helper text under each: `"0 = don't dump"` / `"0 = skip fsck, 1 = root
   filesystem, 2 = checked after root"`.

**FormActionBar** (bottom of drawer, sticky): `Cancel` (secondary, `bg-slate-100
dark:bg-slate-700`) and `Save entry` (primary, `bg-sky-600 hover:bg-sky-700 text-white`,
disabled while blocking validation errors exist — see §5). Saving here only stages the
change in memory as a pending edit; it does **not** write to disk. Disk writes only ever
happen through the DiffView confirm step (§6).

**DeleteConfirmDialog**: small centered modal, `"Remove this entry from {mount
point}?"` + the entry's source/mount-point/fstype summarized read-only, `Cancel` /
`Remove` (`bg-red-600 hover:bg-red-700 text-white`) buttons. Like edit, this only stages
a pending removal — the actual file is untouched until DiffView is confirmed.

---

## 5. Inline validation feedback (SPEC §9: "duplicate mount point, mount point
doesn't exist on disk, malformed UUID" before allowing save)

Component: **ValidationMessage**, a generic renderer that consumes
`{ field: FieldId, message: String, severity: "error" | "warning" }` per field — the
severity classification itself (which checks are hard-blocking vs. advisory) is a
backend/validator decision (T5), not invented here; the UI simply renders whatever
severity the validation response assigns, so e.g. duplicate mount point and malformed
UUID are expected to arrive as `error`, while something like "mount point doesn't exist
yet" *could* legitimately arrive as either `error` or `warning` depending on context the
validator has and the UI doesn't need to know — this document only fixes how each
severity looks and behaves:

- **error** (blocking): field border becomes `ring-2 ring-red-500`, message text
  directly below the field in `text-red-700 dark:text-red-400 text-xs` with a small
  error icon. **The drawer's `Save entry` button is disabled** (`bg-slate-300
  text-slate-500 dark:bg-slate-700 dark:text-slate-500 cursor-not-allowed`) with a
  tooltip `"Fix validation errors before saving"` — this is an actual disabled state,
  not just a styling warning the user could click through.
- **warning** (non-blocking): field border `ring-1 ring-amber-400`, message in
  `text-amber-600 dark:text-amber-400 text-xs` with a caution icon. Does not disable
  Save.
- A warning that is specifically about the mount point of the entry currently being
  edited (the backend's `field`/`line_index` tagging on the finding, matched against
  this draft's own position in the document) renders inline, directly below the
  mount point input, using the same `ValidationMessage` styling (`text-amber-600
  dark:text-amber-400 text-xs` with a caution icon). Every other warning — including
  a mount-point-tagged one about a *different* entry elsewhere in the document —
  still renders in the flat warning stack described below, not next to any field.
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
**PendingChangesBar** appear — a slim bar pinned just below HeaderBar, `bg-amber-50
dark:bg-amber-900/20 border-b border-amber-300 dark:border-amber-800`, text `"N entries
changed"` + a primary button `Review & Save…`. This bar is the only way to reach the
diff/write step; there is no other Save action that skips it.

Clicking `Review & Save…` opens **DiffView**: a centered modal (`max-w-4xl`, up to
~85vh tall with internal scroll), `bg-white dark:bg-slate-800 shadow-lg rounded-lg`,
backdrop `bg-black/50`. **Not** dismissible by clicking the backdrop (Escape still
cancels) — this is intentionally stricter than EntryDetailDrawer because this is the
last gate before an actual write.

**Contents, top to bottom:**

1. Header: `"Review changes before saving"` + a small inline reuse of the TargetBanner
   color coding (a compact badge, same red/emerald logic from §2) showing which file
   will actually be written — the danger signal must carry through to this final
   screen, not just live on the top banner.
2. Two tabs, **Summary** (default) and **Raw diff** — both are required, satisfying
   both halves of the brief ("structured per-field change list" and "exact textual
   diff"):
   - **DiffSummaryList** (Summary tab): entries grouped under `Added (N)` / `Modified
     (N)` / `Removed (N)` headings.
     - Added entries: whole-row summary on a `bg-emerald-50 dark:bg-emerald-900/20`
       tinted card.
     - Removed entries: whole-row summary on `bg-red-50 dark:bg-red-900/20`, text
       `line-through text-slate-500`.
     - Modified entries: one card per entry, listing only the fields that actually
       changed as `label: ~~old value~~ → **new value**` rows — old value muted
       strikethrough, new value bold, unchanged fields not repeated here at all.
   - **DiffRawText** (Raw diff tab): classic unified-diff rendering of the full
     candidate file text against the current target file text, `font-mono text-sm`,
     line-numbered gutter, `-` (removed) lines `bg-red-50 text-red-700 dark:bg-red-950/40
     dark:text-red-300`, `+` (added) lines `bg-emerald-50 text-emerald-700
     dark:bg-emerald-950/40 dark:text-emerald-300`, unchanged context lines neutral
     `text-slate-600 dark:text-slate-400`. This is the exact textual before/after of
     the real file content, including comments/blank-line positions, so a careful user
     can verify byte-level correctness, not just Rustab's interpretation of the change.
3. Footer action bar:
   - `Cancel` (secondary) — closes the modal, pending changes remain staged (nothing is
     lost, nothing is written).
   - Confirm button, styled by the same `is_real` flag as TargetBanner:
     - **Sandbox**: `bg-sky-600 hover:bg-sky-700 text-white`, label `"Save changes to
       sandbox file"`, enabled as soon as the modal opens.
     - **Real target**: `bg-red-600 hover:bg-red-700 text-white`, label `"Write to
       /etc/fstab"`, **disabled until** an explicit checkbox directly above the footer
       is checked: `"I understand this modifies my system's real fstab and takes
       effect on next mount/boot."` — this extra friction step exists only for
       real-target saves, mirroring the asymmetric alarm treatment from §2.
4. On confirm: button becomes a loading state (`"Writing changes…"` + spinner). On
   success: toast/StatusMessage success notice, modal closes, EntriesTable refreshes
   from the new saved state. On failure: the error is shown **inside the still-open
   modal** (not only as a transient toast), since a failed save is safety-relevant and
   the user must not lose the context of what failed; `Cancel`/retry remain available.

---

## 7. Backup/restore panel (SPEC §9: "list recent auto-backups ... and let the
user restore one")

Component: **BackupPanel**, reached via the `Backups` tab in HeaderBar (§1).

- **"Create backup" button**, living in `HeaderBar`'s top-right contextual action
  slot — the same slot `"+ Add Entry"` occupies while the `Entries` tab is active.
  It renders only while the `Backups` tab is active, and `"+ Add Entry"` is absent
  from the DOM during that time (not a row action — it is not tied to an existing
  backup, it acts on the active target's current content, `docs/API_CONTRACT.md`
  §3.11). Primary-ish but not alarming styling
  (`bg-sky-600 hover:bg-sky-700 text-white`, the same non-destructive-primary treatment
  used elsewhere in the app), label `"Create backup"` while idle, `"Creating…"` with the
  same inline spinner treatment as `BackupDeleteConfirmDialog`'s `"Deleting…"` state
  while in flight, disabled while in flight. Deliberately **no confirm dialog**: this
  action is safe/additive/reversible — it only ever adds a new file next to the existing
  backups, and the existing `Delete` action can always clean it up afterward — so a
  loading state and an inline error are the whole of its UI. On failure, the message is
  shown as a small red error line directly beneath the button (no dialog, no toast); on
  success, a success toast (`"Backup created."`) and the list reloads.
- **BackupListItem** rows, newest first: human-readable relative timestamp (e.g. `"2
  hours ago"`, exact ISO timestamp on hover tooltip), a pill badge showing whether the
  backup is automatic or manual (same rounded-pill shape as `EntriesTable`'s source-kind
  badge: `"Automatic"` in the same neutral slate styling, `"Manual"` in the sky tones
  used elsewhere for non-destructive emphasis — distinct but not alarming), file size,
  which target path it was backed up from, and three row-level actions: `Restore`
  (secondary styling, not primary — restoring is not the app's main action), `View diff`
  (secondary styling, identical treatment to `Restore`), and `Delete` (destructive but
  still a row-level secondary button — red text, `text-red-600 hover:bg-red-50
  dark:text-red-400 dark:hover:bg-red-900/20` — the giant red confirm button lives in
  the dialog below, not in the row).
- `EmptyState` if no backups exist yet: `"No backups yet — one will be created
  automatically the first time you save."`
- **BackupRestoreConfirmDialog**: centered modal, same non-click-outside-dismiss
  treatment as DiffView, since this is also a real write:
  - Copy: `"Restore the backup from {timestamp}? This will overwrite the current
    contents of {target path}."` followed by a reassuring-but-accurate safety note:
    `"Restoring itself creates a fresh backup of the current file first, following the
    same atomic write guarantee used everywhere else — but any unsaved pending edits in
    this session will be discarded."`
  - Confirm button follows the identical real/sandbox color + checkbox-gate pattern
    from §6 (red + required understanding checkbox for a real target, plain sky blue
    with no extra gate for sandbox).
  - On confirm: same loading → success/error handling pattern as DiffView's save.
- **BackupDeleteConfirmDialog**: centered modal, triggered by a row's `Delete` button.
  Deliberately **non-dismissible by backdrop click**, the same as
  `BackupRestoreConfirmDialog` and for the same reason: unlike `DeleteConfirmDialog`'s
  staged, undoable entry removal, deleting a backup is an immediate, irreversible
  action that — for the real target — goes through the same `pkexec` prompt as a
  save/restore, so it must not be dismissible by an accidental click.
  - Copy: `"Delete the backup from {timestamp}? This cannot be undone — there is no
    backup of a backup."`
  - For a real target, the confirmation checkbox's copy is deliberately **not** the
    restore/save dialog's sentence (that sentence talks about modifying `/etc/fstab`
    on next mount/boot, which is false for a backup deletion — a backup delete never
    touches the live target file at all). Instead: `"I understand this permanently
    deletes a backup of /etc/fstab and requires administrator privileges."`
  - The confirm button is styled destructively (`bg-red-600 hover:bg-red-700
    text-white`) **regardless of `is_real`** — unlike restore/save, which are sky-blue
    for a sandbox target, a backup deletion is irreversible either way, so there is no
    "safe" variant to signal with a calmer color. Label `"Delete backup"` while idle,
    `"Deleting…"` + spinner while in flight.
  - On confirm: same loading → success/error handling pattern as
    BackupRestoreConfirmDialog; on success, only the backup list is reloaded (a backup
    deletion never touches the live target, so there is nothing else to refresh).
- **BackupDiffView**: a read-only overlay opened by a row's `View diff` button, showing
  `crate::diff::compute_raw_diff` between the backup's raw text (fetched via
  `read_backup`) and the live target's current text — reusing `DiffRawText` directly,
  raw tab only (there is no `render_candidate` equivalent for arbitrary backup text, so
  a parsed Summary tab isn't buildable here, and adding one is out of scope). Unlike the
  two dialogs above, this performs no write and no deletion, so — judgment call,
  mirroring `DeleteConfirmDialog`'s reasoning — it **is dismissible by clicking the
  backdrop**: there is nothing a stray click could lose. Header names both sides, e.g.
  `"Diff: backup from {timestamp} vs. current {target path}"`; a single `Close` button.
  If the `read_backup` call rejected, the error is shown in place of the diff.

---

## 8. Visual design tokens

Tailwind v3 (per the locked architecture decision), class-based dark mode
(`darkMode: 'class'` or media-based — either works; tokens below are written as
light-value / dark-value pairs so either strategy applies them the same way).

**Brand flag**: SPEC/PROJECT_PLAN define no existing brand colors, logo, or voice for
Rustab. The palette below (slate neutrals + sky primary) is a reasonable
dark-mode-friendly default, not a brand decision — **flagging this explicitly**: if the
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
| `target-real` | `red-600` `#dc2626` bg / `red-800` `#991b1b` border | same | TargetBanner real mode only (§2) |
| `target-sandbox` | `emerald-600` `#059669` bg / `emerald-800` `#065f46` border | same | TargetBanner sandbox mode only (§2) |
| `warning` | `amber-500`/`amber-600` | `amber-400` | unparsed-row accents, non-blocking validation |
| `success` | `emerald-600` | `emerald-400` | save/restore success state |

Reused consistently: `danger` and `target-real` are deliberately the **same** red
(`#dc2626`) — the brief asked for "a distinct danger/real-target accent color used
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
(no custom brand typeface specified — system/Inter default, same brand-flag caveat as
color above). `font-mono` = `"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo,
monospace`, used for every raw/technical value: source paths/UUIDs, mount points, the
unparsed-row raw text, and the DiffRawText block.

| Token | Size / line-height | Weight | Used for |
|---|---|---|---|
| `text-xs` | 12px / 16px | normal | timestamps, helper text, chip labels |
| `text-sm` | 14px / 20px | normal/medium | default body, table cells, form inputs & labels — the base UI density |
| `text-base` | 16px / 24px | medium | drawer/modal section sub-headings |
| `text-lg` | 18px / 28px | semibold | drawer/modal titles ("Review changes before saving") |
| `text-xl` | 20px / 28px | bold | app title in HeaderBar ("Rustab") |

`font-bold` is reserved for the TargetBanner's real-mode text and nothing else, so its
visual weight stays a reliable signal (§2).

### Elevation, radius, motion

- Radius: `rounded-md` (6px) default on inputs/buttons/table cells; `rounded-lg` (8px)
  on panels/drawer/modal; `rounded-full` on chips/badges/icon buttons.
- Shadow: `shadow-lg` on drawer and modal surfaces; `shadow-sm` on hovered table rows
  only if a lift effect is wanted — optional, not required.
- Motion: drawer slides in/out over 150–200ms ease-out; modal fades+scales in over
  150ms; respect `prefers-reduced-motion` by disabling the transform/scale and keeping
  only an opacity fade.

### Accessibility baseline

- All text/background pairs above meet WCAG AA (4.5:1 body text, 3:1 large text) in
  both themes — verify `red-600`/white (≈4.8:1), `emerald-600`/white (≈3.9:1, used only
  for large bold banner text at 18px+, which passes the 3:1 large-text threshold; do not
  reuse `emerald-600`-on-white for small body text).
- Every interactive element has a visible `:focus-visible` ring using `focus-ring`
  (§ above) — never remove the outline without replacing it.
- All icon-only buttons (Edit/Delete/Browse/remove-chip) carry an accessible label
  (`aria-label` equivalent in Yew) in addition to the icon.
- Disabled buttons (e.g. Save while validation errors exist) are real `disabled`
  elements, not just visually dimmed clickable ones.

### Responsive breakpoints

- `< 760px` window width: EntriesTable drops Dump/Pass into a per-row overflow menu;
  EntryDetailDrawer becomes full-width instead of fixed 520px; DiffView modal becomes
  full-width/full-height instead of `max-w-4xl` centered.
- `≥ 760px`: full layout as specified above. No further breakpoints are defined — this
  is a desktop app, not a target for phone-width layouts.

---

## 9. Component inventory

Flat checklist for T10 (shell/mock data), T11 (table + form data wiring), and T12
(diff + backup + target indicator wiring):

1. `AppShell` — top-level layout: banner + header + tab content area
2. `TargetBanner` — real-vs-sandbox indicator (§2)
3. `HeaderBar` — app title, `Entries`/`Backups` tab nav, contextual action button
   (`Add Entry` on the `Entries` tab, `Create backup` on the `Backups` tab)
4. `PendingChangesBar` — "N entries changed" + `Review & Save…` trigger (§6)
5. `EntriesTable` — main entry list (§3)
6. `EntryRow` — one parsed entry's row
7. `UnparsedEntryRow` — malformed-line row, raw text + warning (§3)
8. `EmptyState` — generic empty-list placeholder (entries or backups)
9. `EntryDetailDrawer` — add/edit form container (§4)
10. `SourceTypeSelect` — source-type dropdown
11. `DevicePicker` — discovered-device list/combobox for source value
12. `MountPointField` — text input + directory-picker trigger
13. `DirectoryPickerButton` — native directory chooser trigger
14. `FsTypeField` — free-text combobox with common-type suggestions
15. `OptionsTagInput` — well-known toggle chips + systemd/vfat value chips + free tags,
    one ordered chip list (§4)
16. `DumpPassFields` — the two small numeric inputs
17. `ValidationMessage` — generic per-field error/warning renderer (§5)
18. `ValidationSummary` — drawer-top aggregate error banner (§5)
19. `FormActionBar` — Cancel/Save buttons inside the drawer
20. `DeleteConfirmDialog` — single-entry delete confirmation (§4)
21. `DiffView` — the pending-changes/diff confirmation modal (§6)
22. `DiffSummaryList` — structured per-field Added/Modified/Removed list (§6)
23. `DiffRawText` — unified-diff monospace rendering (§6)
24. `RealTargetConfirmCheckbox` — the extra "I understand..." gate, reused by DiffView
    and BackupRestoreConfirmDialog
25. `BackupPanel` — backup list view (§7)
26. `BackupListItem` — one backup's row + Restore/View diff/Delete buttons
27. `BackupRestoreConfirmDialog` — restore confirmation modal (§7)
28. `StatusToast` — transient success/error notification
29. `LoadingSpinner` — inline loading indicator (buttons, panel loads)
30. `BackupDeleteConfirmDialog` — backup delete confirmation modal (§7)
31. `BackupDiffView` — read-only backup-vs-current-target diff overlay (§7)

---

## SPEC §9 checklist (for reviewer sign-off)

- [x] Table/list view of all current entries, including unparsed ones shown as raw
  text + warning, never dropped — §3
- [x] Add/edit/delete form with structured source-type dropdown + device picker,
  mount point + directory picker, filesystem type, options checklist/tag-input with
  toggles for common options, dump, pass — §4
- [x] Inline validation feedback (duplicate mount point, mount point missing on disk,
  malformed UUID) before save is allowed, and it blocks save — §5
- [x] Clear pending-changes/diff view before committing a save — §6
- [x] Visible, unambiguous indicator of real `/etc/fstab` vs. dev/sandbox target — §2
- [x] Backup/restore UI: list of recent auto-backups with restore action — §7
- [x] Tailwind styling, dark-mode friendly, modern/clean — §8
