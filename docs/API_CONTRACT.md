# Rustab — Tauri command API contract

**Status: frozen.** This document is the single source of truth for the
boundary between the Rust backend (`src-tauri/`) and the Yew/wasm frontend
(`frontend/`). T11 and T12 are expected to build against *this file* without
reading the backend's Rust source.

The frontend deliberately has no dependency on `fstab-core` or on
`src-tauri`, so it mirrors the shapes below by hand. That makes the wire
format a real contract rather than an implementation detail:

- Every shape here is produced by hand-written DTO types in
  `src-tauri/src/dto.rs` — never by deriving `Serialize` on a domain type —
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
UUIDs, `findmnt --verify`) is a separate, explicit step — `validate_candidate`
and, authoritatively, the save itself.

---

## 2. Target-path resolution policy

Decided and implemented in `src-tauri/src/target.rs`; summarized here because
it determines what the UI must show and whether a save escalates.

1. If the environment variable **`RUSTAB_TARGET_PATH`** is set to a non-blank
   value, that path is the active target, verbatim, in **both debug and
   release builds**. `source` is `"environment"`.
2. Otherwise the target is a per-user sandbox file,
   `$XDG_DATA_HOME/rustab/fstab.sandbox` (falling back to
   `$HOME/.local/share/rustab/fstab.sandbox`, then to a temp-dir path). It is
   created on first resolution, seeded with a comment-only file that parses
   and validates cleanly. `source` is `"sandbox_default"`.

Rule 2 applies to release builds too. `docs/SPEC.md` §7 only *requires* the
sandbox default in debug builds, but a release build that opens pointed at
`/etc/fstab` would have the file whose corruption §2.1 calls the one
unacceptable outcome loaded before the user has agreed to anything. Editing
the real file is therefore an explicit act: start Rustab with
`RUSTAB_TARGET_PATH=/etc/fstab`. (An in-GUI target switch is a future task,
not something this layer invents. When it arrives it must go through
`target.rs`, not through a path argument on a command.)

**`is_real`** is `true` only when the resolved path *is* `/etc/fstab` —
literally, or because both paths canonicalize to the same file, so a symlink
in either direction is still recognized. The flag drives three things at
once, and they cannot disagree because they all read the same field: the UI's
danger banner and the extra confirmation checkbox (`docs/UI_DESIGN.md` §2,
§6), and whether a save escalates (§4 below).

**No command accepts a target path.** The only paths the frontend may ever
send are the `backup_path` arguments of `restore_backup` (§3.7),
`read_backup` (§3.9) and `delete_backup` (§3.10), and each is matched against
the active target's real backup list before being used as a path.

---

## 3. Commands

### 3.1 `get_target_info`

```
get_target_info() -> TargetInfoDto          (rejects with CommandErrorDto)
```

Which file this session reads and writes. Call it on startup and after any
save; everything else in the UI that mentions the target should come from
here rather than being remembered.

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
`not_utf8` — a target that is not valid UTF-8 is **refused**, not read
lossily, because loading it with replacement characters and then saving would
corrupt a file that was fine.

### 3.3 `validate_candidate`

```
validate_candidate(candidate: DocumentDto) -> ValidationReportDto    (does not reject)
```

Runs the same `fstab_core::validate::validate` a save runs — structural
checks plus a `findmnt --verify` semantic check against a throwaway temp
rendering of the candidate. Writes nothing, anywhere, and never touches the
target.

Use it for the inline feedback of `docs/UI_DESIGN.md` §5 and to decide
whether the save button is enabled (`has_errors`). It is not a substitute for
the save's own validation — the save validates again, inside the same
operation that writes, so a document that passes here can still be rejected
there (e.g. if the system changed in between). Treat a `validation` error from
`save` as a normal outcome, not an impossible one.

Note: `findmnt` is an external process, so this is not free. Debounce it;
don't call it per keystroke.

### 3.4 `render_candidate`

```
render_candidate(candidate: DocumentDto) -> string                   (does not reject)
```

Renders a candidate to the exact text a save would write, byte for byte. Pure
— no I/O at all. Exists because the diff view has to show the real "after"
text and the frontend has no serializer of its own.

