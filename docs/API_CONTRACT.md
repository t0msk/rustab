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
  unit tests (`dto::tests::document_wire_format_is_exactly_as_documented`
  and `remaining_wire_formats_are_exactly_as_documented`). A change to any
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
| Success | The `invoke` promise resolves with the return shape. |
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

**No command accepts a target path.** The only path the frontend may ever
send is `restore_backup`'s `backup_path`, and it is matched against the
active target's real backup list before being used (see §3.7).

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

The active target's auto-backups, **newest first** (ready for
`docs/UI_DESIGN.md` §7's list order). Only files in the target's own directory
whose names match `fstab-core`'s backup scheme for *that* target are
reported — a backup of a different target in the same directory, or a
hand-made `fstab.bak`, is not.

`created_unix_secs` is the backup file's mtime, i.e. when the backup was
taken; format the relative and ISO timestamps from it in the frontend.

At most 10 backups are kept per target (`fstab_core::writer::MAX_BACKUPS`);
older ones are pruned by a save, which reports them in
`WriteOutcomeDto.pruned_backups`.

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

---

## 4. Privilege escalation

### 4.1 The requirement

`docs/SPEC.md` §6 step 6 forbids the obvious-but-wrong design — an
unprivileged temp-file write followed by a separate privileged `mv`. Two
steps mean a window where a crash, a failure of the second step, or a race
can leave `/etc/fstab` in a state nobody planned. The elevated process must
"either do nothing or complete the full atomic swap".

### 4.2 The mechanism

When `save` (or `restore_backup`) runs against a target with
`is_real == true`:

1. The unprivileged side validates the candidate first — purely to avoid
   asking for a password for a save that cannot succeed. This check writes
   nothing and is **not** the safety net.
2. It re-executes **its own binary** (`std::env::current_exe()`) as
   `pkexec <rustab> --internal-privileged-write`, with stdin, stdout and
   stderr piped.
3. It writes one JSON request to the child's stdin and closes the pipe:
   ```json
   {"target": "/etc/fstab", "candidate": { /* DocumentDto */ }}
   ```
4. The elevated process, in `main()`, sees `--internal-privileged-write` as
   `argv[1]` **before anything else starts** — no Tauri runtime, no webview,
   no plugins, no logger are ever initialized in a root process. It reads
   stdin to EOF, refuses any target other than `/etc/fstab`, reconstructs the
   document, and calls `fstab_core::writer::write_document` **once**.
5. It prints exactly one JSON object to stdout and exits:
   ```json
   {"status": "ok",    "outcome": { /* WriteOutcomeDto */ }}
   {"status": "error", "error":   { /* WriteErrorDto   */ }}
   ```
   Exit codes: `0` success, `1` the write was attempted and failed (a JSON
   error body is present), `2` the request itself was unusable (a JSON error
   body is still present; nothing was written). Diagnostics go to stderr,
   never stdout.
6. The unprivileged side parses that object. A parsable response always wins
   over the exit code — it is the helper's own account of what it did. With
   no usable response, the exit code decides, conservatively (see §8).

The candidate travels as a `DocumentDto`, not as rendered text, so the
elevated side reconstructs the identical document the user was shown and lets
`write_document` render it. There is no second serializer anywhere in the
chain whose output could differ from the first one's.

### 4.3 Why this does the *entire* atomic operation as root

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

### 4.4 Why a re-exec and not a separate helper binary

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
  elevated process reads stdin, calls one function, prints JSON and exits.
- **The authorization cost is identical.** `pkexec` prompts per invocation
  either way, so nothing improves for the user.

### 4.5 Why the target path is sent as data, and re-checked

`pkexec` does not forward the caller's environment, so the elevated process
cannot re-resolve the target itself — `RUSTAB_TARGET_PATH` does not survive
the transition. That is a feature (an env var cannot influence the elevated
process at all), and it is why the path must travel inside the request.

