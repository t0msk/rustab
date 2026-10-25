# Rustab - Project Specification

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
   state that fails to boot a machine. See §6 (Atomicity) - this is the
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
  generation). Tailwind's CLI needs Node only as its own runtime - no
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
separation between GUI-free core logic and the Tauri/Yew shells - that
boundary is what makes the atomicity and parsing guarantees testable.

## 5. fstab domain coverage

The parser/model must handle, at minimum:

- **Fields**: `<source>  <mount point>  <fs type>  <options>  <dump>  <pass>`,
  arbitrary whitespace/tab separated, trailing comments.
- **Source identifiers**: raw device path (`/dev/sda1`), `UUID=`, `LABEL=`,
  `PARTUUID=`, `PARTLABEL=`, `none` (for swap/bind/virtual), and network
  sources (e.g. `//server/share`, `server:/path` for cifs/nfs). Preserve
  whatever octal-escaped whitespace (`\040`, `\011`, etc.) appears in paths -
  fstab requires spaces in paths to be escaped this way; do not mangle it.
- **Mount points**: including bind mounts where the "device" field is
  itself a path.
- **Filesystem types**: `ext4`, `btrfs`, `xfs`, `vfat`, `ntfs3`, `swap`,
  `none`, `nfs`/`nfs4`, `cifs`, `overlay`, `tmpfs`, etc. - don't hardcode an
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
  user's existing comments when saving. (Realistic fstabs - see the
  anonymized fixtures - mix section-header comments, inline comments above
  entries, and blank-line separators; all of that must survive a load →
  edit-one-entry → save round trip unchanged elsewhere in the file.)
- **Swap entries**, **bind mounts** (`none` fstype, `bind` option), and
  entries with an empty options/placeholder line (a source line with no
  other fields, as can appear for a commented-out or reserved device) should
  all parse without the file being rejected outright - surface malformed
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
   the target (not `/tmp`, which may be a different filesystem - rename
   across filesystems is not atomic). Use a name like
   `.<target>.rustab.tmp.<pid>`.
4. `fsync` the temp file, then `fsync` the containing directory, then
   `rename()` the temp file onto the target path. `rename(2)` within the
   same filesystem is atomic on Linux - this is what gives the "all or
   nothing" guarantee: the target either has the fully old content or the
   fully new content, never a partial write.