(Not in T8's original command list; added because §6 of the UI design cannot
be built without it, and the alternative — a second serializer in the
frontend — would be a byte-for-byte fidelity risk for no benefit.)

### 3.5 `save`

```
save(candidate: DocumentDto) -> WriteOutcomeDto     (rejects with WriteErrorDto)
```

Atomically replaces the active target with `candidate`. **The only write path
in the application.**

- Non-real target (sandbox or any other file): performed in-process by
  `fstab_core::writer::write_document`.
- Real `/etc/fstab` (`is_real == true`): performed by a single
  privilege-escalated call that runs the *entire* write as root — see §4.

Both paths run the same `write_document`, which means every save, regardless
of target, gets the whole `docs/SPEC.md` §6 sequence: validate → back up the
previous content and prune old backups → write a temp file in the target's own
directory → `fsync` → `fsync` the directory → `rename` → `fsync` the
directory → re-read and verify byte-for-byte.

On failure, **always read `target_unchanged`** before writing UI copy:
`true` means the file on disk is guaranteed to still hold its previous bytes
(offer "fix and retry"); `false` means that guarantee cannot be made (offer
"reload the file" / "restore a backup"). See §8 for the full mapping.

### 3.6 `list_backups`

```
list_backups() -> BackupInfoDto[]           (rejects with CommandErrorDto)
```

The active target's backups, **newest first** (ready for
`docs/UI_DESIGN.md` §7's list order), automatic and manual alike. Only files
in the target's own directory whose names match `fstab-core`'s backup scheme
for *that* target are reported — a backup of a different target in the same
directory, or a hand-made `fstab.bak`, is not.

`created_unix_secs` is the backup file's mtime, i.e. when the backup was
taken; format the relative and ISO timestamps from it in the frontend.

`is_manual` says which kind each row is: `true` for one the user asked for
with `create_backup` (§3.11), `false` for one a save took on its own. Do not
infer this from `file_name` — the backend owns the naming scheme and reports
the answer.

At most 10 **automatic** backups are kept per target
(`fstab_core::writer::MAX_BACKUPS`); older ones are pruned by a save, which
reports them in `WriteOutcomeDto.pruned_backups`. Manual backups are exempt:
they are neither counted towards that bound nor ever pruned to honour it, so
the list can hold any number of them. The only thing that removes a manual
backup is `delete_backup` (§3.10).

Error kinds: `target_resolution`, `list_backups`.

### 3.7 `restore_backup`

```
restore_backup(backup_path: string) -> WriteOutcomeDto   (rejects with WriteErrorDto)
```

Restores a backup onto the active target. `backup_path` must be a `path`
value from a `list_backups` result for the **current** target.

A restore is not a copy and not a shortcut. The backup's text is parsed and
then goes through *the same* save path as `save`: it is validated, the content
it replaces is itself backed up first, and it is written through the same
atomic temp-file/`fsync`/`rename`/verify sequence — elevated exactly once if
the target is the real `/etc/fstab`. Consequences the UI should state plainly
(the copy in `docs/UI_DESIGN.md` §7 already does):

- Restoring creates a fresh backup of what is currently in the file, so a
  restore is itself undoable.
- Restoring a backup whose content no longer validates will be **refused**,
  with a `validation` error. This is intentional: "it used to be in the file"
  is not evidence that writing it now is safe.

`backup_path` is never used as a path until it has been found in the real
backup list, so `/etc/shadow`, `../../etc/fstab`, a different target's backup
and a near-miss file name all fail closed with `invalid_backup` — nothing is
read and nothing is written. (A file in the target's own directory whose name
matches the scheme *exactly* does count as a backup of that target, even if
Rustab did not create it: in that directory the name is the identity. No
privilege is gained by that — creating such a file next to `/etc/fstab`
already requires the access needed to edit `/etc/fstab` directly — and the
restore still validates, backs up and writes atomically.)

Error kinds: `target_resolution`, `invalid_backup`, `read_backup`, plus any
`save` error kind.

### 3.8 `discover_devices`

```
discover_devices() -> DeviceDiscoveryDto    (does not reject)
```

