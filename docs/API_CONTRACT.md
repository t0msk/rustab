# Rustab - Tauri command API contract

**Status: frozen.** This document is the single source of truth for the
boundary between the Rust backend (`src-tauri/`) and the Yew/wasm frontend
(`frontend/`). T11 and T12 are expected to build against *this file* without
reading the backend's Rust source.

The frontend deliberately has no dependency on `fstab-core` or on
`src-tauri`, so it mirrors the shapes below by hand. That makes the wire
format a real contract rather than an implementation detail:

- Every shape here is produced by hand-written DTO types in
  `src-tauri/src/dto.rs` - never by deriving `Serialize` on a domain type -
  so the format cannot change because someone renamed a field in
  `fstab-core`.
- The exact JSON in [§10](#10-wire-format-lock) is asserted literally by
  unit tests (`dto::tests::document_wire_format_is_exactly_as_documented`,
  `remaining_wire_formats_are_exactly_as_documented` and
  `every_error_kind_spelling_is_exactly_as_documented`). A change to any
  field name, `kind` spelling or nesting level breaks those tests first.
- Changing anything in this document means changing the tests and the
  frontend in the same commit. Additive changes (a new command, a new
  *optional* field) are cheap; renames and removals are not.

Related documents: `docs/SPEC.md` (requirements, especially §6 atomicity and
§7 target safety), `docs/UI_DESIGN.md` (what the UI does with all of this).

---

## 1. Conventions

| Aspect | Rule |
| --- | --- |
| Command names | `snake_case`, exactly as listed in §3 (e.g. `invoke("list_entries")`). |
| Argument keys | `snake_case` (the argument-taking commands set `rename_all = "snake_case"`, overriding Tauri's camelCase default). |
| Field names | `snake_case` at every nesting level. |
| Enum values | `snake_case` strings (`"uuid"`, `"part_uuid"`, `"sandbox_default"`, `"privilege_escalation"`). |
| Tagged unions | A `kind` (or `severity`/`status`) string field discriminates; see each type. |
| Absent values | JSON `null`, never an omitted key and never `""`. A `null` and an empty string mean different things (see `OptionDto`). |
| Success | The `invoke` promise resolves with the return shape. A command whose return type is `void`/`null` (only `delete_backup`) resolves with JSON `null`. |
| Failure | The promise rejects with the error *object* described per command. |
| Threading | Every command is `#[tauri::command(async)]`, i.e. it runs on Tauri's thread pool, not the main thread. Calls can overlap; the UI never blocks, including while a `pkexec` prompt is on screen. |

### Argument validation

Arguments are validated by serde at the IPC boundary, before any Rustab code
runs. A payload with an unknown `kind` string, a missing field, a negative
`dump`, or a `pass` that does not fit in a `u32` is rejected there, and the
promise rejects with a **plain string** (serde's message), not with one of
the error objects below. Treat a non-object rejection as a frontend bug, not
as a user-facing condition.

This is deliberate: it means every DTO that reaches Rustab's own code is
already structurally valid, and the conversion from DTO to domain model is
total and infallible. Semantic validation (duplicate mount points, malformed
UUIDs, `findmnt --verify`) is a separate, explicit step - `validate_candidate`
and, authoritatively, the save itself.

---

## 2. Target-path resolution policy

Decided and implemented in `src-tauri/src/target.rs`; summarized here because
it determines what the UI must show and whether a save escalates.

**Revised 2026-10-17 (T34).** The resolved target is still what the GUI reads
and edits, but it is no longer the whole answer to "what does a save write" -
see §3.5. This section describes resolution; the save dispatch that consumes
it is spelled out there.

1. If the environment variable **`RUSTAB_TARGET_PATH`** is set to a non-blank
   value, that path is the active target, verbatim, in **both debug and
   release builds**. `source` is `"environment"`. Such a target is written
   **outright and alone**: no dual write, no draft mirror, whether or not the
   path happens to be `/etc/fstab` itself (§3.5, case 2/3). This is the only
   way to run Rustab without it writing the real `/etc/fstab`.
2. Otherwise the target is the per-user **working draft**,
   `$XDG_DATA_HOME/rustab/fstab.draft` (falling back to
   `$HOME/.local/share/rustab/fstab.draft`, then to a temp-dir path).
   `source` is `"sandbox_default"`. This is the file `list_entries` reads and
   the file the user edits between saves; a save of it writes `/etc/fstab`
   and then mirrors the saved content back into it (§3.5, case 1).

**Draft refresh is conditional (T34), not once-per-launch.** At most once per
process launch - on the first target resolution of a run - Rustab compares
`/etc/fstab`'s **current content** against the content it last knew that file
to have, and refreshes the draft from it **only if they differ**:

- *They match, and the draft file exists*: nothing happens at all. The draft,
  including edits that were never saved, is left byte-for-byte as it was.
  This is what makes an in-progress edit survive a restart.
- *They differ, or there is no record yet (fresh install, or a pre-T34
  draft), or the draft file is missing*: the draft is replaced with a
  byte-for-byte copy of `/etc/fstab` (a literal file copy, never a
  parse/re-serialize round trip), and the new content is recorded as the
  last known one. A missing record is deliberately treated as "differs" -
  current reality wins when Rustab cannot tell.
- *`/etc/fstab` cannot be read* (missing, or no permission): nothing is
  compared, silently - best-effort, never a resolution failure. The draft is
  created, if it does not exist yet, with a comment-only seed that parses and
  validates cleanly; an existing draft is left as it is.

Every later resolution in the same run skips the whole check, so the user's
own edits and saves survive the session that made them.

"What Rustab last knew `/etc/fstab` to hold" is persisted in a **marker file
beside the draft**, `fstab.draft.rustab-lastknown`, holding a verbatim copy
of those bytes (a copy, not a hash: the workspace has no hashing dependency,
and an exact copy cannot produce a false "unchanged" verdict - see
`target.rs`'s `last_known_real_marker` for the full reasoning). The marker
moves forward at the two events that mean "Rustab now knows this is
reality": a refresh, and a successful default-launch save (§3.5). The save
case is what stops the next launch from reading Rustab's *own* write as
external drift and discarding the draft it just mirrored into. The marker is
not a backup: it does not match `fstab-core`'s backup grammar, so
`list_backups` never offers it and backup pruning never deletes it.

Rule 2 applies to release builds too, and `/etc/fstab` is only ever **read**
during resolution, never written - the elevated helper (§4) is the one and
only writer of that file. (An in-GUI target switch is a future task, not
something this layer invents. When it arrives it must go through
`target.rs`, not through a path argument on a command.)

**`is_real`** is `true` only when the resolved path *is* `/etc/fstab` -
literally, or because both paths canonicalize to the same file, so a symlink
in either direction is still recognized. Since T34 it no longer answers "does
a save reach `/etc/fstab`?" - a `sandbox_default` target has
`is_real == false` and its saves go there anyway. What it answers exactly is
"is the resolved path itself the real file", which is what the `environment`
branch of a save needs in order to know whether writing *that path* requires
escalation. The UI's confirmation checkboxes (`docs/UI_DESIGN.md` §2, §6, §7)
therefore cannot be gated on this flag alone any more; every save is now a
real write, and since T35 so is every backup create/delete. The predicate to
use is `is_real || source == "sandbox_default"`, which the frontend calls
`writes_real_file`.

**No command accepts a target path.** The only paths the frontend may ever
send are the `backup_path` arguments of `restore_backup` (§3.7),
`read_backup` (§3.9) and `delete_backup` (§3.10), and each is matched against
the active target's real backup list before being used as a path.

**Second exception: `open_path_in_file_manager`'s `path` (§3.14).** This
command takes a raw, frontend-supplied path and launches the host file
manager on it via `xdg-open`, with no matching against any list first. That
looks like a weakening of the rule above, so it is named here explicitly and
held to the same bar `restore_backup`'s exception is held to, rather than
slipped in quietly:

- The policy this section opens with exists to stop a compromised or buggy
  frontend from redirecting a *write* operation (or a privileged read) at a
  file of the attacker's choosing - `save`, `restore_backup` and the
  `/etc/fstab` case of `delete_backup` are exactly the operations a
  frontend-chosen path could otherwise point somewhere unintended.
  `open_path_in_file_manager` is categorically different from all of those:
  it is **read-only**. It does nothing but start a GUI file-manager process
  and point it at a path; it never opens, writes, renames or deletes
  anything itself.
- It is **unprivileged** for every target, not just the sandbox: there is no
  `privileged.rs` involvement and no `pkexec` anywhere in this command's
  path, so there is no elevated capability for a malicious argument to ride
  on.
- The path itself is **never a secret, and never something the backend
  validates on the user's behalf**. It is the Mount point text already
  rendered in the table the user is looking at - either typed by the user
  directly or filled in via the existing native directory-picker button
  (`docs/UI_DESIGN.md`) - so the frontend is, at most, relaying back a
  string the user already produced.
- Putting those three together: there is no attacker model in which a
  malicious frontend can use this command to show the user something they
  do not already have full, unprivileged visibility into. They could open
  that exact path in a terminal or a file manager themselves, with the
  exact same access Rustab itself runs with - this command does not grant,
  bypass or reveal anything beyond that.

---

## 3. Commands

### 3.1 `get_target_info`

```
get_target_info() -> TargetInfoDto          (rejects with CommandErrorDto)
```

Which file this session reads and writes. Call it on startup and after any
save; everything else in the UI that mentions the target should come from
here rather than being remembered.

Since T39 it also answers a second question - *where this session's backups of
the real `/etc/fstab` are being kept* - in
`real_target_backup_fallback_dir` (§6). That field is `null` on an ordinary
desktop install and names a directory only when this process cannot manage
files in `/etc/` at all, which in practice means the Flatpak build; the UI
shows a visible notice in that case (§3.6, `docs/UI_DESIGN.md` §7).

Resolving can create the sandbox file, which is why it can fail. Error kinds:
`target_resolution`.

### 3.2 `list_entries`

```
list_entries() -> DocumentViewDto           (rejects with CommandErrorDto)
```

Reads the active target and returns the target info, the file's exact text,
and the parsed document.

- Every line of the file is present in `document.lines`, in order, including
  comments, blank lines and lines the parser could not understand. A line's
  identity for edit/delete is its **index** in that array.
- `exists: false` (with `text: ""` and `lines: []`) is not an error: the
  target simply does not exist yet, which is a valid first-save state.
- `text` is provided for the raw-diff tab (`docs/UI_DESIGN.md` §6); the
  frontend cannot render a document itself, so use `render_candidate` for the
  "after" side.

Error kinds: `target_resolution`, `read_target`,
`not_utf8` - a target that is not valid UTF-8 is **refused**, not read
lossily, because loading it with replacement characters and then saving would
corrupt a file that was fine.

See also `resync_external_change` (§3.15), a close relative added by T37: it
reads the *real* `/etc/fstab` rather than the active target and, on success,
publishes what it read onto the active target's draft and marker - a sibling
read/publish command rather than a plain read like this one.

### 3.3 `validate_candidate`

```
validate_candidate(candidate: DocumentDto) -> ValidationReportDto    (does not reject)
```

Runs the same `fstab_core::validate::validate` a save runs - structural
checks plus a `findmnt --verify` semantic check against a throwaway temp
rendering of the candidate. Writes nothing, anywhere, and never touches the
target.

Use it for the inline feedback of `docs/UI_DESIGN.md` §5 and to decide
whether the save button is enabled (`has_errors`). It is not a substitute for
the save's own validation - the save validates again, inside the same
operation that writes, so a document that passes here can still be rejected
there (e.g. if the system changed in between). Treat a `validation` error from
`save` as a normal outcome, not an impossible one.

Note: `findmnt` is an external process, so this is not free. Debounce it;
don't call it per keystroke.

Inside a Flatpak sandbox, `findmnt --verify` runs on the host via
`flatpak-spawn --host` - the candidate is staged in a temp file on the
host, because a sandboxed `findmnt` cannot see the host's devices or
mount points and reports nearly every entry, including `/`, as
unreachable. The validation results reflect the host's state regardless
of where the app runs.

### 3.4 `render_candidate`

```
render_candidate(candidate: DocumentDto) -> string                   (does not reject)
```

Renders a candidate to the exact text a save would write, byte for byte. Pure -
no I/O at all. Exists because the diff view has to show the real "after"
text and the frontend has no serializer of its own.

(Not in T8's original command list; added because §6 of the UI design cannot
be built without it, and the alternative - a second serializer in the
frontend - would be a byte-for-byte fidelity risk for no benefit.)

### 3.5 `save`

```
save(candidate: DocumentDto, allow_external_change: bool)
    -> WriteOutcomeDto                              (rejects with WriteErrorDto)
```

Atomically writes `candidate`. **The only write path in the application.**

`allow_external_change` is **T36's "write anyway" signal** and must always be
sent explicitly: `false` on the normal path, `true` only to repeat a call that
has just rejected with `kind: "external_change"`, after the user has been shown
the diff and asked for the write regardless. It is layered on top of the
existing real-target confirmation (`docs/UI_DESIGN.md` §2), never a replacement
for it, and it has no effect in any case but case 1 below.

**Revised 2026-10-17 (T34): dispatch is on `source`, not on `is_real`.**
Three cases, implemented in one place (`commands.rs::save_document_with`):

1. **`source == "sandbox_default"`** - the default, unconfigured launch, i.e.
   how Rustab is normally used. The save is a **single elevated write of the
   real `/etc/fstab`** (the hardcoded path, never the resolved draft path),
   performed by one privilege-escalated call that runs the *entire* write as
   root - see §4. That call's success or failure **is** the save's result. On
   success, the working draft (the resolved `target` path) and its
   last-known-real marker (§2) are then updated to the same content,
   best-effort.
2. **`source == "environment"`, path is not `/etc/fstab`** - the dev/test
   escape hatch. One in-process write of that path by
   `fstab_core::writer::write_document`. No escalation, no mirror, no marker;
   byte-for-byte the pre-T34 behaviour, which is what keeps `cargo test` and
   CI away from `pkexec` and from the real file.
3. **`source == "environment"`, path *is* `/etc/fstab`** (`is_real == true`,
   literally or through a symlink) - one elevated write of that path, and
   **no draft mirror and no marker update**. An explicit override means "this
   path, outright" for every value it can take; the dual write is a property
   of the default draft/real pair, not of the real file (which is how
   `README.md`'s Safety Notes and `CONTRIBUTING.md` both describe it).
   Consequence worth knowing: because this case does not move the marker, a
   later default launch sees `/etc/fstab` as changed and refreshes the draft -
   correct, and the same answer Rustab gives for an edit made by any other
   tool.

Every case runs the same `write_document`, which means every save, regardless
of case, gets the whole `docs/SPEC.md` §6 sequence: validate → back up the
previous content and prune old backups → write a temp file in the target's own
directory → `fsync` → `fsync` the directory → `rename` → `fsync` the
directory → re-read and verify byte-for-byte. Note where a case-1 save's
**automatic backup** lands: beside `/etc/fstab`, because that is the file
being written - not beside the draft.

**The draft mirror is best-effort and never fails a save.** It is a plain
atomic publish of the saved bytes (temp file plus `rename`), not a second
`write_document`: no second validation pass, and no backup chain of its own
beside the draft. If it cannot be done, the save still **succeeds** - the
file the user asked to change already holds the new content - and the outcome
carries `draft_sync_warning` (§8) with a ready-to-show message. Treat that as
a toast, not an error; the next launch repairs the draft on its own, because
the marker is then stale and `/etc/fstab` wins. The draft is published before
the marker, deliberately: a stale marker costs one redundant copy on the next
launch, whereas a marker ahead of the draft would make the next launch skip a
refresh the draft actually needed.

**Added 2026-10-17 (T36): a case-1 save refuses to overwrite an external
change.** Before the write commits, Rustab compares what it last knew
`/etc/fstab` to hold - the last-known-real marker of §2 - against what that
file holds *now*. If they differ, someone else wrote the file since this
session synced with it, and the call rejects with
`kind: "external_change"` (§8): nothing was written, `target_unchanged` is
`true`, and the error carries both file bodies so the UI can show *what*
changed. The two ways forward are the only two: cancel (nothing happens), or
repeat the call with `allow_external_change: true`, which skips the comparison
for that one call and then behaves like any other successful save (same single
elevated write, same draft mirror, same marker move, `draft_sync_warning`
included). There is deliberately no "reload and re-sync mid-session" option.

Precisely what is compared: the **marker** against the **real file**, never the
draft against the real file. A draft that differs from the marker is the
ordinary state of having unsaved edits and never triggers this. Three
situations also pass the check rather than blocking, because nothing can be
established from them: there is no marker yet, the marker is unreadable, or
`/etc/fstab` itself cannot be read (missing - the write recreates it).

The comparison is made **twice**, and the second one is the one that protects
the file: once on the unprivileged side before the `pkexec` prompt (a courtesy,
so nobody types a password for a write that will be refused), and once *inside
the elevated process* immediately before the write - because the prompt blocks
on a human and the file can change during exactly that wait. See §4.3. The
frontend cannot tell which of the two refused: the error is identical.

Cases 2 and 3 have no such check. Case 2 writes a scratch file nothing else is
touching, and case 3 (an explicit `RUSTAB_TARGET_PATH=/etc/fstab`) keeps no
marker, so there is nothing to compare against; `allow_external_change` is
ignored in both. Case 3 therefore still has this race in theory - a known,
recorded gap (`PROJECT_PLAN.md` §T36), not an oversight.

On failure, **always read `target_unchanged`** before writing UI copy:
`true` means the file on disk is guaranteed to still hold its previous bytes
(offer "fix and retry"); `false` means that guarantee cannot be made (offer
"reload the file" / "restore a backup"). See §8 for the full mapping.

### 3.6 `list_backups`

```
list_backups() -> BackupInfoDto[]           (rejects with CommandErrorDto)
```

The backups of **the file this session's saves write**, newest first (ready
for `docs/UI_DESIGN.md` §7's list order), automatic and manual alike. Which
file that is follows §2/§3.5's dispatch, not the resolved target: the real
`/etc/fstab`'s backups, in `/etc/`, for a default launch, and the named
path's own backups for an explicit `RUSTAB_TARGET_PATH`. Only files in that
file's own directory whose names match `fstab-core`'s backup scheme for
*that* file are reported - a backup of a different target in the same
directory, or a hand-made `fstab.bak`, is not.

`created_unix_secs` is the backup file's mtime, i.e. when the backup was
taken; format the relative and ISO timestamps from it in the frontend.

`is_manual` says which kind each row is: `true` for one the user asked for
with `create_backup` (§3.11), `false` for one a save took on its own. Do not
infer this from `file_name` - the backend owns the naming scheme and reports
the answer.

At most 10 **automatic** backups are kept per target
(`fstab_core::writer::MAX_BACKUPS`); older ones are pruned by a save, which
reports them in `WriteOutcomeDto.pruned_backups`. Manual backups are exempt:
they are neither counted towards that bound nor ever pruned to honour it, so
the list can hold any number of them. The only thing that removes a manual
backup is `delete_backup` (§3.10).

Error kinds: `target_resolution`, `list_backups`.

> **T35 closed T34's gap here.** T34 left this command - and `read_backup`
> (§3.9), `delete_backup` (§3.10), `create_backup` (§3.11),
> `open_backup_location` (§3.12) and `restore_backup`'s resolution of its
> `backup_path` - acting on the **resolved target's** directory, which on a
> default launch is the working draft's, while a default-launch *save* had
> already moved to writing `/etc/fstab` and leaving its automatic backups
> beside it (§3.5). The backup surface therefore described a different file
> than the one a save wrote.
>
> Since T35 all six resolve the backup-owning path the same way a save
> resolves its write target, so a **default launch's backups are
> `/etc/fstab`'s, in `/etc/`** - exactly the files a save backs up, listed
> where a save puts them. That is what the Backups panel is for
> (`PROJECT_PLAN.md` §T35: a real copy of the real fstab, restorable from a
> rescue CD). Two consequences carried into the sections below: `delete_backup`
> and `create_backup` are now `pkexec`-elevated for a default launch too
> (§3.10, §3.11), and the frontend gates the delete behind the same
> `writes_real_file` confirmation checkbox a save has (`docs/UI_DESIGN.md` §2).
> Listing and reading stayed unprivileged - `/etc/` is world-readable.

> **T39/T40: when `/etc/` cannot be *enumerated*, the real file's manual
> backups move.** Flatpak reserves the whole `/etc` path space: the `/etc` a
> sandboxed process sees is the runtime's own, and **no `--filesystem=` grant
> under `/etc` is honoured at all** - neither `/etc:rw` (which Flathub's linter
> rejects anyway) nor the single-file `/etc/fstab:rw`, which the sandbox
> refuses outright (`Path "/etc" is reserved by Flatpak`). So a sandboxed
> Rustab cannot list `/etc/`, and cannot create a file beside the real
> `/etc/fstab` in-process.
>
> **The T39-era open question is resolved (T40), and the answer is "yes,
> writing works - but not for the reason T39 gave".** T39 assumed the `pkexec`
> re-exec "runs unsandboxed as root". It does not: `pkexec` changes a
> process's UID, not its mount namespace, so a `pkexec` child of a sandboxed
> process is root *inside the same sandbox* and cannot touch the host's
> `/etc/fstab` either. T40 replaced that mechanism with the one that does
> work, confirmed end-to-end against a real build: the unprivileged side reads
> the real file with `flatpak-spawn --host`, and the elevated helper is started
> with `flatpak-spawn --host pkexec <host path of the binary>`, which runs it
> as a genuine **host** root process outside every sandbox (needs only
> `--talk-name=org.freedesktop.Flatpak`, no `--filesystem=` at all). So under
> the Flatpak build a save really does write the real `/etc/fstab`, and
> `write_document`'s **automatic** backup and atomic-rename temp file really
> are created in the host's `/etc/` - the colocation property those backups
> exist for is intact.
>
> What stays exactly as T39 built it, and why: the three **manual-management**
> operations move together into a per-user fallback directory
> (`$XDG_DATA_HOME/rustab/etc-fstab-backups/`, which under Flatpak is
> `~/.var/app/dev.tomsk.Rustab/data/rustab/etc-fstab-backups/`), reported by
> `real_target_backup_fallback_dir` (§6). This command lists that directory,
> `create_backup` (§3.11) copies the real file's current content into it,
> `delete_backup` (§3.10) unlinks from it, and neither of the latter two
> escalates, because that directory belongs to the user. `restore_backup`
> (§3.7) resolves its `backup_path` against that same list but is otherwise
> untouched: it still writes `/etc/fstab`, so it still elevates.
>
> The reason they stay there even though creating and deleting *could* now
> happen in the host's `/etc/` (both already go through the elevated helper) is
> **this command**: `list_backups` is deliberately unprivileged - `/etc/` is
> world-readable, and prompting for a password to read a directory would be
> absurd - so it runs in the sandboxed GUI process and enumerates the
> directory *there*, where `/etc` is the runtime's. Moving creation and
> deletion without it would mean a backup the user just created never appearing
> in the list it was created from, which is worse than a consistent fallback.
> The alternative - a second, never-elevated internal re-exec flag that
> host-spawns this binary just to list, read and resolve backups in the host's
> `/etc/` - was considered and rejected; `src-tauri/src/backup_location.rs`'s
> module docs carry the full reasoning (it is not one flag but a second
> request/response protocol covering `list_backups`, `read_backup` and the
> `backup_path` resolution all three path-taking commands depend on, and it
> would put the most fragile mechanism in the app - executing the
> runtime-linked binary on the host - on a routine read path).
>
> **The accepted consequence, which the frontend states plainly rather than
> implying** (`docs/UI_DESIGN.md` §7): in fallback mode this command does
> **not** report the automatic backups each save leaves beside `/etc/fstab`.
> Since T40 those are confirmed to be getting written; a sandboxed process
> simply cannot enumerate them. They remain recoverable from a rescue
> environment, which is the property they exist for - the in-app list just
> cannot prove they are there. Rows in fallback mode therefore all describe the
> fallback directory, and their `target_path` is the mirror file inside it
> (`<fallback dir>/fstab`, a byte-for-byte copy of `/etc/fstab` as of the
> moment the backup was taken) rather than `/etc/fstab` itself - the file name,
> timestamp and `is_manual` flag follow the usual scheme, so such a file can be
> copied back into `/etc/` by hand and is still recognized as a backup of the
> real file.
>
> Fallback mode is also what a sandbox with **no** host access falls back to
> (no `flatpak-spawn` in the runtime, the `org.freedesktop.Flatpak` talk-name
> missing or revoked): such a session cannot read or save the real `/etc/fstab`
> at all, but it can still take, list and restore manual backups in a directory
> it owns.

### 3.7 `restore_backup`

```
restore_backup(backup_path: string, allow_external_change: bool)
    -> WriteOutcomeDto                                   (rejects with WriteErrorDto)
```

Restores a backup onto the active target. `backup_path` must be a `path`
value from a `list_backups` result for the **current** target.

`allow_external_change` means exactly what it means for `save` (§3.5) and is
sent explicitly for the same reason: a `/etc/fstab` that changed between
opening the Backups panel and clicking Restore is the identical hazard through
the identical code path, so a restore rejects with `kind: "external_change"`
in exactly the same situations a save does, and is repeated with `true` after
the user has seen the diff and confirmed.

A restore is not a copy and not a shortcut. The backup's text is parsed and
then goes through *the same* save path as `save`: it is validated, the content
it replaces is itself backed up first, and it is written through the same
atomic temp-file/`fsync`/`rename`/verify sequence - elevated exactly once
wherever a save would be. It therefore **inherits §3.5's dispatch in full**:
on a default launch, restoring a backup writes the real `/etc/fstab` and then
mirrors it into the working draft, `draft_sync_warning` included - and, since
T36, it inherits the external-change refusal from the same place, with no
check of its own (both commands go through one dispatch function; asserted per
command in `commands.rs`'s tests).
Consequences the UI should state plainly
(the copy in `docs/UI_DESIGN.md` §7 already does):

- Restoring creates a fresh backup of what is currently in the file, so a
  restore is itself undoable.
- Restoring a backup whose content no longer validates will be **refused**,
  with a `validation` error. This is intentional: "it used to be in the file"
  is not evidence that writing it now is safe.

`backup_path` is never used as a path until it has been found in the real
backup list, so `/etc/shadow`, `../../etc/fstab`, a different target's backup
and a near-miss file name all fail closed with `invalid_backup` - nothing is
read and nothing is written. Since T35 the list it is matched against is
`list_backups`'s (§3.6), i.e. the real `/etc/fstab`'s on a default launch, so
resolution and the write finally agree about which file this session is
working on; before T35 the resolution looked in the draft's directory while
the write went to `/etc/fstab`. (A file in the target's own directory whose name
matches the scheme *exactly* does count as a backup of that target, even if
Rustab did not create it: in that directory the name is the identity. No
privilege is gained by that - creating such a file next to `/etc/fstab`
already requires the access needed to edit `/etc/fstab` directly - and the
restore still validates, backs up and writes atomically.)

Error kinds: `target_resolution`, `invalid_backup`, `read_backup`, plus any
`save` error kind - `external_change` included.

### 3.8 `discover_devices`

```
discover_devices() -> DeviceDiscoveryDto    (does not reject)
```

Block devices for the source picker, via `lsblk`. Never fails: a system
without a usable `lsblk` yields `{"devices": [], "lsblk_available": false}`,
and the UI must keep manual entry fully available (`docs/SPEC.md` §8). Show
"device discovery unavailable", not "no devices found", when
`lsblk_available` is `false`.

Inside a Flatpak sandbox, `lsblk` likewise runs on the host via
`flatpak-spawn --host`, since a sandboxed `lsblk` exits successfully but
reports no UUIDs or labels at all.

### 3.9 `read_backup`

```
read_backup(backup_path: string) -> string      (rejects with CommandErrorDto)
```

Returns a backup's raw text, exactly as the bytes on disk. `backup_path` must
be a `path` value from a `list_backups` result for the **current** target, and
is resolved the same way `restore_backup` resolves it (§3.7): matched against
the real backup list before it is ever used as a path, so `/etc/shadow`,
`../../etc/fstab`, a different target's backup and a near-miss file name all
fail closed with `invalid_backup` - nothing is read.

Intended for a backup-diff view: diff this against `list_entries`'s `text` to
show what restoring this backup would change. There is no `render_candidate`
step involved - both sides are already raw text.

Since T35 that list is `list_backups`'s (§3.6) for every launch kind, so on a
**default launch this resolves against the real `/etc/fstab`'s backups**, in
`/etc/`, not the working draft's. The backup-diff view therefore diffs a
restore point of the file a save writes against what `list_entries` shows.

**No privilege escalation, for either target kind - including the default
launch's newly `/etc/`-rooted case.** `/etc/fstab` and the backups beside it
are world-readable, which is the same reason `list_entries` and `list_backups`
already work against the real target without a password prompt. This command
never prompts and never writes.

Error kinds: `target_resolution`, `invalid_backup`, `read_backup`, `not_utf8`.
A backup that is not valid UTF-8 is **refused** (`not_utf8`) rather than
returned lossily, for the same reason `list_entries` refuses such a target -
and note that such a backup cannot be restored either (§3.7's `read_backup`
error), so the UI can say the same thing in both places.

### 3.10 `delete_backup`

```
delete_backup(backup_path: string) -> null   (rejects with DeleteBackupErrorDto)
```

Deletes one of this session's backups. `backup_path` must be a `path`
value from a `list_backups` result for the **current** target; it is resolved
exactly as in §3.7 and §3.9 - against §3.6's list, so the real `/etc/fstab`'s
backups on a default launch - and the same set of near misses and arbitrary
paths fail closed with `invalid_backup` and nothing is removed.

- **It only ever removes a backup file.** The active target is never one of
  its own backups, so it can never be the file removed here - asserted in both
  halves of the implementation, including inside the elevated process.
- **It escalates whenever this session's backups are `/etc/fstab`'s, exactly
  like `save` and `restore_backup`.** Since T35 that means a **default launch
  too**, not only an explicit `RUSTAB_TARGET_PATH=/etc/fstab`: a default
  launch's backups now live in root-owned `/etc/`
  (`<target>.rustab-backup.<secs>.<nanos>`), which an unprivileged process
  cannot unlink, so the deletion goes through the same single `pkexec`
  re-exec - see §4. The predicate is the same `is_real || source ==
  sandbox_default` shape §3.5 uses and the frontend calls `writes_real_file`;
  only an explicit override naming some *other*, user-owned file is removed
  in-process with no prompt. The frontend's delete confirmation is gated on
  that same predicate, so a default launch's delete now carries the explicit
  acknowledgement checkbox too (`docs/UI_DESIGN.md` §2).
- **T39's one exception to that:** when `real_target_backup_fallback_dir` (§6)
  is set, the real file's backups are not in `/etc/` but in a directory inside
  the user's own data directory, so the unlink happens in-process with **no**
  prompt - there is no root-owned file involved to need one. The backup being
  deleted is one of that directory's own; anything else, including a backup
  sitting beside the real `/etc/fstab`, is not in §3.6's list for that session
  and fails closed with `invalid_backup` exactly as any other foreign path
  does.
- Resolution happens *before* the dispatch in both branches, so a
  `backup_path` that cannot work is refused without costing the user a
  password prompt. That is a courtesy only: the elevated process re-resolves
  the path itself and does not trust this side (§4.5).

On success the promise resolves with `null`. There is deliberately no outcome
object: either the backup is gone or the promise rejected. Re-call
`list_backups` afterwards rather than mutating a local list.

On failure the rejection is a **`DeleteBackupErrorDto`** (§8), *not* a
`WriteErrorDto`: this operation never opens, renames or writes the target, so
there is no `target_unchanged` question to answer and the field does not exist.

Error kinds: `target_resolution`, `invalid_backup`, `privilege_escalation`,
`delete`.

Deleting works on either kind of backup: the automatic/manual distinction
governs *pruning* (§3.6), never whether the user may remove a backup.

### 3.11 `create_backup`

```
create_backup() -> BackupInfoDto   (rejects with CreateBackupErrorDto)
```

Copies the **current** content of the file this session's saves write to a new
backup beside it and returns that backup as a `BackupInfoDto` - the same row
shape `list_backups` produces, with `is_manual: true`. Takes no arguments:
like every other command, which file it acts on is resolved server-side and
never supplied by the frontend - and since T35 that is §3.6's backup-owning
file, so a **default launch backs up the real `/etc/fstab`**, not the working
draft. That is the point of the button: a real copy of the real file, which a
copy of the draft could not be.

- **It never changes the target.** No temp file, no rename, no validation -
  the target is only ever *read*. That is why the rejection is a
  `CreateBackupErrorDto` and not a `WriteErrorDto`: there is no
  `target_unchanged` question to answer, exactly as for `delete_backup`
  (§3.10).
- **The backup it creates is exempt from pruning.** Automatic backups (a
  save's side effect) are bounded at 10 per target and pruned oldest-first;
  a manual backup is neither counted towards that bound nor removed to honour
  it, so a later flurry of saves cannot silently discard a snapshot the user
  deliberately took. Only `delete_backup` removes it.
- **It escalates whenever this session's backups are `/etc/fstab`'s, exactly
  like `save` and `delete_backup`.** Since T35 that includes a **default
  launch**, not only an explicit `RUSTAB_TARGET_PATH=/etc/fstab`: a backup of
  `/etc/fstab` has to be created *in* root-owned `/etc/`, so it goes through
  the same single `pkexec` re-exec - see §4, and the same `is_real || source
  == sandbox_default` predicate as §3.10. Only an explicit override naming
  some other, user-owned file is copied in-process with no prompt.
- **T39: and, now, a real file whose backups are not in `/etc/`.** When
  `real_target_backup_fallback_dir` (§6) is set, this command reads the real
  `/etc/fstab`'s current bytes, stages them in that directory's mirror file and
  copies *that* to a new manual backup there - all in-process, with no prompt,
  because the directory is the user's own. The resulting backup is therefore
  still a snapshot of `/etc/fstab` as it is at the moment the button is
  pressed, which is the only thing that would make the button honest. If the
  real file cannot be read at all, the rejection is the usual
  `target_missing` (it does not exist) or `backup` (any other I/O failure),
  and nothing is created.
- **A target that does not exist yet is an error, not a no-op.** The kind is
  `target_missing`, and the UI should say what the message says: there is no
  file to back up yet, save once first. An explicit "back this up now" must
  never resolve with nothing to show for it.

On success, the returned row is read back from the real backup list, so it
describes what is actually on disk. Re-call `list_backups` (or prepend the
returned row) to refresh the panel.

Error kinds: `target_resolution`, `target_missing`, `invalid_target`,
`backup`, `privilege_escalation`.

### 3.12 `open_backup_location`

```
open_backup_location() -> null   (rejects with CommandErrorDto)
```

Opens the directory holding **this session's backups** in the host file
manager (`xdg-open`). A file's own directory is also where its backups live -
there is no separate backup folder (`src-tauri/src/backups.rs`'s module
docs) - so this is the one button the Backups panel needs to let the user
browse the backup files directly. Takes no arguments: like every other
command, which directory it opens is resolved server-side and never supplied
by the frontend.

Which directory that is follows §3.6, not the resolved target: since T35 a
**default launch opens `/etc/`**, where the backups `list_backups` just
listed actually are, rather than the working draft's own directory, which
holds none of them. An explicit `RUSTAB_TARGET_PATH` opens that path's own
directory, unchanged. By the same rule, since T39 a session whose
`real_target_backup_fallback_dir` (§6) is set opens *that* directory - it is
still "where the rows on screen are".

**No privilege escalation, for either case.** `/etc/` is world-readable and
world-executable, the same reasoning §3.9 already gives for reading a
backup's content - listing a directory needs strictly less access than
reading a file inside it. This command never prompts.

On success the promise resolves with `null`; the file manager window is not
waited on. On failure (most commonly `xdg-open` missing) the rejection names
the directory that could not be opened.

Error kinds: `target_resolution`, `open_location`.

### 3.13 `open_external_link`

```
open_external_link(link: ExternalLinkDto) -> null   (rejects with CommandErrorDto)
```

Opens one of this app's five known external links (the GitHub repo page, the
GitHub issues page, the GitHub Sponsors page, the license (repo root, since
Rustab is dual MIT/Apache-2.0 - see `docs/SPEC.md` §10 - rather than one
specific license file), or the `opensource@tomsk.dev` contact address, all used
by the About dialog) via `xdg-open`. `link` is a closed enum (`github_repo`,
`github_sponsors`, `email`, `issues` or `license`), never a raw URL string:
the URL/URI this command will ever open for a given variant is fixed and
hardcoded on the backend side, not anything the frontend supplies. Note that
`license` resolves to the exact same URL as `github_repo` today (the repo
root) - it is a separate enum variant anyway because the two buttons
represent distinct user intent and may point elsewhere independently later.

For every variant except `email`, `xdg-open` launches the host's default web
browser on the `https:` URL. For `email`, `xdg-open` is given a
`mailto:opensource@tomsk.dev` URI instead, which it dispatches to the host's
default mail client - the same `xdg-open` call handles both cases, since it
resolves the right handler from the URI's scheme. The About dialog
deliberately does not use a plain `<a href="mailto:...">` for the email
link: an in-webview anchor navigation to a non-`http(s)` scheme is not
guaranteed to be handled by the host webview (WebKitGTK shows "The URL
can't be shown" for `mailto:` rather than falling back to the OS's scheme
handler), so this command is the only reliable way to reach the user's mail
client from inside the app.

**No privilege escalation.** Opening a browser or mail client needs no
elevated access, the same reasoning §3.12 already gives for opening a file
manager. This command never prompts.

Unlike `open_backup_location`, this command's only failure mode is the
launch itself: it does not touch the active target at all, so there is no
target-resolution step and no `target_resolution` error kind.

On success the promise resolves with `null`; the opened program's window is
not waited on. On failure (most commonly `xdg-open` missing) the rejection
names the URL/URI that could not be opened.

Error kinds: `open_external_link`.

### 3.14 `open_path_in_file_manager`

```
open_path_in_file_manager(path: string) -> null   (rejects with CommandErrorDto)
```

Opens an arbitrary frontend-supplied path in the host file manager
(`xdg-open`). `path` is this project's **second** explicit exception to §2's
"no command accepts a target path" rule (the first is `restore_backup`'s
`backup_path`, §3.7) - see §2 for why it is safe: this command is read-only,
needs no privilege escalation for either target, and `path` is never a
secret or something the backend validates, only the Mount point text the
user already sees and typed or picked themselves.

**No privilege escalation.** Same reasoning as §3.12 and §3.13: launching a
GUI program needs no elevated access. This command never prompts.

On success the promise resolves with `null`; the file manager window is not
waited on. On failure (most commonly `xdg-open` missing) the rejection names
the path that could not be opened.

Error kinds: `open_path`.

### 3.15 `resync_external_change`

```
resync_external_change() -> DocumentViewDto   (rejects with CommandErrorDto)
```

**Added in T37.** Re-syncs the active target's working draft - and its
last-known-real marker (§2) - from the real `/etc/fstab`'s *current* content,
without writing to `/etc/fstab` itself. This is the third way forward
`ExternalChangeDialog` offers once a `save`/`restore_backup` has been refused
with `kind: "external_change"` (§8, T36): besides Cancel (discard the pending
write) and Write anyway (overwrite the external change), the user can ask
Rustab to adopt the real file's current content as the new baseline and
re-review their still-pending edits against it, via `DiffView`, rather than
being forced to choose between discarding their edits or steamrolling someone
else's. Takes no arguments: like every other command, which file this reads
from and writes to is resolved server-side, never supplied by the frontend.

- It reuses the exact [`target::sync_sandbox_after_save`] primitive a
  successful save already calls to publish fresh bytes onto the draft *and*
  the marker, atomically, as one event ("Rustab now knows this is reality") -
  just from a new, explicit, user-initiated entry point instead of only from a
  successful write.
- It never touches the user's pending edits - there is nothing in this
  command's request *or* response that could, since `DocumentViewDto` only
  ever describes the target's own content, never a frontend-held candidate.
- **It fails closed, touching nothing, unless the active target's `source` is
  `"sandbox_default"`.** This command only makes sense for the default
  working draft, whose marker records what Rustab last knew the real
  `/etc/fstab` to hold; an explicit `RUSTAB_TARGET_PATH` target - real or not -
  is not a mirror of `/etc/fstab` at all, and silently overwriting its draft
  with the real file's content would be a data-safety bug, not a cosmetic
  one. In normal frontend use this guard should never actually fire, because
  `ExternalChangeDialog` (and so this command) only ever appears for the
  `sandbox_default` dispatch case of §3.5 - it exists for the same reason a
  seatbelt exists for a car that signals before it turns.
- Reads the real `/etc/fstab`'s current bytes. A missing file is **not** an
  error - the same `exists: false`, empty-`text` handling `list_entries`
  (§3.2) already gives a missing target - and the draft and marker are synced
  to empty right along with it. Bytes that are not valid UTF-8 are refused
  outright (`not_utf8`), for the identical reason `list_entries` refuses them
  for its own target: publishing mangled content onto the draft could only
  make things worse.
- **No privilege escalation, ever.** Reading `/etc/fstab` is already
  unprivileged everywhere else in this contract (`list_entries`, `list_backups`
  §3.6), and writing the local draft in `$XDG_DATA_HOME` is the same
  unprivileged primitive the once-per-launch draft refresh (§2) already uses.
  This command never prompts, which is also why the frontend gates it on
  nothing - no `RealTargetConfirmCheckbox`, unlike `save` and `restore_backup`.
- On success, the returned `DocumentViewDto` describes the real file's content
  exactly as read - not a second read-back, since the draft now *is* this
  text by construction of the call that just ran.

Error kinds: `target_resolution`, `resync`, `read_target`, `not_utf8`.
`resync` covers two distinct situations under one kind, deliberately: the
fail-closed guard above, and a `sync_sandbox_after_save` publish failure once
the real file *was* read successfully. The frontend's treatment of either is
identical - show the message in `ExternalChangeDialog`'s existing error slot
and leave Cancel/Write-anyway/Reload all available - so a second kind would
describe a distinction nothing branches on.

---

## 4. Privilege escalation

### 4.1 The requirement

`docs/SPEC.md` §6 step 6 forbids the obvious-but-wrong design - an
unprivileged temp-file write followed by a separate privileged `mv`. Two
steps mean a window where a crash, a failure of the second step, or a race
can leave `/etc/fstab` in a state nobody planned. The elevated process must
"either do nothing or complete the full atomic swap".

### 4.2 Three operations, one flag

Three things need root, and since T35 all three share **one** condition:
`is_real || source == sandbox_default` - the same predicate §3.5's save
dispatch encodes and the frontend calls `writes_real_file`. The condition is
no longer "only when `is_real == true`" for any of them: a **default launch**
has `is_real == false` and yet writes `/etc/fstab`, and (since T35) keeps its
backups beside it, so all three operations land in root-owned `/etc/` in that
case. T34 moved `write` onto this predicate and left `delete_backup`/
`create_backup` on raw `is_real` as a recorded gap; T35 closed that - see
§3.6. Only an explicit `RUSTAB_TARGET_PATH` naming some other, user-owned
file escalates nothing.

| Operation | Used by | What the elevated process does |
| --- | --- | --- |
| `write` | `save` (§3.5), `restore_backup` (§3.7) | Re-check that the target still holds what the request says Rustab last knew (T36), then one `fstab_core::writer::write_document` call: the whole `docs/SPEC.md` §6 sequence. |
| `delete_backup` | `delete_backup` (§3.10) | Resolve the requested path against the target's real backup list, then one `remove_file`. |
| `create_backup` | `create_backup` (§3.11) | One `fstab_core::writer::create_backup` call: copy the target to a new `.manual` backup beside it and `fsync` the copy. The target is only read. |

They share a **single** hidden re-exec flag. Which operation is being asked
for is a field of the request, not a second flag, so there is exactly one
branch in the whole program that can turn Rustab into a root process, and the
"no Tauri runtime, webview, plugin or logger is ever initialized as root"
property stays a one-flag check.

### 4.3 The mechanism

When `save` or `restore_backup` takes an elevated case of §3.5's dispatch, or
`delete_backup`/`create_backup` runs in a session whose backups are
`/etc/fstab`'s per §4.2's predicate (a default launch, or an explicit
`RUSTAB_TARGET_PATH=/etc/fstab`):

1. The unprivileged side does its cheap pre-checks first - purely to avoid
   asking for a password for an operation that cannot succeed. For a write
   those are validating the candidate and (T36) comparing `/etc/fstab`'s
   current bytes against the marker's; for a delete it is resolving
   `backup_path` against the backup list; a `create_backup` has nothing to
   pre-check, since its request carries no path but the target. None of these
   write anything and none is **the safety net** (see §4.6) - each has a
   counterpart inside the elevated process, which is the one that decides.
2. It re-executes **its own binary** as
   `pkexec <rustab> --internal-privileged-write`, with stdin, stdout and
   stderr piped. (The flag's name is historical - it predates the two backup
   operations and now carries any of them. It is deliberately unchanged: the
   point is that there is exactly one such string.)

   Which `<rustab>` and *where* that `pkexec` runs depends on one thing, and
   it is the only part of this mechanism that is environment-dependent (T40):

   - **ordinary install**: `std::env::current_exe()`, `pkexec` run directly;
   - **inside a Flatpak sandbox** (detected by `/.flatpak-info`):
     `flatpak-spawn --host pkexec <host path of this binary>
     --internal-privileged-write`. `pkexec` alone would not do: it changes a
     process's UID, not its mount namespace, so a `pkexec` child of a
     sandboxed process is root *inside the same sandbox*, where `/etc` is the
     runtime's own and the host's `/etc/fstab` is unreachable by any path
     (Flatpak reserves `/etc` and refuses even `--filesystem=/etc/fstab:rw`).
     `flatpak-spawn --host` is what puts the process on the host; `pkexec`
     behind it is what makes it root. The host path is resolved from the
     `app-path` key Flatpak itself writes into the sandbox's `/.flatpak-info`,
     falling back to the user and system Flatpak installation layouts, and the
     operation fails with a `privilege_escalation` error naming those paths if
     none of them exists - it never guesses. Needs
     `--talk-name=org.freedesktop.Flatpak` in the manifest and nothing else.

   Everything in steps 3-6 below is identical either way: `flatpak-spawn
   --host` passes stdio through and exits with the child's status, and the
   elevated process - being outside the sandbox - has no `/.flatpak-info` of
   its own, so its own reads and writes are ordinary filesystem access to the
   real `/etc/`.
3. It writes one JSON request to the child's stdin and closes the pipe. The
   request is a union tagged by `op`:
   ```json
   {"op": "write",         "target": "/etc/fstab", "candidate": { /* DocumentDto */ }, "last_known_real_content": "# ...\n", "allow_external_change": false}
   {"op": "delete_backup", "target": "/etc/fstab", "backup_path": "/etc/fstab.rustab-backup.0000000001.000000002"}
   {"op": "create_backup", "target": "/etc/fstab"}
   ```
   A `write`'s `last_known_real_content` is what the unprivileged side
   believes `/etc/fstab` currently holds (its marker's content, §2), or `null`
   when it has no usable record; `allow_external_change` is the user's
   explicit "write anyway" answer from §3.5. Both are required fields of the
   request - unlike the *additive, optional* fields of the DTOs in §5-§9, this
   protocol is one build talking to itself, and a missing field defaulting to
   "no record" would be a fail-open default on the one check that exists to
   fail closed.
4. The elevated process, in `main()`, sees `--internal-privileged-write` as
   `argv[1]` **before anything else starts** - no Tauri runtime, no webview,
   no plugins, no logger are ever initialized in a root process. It reads
   stdin to EOF exactly once, refuses any `target` other than `/etc/fstab`
   (for either operation), and then:
   - `write`: unless `allow_external_change` is `true`, re-reads the verified
     target **in this process** and compares it with
     `last_known_real_content`; if they differ it answers
     `{"status": "error"}` with `kind: "external_change"` and writes nothing
     (T36). Otherwise it reconstructs the document and calls
     `fstab_core::writer::write_document` **once**.
   - `delete_backup`: resolves `backup_path` against the *already-validated*
     target's real backup list - in this process, trusting nothing the
     unprivileged side claimed - and calls `remove_file` on the path that
     list yielded. If the resolution fails, nothing is unlinked.
   - `create_backup`: calls `fstab_core::writer::create_backup` **once** on
     the verified target. The backup's path is derived there from the
     target's own name, so the request contains nothing that could redirect
     the copy.
5. It prints exactly one JSON object to stdout and exits:
   ```json
   {"status": "ok",                   "outcome": { /* WriteOutcomeDto */ }}
   {"status": "error",                "error":   { /* WriteErrorDto */ }}
   {"status": "backup_deleted"}
   {"status": "backup_delete_failed", "error":   { /* DeleteBackupErrorDto */ }}
   {"status": "backup_created",       "outcome": { /* CreateBackupOutcomeDto */ }}
   {"status": "backup_create_failed", "error":   { /* CreateBackupErrorDto */ }}
   ```
   The first two answer a `write`, the next two a `delete_backup`, the last
   two a `create_backup`. `{"status": "error"}` is *also* how a framing
   failure (stdin unreadable, request unparsable) is reported, because at
   that point the helper does not yet know which operation it was asked for;
   the delete and create paths read that variant too and report it as
   "nothing was deleted" / "no backup was created".

   T36 needed **no new response variant**: a refused-because-changed write is
   a `write` that did not complete, which is exactly what
   `{"status": "error"}` means, and `WriteErrorDto` already carries the kind
   and the payload (§8). The unprivileged side's existing rule - a reported
   error is forwarded to the frontend unchanged - delivers it as-is.

   `CreateBackupOutcomeDto` is internal to this protocol - it never reaches
   the frontend. The `create_backup` command turns it into the
   `BackupInfoDto` §3.11 documents.

   Exit codes: `0` success, `1` the operation was attempted and failed (a JSON
   error body is present), `2` the request itself was unusable (a JSON error
   body is still present; nothing was written and nothing was deleted).
   Diagnostics go to stderr, never stdout.
6. The unprivileged side parses that object. A parsable response always wins
   over the exit code - it is the helper's own account of what it did. A
   response that answers a *different* operation is not an account of this
   one and is treated as no response at all. With no usable response, the
   exit code decides, conservatively (see §8), and **all three operations
   classify `pkexec`'s own exit codes through one shared function**, so they
   can never disagree about whether a dismissed prompt means "nothing
   happened".

The candidate travels as a `DocumentDto`, not as rendered text, so the
elevated side reconstructs the identical document the user was shown and lets
`write_document` render it. There is no second serializer anywhere in the
chain whose output could differ from the first one's.

This stdin/stdout protocol is internal, undocumented as an interface and
unstable by design (§11): nothing but this binary ever speaks it, to itself,
in a single build. It is described here because it is the mechanism the
`privilege_escalation` error kinds refer to, not because anything else may
use it.

### 4.4 Why a write does the *entire* atomic operation as root

By construction, not by convention:

- The elevated process's whole job is one `write_document` call. That one
  function performs validation, the backup and its `fsync`, retention
  pruning, the same-directory temp file, the temp-file `fsync`, the
  pre-rename directory `fsync`, the `rename`, the post-rename directory
  `fsync`, and the byte-for-byte read-back verification - in a single
  process, under a single authorization.
- The unprivileged side writes **nothing, anywhere, ever**. It serializes
  JSON to a pipe and reads JSON back. There is no temp file on that side to
  be moved later, no partial state to resume, and no second privileged call.
- Therefore, kill the elevated process at any instant and the possible
  outcomes are exactly the ones `write_document` itself guarantees - the
  target holds either its complete old content or its complete new content -
  because `write_document` is the only thing that ever touched the disk.
- It is the *same* audited code: not a copy, not a port, not a
  reimplementation. The elevated path calls the identical function compiled
  from the identical source in the same build.

None of this applies to the two backup operations, and none of it is needed
there: neither touches the target file (a `create_backup` only *reads* it),
so there is nothing to make atomic about the target. The elevated delete is
deliberately two checks and one `remove_file`; the elevated create is one
check and one `create_backup` call - no backup-of-a-backup, no temp file, no
rename, no verification pass in either. Their guarantee is narrower and
simpler: either the named backup is gone / the new backup exists, or nothing
changed.

### 4.5 Why a re-exec and not a separate helper binary

A dedicated `rustab-helper` executable is the textbook answer. It was
rejected deliberately:

- **One artifact to build, package and sign.** A second binary has to be
  added to the bundle, installed at a known absolute path, kept
  version-locked with the GUI, and signed separately in formats that sign
  executables. `docs/SPEC.md` §13/§14 describe a single-binary bundle.
- **A helper can drift; a re-exec cannot.** A stale helper speaking an older
  request format while the GUI speaks a newer one is a real hazard for a tool
  that writes `/etc/fstab`. There is no "the helper's copy of the write
  logic" here, because there is no copy.
- **No security benefit is given up.** A helper's advantage would be a
  smaller elevated code surface, but the elevated surface here is already
  minimal and provably so: the flag is checked as the first statement of
  `main`, matched as `argv[1]` exactly, extra arguments are refused, and the
  elevated process reads stdin, performs one of three operations, prints JSON
  and exits. Adding the second and third operations did not add entry points -
  each added one `match` arm behind the same single flag (§4.2).
- **The authorization cost is identical.** `pkexec` prompts per invocation
  either way, so nothing improves for the user.

### 4.6 Why the target path is sent as data, and re-checked

`pkexec` does not forward the caller's environment, so the elevated process
cannot re-resolve the target itself - `RUSTAB_TARGET_PATH` does not survive
the transition. That is a feature (an env var cannot influence the elevated
process at all), and it is why the path must travel inside the request.

Because the path arrives as data, the elevated side refuses to act on anything
except `/etc/fstab` (literally, or via a symlink that resolves to it), using
the *same* comparison that decided `is_real` in the first place. Escalation
only ever happens for the real file - a default-launch save passes the
hardcoded `/etc/fstab`, never its own draft path (§3.5), and since T35 a
default-launch `delete_backup`/`create_backup` passes the same hardcoded
`/etc/fstab` as the backup-owning target (§3.6) - so this costs nothing in
normal operation and bounds the elevated write to exactly one file. No
signature change was needed for either backup helper: they already took a
bare path and already refused anything but `/etc/fstab`.

`delete_backup`'s `backup_path` arrives as data for the same reason and is
re-checked the same way, with one extra step on top of the target check: the
elevated process resolves it against the real backup list of the target it
has *already* verified, and unlinks the path that list yielded - never the
string from the request. So the only files the elevated delete can ever
remove are that one target's own backups. Not `/etc/shadow`, not
`../../etc/fstab`, not a different target's backup sitting in the same
directory, not a near-miss file name, and not the target file itself. A bug
(or a lie) on the unprivileged side cannot widen that, because the
unprivileged side's own resolution is not consulted.

`create_backup` needs no second check, because its request carries no second
path: the target check is the whole of its authority, and the file it writes
is named by `fstab_core::writer::create_backup` from the verified target's own
name, in that target's own directory.

**T36's external-change check is in the child for the same structural reason,
plus one specific to this mechanism.** The expectation
(`last_known_real_content`) arrives as data and is re-verified against the file
itself, in the elevated process, immediately before the write - so a bug or a
lie on the unprivileged side cannot turn the write into a blind overwrite. The
mechanism-specific reason is `pkexec` itself: it blocks on a human reading a
dialog and typing a password, an arbitrarily long wait that sits *between* any
check the parent could make and the write. A check made only before the prompt
would leave the largest part of the window uncovered, which is most of the
point of having one. The parent's copy of the comparison is therefore exactly
what its `validate()` call already is - a courtesy that saves a pointless
password prompt, deletable without weakening any guarantee - and the child's
is binding.

**Known limitation:** escalation is therefore only available for the real
`/etc/fstab`. Pointing `RUSTAB_TARGET_PATH` at some *other* root-owned file
gives an in-process save that fails with a permission error
(`kind: "write"`/`"backup"`, `target_unchanged: true`), not a password
prompt - and, likewise, an in-process backup deletion that fails with
`kind: "delete"` and an in-process backup creation that fails with
`kind: "backup"`. That is the intended trade for v1; widening it would mean
the elevated process accepting arbitrary paths from data.

### 4.7 What the frontend sees

Nothing special. `save`, `restore_backup`, `delete_backup` and
`create_backup` behave identically for both targets;
`WriteOutcomeDto.elevated` is `true` when a write went through `pkexec`. A
dismissed or refused password prompt is an ordinary rejection with
`kind: "privilege_escalation"` - plus `target_unchanged: true` for a write;
for a backup deletion or creation there is no such field and the message says
the backup was not deleted / not created. The call can sit unresolved for as
long as the prompt is on screen, so keep the "Writing changes…" state of
`docs/UI_DESIGN.md` §6 (and its equivalent for a deletion or a backup
creation) until it settles, and do not add a client-side timeout.

`read_backup` never escalates, for either target, so it never prompts.

**Requirement:** `pkexec` (polkit) must be installed. Without it, a real-target
save, backup deletion or backup creation rejects with
`kind: "privilege_escalation"` and a message saying so. No polkit `.policy` file ships yet, so `pkexec` applies its
default "authenticate as an administrator" rule; adding a policy file (to
refine the prompt text and the auth rule) is a packaging-time follow-up, not
an API change.

---

## 5. Document DTOs

### `DocumentDto`

```json
{ "lines": [ /* LineDto, in file order */ ] }
```

Sent to `save`, `validate_candidate` and `render_candidate`; received inside
`DocumentViewDto`. Round-trips losslessly: what `list_entries` gave you,
handed back to `save` unmodified, writes a byte-identical file (asserted
against every fixture in `tests/fixtures/`).

### `LineDto`

A union tagged by `kind`:

```json
{ "kind": "entry",     "entry": { /* EntryDto */ } }
{ "kind": "comment",   "text": " a comment, without the leading #" }
{ "kind": "blank" }
{ "kind": "malformed", "raw": "/dev/sdb1" }
```

`malformed` lines must be displayed verbatim with a warning, never dropped
(`docs/SPEC.md` §5, §9). Keeping them in the array is what makes the round
trip byte-faithful.

### `EntryDto`

```json
{
  "source": { "kind": "uuid", "value": "1111-2222" },
  "mount_point": "/mnt/data",
  "fs_type": "ext4",
  "options": [ { "key": "noauto", "value": null },
               { "key": "fmask", "value": "0022" } ],
  "dump": 0,
  "pass": 2,
  "trailing_comment": " a note after the six fields, without the #",
  "description": "photo archive\nonly mounted when backing up"
}
```

- `source.kind` is one of `"path"`, `"uuid"`, `"label"`, `"part_uuid"`,
  `"part_label"`, `"none"`, `"network"`, `"other"`. `source.value` never
  includes the `KEY=` prefix - it is re-added when the file is written. For
  `"none"` the value is ignored on the way in and reported as `"none"`.
- `options[].value` is `null` for a bare flag (`noauto`) and a string for a
  `key=value` option. `""` means a literal `key=` with nothing after the `=`,
  which is a real thing in fstab (`x-systemd.automount=`) and is preserved.
- An **empty** `options` array renders as an empty options field, which
  `findmnt --verify` may reject. Offer `defaults` rather than nothing.
- `mount_point` and path-like `source.value`s keep fstab's octal escapes
  (`\040` for a space) **exactly as they appear in the file** - undecoded on
  the way out and un-re-encoded on the way in. If the UI displays a decoded
  form, it must re-encode before sending, or it will rewrite the user's paths.
- `dump` and `pass` are unsigned integers. fstab imposes no tighter range, so
  neither does this contract; validation flags the odd ones.
- `trailing_comment` is `null` when there is none. Like `description` used
  to be, it is validated for an embedded newline, which is a blocking error
  (see §7 for the `field` value this sets).
- `description` is an optional human-readable note about what the entry is
  *for*, and is `null` when there is none. It is **a different mechanism from
  `trailing_comment`**, carried in a separate field: a trailing comment is
  inline on the entry's own line, after the six fields, whereas a description
  round-trips to and from the ordinary `#` comment line(s) directly *above*
  the entry. **No marker is involved** - adjacency alone decides it, so any
  ordinary comment sitting above an entry is that entry's description (this
  replaced an earlier `# // <text>` marker convention). See `fstab-core`'s
  `model.rs` and `parser.rs` doc comments for the exact walk (how far up it
  goes, which blank lines it crosses) and the serialization rules. Three
  consequences for a caller:
  - A comment line absorbed this way does **not** also appear as a
    `{"kind":"comment"}` line, so `DocumentDto.lines` can be shorter than the
    file's physical line count. What is still a `comment` line is every
    comment no entry claimed - e.g. one above a `malformed` line, or one with
    no entry below it at all.
  - The value **may be multi-line**: one `\n` per comment line, no trailing
    newline, never `\r`. It is written back out as one comment line per
    `\n`-separated piece, contiguously above the entry. A UI editing this
    field needs a multi-line input, and must not strip newlines.
  - A value sent *in* is normalized at the boundary: `\r\n` and lone `\r` are
    folded to `\n`, the result is trimmed at both ends, and an
    empty-after-trim value is treated as `null` - exactly as the text parser
    treats a comment block whose text trims away to nothing.

### `DocumentViewDto`

```json
{
  "target": { /* TargetInfoDto */ },
  "exists": true,
  "text": "# the file's exact content as read\n",
  "document": { /* DocumentDto */ },
  "has_malformed_lines": false
}
```

---

## 6. Target DTO

### `TargetInfoDto`

```json
{ "path": "/home/u/.local/share/rustab/fstab.draft",
  "is_real": false,
  "source": "sandbox_default",
  "real_target_backup_fallback_dir": null }
```

`source` is `"environment"` or `"sandbox_default"` (see §2). Read **both**
fields since T34: `source` is what decides where a save goes (§3.5) and,
since T35, whose backups this session acts on (§3.6), while `is_real` says
only whether `path` itself is the real file. A `"sandbox_default"` target
with `is_real: false` - the shape above, the default launch - is still one
whose saves write `/etc/fstab` and whose backups are `/etc/fstab`'s.

`real_target_backup_fallback_dir` (T39) is the directory this session's
backups of the **real `/etc/fstab`** are being kept in *instead of* beside the
file itself - or `null` when they are beside it after all, which is what every
ordinary desktop install reports. It is a string only when **both** of these
hold:

1. this session's backups are the real `/etc/fstab`'s - i.e.
   `source == "sandbox_default"` (the default launch, whose saves write that
   file) or `is_real == true` (an explicit `RUSTAB_TARGET_PATH=/etc/fstab`);
   **and**
2. this process cannot **enumerate** the real file's own directory - in
   practice it is running inside the Flatpak sandbox, where the `/etc` it sees
   is the runtime's own and no `--filesystem=` grant under `/etc` is honoured
   at all (Flatpak reserves that path space; Flathub's linter rejects
   `/etc:rw` on top of that).

So it is `null` for **every** `"environment"` target that is not `/etc/fstab`
itself: such a target's backups are its own, in its own directory, and nothing
about `/etc/` applies to them. It is also `null` for the real file whenever
`/etc/` *is* enumerable, which is the case this whole mechanism is designed to
leave completely unchanged.

Note what this field does **not** say anything about, since T40: whether this
session can read or save the real `/etc/fstab`. A Flatpak session reports a
fallback directory here *and* reads and writes the real file normally, through
`flatpak-spawn --host` (§3.6, §4). The field answers one narrow question -
"where are this session's manual backups of `/etc/fstab`?" - and nothing
broader.

When it is a string, that directory is exactly where `list_backups` (§3.6),
`create_backup` (§3.11) and `delete_backup` (§3.10) act, and what
`open_backup_location` (§3.12) opens - and two things are true that the UI has
to state plainly rather than imply (`docs/UI_DESIGN.md` §7):

- backups the user creates explicitly land in that directory, not in `/etc/`,
  and need no password prompt at all;
- the automatic backups each save takes are **still written beside the real
  `/etc/fstab`** by the elevated writer - which since T40 is a verified fact
  rather than an assumption, because that writer now runs as a real host root
  process (§3.6) - and are **not listed**, because this process cannot
  enumerate that directory. They are not lost: they remain recoverable from a
  rescue environment, which is the whole reason they are put there - the
  in-app list simply cannot prove they exist.

Treat it as display text. No command accepts a directory, and the three
commands that do take a path (§3.7, §3.9, §3.10) match what they are given
against the real backup list regardless, so nothing here is a path to be sent
back.

---

## 7. Validation DTOs

### `ValidationReportDto`

```json
{ "findings": [
    { "severity": "warning", "message": "...", "field": null, "line_index": null },
    { "severity": "warning", "message": "mount point \"/mnt/usb\" does not currently exist on disk",
      "field": "mount_point", "line_index": 2 }
  ],
  "has_errors": false }
```

`severity` is `"error"` (blocks a save) or `"warning"` (advisory). `message`
is written for a user and is safe to display verbatim; it may be multi-line
(the fallback `findmnt` finding described below carries `findmnt`'s whole
raw output). `has_errors` is
precomputed so the UI's "can I save?" test cannot disagree with the backend's.

`field` and `line_index` let the UI render a finding next to the specific
input it is about, instead of only in a flat list:

- `field` is `"mount_point"`, `"source"`, `"fs_type"`, `"description"`, or
  `"trailing_comment"` when the finding is about exactly one of those fields
  on one entry, and `null` otherwise. `mount_point_missing` sets
  `"mount_point"`; `malformed_uuid` and `malformed_partuuid` set `"source"`;
  `empty_fs_type` sets `"fs_type"`. No current check sets `"description"` -
  the only one that did required a description to be a single line, which
  stopped being true when descriptions became multi-line (§5), so it was
  removed. The value stays part of this enum and a UI must keep handling it:
  a description is still an editable per-entry input (not one of the six
  fstab fields) that a future check can attach a finding to, without a
  contract change. `trailing_comment_contains_newline` sets
  `"trailing_comment"`: a trailing comment is appended inline after the
  entry's six fields on the same physical line (§5), so unlike a
  description it must stay a single line, and an embedded newline in it is a
  blocking error.
- `line_index` is the 0-based index, into the `DocumentDto.lines` array that
  was sent to `validate_candidate`, of the single line the finding is about -
  the same identity scheme `LineDto` already uses elsewhere in this
  contract (a line's identity is simply its index in `lines`). Usually set
  exactly when `field` is set (both or neither) - with two exceptions, both
  of which set `line_index` alone and leave `field` null. One is a
  `malformed` line present finding: it is about exactly one identifiable
  line, but that line was never a parsed entry with fields to localize to.
  The other is a `findmnt --verify` finding (see below): `findmnt` names the
  mount point each of its complaints is about, which identifies the line,
  but not which of that entry's fields is at fault. A consumer must treat
  `line_index: <N>, field: null` as "about this whole line, not one field of
  it," not assume the pairing always holds.
- `findmnt --verify` findings are, most of the time, tagged exactly like any
  other line-specific finding - `line_index` set, `field` null, the same
  shape as the malformed-line case above. `findmnt` groups its output by
  mount point, so the backend splits it into one finding per complaint
  (each carrying its own `error`/`warning` severity, from `findmnt`'s own
  `[E]`/`[W]` marker) and attaches each to the entry whose mount point it
  names. A UI can therefore render these per row and per drawer, not as one
  unscoped blob. The untagged whole-document form (both fields `null`, one
  finding whose `message` carries `findmnt`'s entire raw output) is the
  fallback, used only when the output cannot be confidently attributed:
  `findmnt` printed something the backend does not recognize, or it named a
  mount point matching no entry or more than one (e.g. the `none` several
  swap entries share). The fallback is all-or-nothing - a response never
  mixes per-line `findmnt` findings with a raw dump of the same run - so a
  consumer must handle both forms, and must not assume a `findmnt` finding
  always has a `line_index`.
- Both are `null` for a finding that is genuinely about no single line:
  duplicate mount point (spans two or more lines, not one), the
  degraded-mode warning (`findmnt` unavailable), and the `findmnt --verify`
  fallback described above.

Because `validate_candidate` can be called against a document containing
several entries, a response can carry several independent findings with the
same `field` but different `line_index` values (e.g. two different entries
both missing their mount point) - a caller must key off *both* `field` and
`line_index` together to tell which entry a given finding is about, not
`field` alone.

Typical findings: duplicate mount point, empty filesystem type, malformed
`UUID=`/`PARTUUID=` value, mount point missing on disk (warning),
malformed lines present (warning), a `findmnt --verify` complaint about one
entry (usually per-line, falling back to one untagged finding carrying
`findmnt`'s whole output when attribution fails - see above), and a warning
that the semantic check ran in degraded mode because `findmnt` is
unavailable.

---

## 8. Write DTOs and error semantics

### `WriteOutcomeDto`

```json
{
  "target": "/etc/fstab",
  "backup": "/etc/fstab.rustab-backup.0000000001.000000002",
  "pruned_backups": [],
  "validation_warnings": [ /* FindingDto */ ],
  "notes": ["..."],
  "elevated": true,
  "draft_sync_warning": null
}
```

- `target` is the file that was actually written. For a default-launch save
  (§3.5 case 1) that is `/etc/fstab`, **not** the active target's draft path.
  It differs from the written path only if that path was a symlink, in which
  case this is its destination (the write goes *through* the link, never
  replacing it).
- `backup` is `null` when the written file did not exist before (first-ever
  save). It is a backup of the file named in `target` - so, for a
  default-launch save, one that lives beside `/etc/fstab`.
- `validation_warnings` are advisory findings that did not block the save -
  worth a toast or an expandable notice.
- `notes` are non-fatal deviations (a symlink was resolved, ownership could
  not be carried over, an old backup could not be pruned). None of them
  affect the integrity of the file just written.
- `elevated` is `true` if the write was performed through `pkexec`.
- `draft_sync_warning` (T34) is the one **non-fatal warning** a *successful*
  save can carry: the real `/etc/fstab` was written, but the local working
  draft beside it could not be updated to match (§3.5). It is a complete,
  ready-to-show message, or `null` when there is nothing to say - which is
  every save that did not mirror into a draft at all (an explicit
  `RUSTAB_TARGET_PATH` target) and every one whose mirror succeeded. Show it
  as a warning toast on top of the success path; do **not** treat it as a
  failed save, and do not fold it into `notes`, which are statements about
  the file that *was* written rather than about a second file that was not.
  Optional on the wire: a response without the field deserializes as `null`.

### `WriteErrorDto`

```json
{
  "kind": "privilege_escalation",
  "message": "...",
  "target_unchanged": true,
  "report": null,
  "path": null,
  "backup": null,
  "external_change": null
}
```

- `message` already explains the consequence ("the original file is
  unchanged", "it is in place but may not survive a power loss") and is safe
  to display verbatim.
- `report` is non-null only for `kind: "validation"`, and then carries every
  finding, warnings included.
- `path` is the single path the failure concerned, when there is one.
- `backup` is set for `kind: "verify"` - the backup holding the pre-save
  content, i.e. the user's way out.
- For `kind: "verify"` the two file bodies are deliberately **not** sent: the
  recovery path is "restore the backup", not "diff two blobs in a toast".
- `external_change` (T36) is non-null only for `kind: "external_change"`, and
  then carries the two bodies the UI *does* diff:

  ```json
  { "last_known_content": "# what Rustab last knew\n",
    "current_content":    "# what the file holds now\n" }
  ```

  `last_known_content` is the old side (the marker's content, §2) and
  `current_content` the new side (what is on disk right now, i.e. what the
  refused write would have replaced). Both are complete file bodies, always
  sent together - that is why they are one nested object rather than two
  sibling fields - and `path` names the file they describe. Non-UTF-8 bytes in
  the file on disk are rendered lossily here, since the payload exists to be
  displayed; the comparison that produced it was byte-exact.
  Optional on the wire: a response without the field deserializes as `null`.

**`target_unchanged` is the field to branch on.** `true` is a guarantee:
the target still holds its previous bytes. `false` is *not* a claim that the
file is damaged - it means the guarantee cannot be made. Nothing ever sets it
to `true` on a maybe.

| `kind` | Stage | `target_unchanged` |
| --- | --- | --- |
| `target_resolution` | The active target could not be determined | `true` |
| `validation` | Candidate has blocking findings (nothing was opened for writing) | `true` |
| `invalid_target` | Path is a directory / dangling symlink / has no file name; or the elevated writer refused a non-`/etc/fstab` target | `true` |
| `backup` | Inspecting or copying the previous content failed | `true` |
| `write` | Creating or writing the temp file failed (e.g. `ENOSPC`) | `true` |
| `metadata` | Applying the previous file's mode to the temp file failed | `true` |
| `fsync` | An `fsync` failed | `true` before the rename, **`false`** for the post-rename directory `fsync` (content is live, durability unconfirmed) |
| `rename` | The atomic rename failed | `true` |
| `read_back` | The target could not be re-read to verify it | `false` (the rename had succeeded) |
| `verify` | The re-read content did not match what was written | `false` |
| `privilege_escalation` | `pkexec` missing, prompt dismissed or refused, or the helper gave no usable result | `true` for dismissed/refused/not-launched; **`false`** when the helper exited without a usable response |
| `invalid_backup` | The requested backup is not one of this target's backups, or the list could not be read | `true` |
| `read_backup` | The backup could not be read, or is not valid UTF-8 | `true` |
| `internal` | A bug or protocol violation between the two halves of the elevated write | `true` |
| `external_change` | The real `/etc/fstab` changed outside Rustab since this session synced with it, so the write was refused before it began (§3.5) | `true`, always |

Suggested UI treatment: `target_unchanged: true` → show the message, keep the
user's pending changes, offer retry. `target_unchanged: false` → show the
message, reload the target with `list_entries`, and point at the backup panel.

`external_change` is the one kind that is not a fault report but a question,
and the only one with a retry that is *not* the same call again: show the diff
from its `external_change` payload, then either cancel (nothing happened; the
file still holds the other content) or repeat the call with
`allow_external_change: true`. Do not retry it silently, and do not treat it
as a failed save - nothing was attempted.

### `CommandErrorDto` (read-side commands)

```json
{ "kind": "not_utf8", "message": "..." }
```

`kind` is one of `target_resolution`, `read_target`, `not_utf8`,
`list_backups`, `invalid_backup`, `read_backup`, `open_location`,
`open_external_link`, `open_path`, `resync`. None of these commands run the
backup/validate/atomic-rename sequence `WriteErrorDto`'s `target_unchanged`
exists to describe, so there is no such field here - not even for
`resync_external_change` (§3.15), whose own local-draft publish is the same
lightweight copy-and-marker primitive the once-per-launch draft refresh (§2)
already uses, never `write_document`.

| `kind` | Meaning | Raised by |
| --- | --- | --- |
| `target_resolution` | The active target could not be determined | every read-side command |
| `read_target` | The target exists but could not be read | `list_entries` |
| `not_utf8` | The bytes read are not valid UTF-8 and are refused rather than mangled | `list_entries` (the target), `read_backup` (a backup), `resync_external_change` (the real `/etc/fstab`) |
| `list_backups` | The target's directory could not be listed | `list_backups` |
| `invalid_backup` | The requested backup is not one of this target's backups, or the list could not be read. Nothing was read. | `read_backup` |
| `read_backup` | The backup is a genuine one but could not be read (permissions, I/O) | `read_backup` |
| `open_location` | The host file manager could not be launched | `open_backup_location` |
| `open_external_link` | The browser could not be launched | `open_external_link` |
| `open_path` | The host file manager could not be launched for the frontend-supplied path | `open_path_in_file_manager` |
| `resync` | (T37) Either the active target is not the default working draft (fail-closed guard; nothing touched), or the real file's current content was read but could not be published onto the draft and its marker | `resync_external_change` |

### `DeleteBackupErrorDto` (`delete_backup` only)

```json
{ "kind": "invalid_backup", "message": "..." }
```

The same two fields as `CommandErrorDto` and nothing else. In particular
there is **no `target_unchanged`**, on purpose: `delete_backup` never opens,
renames or writes the active target, so that flag would be trivially `true`
for every variant and would invite the UI to read significance into it. The
only question this error answers is "is the backup still there", and the
`kind` plus the message say it.

| `kind` | Meaning | Is the backup still there? |
| --- | --- | --- |
| `target_resolution` | The active target could not be determined, so its backups could not be identified | yes |
| `invalid_backup` | The requested path is not one of this target's backups, or the list could not be read. The path was never used as a path. | yes (and it may never have existed) |
| `privilege_escalation` | Elevated cases only (§4.2 - a default launch, or an explicit `/etc/fstab` override): `pkexec` missing, prompt dismissed or refused, or the elevated helper gave no usable response | yes for dismissed/refused/not-launched; **unknown** when the helper exited without a usable response - the message says so, and the fix is to re-call `list_backups` |
| `delete` | The file is a genuine backup of this target but could not be removed (permissions, I/O error, or the elevated helper refusing a target other than `/etc/fstab`) | yes |

`message` is written for a user and is safe to display verbatim.

### `CreateBackupErrorDto` (`create_backup` only)

```json
{ "kind": "target_missing", "message": "..." }
```

The same two fields as `DeleteBackupErrorDto`, and **no `target_unchanged`**,
for the same reason: `create_backup` never opens, renames or writes the active
target, so the flag would be trivially `true` everywhere. The only question
this error answers is "does a new backup exist", and the `kind` plus the
message say it.

| `kind` | Meaning | Was a backup created? |
| --- | --- | --- |
| `target_resolution` | The active target could not be determined, so there was nothing to back up | no |
| `target_missing` | The target does not exist on disk yet (a first-ever-save state). Tell the user to save once first - this is an ordinary state, not a fault. | no |
| `invalid_target` | The target cannot be backed up at all: it is a directory, a dangling symlink, or has no file-name component. Also how the elevated helper refuses a target other than `/etc/fstab`. | no |
| `backup` | The copy, its `fsync`, or the read-back of the created backup failed (permissions, I/O, no space). The target itself was only ever read. | no, or it exists but could not be described - the message says which |
| `privilege_escalation` | Elevated cases only (§4.2 - a default launch, or an explicit `/etc/fstab` override): `pkexec` missing, prompt dismissed or refused, or the elevated helper gave no usable response | no for dismissed/refused/not-launched; **unknown** when the helper exited without a usable response - the message says so, and the fix is to re-call `list_backups` |

---

## 9. Backup and device DTOs

### `BackupInfoDto`

```json
{ "path": "/etc/fstab.rustab-backup.0000000001.000000002",
  "file_name": "fstab.rustab-backup.0000000001.000000002",
  "target_path": "/etc/fstab",
  "size_bytes": 1842,
  "created_unix_secs": 1760000000,
  "is_manual": false }
```

`path` is the exact string to hand back to `restore_backup` (§3.7),
`read_backup` (§3.9) and `delete_backup` (§3.10).
`target_path` is the file this is a backup *of* - the backup-owning file of
§3.6, so since T35 it reads `/etc/fstab` on a default launch (as in the
example above) rather than the working draft's path. It will therefore differ
from `TargetInfoDto.path` (§6) in exactly that case; do not assume the two
match.
`created_unix_secs` may be `null` if the platform could not report an mtime.

`is_manual` is `true` for a backup the user asked for with `create_backup`
(§3.11) and `false` for one a save took on its own. The backend owns the
naming scheme the distinction is encoded in and reports it here; the frontend
must not parse `file_name` to work it out. The practical difference is
retention: automatic backups are pruned past 10 per target, manual ones never
are (§3.6).

### `DeviceDiscoveryDto` / `DeviceInfoDto`

```json
{ "devices": [ { "path": "/dev/sda1",
                 "uuid": "4f3a9c2e-...",
                 "label": null,
                 "partuuid": null,
                 "partlabel": null,
                 "fs_type": "ext4" } ],
  "lsblk_available": true }
```

Only `path` is always present; every other field is `null` when the device
does not have it.

---

## 10. Wire-format lock

These exact strings are asserted by unit tests in `src-tauri/src/dto.rs`:

```json
{"lines":[{"kind":"entry","entry":{"source":{"kind":"uuid","value":"1111"},"mount_point":"/mnt/d","fs_type":"ext4","options":[{"key":"noauto","value":null},{"key":"fmask","value":"0022"}],"dump":0,"pass":2,"trailing_comment":" note","description":"hi"}},{"kind":"blank"},{"kind":"comment","text":" freestanding"},{"kind":"malformed","raw":"/dev/sdb1"}]}
```

```json
{"target":{"path":"/p","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":null},"exists":true,"text":"# hi\n","document":{"lines":[{"kind":"blank"}]},"has_malformed_lines":false}
{"findings":[{"severity":"warning","message":"m","field":null,"line_index":null}],"has_errors":false}
{"findings":[{"severity":"warning","message":"m","field":"mount_point","line_index":2}],"has_errors":false}
{"target":"/p","backup":"/p.bak","pruned_backups":[],"validation_warnings":[],"notes":["n"],"elevated":true,"draft_sync_warning":null}
{"kind":"privilege_escalation","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null,"external_change":null}
{"kind":"external_change","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null,"external_change":{"last_known_content":"# old\n","current_content":"# new\n"}}
{"kind":"not_utf8","message":"m"}
{"kind":"invalid_backup","message":"m"}
{"path":"/p.b","file_name":"p.b","target_path":"/p","size_bytes":12,"created_unix_secs":1760000000,"is_manual":true}
{"target":"/p","backup":"/p.b","pruned_backups":[],"notes":["n"]}
{"kind":"target_missing","message":"m"}
{"devices":[{"path":"/dev/sda1","uuid":"u","label":null,"partuuid":null,"partlabel":null,"fs_type":"ext4"}],"lsblk_available":true}
```

`TargetInfoDto` (§6) is pinned on its own as well, in both of its T39 states,
by `dto::tests::target_info_carries_the_real_target_backup_fallback_dir_verbatim`:

```json
{"path":"/home/u/.local/share/rustab/fstab.draft","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":null}
{"path":"/home/u/.local/share/rustab/fstab.draft","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":"/home/u/.local/share/rustab/etc-fstab-backups"}
```

Note that the field is always present: `null` is sent, never omitted, so a
frontend may read it unconditionally.

Line by line after the `DocumentViewDto`: the next two are both
`ValidationReportDto`, showing an untagged (`field`/`line_index` both `null`)
and a field/line-tagged finding respectively - see §7 for what the two extra
fields mean. Then `WriteOutcomeDto`, then **two** `WriteErrorDto`s: an ordinary
one (every kind-specific field `null`) and a `kind: "external_change"` one
carrying its `external_change` payload (§8), which is the only shape in which
those two file bodies are ever sent. Then `CommandErrorDto` and
`DeleteBackupErrorDto` - the same two-field shape, distinguished only by which
command produced them and by which `kind` strings are possible - then
`BackupInfoDto`, then `CreateBackupOutcomeDto`, which only ever travels inside
the elevated protocol (§4.3) and never reaches the frontend, then
`CreateBackupErrorDto` (that same two-field shape again) and
`DeviceDiscoveryDto`.

Every `kind` spelling of all four error-kind enums is pinned individually by
`dto::tests::every_error_kind_spelling_is_exactly_as_documented`, because the
frontend matches these strings exhaustively:

- `WriteErrorKindDto`: `target_resolution`, `validation`, `invalid_target`,
  `backup`, `write`, `metadata`, `fsync`, `rename`, `read_back`, `verify`,
  `privilege_escalation`, `invalid_backup`, `read_backup`, `internal`,
  `external_change`.
- `CommandErrorKindDto`: `target_resolution`, `read_target`, `not_utf8`,
  `list_backups`, `invalid_backup`, `read_backup`, `open_location`,
  `open_external_link`, `open_path`, `resync`.
- `DeleteBackupErrorKindDto`: `target_resolution`, `invalid_backup`,
  `privilege_escalation`, `delete`.
- `CreateBackupErrorKindDto`: `target_resolution`, `target_missing`,
  `invalid_target`, `backup`, `privilege_escalation`.

Likewise, every `field` spelling of `ValidationFieldDto` (§7) is pinned
individually by `dto::tests::every_validation_field_spelling_is_exactly_as_documented`:
`mount_point`, `source`, `fs_type`, `description`.

---

## 11. Other notes for integrators

- **Non-UTF-8 paths.** Paths cross the boundary as strings and a non-UTF-8
  path is rendered lossily. Such a path can be displayed but not acted on:
  `restore_backup`, `read_backup` and `delete_backup` each match what they are
  given against the real backup list, so a mangled string fails closed with
  `invalid_backup` rather than reading, writing or deleting somewhere
  unintended. Non-UTF-8 target paths are not a supported configuration.
  `open_path_in_file_manager`'s `path` is the second kind of path the
  frontend may send (§2) and is not matched against any list - it is
  deliberately safe to send as-is, for the reasons §2 gives in full; a
  non-UTF-8 value there is rendered lossily like any other path, and the
  file manager is launched on whatever that lossy string names.
- **Reload after every save.** `WriteOutcomeDto` intentionally does not carry
  the new document. Call `list_entries` again so the UI shows what is
  actually on disk, not what it hoped to put there.
- **Reload the backup list after every deletion.** `delete_backup` resolves
  with `null` and carries no outcome object, and a
  `privilege_escalation` failure can leave the outcome genuinely unknown.
  Call `list_backups` again rather than removing a row locally.
- **Concurrency.** Commands run on a thread pool and can overlap. The backend
  keeps no session state at all - every command re-resolves the target - so
  there is nothing to invalidate, but two concurrent saves would race on the
  file. Keep the UI's single-save-at-a-time gating (`docs/UI_DESIGN.md` §6).
  The same applies to a deletion: a backup deleted while a save is pruning
  old backups can produce an `invalid_backup` from either side, which means
  "it is already gone", not "something is wrong".
- **The internal flag is not a public CLI.** `rustab
  --internal-privileged-write` and its stdin/stdout protocol (all three
  operations of it) are an implementation detail of §4, unstable by design,
  and must not be invoked by anything but the escalation path.
- **Manual smoke test of the elevated paths** (deliberately not automated -
  they need a password prompt and touch the real file and its backups; run
  them by hand on a machine you can afford to fix, and check
  `findmnt --verify` afterwards):
  ```sh
  cargo build -p rustab
  RUSTAB_TARGET_PATH=/etc/fstab ./target/debug/rustab
  ```
  Save once (that creates a backup), then delete that backup from the backup
  panel and confirm with `ls /etc/fstab.rustab-backup.*` that exactly that
  file is gone and `/etc/fstab` is untouched. Then create a backup from the
  panel and confirm a `.manual`-suffixed file appeared next to `/etc/fstab`
  holding its current content, with `/etc/fstab` itself unchanged
  (`cmp /etc/fstab /etc/fstab.rustab-backup.*.manual`).

  Automated tests instead exercise the elevated handler's own function
  directly, with a temp file standing in for `/etc/fstab` - including the
  delete operation's refusal of every near-miss path and each operation's
  refusal of a target other than the allowed one - and the `pkexec`-result
  interpretation as a pure function shared by all three operations.