5. Before step 3-4, copy the current target to a timestamped backup
   (e.g. `/etc/fstab.rustab-backup.<timestamp>`, or a configurable backup
   directory) so the user always has a one-click restore path. Keep a
   bounded number of recent backups (don't grow unbounded).
   Alongside these **automatic** backups, the user can also take a
   **manual** one at any time without saving (an explicit "back up now"
   action, which copies the target and never writes it); such a backup is
   named with one extra trailing `.manual` suffix
   (`<target>.rustab-backup.<secs>.<nanos>.manual`) and is **exempt** from
   the automatic bound - it is neither counted towards it nor pruned to
   honour it, and only an explicit user deletion removes it. The rationale:
   a snapshot the user deliberately asked to keep must not be evicted by
   unrelated later saves, and manual snapshots must not be able to evict the
   automatic safety net either.
6. If writing to a privileged path like the real `/etc/fstab`, do the
   privilege escalation (`pkexec`) around a single helper invocation that
   performs steps 3-5 atomically as root - do not do an unprivileged
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

**Revised 2026-10-17 (T34).** Rustab's purpose is editing the real
`/etc/fstab` (§1), and its default, unconfigured behavior now reflects that
directly: a save writes the real `/etc/fstab`, full stop. The sandbox file
exists to protect in-progress work, not to withhold writes from the real
file - see the rationale below. This supersedes this section's pre-T34
text, which described the sandbox as an isolated, non-writing target
selected in place of the real file; it is no longer that.

- **Every save writes `/etc/fstab`, regardless of `RUSTAB_TARGET_PATH`.** A
  save always goes through the single privilege-escalated (`pkexec`) write
  path described in §6, gated by the same mandatory diff-review and explicit
  confirmation checkbox the real target has always required (§9) - every
  save is now the consequential kind, so that gate is no longer conditional
  on "is this the real target."
- **The local draft file** (`$XDG_DATA_HOME/rustab/fstab.draft`, falling
  back as before) is what the GUI actually reads and edits day to day. Its
  purpose is purely to protect in-progress, not-yet-saved work across an app
  restart or crash - it is a *session*, not an isolated test target. A
  successful save writes `/etc/fstab` first, then updates the draft to
  match, so they do not drift under normal use.
- **Draft refresh on launch is now conditional, not unconditional.** Where
  the pre-T34 design refreshed the draft from `/etc/fstab` on every single
  launch (deliberately discarding whatever was there), that would now throw
  away an unsaved draft on every restart - unacceptable once the draft's
  entire purpose is protecting in-progress work. The launch-time check must
  instead compare the real `/etc/fstab`'s current state against the state
  Rustab last knew about it (as of the last refresh or the last successful
  save) and refresh the draft **only if they differ** - i.e. only when
  something changed `/etc/fstab` outside Rustab since it last looked. If
  they match, the draft - including any unsaved edits - is left exactly as
  it was. The comparison must not be fooled by Rustab's own last write to
  `/etc/fstab` appearing as "external drift" (it isn't). As before, the
  real file is only ever *read* for this, never written, and the whole
  mechanism is best-effort: an unreadable `/etc/fstab` falls back to a
  comment-only seed when the draft is absent, and otherwise leaves the
  draft alone.
- **Development and testing still need a way to never touch the real
  file.** `RUSTAB_TARGET_PATH`, set to anything other than `/etc/fstab`
  itself (a fixture, a scratch copy, `dev-data/fstab.real-copy`, etc.),
  continues to make that path the target outright - no dual-write to
  `/etc/fstab`, no draft-mirroring, exactly as a plain non-real target
  worked before T34. This is now the **only** way to run or test Rustab
  without touching the real file - see `CONTRIBUTING.md`'s "Safety-first
  contribution notes", which must stay in sync with this.
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
  fixtures and injected stand-ins for the real-file/elevated-write
  collaborators, never on `/etc/fstab` itself, never invoking real `pkexec`,
  and never on `dev-data/`.

## 8. Device/UUID/LABEL discovery helpers

To avoid the user having to hand-type UUIDs, add Tauri commands that shell
out to already-available system tools (`lsblk`, `blkid`, `findmnt`) to list
available block devices with their UUID/LABEL/PARTUUID/filesystem type, and
let the GUI offer them as a picker when adding/editing an entry's source.
Treat their absence (sandboxed/minimal systems) as a soft failure - the
manual text entry must still work if these tools are missing.

## 9. GUI requirements

- Table/list view of all current entries (including ones Rustab doesn't
  "understand" perfectly - show raw text with a warning rather than
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
  is currently targeted - this must never be ambiguous to the user.
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
  fails") - not restating the signature. Keep inline comments rare and
  reserved for non-obvious *why* (e.g. the same-filesystem temp-file
  reasoning in §6).
- `README.md` (repo root): what it is, screenshot/GIF placeholder, build &
  run instructions (`trunk`, `cargo tauri dev`/`build`, the Tailwind CLI
  step), the dual-license note, and a clear safety note about the
  real-write-by-default / persistent-draft behavior from §7.
- **README must also include an explicit, public "AI use" disclosure
  section.** Required content (the maintainer's own words, adapt phrasing
  but keep every point - this is a deliberate transparency statement, not
  boilerplate):
  - Yes, AI assistance (Claude Code) was used during development.
  - The maintainer is a senior software developer with 10+ years of
    experience, who personally code-reviewed and tested every change.
  - An automated test suite backs the codebase (link/mention the test
    counts if it reads naturally), in addition to manual review.
  - Tone: confident and matter-of-fact, not apologetic or defensive -
    stating a development practice, not making an excuse.
  - Contact email for the project throughout README/CONTRIBUTORS/issue
    templates is `opensource@tomsk.dev` (not a personal address).
  - README must also include a Discord badge/link to the maintainer's dev
    server, `https://discord.gg/vymDkeQnpj`, with a Discord icon (e.g. a
    shields.io badge: `https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white`
    linking to that invite), placed near the other contact info so people
    who want to reach the maintainer can find it easily.
  - README must also include a GitHub Sponsors support badge/button
    linking to `https://github.com/sponsors/t0msk` (e.g. a shields.io
    badge: `https://img.shields.io/badge/Sponsor-%E2%9D%A4-db61a2?logo=githubsponsors&logoColor=white`),
    placed alongside the Discord/contact badges. Also add a
    `.github/FUNDING.yml` with `github: [t0msk]` - this is a one-line,
    low-effort addition that makes GitHub render its own native "Sponsor"
    button on the repo's main page too, on top of the README badge.
- `CONTRIBUTING.md`: how to set up the dev environment, how to run tests,
  code style expectations (fmt/clippy gates), PR process, and a note that
  contributors should add themselves to `CONTRIBUTORS.md` in their PR.
- `CONTRIBUTORS.md` and `LICENSE-MIT` / `LICENSE-APACHE` already exist at
  the repo root (dual MIT/Apache-2.0, the standard Rust ecosystem choice -
  set `license = "MIT OR Apache-2.0"` in each crate's `Cargo.toml`).
- `.env.example` only if any env var is actually needed (e.g. overriding
  the dev target path from §7) - never commit a real `.env`.
- **Commit/PR authorship**: see the repo-root `CLAUDE.md` - no AI
  co-author trailer in commits or PR descriptions in this repository, by
  the maintainer's explicit request. AI use is disclosed once, clearly, in
  README instead of on every commit.

## 12. Versioning

- Every crate (`fstab-core`, `rustab-privileged`, `rustab-helper`, the
  `src-tauri` app, `frontend`) and
  `src-tauri/tauri.conf.json` start at **`0.0.0`**, not Cargo's default
  `0.1.0`. Keep all of them in lockstep at the same version - this is a
  single-product release, not independently versioned libraries.
- Follow semver (`MAJOR.MINOR.PATCH`). The maintainer decides, per release,
  which part bumps - see §13; don't infer it automatically from commit
  messages or diff size.

## 13. Release & packaging CI/CD

Two distinct GitHub Actions concerns, kept separate so a routine merge
never silently cuts a version the maintainer didn't ask for:

1. **Continuous build on the `release` branch** (separate from the
   existing `ci.yml`, which already gates every push/PR with
   fmt/clippy/test/build). Add a workflow triggered on push to the
   `release` branch that builds installable packages from the current
   `release` HEAD every time something is merged into it:
   - `.deb` via `cargo-deb` (or an equivalent Tauri bundler step -
     `cargo tauri build` already produces `.deb` and `.AppImage` bundles
     out of the box for a Tauri 2 app; check whether that covers it
     before adding a separate `cargo-deb` step).
   - `.rpm` via `cargo tauri build` targeting rpm (Tauri's bundler
     supports rpm) or `cargo-generate-rpm` if the Tauri bundler output
     isn't sufficient.
   - `.AppImage` via the Tauri bundler's AppImage target.
   - Upload all three as workflow artifacts on every push to `release`,
     so there's always a downloadable build from the latest `release`
     state, independent of whether a version bump happens.
2. **Explicit, maintainer-triggered version bump + GitHub Release**, kept
   separate from (1) so the bump level is always a deliberate choice, not
   inferred: a `workflow_dispatch`-triggered workflow (run manually from
   the `release` branch, after merging) with a **required choice input**
   `bump` restricted to `patch` / `minor` / `major`. On run, it should:
   - Read the current version (from `src-tauri/tauri.conf.json` or any
     one crate - they're kept in lockstep per §12), compute the new
     semver value for the chosen bump level.
   - Update the version field in every workspace crate's `Cargo.toml` and
     in `tauri.conf.json`, commit that as `chore(release): vX.Y.Z`
     (respecting the no-AI-attribution policy), and create an annotated
     git tag `vX.Y.Z`.
   - Re-run the packaging step from (1) against that exact commit and
     attach the resulting `.deb`/`.rpm`/`.AppImage` to a new GitHub
     Release for tag `vX.Y.Z`, using the tag as the release title.
   - This is the *only* thing that advances the version - a plain push to
     `release` (point 1) must never bump it on its own.

## 14. Flatpak / Flathub - documentation only (not implemented in v1)

Add `docs/FLATPAK.md`: a self-contained, step-by-step guide for a future
contributor (or the maintainer later) to package Rustab as a Flatpak and
submit it to Flathub. Cover, concretely for *this* project's stack:
- Why Flatpak packaging for a Tauri app has extra considerations (WebKitGTK
  runtime dependency, needing the `org.freedesktop.Platform`/`Sdk` plus the
  GNOME or similar runtime that provides WebKitGTK, and that Tauri's own
  docs have Linux-packaging guidance worth cross-referencing).
- How to write the Flatpak manifest (`dev.tomsk.Rustab.yml` or similar
  reverse-DNS app ID - note the app will need a reverse-DNS id distinct
  from the GitHub repo name) using `flatpak-builder`, and how a Rust/Cargo
  build integrates (e.g. `flatpak-cargo-generator` for vendoring
  dependencies offline, since Flathub's build sandbox has no network
  access).
- Why `pkexec`/polkit-based privilege escalation needs special handling
  under Flatpak's sandbox (portals, or documenting that the Flatpak build
  may need `--filesystem=/etc/fstab:rw` or a host-system D-Bus/portal
  approach instead of a bare `pkexec` call) - flag this as something that
  needs real testing on a Flatpak build before submission, not something
  to assume works unchanged.
- The actual Flathub submission process: forking `flathub/flathub`,
  opening a PR with the manifest, their review/CI bot, and ongoing
  maintenance expectations (keeping the manifest's dependency versions
  updated).
- This is guidance for a reader to follow later - do not actually build or
  submit a Flatpak package as part of this task.

## 11. Explicit non-goals (for v1)

- No remote/SSH management of other machines' fstabs.
- No live mount/unmount execution from the GUI beyond what's needed for
  validation (`findmnt --verify`) - editing the file is the scope, not
  acting as a mount manager.
- No Windows/macOS support - Linux-only, matches `/etc/fstab`,
  `systemd.mount(5)`, `pkexec`/polkit assumptions throughout.