Block devices for the source picker, via `lsblk`. Never fails: a system
without a usable `lsblk` yields `{"devices": [], "lsblk_available": false}`,
and the UI must keep manual entry fully available (`docs/SPEC.md` §8). Show
"device discovery unavailable", not "no devices found", when
`lsblk_available` is `false`.

### 3.9 `read_backup`

```
read_backup(backup_path: string) -> string      (rejects with CommandErrorDto)
```

Returns a backup's raw text, exactly as the bytes on disk. `backup_path` must
be a `path` value from a `list_backups` result for the **current** target, and
is resolved the same way `restore_backup` resolves it (§3.7): matched against
the real backup list before it is ever used as a path, so `/etc/shadow`,
`../../etc/fstab`, a different target's backup and a near-miss file name all
fail closed with `invalid_backup` — nothing is read.

Intended for a backup-diff view: diff this against `list_entries`'s `text` to
show what restoring this backup would change. There is no `render_candidate`
step involved — both sides are already raw text.

**No privilege escalation, for either target kind.** `/etc/fstab` and the
backups beside it are world-readable, which is the same reason `list_entries`
and `list_backups` already work against the real target without a password
prompt. This command never prompts and never writes.

Error kinds: `target_resolution`, `invalid_backup`, `read_backup`, `not_utf8`.
A backup that is not valid UTF-8 is **refused** (`not_utf8`) rather than
returned lossily, for the same reason `list_entries` refuses such a target —
and note that such a backup cannot be restored either (§3.7's `read_backup`
error), so the UI can say the same thing in both places.

### 3.10 `delete_backup`

```
delete_backup(backup_path: string) -> null   (rejects with DeleteBackupErrorDto)
```

Deletes one of the active target's backups. `backup_path` must be a `path`
value from a `list_backups` result for the **current** target; it is resolved
exactly as in §3.7 and §3.9, so the same set of near misses and arbitrary
paths fail closed with `invalid_backup` and nothing is removed.

- **It only ever removes a backup file.** The active target is never one of
  its own backups, so it can never be the file removed here — asserted in both
  halves of the implementation, including inside the elevated process.
- **It escalates for the real target, exactly like `save` and
  `restore_backup`.** The backups of `/etc/fstab` live beside it in
  root-owned `/etc/` (`<target>.rustab-backup.<secs>.<nanos>`), which an
  unprivileged process cannot unlink, so a real-target deletion goes through
  the same single `pkexec` re-exec — see §4. A non-real target's backups live
  in a user-owned directory and are removed in-process with no prompt.
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

Copies the active target's **current** content to a new backup beside it and
returns that backup as a `BackupInfoDto` — the same row shape `list_backups`
produces, with `is_manual: true`. Takes no arguments: like every other
command, which file it acts on is resolved server-side and never supplied by
the frontend.

- **It never changes the target.** No temp file, no rename, no validation —
  the target is only ever *read*. That is why the rejection is a
  `CreateBackupErrorDto` and not a `WriteErrorDto`: there is no
  `target_unchanged` question to answer, exactly as for `delete_backup`
  (§3.10).