Because the path arrives as data, the elevated side refuses to write anywhere
except `/etc/fstab` (literally, or via a symlink that resolves to it), using
the *same* comparison that decided `is_real` in the first place. Escalation
only ever happens for a real target, so this costs nothing in normal
operation and bounds the elevated write to exactly one file.

**Known limitation:** escalation is therefore only available for the real
`/etc/fstab`. Pointing `RUSTAB_TARGET_PATH` at some *other* root-owned file
gives an in-process save that fails with a permission error
(`kind: "write"`/`"backup"`, `target_unchanged: true`), not a password
prompt. That is the intended trade for v1; widening it would mean the
elevated writer accepting arbitrary paths from data.

### 4.6 What the frontend sees

Nothing special. `save` and `restore_backup` behave identically for both
targets; `WriteOutcomeDto.elevated` is `true` when the write went through
`pkexec`. A dismissed or refused password prompt is an ordinary rejection with
`kind: "privilege_escalation"` and `target_unchanged: true`. The call can sit
unresolved for as long as the prompt is on screen, so keep the "Writing
changes…" state of `docs/UI_DESIGN.md` §6 until it settles, and do not add a
client-side timeout.

**Requirement:** `pkexec` (polkit) must be installed. Without it, a real-target
save rejects with `kind: "privilege_escalation"` and a message saying so. No
polkit `.policy` file ships yet, so `pkexec` applies its default
"authenticate as an administrator" rule; adding a policy file (to refine the
prompt text and the auth rule) is a packaging-time follow-up, not an API
change.

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
`list_backups`. These commands never wrote anything, so there is no
`target_unchanged` field.

---

## 9. Backup and device DTOs

### `BackupInfoDto`

```json
{ "path": "/etc/fstab.rustab-backup.0000000001.000000002",
  "file_name": "fstab.rustab-backup.0000000001.000000002",
  "target_path": "/etc/fstab",
  "size_bytes": 1842,
  "created_unix_secs": 1760000000 }
```

`path` is the exact string to hand back to `restore_backup`.
`created_unix_secs` may be `null` if the platform could not report an mtime.

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
{"path":"/p.b","file_name":"p.b","target_path":"/p","size_bytes":12,"created_unix_secs":1760000000}
{"devices":[{"path":"/dev/sda1","uuid":"u","label":null,"partuuid":null,"partlabel":null,"fs_type":"ext4"}],"lsblk_available":true}
```

---

## 11. Other notes for integrators

- **Non-UTF-8 paths.** Paths cross the boundary as strings and a non-UTF-8
  path is rendered lossily. Such a path can be displayed but not restored
  *to*: `restore_backup` matches what it is given against the real backup
  list, so a mangled string fails closed with `invalid_backup` rather than
  writing somewhere unintended. Non-UTF-8 target paths are not a supported
  configuration.
- **Reload after every save.** `WriteOutcomeDto` intentionally does not carry
  the new document. Call `list_entries` again so the UI shows what is
  actually on disk, not what it hoped to put there.
- **Concurrency.** Commands run on a thread pool and can overlap. The backend
  keeps no session state at all — every command re-resolves the target — so
  there is nothing to invalidate, but two concurrent saves would race on the
  file. Keep the UI's single-save-at-a-time gating (`docs/UI_DESIGN.md` §6).
- **The internal flag is not a public CLI.** `rustab
  --internal-privileged-write` and its stdin/stdout protocol are an
  implementation detail of §4, unstable by design, and must not be invoked by
  anything but the escalation path.
- **Manual smoke test of the elevated path** (deliberately not automated —
  it needs a password prompt and writes the real file; run it by hand on a
  machine you can afford to fix, and check `findmnt --verify` afterwards):
  ```sh
  cargo build -p rustab
  RUSTAB_TARGET_PATH=/etc/fstab ./target/debug/rustab
  ```
  Automated tests instead exercise the elevated writer's own function
  directly, with a temp file standing in for `/etc/fstab`, and the
  `pkexec`-result interpretation as a pure function.
