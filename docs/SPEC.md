# Rustab — Project Specification

## 1. Purpose

Rustab is a modern, native Linux desktop GUI application for viewing, editing,
adding, and removing entries in `/etc/fstab`. It targets users comfortable
with the general concept of fstab but who want a safer, clearer interface
than hand-editing the file, with full fidelity to everything classic fstab
and modern systemd-generator-aware fstab supports.

This project will be published as open source on GitHub. Code quality,
documentation, and conventions must be production-grade from the first
commit, not a prototype.

## 2. Non-negotiable priorities (in order)

1. **Stability / data safety.** It must never be possible for a crash,
   partial write, invalid entry, or lost privilege escalation to leave
   `/etc/fstab` (or whatever file is targeted) truncated, corrupted, or in a
   state that fails to boot a machine. See §6 (Atomicity) — this is the
   single most important requirement in this spec and overrides convenience
   or feature requests if they ever conflict.
2. **Correctness / full fstab fidelity.** Every field, device-identifier
   style, filesystem type, and option fstab/mount(8)/systemd.mount(5)
   support must be representable and round-trip losslessly.
3. **Code quality or craftsmanship.** As much logic as reasonably possible
   lives in Rust. Idiomatic, `clippy`-clean, `rustfmt`-formatted, documented
   Rust. Tailwind/JS is a build-time styling tool only, never application
   logic.
4. **Usability / modern UI.** Clear, modern GUI, but never at the cost of 1-3.

## 3. Tech stack

- **Backend / shell**: [Tauri 2](https://tauri.app/) (already verified
  available locally: `cargo-tauri` 2.2.7).