- **The backup it creates is exempt from pruning.** Automatic backups (a
  save's side effect) are bounded at 10 per target and pruned oldest-first;
  a manual backup is neither counted towards that bound nor removed to honour
  it, so a later flurry of saves cannot silently discard a snapshot the user
  deliberately took. Only `delete_backup` removes it.
- **It escalates for the real target, exactly like `save` and
  `delete_backup`.** A backup of `/etc/fstab` has to be created *in*
  root-owned `/etc/`, so a real-target backup goes through the same single
  `pkexec` re-exec — see §4. A non-real target's backups live in a user-owned
  directory and are created in-process with no prompt.
- **A target that does not exist yet is an error, not a no-op.** The kind is
  `target_missing`, and the UI should say what the message says: there is no
  file to back up yet, save once first. An explicit "back this up now" must
  never resolve with nothing to show for it.

On success, the returned row is read back from the real backup list, so it
describes what is actually on disk. Re-call `list_backups` (or prepend the
returned row) to refresh the panel.

Error kinds: `target_resolution`, `target_missing`, `invalid_target`,
`backup`, `privilege_escalation`.

---

## 4. Privilege escalation

### 4.1 The requirement

`docs/SPEC.md` §6 step 6 forbids the obvious-but-wrong design — an
unprivileged temp-file write followed by a separate privileged `mv`. Two
steps mean a window where a crash, a failure of the second step, or a race
can leave `/etc/fstab` in a state nobody planned. The elevated process must
"either do nothing or complete the full atomic swap".

### 4.2 Three operations, one flag

Three things need root, and only when `is_real == true`:

| Operation | Used by | What the elevated process does |
| --- | --- | --- |
| `write` | `save` (§3.5), `restore_backup` (§3.7) | One `fstab_core::writer::write_document` call: the whole `docs/SPEC.md` §6 sequence. |
| `delete_backup` | `delete_backup` (§3.10) | Resolve the requested path against the target's real backup list, then one `remove_file`. |
| `create_backup` | `create_backup` (§3.11) | One `fstab_core::writer::create_backup` call: copy the target to a new `.manual` backup beside it and `fsync` the copy. The target is only read. |

They share a **single** hidden re-exec flag. Which operation is being asked
for is a field of the request, not a second flag, so there is exactly one
branch in the whole program that can turn Rustab into a root process, and the
"no Tauri runtime, webview, plugin or logger is ever initialized as root"
property stays a one-flag check.

### 4.3 The mechanism

When `save`, `restore_backup`, `delete_backup` or `create_backup` runs against
a target with `is_real == true`:

1. The unprivileged side does its cheap pre-check first — purely to avoid
   asking for a password for an operation that cannot succeed. For a write
   that is validating the candidate; for a delete it is resolving
   `backup_path` against the backup list; a `create_backup` has nothing to
   pre-check, since its request carries no path but the target. None of these
   write anything and none is **the safety net** (see §4.6).
2. It re-executes **its own binary** (`std::env::current_exe()`) as
   `pkexec <rustab> --internal-privileged-write`, with stdin, stdout and
   stderr piped. (The flag's name is historical — it predates the two backup
   operations and now carries any of them. It is deliberately unchanged: the
   point is that there is exactly one such string.)
3. It writes one JSON request to the child's stdin and closes the pipe. The
   request is a union tagged by `op`:
   ```json
   {"op": "write",         "target": "/etc/fstab", "candidate": { /* DocumentDto */ }}
   {"op": "delete_backup", "target": "/etc/fstab", "backup_path": "/etc/fstab.rustab-backup.0000000001.000000002"}
   {"op": "create_backup", "target": "/etc/fstab"}
   ```
4. The elevated process, in `main()`, sees `--internal-privileged-write` as
   `argv[1]` **before anything else starts** — no Tauri runtime, no webview,
   no plugins, no logger are ever initialized in a root process. It reads
   stdin to EOF exactly once, refuses any `target` other than `/etc/fstab`
   (for either operation), and then:
   - `write`: reconstructs the document and calls
     `fstab_core::writer::write_document` **once**.
   - `delete_backup`: resolves `backup_path` against the *already-validated*
     target's real backup list — in this process, trusting nothing the
     unprivileged side claimed — and calls `remove_file` on the path that
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

   `CreateBackupOutcomeDto` is internal to this protocol — it never reaches
   the frontend. The `create_backup` command turns it into the
   `BackupInfoDto` §3.11 documents.

   Exit codes: `0` success, `1` the operation was attempted and failed (a JSON
   error body is present), `2` the request itself was unusable (a JSON error
   body is still present; nothing was written and nothing was deleted).
   Diagnostics go to stderr, never stdout.
6. The unprivileged side parses that object. A parsable response always wins
   over the exit code — it is the helper's own account of what it did. A
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
  `fsync`, and the byte-for-byte read-back verification — in a single
  process, under a single authorization.
- The unprivileged side writes **nothing, anywhere, ever**. It serializes
  JSON to a pipe and reads JSON back. There is no temp file on that side to
  be moved later, no partial state to resume, and no second privileged call.
- Therefore, kill the elevated process at any instant and the possible
  outcomes are exactly the ones `write_document` itself guarantees — the
  target holds either its complete old content or its complete new content —
  because `write_document` is the only thing that ever touched the disk.
- It is the *same* audited code: not a copy, not a port, not a
  reimplementation. The elevated path calls the identical function compiled
  from the identical source in the same build.

None of this applies to the two backup operations, and none of it is needed
there: neither touches the target file (a `create_backup` only *reads* it),
so there is nothing to make atomic about the target. The elevated delete is
deliberately two checks and one `remove_file`; the elevated create is one
check and one `create_backup` call — no backup-of-a-backup, no temp file, no
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
  and exits. Adding the second and third operations did not add entry points —
  each added one `match` arm behind the same single flag (§4.2).
- **The authorization cost is identical.** `pkexec` prompts per invocation
  either way, so nothing improves for the user.

### 4.6 Why the target path is sent as data, and re-checked

`pkexec` does not forward the caller's environment, so the elevated process
cannot re-resolve the target itself — `RUSTAB_TARGET_PATH` does not survive
the transition. That is a feature (an env var cannot influence the elevated
process at all), and it is why the path must travel inside the request.

Because the path arrives as data, the elevated side refuses to act on anything
except `/etc/fstab` (literally, or via a symlink that resolves to it), using
the *same* comparison that decided `is_real` in the first place. Escalation
only ever happens for a real target, so this costs nothing in normal
operation and bounds the elevated write to exactly one file.

`delete_backup`'s `backup_path` arrives as data for the same reason and is
re-checked the same way, with one extra step on top of the target check: the
elevated process resolves it against the real backup list of the target it
has *already* verified, and unlinks the path that list yielded — never the
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

**Known limitation:** escalation is therefore only available for the real
`/etc/fstab`. Pointing `RUSTAB_TARGET_PATH` at some *other* root-owned file
gives an in-process save that fails with a permission error
(`kind: "write"`/`"backup"`, `target_unchanged: true`), not a password
prompt — and, likewise, an in-process backup deletion that fails with
`kind: "delete"` and an in-process backup creation that fails with
`kind: "backup"`. That is the intended trade for v1; widening it would mean
the elevated process accepting arbitrary paths from data.

### 4.7 What the frontend sees

Nothing special. `save`, `restore_backup`, `delete_backup` and
`create_backup` behave identically for both targets;
`WriteOutcomeDto.elevated` is `true` when a write went through `pkexec`. A
dismissed or refused password prompt is an ordinary rejection with
`kind: "privilege_escalation"` — plus `target_unchanged: true` for a write;
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
  "trailing_comment": " a note after the six fields, without the #"
}
```

- `source.kind` is one of `"path"`, `"uuid"`, `"label"`, `"part_uuid"`,
  `"part_label"`, `"none"`, `"network"`, `"other"`. `source.value` never
  includes the `KEY=` prefix — it is re-added when the file is written. For
  `"none"` the value is ignored on the way in and reported as `"none"`.
- `options[].value` is `null` for a bare flag (`noauto`) and a string for a
  `key=value` option. `""` means a literal `key=` with nothing after the `=`,
  which is a real thing in fstab (`x-systemd.automount=`) and is preserved.
- An **empty** `options` array renders as an empty options field, which
  `findmnt --verify` may reject. Offer `defaults` rather than nothing.
- `mount_point` and path-like `source.value`s keep fstab's octal escapes
  (`\040` for a space) **exactly as they appear in the file** — undecoded on
  the way out and un-re-encoded on the way in. If the UI displays a decoded
  form, it must re-encode before sending, or it will rewrite the user's paths.
- `dump` and `pass` are unsigned integers. fstab imposes no tighter range, so
  neither does this contract; validation flags the odd ones.
- `trailing_comment` is `null` when there is none.

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
{ "path": "/home/u/.local/share/rustab/fstab.sandbox",
  "is_real": false,
  "source": "sandbox_default" }
```

`source` is `"environment"` or `"sandbox_default"` (see §2). `is_real` is the
one flag the danger banner and the escalation decision both read.

---

## 7. Validation DTOs

### `ValidationReportDto`

```json
{ "findings": [ { "severity": "warning", "message": "..." } ],
  "has_errors": false }
```

`severity` is `"error"` (blocks a save) or `"warning"` (advisory). `message`
is written for a user and is safe to display verbatim; it may be multi-line
(the `findmnt` finding includes `findmnt`'s own output). `has_errors` is
precomputed so the UI's "can I save?" test cannot disagree with the backend's.

Typical findings: duplicate mount point, empty filesystem type, malformed
`UUID=`/`PARTUUID=` value, mount point missing on disk (warning), malformed
lines present (warning), `findmnt --verify` output, and a warning that the
semantic check ran in degraded mode because `findmnt` is unavailable.

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
  "elevated": true
}
```

- `target` differs from the active target's path only if that path was a
  symlink, in which case this is its destination (the write goes *through*
  the link, never replacing it).
- `backup` is `null` when the target did not exist before (first-ever save).
- `validation_warnings` are advisory findings that did not block the save —
  worth a toast or an expandable notice.
- `notes` are non-fatal deviations (a symlink was resolved, ownership could
  not be carried over, an old backup could not be pruned). None of them
  affect the integrity of the file just written.
- `elevated` is `true` if the write was performed through `pkexec`.

### `WriteErrorDto`

```json
{
  "kind": "privilege_escalation",
  "message": "...",
  "target_unchanged": true,
  "report": null,
  "path": null,
  "backup": null
}
```

- `message` already explains the consequence ("the original file is
  unchanged", "it is in place but may not survive a power loss") and is safe
  to display verbatim.
- `report` is non-null only for `kind: "validation"`, and then carries every
  finding, warnings included.
- `path` is the single path the failure concerned, when there is one.
- `backup` is set for `kind: "verify"` — the backup holding the pre-save
  content, i.e. the user's way out.
- For `kind: "verify"` the two file bodies are deliberately **not** sent: the
  recovery path is "restore the backup", not "diff two blobs in a toast".

**`target_unchanged` is the field to branch on.** `true` is a guarantee:
the target still holds its previous bytes. `false` is *not* a claim that the
file is damaged — it means the guarantee cannot be made. Nothing ever sets it
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

Suggested UI treatment: `target_unchanged: true` → show the message, keep the
user's pending changes, offer retry. `target_unchanged: false` → show the
message, reload the target with `list_entries`, and point at the backup panel.

### `CommandErrorDto` (read-side commands)

```json
{ "kind": "not_utf8", "message": "..." }
```

`kind` is one of `target_resolution`, `read_target`, `not_utf8`,
`list_backups`, `invalid_backup`, `read_backup`. These commands never wrote
anything, so there is no `target_unchanged` field.

| `kind` | Meaning | Raised by |
| --- | --- | --- |
| `target_resolution` | The active target could not be determined | every read-side command |
| `read_target` | The target exists but could not be read | `list_entries` |
| `not_utf8` | The bytes read are not valid UTF-8 and are refused rather than mangled | `list_entries` (the target), `read_backup` (a backup) |
| `list_backups` | The target's directory could not be listed | `list_backups` |
| `invalid_backup` | The requested backup is not one of this target's backups, or the list could not be read. Nothing was read. | `read_backup` |
| `read_backup` | The backup is a genuine one but could not be read (permissions, I/O) | `read_backup` |

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
| `privilege_escalation` | Real target only: `pkexec` missing, prompt dismissed or refused, or the elevated helper gave no usable response | yes for dismissed/refused/not-launched; **unknown** when the helper exited without a usable response — the message says so, and the fix is to re-call `list_backups` |
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
| `target_missing` | The target does not exist on disk yet (a first-ever-save state). Tell the user to save once first — this is an ordinary state, not a fault. | no |
| `invalid_target` | The target cannot be backed up at all: it is a directory, a dangling symlink, or has no file-name component. Also how the elevated helper refuses a target other than `/etc/fstab`. | no |
| `backup` | The copy, its `fsync`, or the read-back of the created backup failed (permissions, I/O, no space). The target itself was only ever read. | no, or it exists but could not be described — the message says which |
| `privilege_escalation` | Real target only: `pkexec` missing, prompt dismissed or refused, or the elevated helper gave no usable response | no for dismissed/refused/not-launched; **unknown** when the helper exited without a usable response — the message says so, and the fix is to re-call `list_backups` |

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
{"lines":[{"kind":"comment","text":" hi"},{"kind":"blank"},{"kind":"entry","entry":{"source":{"kind":"uuid","value":"1111"},"mount_point":"/mnt/d","fs_type":"ext4","options":[{"key":"noauto","value":null},{"key":"fmask","value":"0022"}],"dump":0,"pass":2,"trailing_comment":" note"}},{"kind":"malformed","raw":"/dev/sdb1"}]}
```