- **Frontend**: [Yew](https://yew.rs/) compiled to WASM via
  [Trunk](https://trunkrs.dev/) (verified available: `trunk` 0.21.14).
  No React/Vue/Svelte, no hand-written JS application logic. The only
  JS/TS allowed is what Tauri's own generated API glue requires.
- **Styling**: Tailwind CSS, invoked as a build-time CLI step from a Trunk
  pre-build hook (or a small `build.rs`/npm script limited to CSS
  generation). Tailwind's CLI needs Node only as its own runtime — no
  `node_modules` dependency may ship in the final bundle or be required at
  app runtime. Document this build step clearly in the README.
- **Core logic**: a pure Rust library crate with **zero** Tauri or GUI
  dependency, so the fstab parser/writer/validator can be unit-tested in
  isolation and reused (e.g. from a future CLI) without pulling in a GUI
  stack.

## 4. Suggested workspace layout

```
rustab/
├── Cargo.toml                  # workspace root
├── crates/
│   └── fstab-core/             # pure Rust: model, parser, atomic writer, validation
├── src-tauri/                  # Tauri app shell, exposes #[tauri::command]s, polkit/pkexec glue
├── frontend/                   # Yew + Trunk + Tailwind UI
├── tests/fixtures/             # synthetic, hand-crafted sample fstab files (safe to publish)
├── dev-data/                   # gitignored. Local-only copy of a *real* fstab for manual
│                                # sandbox testing. NEVER read by automated tests, NEVER committed.
├── docs/                       # SPEC.md (this file), ADRs if useful
└── .github/workflows/          # CI: fmt --check, clippy -D warnings, test, build
```

Adjust naming/structure if there's a better idiomatic layout, but keep the
separation between GUI-free core logic and the Tauri/Yew shells — that
boundary is what makes the atomicity and parsing guarantees testable.

## 5. fstab domain coverage

The parser/model must handle, at minimum:

- **Fields**: `<source>  <mount point>  <fs type>  <options>  <dump>  <pass>`,
  arbitrary whitespace/tab separated, trailing comments.
- **Source identifiers**: raw device path (`/dev/sda1`), `UUID=`, `LABEL=`,
  `PARTUUID=`, `PARTLABEL=`, `none` (for swap/bind/virtual), and network
  sources (e.g. `//server/share`, `server:/path` for cifs/nfs). Preserve
  whatever octal-escaped whitespace (`\040`, `\011`, etc.) appears in paths —
  fstab requires spaces in paths to be escaped this way; do not mangle it.
- **Mount points**: including bind mounts where the "device" field is
  itself a path.
- **Filesystem types**: `ext4`, `btrfs`, `xfs`, `vfat`, `ntfs3`, `swap`,
  `none`, `nfs`/`nfs4`, `cifs`, `overlay`, `tmpfs`, etc. — don't hardcode an
  exhaustive allowlist that rejects valid-but-uncommon types; validate
  structurally, not against a closed enum of "known" filesystems.
- **Options**: arbitrary comma-separated option list including common ones
  (`defaults`, `noatime`, `relatime`, `ro`, `rw`, `user`, `nofail`, `auto`,
  `noauto`, `bind`, `sw`) *and* the `x-systemd.*` family used heavily by
  modern systemd-mount-generator fstabs (`x-systemd.automount`,
  `x-systemd.idle-timeout=`, `x-systemd.requires-mounts-for=`,
  `x-systemd.device-timeout=`, etc.) and vfat-specific options
  (`fmask=`, `dmask=`, `codepage=`, `iocharset=`, `shortname=`,
  `errors=remount-ro`). Treat options generically as a `key` or `key=value`
  list rather than a fixed set, while still offering first-class UI
  affordances (toggles, pickers) for the well-known ones.
- **dump/pass**: integers, default handling per convention (0 for most,
  1/2 for fsck ordering).
- **Comments and blank lines**: must be preserved on write, in their
  original position relative to entries, including a file's leading
  comment block. Rustab is not allowed to silently reformat or drop a
  user's existing comments when saving. (Realistic fstabs — see the
  anonymized fixtures — mix section-header comments, inline comments above
  entries, and blank-line separators; all of that must survive a load →
  edit-one-entry → save round trip unchanged elsewhere in the file.)
- **Swap entries**, **bind mounts** (`none` fstype, `bind` option), and
  entries with an empty options/placeholder line (a source line with no
  other fields, as can appear for a commented-out or reserved device) should
  all parse without the file being rejected outright — surface malformed
  lines to the user as a visible warning/skip rather than crashing or
  silently corrupting the rest of the file.

## 6. Atomicity (the core safety requirement)

Every save, with no exceptions, must follow a write-ahead, all-or-nothing
sequence. A suggested implementation (adjust as needed, but keep the
guarantees):

1. Parse and validate the **entire** proposed new file content in memory
   first. If any entry fails structural validation, abort before touching
   disk at all.
2. Run a semantic check equivalent to `findmnt --verify --tab-file <path>`
   (or an in-process equivalent) against the fully-rendered candidate file.
   Reject on failure, again before touching the real target file.
3. Write the candidate content to a temp file in the **same directory** as
   the target (not `/tmp`, which may be a different filesystem — rename
   across filesystems is not atomic). Use a name like
   `.<target>.rustab.tmp.<pid>`.
4. `fsync` the temp file, then `fsync` the containing directory, then
   `rename()` the temp file onto the target path. `rename(2)` within the
   same filesystem is atomic on Linux — this is what gives the "all or
   nothing" guarantee: the target either has the fully old content or the
   fully new content, never a partial write.
5. Before step 3-4, copy the current target to a timestamped backup
   (e.g. `/etc/fstab.rustab-backup.<timestamp>`, or a configurable backup
   directory) so the user always has a one-click restore path. Keep a
   bounded number of recent backups (don't grow unbounded).
6. If writing to a privileged path like the real `/etc/fstab`, do the
   privilege escalation (`pkexec`) around a single helper invocation that
   performs steps 3-5 atomically as root — do not do an unprivileged
   temp-file write followed by a separate privileged move where a crash
   between the two could matter; design so the elevated helper either does
   nothing or completes the full atomic swap.
7. After a successful swap, re-read the file back and verify it matches
   what was intended, surfacing a hard error (not a silent warning) if it
   doesn't.

This logic belongs entirely in `fstab-core` (steps 1-2, and the
write/rename/backup primitives), with `src-tauri` only wiring it to a
concrete target path and privilege escalation. Write thorough unit tests
specifically for failure injection (e.g. validation failure leaves the
original file byte-for-byte untouched).

## 7. Target file selection / test safety

- The app must **never** default to writing the real `/etc/fstab` during
  development. Support a configurable target path (env var or app setting),
  defaulting in debug builds to a local sandbox file.
- `dev-data/fstab.real-copy` is a real, personal copy of the maintainer's
  actual `/etc/fstab`, kept **outside of version control** (already in
  `.gitignore`) purely so the app can be exercised by hand against a
  realistic, complex file (multiple UUID entries, swap, several bind mounts,
  `x-systemd.automount` encrypted-volume entries, escaped spaces in paths).
  Treat it as a manual smoke-test fixture, never read it from automated
  tests or ship any content derived from it.
- `tests/fixtures/` must instead contain **synthetic, hand-written** sample
  fstab files that exercise the same syntactic variety (UUID/LABEL/PARTUUID
  sources, bind mounts, swap, systemd options, escaped-space paths, comments
  in various positions, edge cases like a source line with missing fields)
  without containing anyone's real UUIDs, usernames, or paths. These are
  safe to publish and are what the automated test suite runs against.
- All automated/integration tests must operate on temp-directory copies of
  fixtures, never on `/etc/fstab` itself, and never on `dev-data/`.

## 8. Device/UUID/LABEL discovery helpers

To avoid the user having to hand-type UUIDs, add Tauri commands that shell
out to already-available system tools (`lsblk`, `blkid`, `findmnt`) to list
available block devices with their UUID/LABEL/PARTUUID/filesystem type, and
let the GUI offer them as a picker when adding/editing an entry's source.
Treat their absence (sandboxed/minimal systems) as a soft failure — the
manual text entry must still work if these tools are missing.

## 9. GUI requirements

- Table/list view of all current entries (including ones Rustab doesn't
  "understand" perfectly — show raw text with a warning rather than
  dropping them).
- Add / edit / delete an entry, with a form that offers structured inputs
  for source type (dropdown: path/UUID/LABEL/PARTUUID/none, plus a device
  picker from §8), mount point (with a directory picker), filesystem type,
  a checklist/tag-input for options (common ones as toggles, free text for
  anything else), dump, pass.
- Inline validation feedback (e.g. duplicate mount point, mount point
  doesn't exist on disk, malformed UUID) before allowing save.
- A clear "pending changes" / diff view before committing a save, and a
  visible indicator of which file (real `/etc/fstab` vs. a dev/sandbox path)
  is currently targeted — this must never be ambiguous to the user.
- Backup/restore UI: list recent auto-backups (see §6) and let the user
  restore one.
- Tailwind for styling; keep it modern and clean, dark-mode friendly.

## 10. Code quality / OSS conventions

- `rustfmt` and `clippy -D warnings` must pass cleanly; wire both into CI
  (`.github/workflows/ci.yml`): fmt check, clippy, `cargo test` for the
  workspace (especially `fstab-core`), and a build step for both the
  Tauri app and the Trunk frontend.
- Public items (functions, types, modules) in `fstab-core` get `///` rustdoc
  comments explaining contracts/invariants (e.g. "never leaves the target
  file partially written", "returns Err without touching disk if validation
  fails") — not restating the signature. Keep inline comments rare and
  reserved for non-obvious *why* (e.g. the same-filesystem temp-file
  reasoning in §6).
- `README.md` (repo root): what it is, screenshot/GIF placeholder, build &
  run instructions (`trunk`, `cargo tauri dev`/`build`, the Tailwind CLI
  step), the dual-license note, and a clear safety note about the
  dev-sandbox-by-default behavior from §7.
- `CONTRIBUTING.md`: how to set up the dev environment, how to run tests,
  code style expectations (fmt/clippy gates), PR process, and a note that
  contributors should add themselves to `CONTRIBUTORS.md` in their PR.
- `CONTRIBUTORS.md` and `LICENSE-MIT` / `LICENSE-APACHE` already exist at
  the repo root (dual MIT/Apache-2.0, the standard Rust ecosystem choice —
  set `license = "MIT OR Apache-2.0"` in each crate's `Cargo.toml`).
- `.env.example` only if any env var is actually needed (e.g. overriding
  the dev target path from §7) — never commit a real `.env`.

## 11. Explicit non-goals (for v1)

- No remote/SSH management of other machines' fstabs.
- No live mount/unmount execution from the GUI beyond what's needed for
  validation (`findmnt --verify`) — editing the file is the scope, not
  acting as a mount manager.
- No Windows/macOS support — Linux-only, matches `/etc/fstab`,
  `systemd.mount(5)`, `pkexec`/polkit assumptions throughout.