```json
{"target":{"path":"/p","is_real":false,"source":"sandbox_default"},"exists":true,"text":"# hi\n","document":{"lines":[{"kind":"blank"}]},"has_malformed_lines":false}
{"findings":[{"severity":"warning","message":"m"}],"has_errors":false}
{"target":"/p","backup":"/p.bak","pruned_backups":[],"validation_warnings":[],"notes":["n"],"elevated":true}
{"kind":"privilege_escalation","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null}
{"kind":"not_utf8","message":"m"}
{"kind":"invalid_backup","message":"m"}
{"path":"/p.b","file_name":"p.b","target_path":"/p","size_bytes":12,"created_unix_secs":1760000000,"is_manual":true}
{"target":"/p","backup":"/p.b","pruned_backups":[],"notes":["n"]}
{"kind":"target_missing","message":"m"}
{"devices":[{"path":"/dev/sda1","uuid":"u","label":null,"partuuid":null,"partlabel":null,"fs_type":"ext4"}],"lsblk_available":true}
```

The fifth line above is `CommandErrorDto` and the sixth is
`DeleteBackupErrorDto` — they are the same two-field shape, distinguished only
by which command produced them and by which `kind` strings are possible. The
same goes for the ninth, `CreateBackupErrorDto`. The eighth is
`CreateBackupOutcomeDto`, which only ever travels inside the elevated
protocol (§4.3), never to the frontend.

Every `kind` spelling of all three error-kind enums is pinned individually by
`dto::tests::every_error_kind_spelling_is_exactly_as_documented`, because the
frontend matches these strings exhaustively:

- `CommandErrorKindDto`: `target_resolution`, `read_target`, `not_utf8`,
  `list_backups`, `invalid_backup`, `read_backup`.
- `DeleteBackupErrorKindDto`: `target_resolution`, `invalid_backup`,
  `privilege_escalation`, `delete`.
- `CreateBackupErrorKindDto`: `target_resolution`, `target_missing`,
  `invalid_target`, `backup`, `privilege_escalation`.

---

## 11. Other notes for integrators

- **Non-UTF-8 paths.** Paths cross the boundary as strings and a non-UTF-8
  path is rendered lossily. Such a path can be displayed but not acted on:
  `restore_backup`, `read_backup` and `delete_backup` each match what they are
  given against the real backup list, so a mangled string fails closed with
  `invalid_backup` rather than reading, writing or deleting somewhere
  unintended. Non-UTF-8 target paths are not a supported configuration.
- **Reload after every save.** `WriteOutcomeDto` intentionally does not carry
  the new document. Call `list_entries` again so the UI shows what is
  actually on disk, not what it hoped to put there.
- **Reload the backup list after every deletion.** `delete_backup` resolves
  with `null` and carries no outcome object, and a
  `privilege_escalation` failure can leave the outcome genuinely unknown.
  Call `list_backups` again rather than removing a row locally.
- **Concurrency.** Commands run on a thread pool and can overlap. The backend
  keeps no session state at all — every command re-resolves the target — so
  there is nothing to invalidate, but two concurrent saves would race on the
  file. Keep the UI's single-save-at-a-time gating (`docs/UI_DESIGN.md` §6).
  The same applies to a deletion: a backup deleted while a save is pruning
  old backups can produce an `invalid_backup` from either side, which means
  "it is already gone", not "something is wrong".
- **The internal flag is not a public CLI.** `rustab
  --internal-privileged-write` and its stdin/stdout protocol (all three
  operations of it) are an implementation detail of §4, unstable by design,
  and must not be invoked by anything but the escalation path.
- **Manual smoke test of the elevated paths** (deliberately not automated —
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
  directly, with a temp file standing in for `/etc/fstab` — including the
  delete operation's refusal of every near-miss path and each operation's
  refusal of a target other than the allowed one — and the `pkexec`-result
  interpretation as a pure function shared by all three operations.
