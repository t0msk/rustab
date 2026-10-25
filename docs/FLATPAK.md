# Flatpak Packaging Guide for Rustab

**Status: planning document with real scaffolding, plus one part that is now
settled by experiment.** As of T40, §3 (privilege escalation under Flatpak)
is **no longer a planning section**: the app has been built with
`flatpak-builder --user --install`, installed, and exercised in a real
sandbox, and the mechanism that works is `flatpak-spawn --host pkexec`
with the single permission `--talk-name=org.freedesktop.Flatpak`. Read §3
before anything else in here, and treat its Options A and B as a record of
what does *not* work. Everything else in this guide - the submission
process, the offline-build chain, the checklists - is still planning
material, and the scope boundary below still applies to it. This revision also corrects several things the previous version got
wrong or stale about the actual Flathub submission process - see "What
changed in this revision" below if you read the old version before.

This guide was produced by an orchestrating session reading the actual repo
tree and Flathub's live, current documentation (`docs.flathub.org`,
checked 2026-10-16) rather than from memory, specifically to avoid
repeating the previous version's mistake of describing a plausible-sounding
but stale process. Anywhere this guide says "verified" below, it means
"checked live against an actual tool or a live request in this session,"
not "asserted confidently." Anywhere it says "unverified" or "untested," it
means exactly that - don't read it as false modesty.

**Scope boundary**, as originally written and still true of everything
except §3: nothing in this guide or the files in `flatpak/` was submitted
anywhere by the session that wrote it. No GitHub repository was created, no
git remote was added or changed, nothing was pushed, no fork of
`flathub/flathub` was made. Every step below that requires one of those is
labelled **[MAINTAINER ACTION]** and is real follow-up work, not something
this guide pretends was already done.

What *has* happened since, and what §3 now rests on: a local
`flatpak-builder --user --install` build of this app was run and installed,
and the sandboxed read/elevate mechanism was exercised against the real host
`/etc/fstab` (reads and the full helper spawn chain). The one thing still
untested is a **completed elevated write**: finishing that needs a human at
a polkit authentication dialog, and no automated session should drive one.
The maintainer's remaining check is therefore "press Save in the Flatpak
build, authenticate, confirm `/etc/fstab` changed and a backup appeared
beside it".

---

## 0. What changed in this revision

If you read an earlier version of this file, the load-bearing corrections
are:

1. **The Flathub submission process itself is different from what the old
   version described.** The old version said "fork `flathub/flathub`, open
   a PR against `master`, which merges into a new repo later." That is
   **stale**. Verified live against `docs.flathub.org/docs/for-app-authors/submission`
   on 2026-10-16: you fork `flathub/flathub`, clone with
   `--branch=new-pr`, branch off **`new-pr`** (not `master`), and open the
   PR against the **`new-pr`** base branch, not `master`. The dedicated
   `github.com/flathub/<app-id>` repository genuinely is something
   *Flathub creates for you* after the submission PR is approved and
   merged - not something you create yourself. §5 below has the exact,
   current steps.
2. **AppStream metainfo filename**: confirmed current is `.metainfo.xml` in
   `/app/share/metainfo/`; `.appdata.xml` / `/app/share/appdata/` is
   explicitly documented by Flathub itself as the **legacy** path and
   filename. `flatpak/dev.tomsk.Rustab.metainfo.xml` uses the current name.
3. **Paths fixed to match this actual repo.** The old version wrote
   `resources/dev.tomsk.Rustab.desktop`, `resources/icon.png`, etc., as if
   a `resources/` directory existed. It doesn't, anywhere in this repo
   (confirmed by listing the tree). The real icon assets live at
   `src-tauri/icons/` - already used by Tauri's own bundler config in
   `src-tauri/tauri.conf.json` - and the new desktop/metainfo files live in
   the new `flatpak/` directory this revision adds.
4. **Runtime version**: `org.gnome.Platform`/`org.gnome.Sdk` `'46'` (the old
   value) is stale. Checked live against Flathub's own API for
   `org.gnome.TextEditor` (an official GNOME app kept maximally current) on
   2026-10-16: current is **`51`**. This number moves roughly twice a year
   (GNOME's release cadence) - re-check before relying on this number.
5. **A previously-undocumented, project-specific wrinkle**: Rustab has
   **two independent `Cargo.lock` files** (the root workspace, covering
   `fstab-core` + `src-tauri`; and `frontend/Cargo.lock`, a separate
   `wasm32-unknown-unknown` project driven by Trunk). Both need their own
   `flatpak-cargo-generator` output. The old version's manifest skeleton
   only accounted for one `Cargo.lock`.
6. **A new, genuinely unresolved problem this revision surfaces rather than
   papering over**: getting Trunk's own build chain (Tailwind CLI via
   Node, then `wasm-bindgen`/`sass` that Trunk itself fetches) to run
   *offline* inside Flathub's network-less build sandbox. See "The Trunk
   offline build problem" in §2 below. Neither of the two real Tauri+Rust
   Flatpak apps this guide's manifest was checked against
   (`pl.fuzjajadrowa.pulsar`, `io.github.lullabyX.sone`) use Trunk, so
   there was no existing precedent to copy here - this is specific to
   Rustab's toolchain choice (Yew + Trunk) and is flagged, not solved.
7. **Flathub's Generative AI policy** (new, and important - see §1.4
   below): it directly affects whether the manifest this revision
   scaffolds can be submitted as drafted, without further human rewriting.

---

## 1. Why Tauri apps on Linux need extra Flatpak considerations

### 1.1 WebKitGTK and runtime dependencies

Tauri 2 on Linux uses WebKitGTK (via `libwebkit2gtk-4.1`) to render the
frontend, rather than bundling Chromium. This has implications for
Flatpak:

- **No bundled browser engine**: unlike Electron apps, you don't ship a
  copy of Chromium inside the bundle. Flatpak's runtime (e.g.
  `org.gnome.Platform`) provides WebKitGTK as a system library instead.
- **Runtime choice**: Flathub's standard approach for a WebKitGTK app is
  the GNOME runtime (`org.gnome.Platform`), which bundles WebKitGTK. The
  plainer `org.freedesktop.Platform` does not, by itself.
- **SDK requirement**: building Rustab's Rust binary inside the sandbox
  needs Rust/Cargo, which does **not** ship in `org.gnome.Sdk` by default -
  add the `org.freedesktop.Sdk.Extension.rust-stable` SDK extension (see
  §2). This is what both real Tauri+Rust apps checked for this guide
  actually do.

For Tauri's own Linux packaging guidance (worth cross-referencing, not a
substitute for this document), see
[Tauri's Linux distribution docs](https://v2.tauri.app/distribute/linux/).

### 1.2 The actual repo-accurate asset locations

- **Icon**: `src-tauri/icons/icon.png` - confirmed 512x512 pixels (checked
  with Pillow against the real file in this repo), which is exactly the
  size Flathub's own icon convention wants
  (`/app/share/icons/hicolor/512x512/apps/<app-id>.png`). There is no
  `resources/` directory anywhere in this repo; don't invent one.
- **New packaging files**: `flatpak/dev.tomsk.Rustab.yml` (manifest),
  `flatpak/dev.tomsk.Rustab.desktop`, `flatpak/dev.tomsk.Rustab.metainfo.xml`
  - all added by this revision, described in detail in §4.
- **Not included, and never fabricated by this revision**:
  `generated-sources.json`-style vendoring files (here named
  `cargo-sources.json` / `frontend-cargo-sources.json` / `node-sources.json`
  - see §2.2). These are large, generated, and go stale the moment a
  dependency changes - they must be produced fresh, by whoever is actually
  submitting, against the `Cargo.lock`/`package-lock.json` that is actually
  being shipped. Fabricating a stand-in here would be actively misleading.

### 1.3 Rustab's Tauri identifier now matches the Flatpak app ID (resolved, T44)

`src-tauri/tauri.conf.json`'s `"identifier"` field was `com.rustab.app`
while the Flatpak app ID used throughout this guide and in `flatpak/` was
(and still is) `dev.tomsk.Rustab`. This was flagged by a previous revision
of this doc as a deliberate decision the maintainer still needed to make.
The maintainer decided (directly, in conversation) to unify on
`dev.tomsk.Rustab` project-wide - the domain-based ID the Flatpak/Flathub
scaffolding already used - rather than the arbitrary `com.rustab.app`.
T44 updated `tauri.conf.json`'s `identifier` to `dev.tomsk.Rustab`
accordingly; the two no longer diverge. (Verified at the time of that
change that nothing in the Rust backend derives the working-draft/backup
storage path from this identifier - it's a hardcoded literal `"rustab"`
under `$XDG_DATA_HOME`, see `src-tauri/src/target.rs` - so this rename
carries no data-migration risk for existing installs.)

### 1.4 Flathub's Generative AI policy - read this before submitting anything

Verified live against `docs.flathub.org/docs/for-app-authors/requirements`
on 2026-10-16. Flathub's current policy states, in part:

> Submitters must disclose any AI-generated code, documentation, packaging,
> or other material they know or reasonably believe is included in the
> application or its Flathub packaging... **Flathub manifests must not
> contain AI-generated or AI-assisted content. Disclosure does not exempt
> manifests from this restriction.**

This matters directly for this revision: `flatpak/dev.tomsk.Rustab.yml`
(the manifest) was drafted by an AI orchestration session (this one).
**Per Flathub's own stated policy, that manifest - as written by this
session - must not be submitted to Flathub as-is.** The maintainer needs
to personally rewrite/re-author the manifest by hand (using this draft as
a reference for the researched facts in it - app ID, runtime version,
asset paths, etc. - not as a file to submit unmodified) before it is
Flathub-submission-eligible. This is a hard restriction, not a disclosure
checkbox, and it is the single most important thing in this section: don't
submit `flatpak/dev.tomsk.Rustab.yml` verbatim.

The same policy also says AI-generated application code/documentation
*is* permitted with disclosure (which this project already does, in
README.md's AI Use Disclosure section), and separately: "AI tools or
agents must not open or automate Flathub submission pull requests, or
generate their commit messages, descriptions, review comments, or
replies." The actual PR open/description/commit-message step is listed as
a **[MAINTAINER ACTION]** throughout §5 for this reason too, independent
of the earlier scope boundary that already ruled it out for this session.

### 1.5 Known WebKitGTK rendering issue (blank window / choppy scrolling) and its fix

On some GPU/driver combinations (seen directly during this project's own
development - certain Nvidia drivers, Wayland sessions, and VMs are the
common pattern), WebKitGTK's hardware-compositing/DMA-BUF renderer fails
silently instead of falling back, producing a completely blank white
window instead of the UI. The documented fix (see `README.md`'s own
"Troubleshooting: blank/white window on launch" section, which this
mirrors for the Flatpak case) is forcing software rendering via two
environment variables:

```
WEBKIT_DISABLE_COMPOSITING_MODE=1
WEBKIT_DISABLE_DMABUF_RENDERER=1
```

**The tradeoff**: this fixes the blank window, but software-rendered
WebKitGTK scrolls/repaints noticeably slower than GPU-accelerated
rendering - more visible with a longer entries table (a real `/etc/fstab`
has far more DOM content per row than a short practice file) or a large
raw-text view. This is inherent to forcing software rendering on an
affected system, not something fixable in the app's own code.

**This is deliberately not baked into the manifest/`finish-args` as an
always-on default** - most users will never hit the underlying GPU bug,
and forcing software rendering unconditionally would make the app
artificially slower for everyone else for no benefit. Instead, a user who
actually hits the blank-window symptom applies the fix themselves, the
Flatpak-native way (equivalent to setting the env vars before launching a
plain binary, but persists across launches including from a GUI
launcher - no need to open a terminal every time):

```bash
flatpak override --user \
  --env=WEBKIT_DISABLE_COMPOSITING_MODE=1 \
  --env=WEBKIT_DISABLE_DMABUF_RENDERER=1 \
  dev.tomsk.Rustab
```

To undo it: `flatpak override --user --reset dev.tomsk.Rustab` (or
`--unset-env` for just these two keys).

---

## 2. Writing the Flatpak manifest

### 2.1 Manifest structure and app ID

The manifest lives at `flatpak/dev.tomsk.Rustab.yml` in this repo (the name
is a convention: Flathub requires the manifest be named after the
application ID, with a `.yml`/`.yaml`/`.json` extension, at the top level
of whatever gets submitted). The reverse-DNS app ID **`dev.tomsk.Rustab`**
is derived from the project's contact domain (`opensource@tomsk.dev`) and is
distinct from the GitHub repo name (`rustab`) - see §1.3 above for the one
place this diverges from Tauri's own `identifier`.

The manifest this revision added was structure-checked (not just
hand-written from a template) against two real, currently-published
Tauri+Rust Flatpak apps on Flathub as of 2026-10-16:
`pl.fuzjajadrowa.pulsar` and `io.github.lullabyX.sone`, both fetched live
from `raw.githubusercontent.com/flathub/<app-id>/master/<app-id>.yml`.
Both use `runtime: org.gnome.Platform`, `sdk: org.gnome.Sdk`, the
`org.freedesktop.Sdk.Extension.rust-stable` SDK extension with
`append-path: /usr/lib/sdk/rust-stable/bin`, `CARGO_HOME`/
`CARGO_NET_OFFLINE: 'true'` build-options, and install the binary/desktop
file/metainfo/icon the same way `flatpak/dev.tomsk.Rustab.yml` does. Where
Rustab's manifest differs from both of them, it's called out explicitly in
the manifest's own comments (no `--share=network` - Rustab has no runtime
network use; the two independent `Cargo.lock` files; the privilege-
escalation `finish-args`, which neither reference app needed at all).

Also verified live (not assumed): `appstreamcli validate
flatpak/dev.tomsk.Rustab.metainfo.xml` passes with only expected
URL-unreachable warnings (the manifest points at
`https://github.com/t0msk/rustab`, confirmed by a live `curl` to return
`404` - the repo does not exist yet, as expected, since this repo has no
configured remote). `desktop-file-validate flatpak/dev.tomsk.Rustab.desktop`
passes clean. `python3 -c "import yaml; yaml.safe_load(...)"` confirms the
manifest is valid YAML. `xmllint --noout` confirms the metainfo file is
well-formed XML.

### 2.2 Vendoring Cargo dependencies for offline builds

**Automated since T51**: `flatpak/generate-sources.sh` runs both
generator invocations below (plus the node one in §2.3) end-to-end -
cloning `flatpak-builder-tools` into a local cache, setting up its own
venv, and writing all three output files into `flatpak/`. Run it, don't
hand-type the commands below; they're kept here as the documented
reference for what the script actually does, not as a separate manual
path.

Flathub's build environment has **no network access** during the build
(confirmed current, `docs.flathub.org`'s Requirements page, "No network
access during build" - `--share=network` inside `build-args` explicitly
does not work). To supply dependencies offline:

1. **[MAINTAINER ACTION] Run `flatpak-cargo-generator`**, from the
   `cargo/` directory of
   [`flatpak/flatpak-builder-tools`](https://github.com/flatpak/flatpak-builder-tools),
   **once per `Cargo.lock`** - Rustab has two:
   ```bash
   # from the repo root, with network access, after installing the
   # generator's own dependencies per its README (poetry, or tomlkit +
   # aiohttp manually)
   python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
   python3 flatpak-cargo-generator.py frontend/Cargo.lock -o flatpak/frontend-cargo-sources.json
   ```
   This is a **project-specific step the old version of this doc missed**:
   it only ever described one `Cargo.lock`. Forgetting the second one means
   the frontend's Rust dependencies silently have no offline source and
   the build fails inside the sandbox with no network to fall back on.
2. **Reference both output files from the manifest's `sources`** (already
   wired up in `flatpak/dev.tomsk.Rustab.yml`).
3. **Regenerate both before every submission or update** that touches
   either `Cargo.lock` - a stale `cargo-sources.json` is a broken, silently
   out-of-sync build, not a warning.

For the generator's current usage and requirements, see its own
[README](https://github.com/flatpak/flatpak-builder-tools/blob/master/cargo/README.md)
(checked live 2026-10-16) - it now documents Python 3.9+, `poetry`, and
outputs a file it itself calls `cargo-sources.json` (this guide follows
that naming rather than the older `generated-sources.json` convention the
previous version of this doc used, to match current upstream tooling
examples).

### 2.3 The Node/Tailwind dependency - also needs offline vendoring

**Also covered by `flatpak/generate-sources.sh` (T51)** - see §2.2.

`frontend/package.json`'s only dependencies are `tailwindcss` and
`@tailwindcss/cli` (dev-only build dependencies - see its own description
field: "Nothing here ships at runtime"; Tailwind v4 ships the standalone
CLI as its own package, hence two entries rather than v3's one).
`frontend/Trunk.toml` invokes the CLI via `npx` as a `pre_build` hook.
Under Flathub's no-network-during-build rule, this also needs offline
vendoring: an `org.freedesktop.Sdk.Extension.node<NN>` SDK extension (pick
whatever LTS version is current - not pinned precisely in the manifest,
unlike the runtime version, because it matters far less here and goes
stale just as fast) plus a `node-sources.json`-style vendoring file
generated by `flatpak-node-generator` (same `flatpak-builder-tools`
project, `node/` directory) against `frontend/package-lock.json`. This
mirrors exactly what both `pl.fuzjajadrowa.pulsar` and
`io.github.lullabyX.sone` do for their own npm dependencies.

One wrinkle Tailwind v4 adds over v3 here: the v4 CLI is a native binary
distributed through platform-specific optional dependencies
(`@tailwindcss/oxide-*`, `lightningcss-*`, and transitively
`@parcel/watcher-*`). The vendored node sources must therefore include the
packages for the build architecture, not just the pure-JS tree.
**Verified, not just assumed (2026-10-19)**: a real run of
`flatpak/generate-sources.sh` against this project's actual
`frontend/package-lock.json` (66 packages read, 134 sources written)
confirms the current `flatpak-node-generator` *does* include all of these
by default, with no extra flag needed - re-check this the same way if a
future generator version changes that default, rather than trusting this
note forever.

### 2.4 The Trunk offline build problem (unresolved - read before relying on this manifest)

This is the one genuinely open problem this revision surfaces rather than
quietly assuming away, because neither of the two real reference apps use
Trunk (they use `npm run tauri:build`/`pnpm tauri build`, which only needed
*their* npm-vendored dependencies, not a separate Rust-side build tool).

Trunk is not a plain `cargo build` invocation. When it runs, it:

1. Compiles the frontend crate to `wasm32-unknown-unknown` (a Cargo
   target - this part is the same kind of offline-vendorable dependency
   problem §2.2 already covers, just for a different target triple).
2. Invokes **its own** bundled tool dependencies - `wasm-bindgen-cli` and
   `sass` are the usual ones - which Trunk fetches **itself**, separately
   from Cargo's own dependency resolution, the first time it needs them.

That second part is a distinct offline-vendoring problem this manifest
does not yet solve. `flatpak/dev.tomsk.Rustab.yml`'s build-commands call
`trunk build --release` with a comment flagging exactly this; it has not
been proven to work inside a network-less sandbox. Two candidate
approaches to actually resolve this, **neither attempted or verified
here**:

- **Pre-vendor Trunk's tool dependencies as their own Flatpak sources.**
  `wasm-bindgen-cli` is itself a `cargo install`-able crate, so it can
  likely be built from a vendored source the same way as any other Rust
  dependency, then pointed at via Trunk's own environment variables/config
  for "use this binary instead of fetching one" (Trunk supports pinning
  tool versions and, in recent versions, a local tool cache directory -
  check Trunk's current docs for the exact mechanism, not asserted here).
- **Skip Trunk inside the sandbox entirely**, driving `wasm-bindgen` and
  the Tailwind step by hand with explicit `cargo build --target
  wasm32-unknown-unknown` + a manually-invoked `wasm-bindgen` binary (built
  from its own vendored source) to reproduce what Trunk's `pre_build`/
  build pipeline does, assembling `dist/` without Trunk ever running. More
  build-commands, but it removes Trunk's own network-fetching behavior
  from the picture entirely.

**[MAINTAINER ACTION]**: prototype one of these locally (`flatpak-builder`
against `flatpak/dev.tomsk.Rustab.yml`, watching for any network access
Trunk attempts) before trusting this manifest's build-commands section.
Update this guide and the manifest's comments with whichever approach
actually works, and remove the "UNVERIFIED" labels once it's proven - don't
leave them stale once this is actually solved.

### 2.5 Rust features and build flags

The manifest's `cargo --offline build --release -p rustab` matches the
flags `cargo tauri build` itself uses for a release build. If build
features are ever added, mirror them in the manifest's build-commands too.

---

## 3. Privilege escalation under Flatpak: SOLVED (T40)

> **This section's question is no longer open.** It used to end in "no
> single correct answer yet - here are three options, none tested". It has
> since been tested, twice over (two throwaway diagnostic Flatpak apps, then
> Rustab itself, built with `flatpak-builder --user --install` and exercised
> in a real sandbox), and the answer is **§3.2's Option D**:
> `flatpak-spawn --host pkexec <host path of this binary>`, enabled by the
> single permission `--talk-name=org.freedesktop.Flatpak`.
>
> Options A and B below are **kept for the record only**, because both rest
> on a premise that turned out to be false and someone will otherwise
> rediscover them as "the obvious thing to try". Read §3.1 first: it is the
> reason they cannot work.
>
> The implementation lives in `src-tauri/src/host.rs` (new in T40) and is
> documented in `docs/API_CONTRACT.md` §4.3.

### 3.1 Why pkexec doesn't "just work" in a sandbox - and why no `--filesystem=` grant fixes it

Two independent facts, both confirmed by experiment rather than inference:

1. **`pkexec` does not leave the sandbox.** It changes a process's **UID**,
   not its **mount namespace**. A `pkexec` child of a sandboxed process is
   therefore root *inside the same sandbox*, looking at the same filesystem
   view its parent had. The intuition that "the elevated process runs
   unsandboxed, so it has full access to `/etc/`" - which earlier revisions
   of this doc and of `src-tauri/src/backup_location.rs` both relied on - is
   simply wrong.
2. **Flatpak reserves the whole `/etc` path space.** `--filesystem=/etc:rw`
   is rejected by `flatpak-builder-lint` (so it can never pass Flathub
   review), and the narrower-looking `--filesystem=/etc/fstab:rw` is
   rejected by the *sandbox itself* at startup:
   ```
   F: Not sharing "/etc/fstab" with sandbox: Path "/etc" is reserved by Flatpak
   ```
   The sandbox then shows the runtime's own synthetic `/etc` (`ls /etc`
   inside it lists the runtime's files and no `fstab` at all). Nothing
   reaches the real file through that grant: not a read, not a write, and -
   by fact 1 - not an elevated write either.

Together these mean the elevated writer must be made to run as a **genuine
host process**, not merely as root. That is what §3.2's Option D does.

Rustab's architecture (`docs/API_CONTRACT.md` §4) uses a re-exec
mechanism: when saving, restoring, deleting, or creating a backup against
the real `/etc/fstab`, the unprivileged GUI process re-executes its own
binary via `pkexec`, passing a tagged JSON request on stdin
(`{"op": "write" | "delete_backup" | "create_backup", ...}` - the protocol
grew two more operations since the previous version of this doc was
written, see §4.2-4.3 of `API_CONTRACT.md`; nothing about the sandbox
analysis below changes because of that, since all three still go through
the identical single re-exec flag).

In a normal (non-sandboxed) Linux system, `pkexec` can reach the host's
polkit daemon, authenticate, run elevated, and write to the host
filesystem. **Inside a Flatpak sandbox**, none of that is guaranteed: the
sandbox may not have access to the host's D-Bus system bus where polkit
listens, the filesystem is restricted to explicitly granted paths, and the
process cannot arbitrarily spawn host tools.

### 3.2 The approaches, and which one is real

#### Option D: `flatpak-spawn --host pkexec` - THE ANSWER, CONFIRMED

```yaml
finish-args:
  - --talk-name=org.freedesktop.Flatpak
```

That is the whole permission set this needs: no `--filesystem=` of any
kind, no system-bus access. It lets the app call the `org.freedesktop.Flatpak`
session-bus service, i.e. use `flatpak-spawn --host`, which is part of every
`org.gnome.Platform`/`org.freedesktop.Platform` runtime (no new dependency).

What was confirmed, from inside a sandbox granted *only* that:

| Command | Result |
| --- | --- |
| `flatpak-spawn --host cat /etc/fstab` | prints the real host file (byte-identical, checked by `sha256sum`) |
| `flatpak-spawn --host id` | the real host user - proof the process is outside the sandbox's mount namespace, not merely outside its cgroup |
| `flatpak-spawn --host sh -c 'touch /etc/.test'` | `Permission denied` - correct for an unprivileged host process |
| `flatpak-spawn --host pkexec sh -c 'touch /etc/.test && rm -f /etc/.test'` | **succeeds** - real root on the real `/etc/` |
| `echo '<request>' \| flatpak-spawn --host <app-path>/bin/rustab --internal-privileged-write` | the helper's own JSON response on stdout, correct exit code - stdio and exit status pass through unchanged |

Two details the implementation has to get right, both in
`src-tauri/src/host.rs`:

- **The binary must be named by its *host* path.** `std::env::current_exe()`
  inside the sandbox is `/app/bin/rustab`, which does not exist on the host.
  The host path comes from the `app-path` key Flatpak writes into the
  sandbox's own `/.flatpak-info`, with the user/system installation layouts
  (`$HOME/.local/share/flatpak/app/<id>/current/active/files/bin/rustab`,
  `/var/lib/flatpak/app/<id>/…`) as fallbacks.
- **Existence of that path must be checked on the host, not locally.** None
  of those paths is visible from inside the sandbox, so `Path::exists()`
  answers `false` for all of them - including the right one. The check is one
  more `flatpak-spawn --host` call.

- **`findmnt --verify`, `lsblk` and the mount-point existence check must run
  on the host too.** A sandboxed
  `findmnt` cannot see the host's mount points and incorrectly reports most
  entries (including `/`) as unreachable; a sandboxed `lsblk` exits
  successfully but reports no UUIDs or labels for any device; and the
  advisory "mount point does not currently exist on disk" check stats the
  *runtime's* `/`, so `/boot`, `/opt`, `/mnt/…` and every `/run/media/…`
  that the host really has is reported missing - a false warning on
  nearly every real fstab. The fix routes all three through
  `flatpak-spawn --host` via an injectable `fstab_core::exec::CommandRunner`
  (selected in `src-tauri/src/host.rs`). For `findmnt`, the candidate is
  staged in a host-side `mktemp` file and passed to `--tab-file`, because
  the sandbox's `/tmp` is invisible to the host and `--tab-file /dev/stdin`
  fails on a pipe. The existence check is **batched**: the trait method
  takes the whole list of paths and the host runner answers it with one
  `flatpak-spawn --host sh -c 'for p in "$@"; …'` call per validation,
  printing one `0`/`1` per path in order, so the cost does not scale with
  the number of entries (each spawn is a portal round trip). A reply that
  is not exactly one `0`/`1` per path is an error, and an existence check
  that cannot be answered emits no findings rather than inventing them. The
  elevated helper is unaffected - it already runs on the host.
  **Untested in a sandbox so far:** unlike the `findmnt`/`lsblk` routing,
  the batched existence call has only been exercised by unit tests (its
  exact command line and its reply parsing). `host.rs`'s
  `sandbox_smoke_host_runner_runs_on_the_host` now also asserts that the
  host answers `[true, false]` for `["/etc/fstab", "/.flatpak-info"]`, but
  that test is `#[ignore]`d and has to be run by hand from an installed
  Flatpak.

The one real caveat, recorded honestly: the binary that runs on the host is
the Flatpak-built one, dynamically linked against the *runtime's* GTK,
WebKitGTK and glibc. It runs on a host that can satisfy those (verified here
with `ldd` and by running it), but a host without `libwebkit2gtk-4.1.so.0`,
or with an older glibc than the runtime's, would fail at exec and the save
would fail with a `privilege_escalation` error. If that ever shows up in the
field, the mitigations are to invoke it through the runtime's own loader
(`/.flatpak-info` also records `runtime-path`) or to ship a small
statically-linked helper binary.

#### Option A (KEPT FOR THE RECORD - does not work): Host filesystem access + implicit policy

```yaml
finish-args:
  - --filesystem=host
  - --system-bus-talk=org.freedesktop.PolicyKit1
  - --system-bus-talk=org.freedesktop.DBus.Properties
```

**Why it is dead**: `--filesystem=host` does not expose the host's `/etc`
either (§3.1, fact 2 - the reservation applies regardless of the grant), so
the elevated write path would *still* not reach `/etc/fstab`, while the app
would have given up most of its sandbox for nothing. Also: Flathub review
would rightly question `--filesystem=host` in an fstab editor.

#### Option B (KEPT FOR THE RECORD - does not work): Restricted filesystem access + explicit policy

```yaml
finish-args:
  - --filesystem=/etc/fstab:rw
  - --filesystem=xdg-data/rustab:create
  - --system-bus-talk=org.freedesktop.PolicyKit1
```

**Why it is dead**: the single-file `/etc/fstab:rw` grant is refused by the
sandbox outright (§3.1, fact 2), so it buys nothing at all, and the
`pkexec`-reaches-polkit question it was meant to answer is moot under Option
D - `pkexec` is invoked by a process that is already on the host, so it uses
the host's own system bus and needs no proxied access. (Note also that
`--system-bus-talk` is not a real `flatpak build-finish` option; the correct
spelling is `--system-talk-name`. The repo's draft manifest carried the
misspelling for a while, which would have failed the build.)

`--filesystem=xdg-data/rustab:create` is not needed either: under Flatpak
`XDG_DATA_HOME` already points inside the app's own
`~/.var/app/dev.tomsk.Rustab/data`, which the app owns by default. That is
where the working draft and the fallback backup directory live.

#### Option C: Portal-based approach (future)

Use Flatpak's
[file portal system](https://flatpak.org/documentation/portal-docs/index.html)
for a more principled, tightly-controlled mechanism instead of a bare
`pkexec` call. Newer and less established for `pkexec`-style workflows.

### 3.3 The testing requirement: what has been done, and what is left

**Non-negotiable**: before submitting to Flathub, any Flatpak manifest
must be built and tested on an actual Flatpak-enabled system. Since T40,
steps 1 and 2 below **have been done** (local `flatpak-builder --user
--install`, app launches, draft loads from the real `/etc/fstab`), and step
3's first question is answered (the whole spawn chain reaches the elevated
helper and gets its JSON response back; `pkexec` is reachable from the
sandbox via the host). What remains **[MAINTAINER ACTION]** is everything
that needs a human at an authentication dialog, i.e. step 3's "does the
write complete" onwards:

1. Build locally:
   ```bash
   flatpak install -y flathub org.flatpak.Builder
   flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
   flatpak run --command=flathub-build org.flatpak.Builder --install flatpak/dev.tomsk.Rustab.yml
   ```
   (`flathub-build`/`org.flatpak.Builder` is Flathub's current recommended
   wrapper around `flatpak-builder` - verified live against
   `docs.flathub.org`'s Submission page 2026-10-16; the previous version
   of this doc referenced a bare `flatpak-builder` invocation, which still
   works but is no longer what Flathub itself recommends leading with.)
2. **Test the sandbox target first**: confirm the app opens, loads a
   sandbox fstab file, and edits it without elevation. Should work
   regardless of which privilege-escalation option is chosen.
3. **Test real-target writes** with `RUSTAB_TARGET_PATH=/etc/fstab`:
   - Does `pkexec` appear at all?
   - If yes, does authentication succeed and the write complete?
   - Does `findmnt --verify` confirm the written file is valid?
   - Does restore/delete/create-backup all still work against the real
     target too (the three operations §4.2 of `API_CONTRACT.md`
     documents, not just `save`)?
4. **Check system state afterward**: `findmnt --verify /etc/fstab`
   manually; if you have a test VM, reboot it to be sure.

**If any of steps 3-4 fail, do not proceed to Flathub.** A sandboxed build
that breaks the privilege escalation is a serious safety regression, worse
than no Flatpak at all.

---

## 4. The packaging files in this repo

Added by this revision, in a new `flatpak/` directory (deliberately not
scattered elsewhere):

- **`flatpak/dev.tomsk.Rustab.yml`** - the manifest (§2 above). Per §1.4,
  must be hand-rewritten/re-authored by the maintainer, not submitted
  as-is, before it is Flathub-eligible.
- **`flatpak/dev.tomsk.Rustab.desktop`** - the desktop entry. `Name`,
  `Comment` (drawn from README.md's own one-line description), `Exec=rustab`,
  `Icon=dev.tomsk.Rustab` (matches the app ID, not a filename - Flatpak
  installs the icon under that name), `Categories=System;` (a single main
  category - `desktop-file-validate` flags more than one main category as
  a hint, confirmed by actually running it against an earlier draft of
  this file that had `System;Utility;`). Validated clean with
  `desktop-file-validate` (installed and run live in this session).
- **`flatpak/dev.tomsk.Rustab.metainfo.xml`** - AppStream metadata: id,
  name, summary, developer, description (drawn from README's Features
  list), dual MIT/Apache-2.0 `project_license` (matching
  `docs/SPEC.md` §10 and the About dialog), `launchable` pointing at the
  desktop file, `content_rating type="oars-1.1"`, `url` tags (homepage,
  bugtracker, vcs-browser, donation - the last pointing at the existing
  GitHub Sponsors link README.md already uses). Validated well-formed with
  `xmllint` and passed `appstreamcli validate` live in this session (only
  warnings: the GitHub URLs aren't reachable yet, because the repo doesn't
  exist there yet - confirmed by a live `curl` returning `404` for
  `https://github.com/t0msk/rustab`). Running with `--pedantic` also surfaces
  one pedantic-level (not warning/error) note,
  `cid-contains-uppercase-letter`: AppStream's own linter prefers an
  all-lowercase component ID, while `dev.tomsk.Rustab`'s capital `R`
  matches the app ID this project already established (and Flathub's own
  app-ID character rules explicitly permit uppercase letters per
  `docs.flathub.org`'s Requirements page). Kept as-is per that existing
  convention rather than silently renamed; worth knowing about if a future
  reviewer's pedantic lint run flags it.

  **Two fields in this file are explicitly marked as submission blockers
  in its own XML comments, not silently left incomplete:**
  - **Screenshots** - Flathub requires at least one real screenshot for
    every graphical app, hosted at a stable tagged URL. Real screenshots
    now exist, committed at `docs/screenshots/` (also shown in README.md's
    "Screenshots" section) and wired into a real `<screenshots>` block in
    the metainfo file - but every `<image>` URL still points at the
    `vX.Y.Z` placeholder tag, because no public GitHub repo or tagged
    release exists yet to point a stable `raw.githubusercontent.com` URL
    at (same blocker the manifest's own `sources[0].tag` already has).
    `appstreamcli validate` confirms the block itself is well-formed -
    its only complaints are 5 `screenshot-image-not-found` warnings for
    the still-placeholder URLs, which is expected and resolves itself the
    moment `vX.Y.Z` is swapped for a real tag post-release.
  - **Releases** - Flathub requires a `<releases>` tag to pass validation.
    This project has not cut its first tagged release (every crate is at
    `0.0.0` in lockstep, per `docs/SPEC.md` §12). A single placeholder
    `type="development"` entry was added (the same release type AppStream
    itself uses for its own in-progress builds, confirmed via a live query
    against Flathub's API for `org.gnome.TextEditor`) so the file is
    well-formed today; it needs a real dated entry once `release-publish.yml`
    (`docs/SPEC.md` §13) actually cuts a tag.
  - A third, lower-stakes placeholder: the `<branding>` colors are a
    neutral light/dark pair, not a real brand palette - Tailwind's config
    (`frontend/styles/input.css` since the v4 migration, previously
    `frontend/tailwind.config.js`) has no custom theme to draw one from
    (confirmed by reading it). Flagged in the file's own comment; drop or
    replace if/when Rustab gets an actual brand color.

### Keeping these files in sync (see also "Best practices" below)

- The icon install path in the manifest (`src-tauri/icons/icon.png`) will
  break silently if that file is ever renamed or moved - there is no
  automated check tying `flatpak/dev.tomsk.Rustab.yml` to
  `src-tauri/icons/`.
- The `<id>` in the metainfo file, the filename stem, the `Icon=` value in
  the desktop file, and the manifest's `app-id` must all stay character-for-
  character identical (`dev.tomsk.Rustab`) - Flathub's own requirements
  page is explicit that a mismatch here is a validation failure, not a
  style nit.

---

## 5. Submitting to Flathub (current process, verified 2026-10-16)

Everything in this section is **[MAINTAINER ACTION]** - none of it was or
could be done from this session (no GitHub repo for this project exists
yet to fork from in the first place; `gh` is not installed here; no
remotes are configured; this task was explicitly scoped to not attempt any
of it).

**Before starting**: push this repo to a public `https://github.com/t0msk/rustab`
(or wherever it actually ends up living - update the manifest's `url` if
not), cut a real tagged release (`docs/SPEC.md` §13's `release-publish.yml`
workflow), re-point the manifest's `sources[0].tag` *and* every
`<screenshot>` `<image>` URL in the metainfo file at that real tag instead
of the `vX.Y.Z` placeholder (the screenshots themselves already exist, at
`docs/screenshots/` - only the URL needs updating), add a real
`<releases>` entry to the metainfo file, regenerate both
`cargo-sources.json` files and `node-sources.json` fresh (§2.2-2.3), and
resolve §2.4's Trunk offline-build problem for real - this is a long list
on purpose; §1.4's Generative AI policy also means hand-rewriting the
manifest itself before any of this, not just filling in placeholders.

1. **Build and test locally first** (§2 and §3.3) - Flathub explicitly
   recommends this to catch problems before review, and some problems
   (the pkexec question) are safety-critical, not just convenience.
2. **Run the linter**:
   ```bash
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest flatpak/dev.tomsk.Rustab.yml
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
   ```
   This wraps `appstreamcli validate` plus Flathub-specific checks (e.g.
   the mandatory-screenshot rule §4 already flags) that plain
   `appstreamcli validate` alone - which this session *did* run, and which
   passed - does not enforce. Passing plain `appstreamcli validate` is
   necessary but not sufficient; don't treat it as the whole check.
3. **Fork `flathub/flathub` on GitHub**, unchecking "Copy the master branch
   only."
4. **Clone the fork and check out the submission base branch** - this is
   the part that changed from the previous version of this doc:
   ```bash
   git clone --branch=new-pr git@github.com:<your-username>/flathub.git
   cd flathub
   git checkout -b add-dev-tomsk-rustab new-pr
   ```
5. **Add the manifest (your hand-rewritten one, per §1.4) and any
   generated-sources files**, following Flathub's manifest style guide
   (2-space YAML indent, LF endings, UTF-8, trailing newline, logical not
   alphabetical key ordering - all of which `flatpak/dev.tomsk.Rustab.yml`
   already follows, verified by re-reading it against the style guide
   page). Commit and push to your fork.
6. **Open a pull request against the `new-pr` base branch** of
   `flathub/flathub` (not `master` - opening against `master` is
   explicitly called out as wrong by Flathub's own docs). Title: "Add
   dev.tomsk.Rustab". Per §1.4, the PR description itself must not be
   AI-generated or AI-assisted either.
7. **Respond to review**. Comments, requested changes, and a `bot, build`
   comment to trigger a test build all happen on the same PR - there is no
   need to close and reopen it, even for an app-ID change.
8. **On approval**: Flathub merges it and creates
   `github.com/flathub/dev.tomsk.Rustab` for you; you get a write-access
   invitation (accept within a week, with 2FA enabled). From then on,
   that new repo - not this one - is where manifest updates for Flathub
   releases happen, pointed at this repo's tagged releases via its `url`/
   `tag` source fields, per Flathub's normal maintenance model.

### 5.1 How the two repos relate (no submodule needed - read this before assuming you have to duplicate anything)

A natural question once `github.com/flathub/dev.tomsk.Rustab` exists: do
you now have to copy everything from this repo's `flatpak/` directory
into that new repo, and keep the two in sync by hand? **No** - re-reading
the manifest's own `sources:` block (§2.1) answers this directly:

```yaml
sources:
  - type: git
    url: https://github.com/t0msk/rustab.git
    tag: vX.Y.Z
```

combined with the build commands further up, which `install` straight out
of paths like `flatpak/dev.tomsk.Rustab.desktop`, `src-tauri/icons/
icon.png`, and `LICENSE-MIT`/`LICENSE-APACHE` - **all relative to that
freshly-cloned source, not to any file living in the Flathub-org repo**.

What this means in practice:

- **`github.com/flathub/dev.tomsk.Rustab` only ever needs to contain the
  manifest itself** (`dev.tomsk.Rustab.yml`, renamed to the Flathub-
  expected filename) plus the three generated vendoring files
  (`cargo-sources.json`, `frontend-cargo-sources.json`, `node-sources.json`
  - §2.2-2.3). Nothing else.
- **The desktop entry, the AppStream metainfo, the icon, and the license
  files never leave this repo.** Every Flathub build re-clones this
  repo's tag fresh and installs those files straight from it. There is
  nothing to copy, and nothing to keep in sync by hand for those files -
  a change to `flatpak/dev.tomsk.Rustab.desktop` here is automatically
  what the *next* Flathub build picks up, with zero duplication.
- **No git submodule, subtree, or CI-sync action is needed** to achieve
  this - the `sources: type: git` directive already *is* the link between
  the two repos. A submodule would be solving a problem that doesn't
  exist here; Flathub's own build infrastructure doesn't know or care
  about this repo's structure beyond what that one `sources:` entry
  names.
- **The one thing that is *not* automatic: the `tag:` field is pinned.**
  Pushing a change to this repo - even to `flatpak/dev.tomsk.Rustab.desktop`
  itself - does not retrigger a Flathub build by itself. Flathub only
  rebuilds when something changes in *its own* repo
  (`github.com/flathub/dev.tomsk.Rustab`), which for a routine update
  means: cut a new tagged release here as usual
  (`release-publish.yml`), then edit exactly one line - the `tag:` value -
  in that repo's copy of the manifest, commit/PR it there (Flathub's own
  review process applies to that repo too, though routine version bumps
  are typically fast), and *that* triggers the rebuild. Regenerating the
  three vendoring files (§2.2-2.3) is also needed whenever a release
  changes any dependency, not just a version bump with no `Cargo.lock`/
  `package-lock.json` changes.

---

## 6. Best practices

Kept distinct from the walkthrough above on purpose - these are ongoing
habits, not one-time steps.

- **Vendoring hygiene**: regenerate *both* `cargo-sources.json` files
  (§2.2) and `node-sources.json` (§2.3) every time either `Cargo.lock`
  file or `frontend/package-lock.json` changes, not just before a Flathub
  submission. A stale vendoring file is a build that silently can't fetch
  something it needs, with no network to fall back on inside the sandbox -
  treat "lockfile changed" and "regenerate vendoring files" as one atomic
  habit, the same way this project already treats version-lockstep
  (`docs/SPEC.md` §12) as one atomic habit across crates.
- **Keep the runtime version current**: re-check `runtime-version`/`sdk`
  in the manifest against Flathub's actually-current GNOME runtime (e.g.
  query `https://flathub.org/api/v2/appstream/org.gnome.TextEditor` and
  read its `bundle.runtime` field, the same live check this revision used)
  before every Flathub update, not just at initial submission. Don't let
  it drift more than one GNOME release behind, per Flathub's own
  maintenance guidance.
- **Validate before every submission or update**, not just the first one:
  `desktop-file-validate flatpak/dev.tomsk.Rustab.desktop`,
  `appstreamcli validate flatpak/dev.tomsk.Rustab.metainfo.xml` (or the
  Flathub-specific `flatpak-builder-lint` wrapper, §5 step 2, which is the
  stricter and more authoritative check), and `python3 -c "import yaml;
  yaml.safe_load(open('flatpak/dev.tomsk.Rustab.yml'))"` for the manifest
  itself. All three are cheap, fast, and were run live in this session -
  there's no excuse to skip them before any future change to these files.
- **Re-test after every Rustab release**: a new tagged release means the
  manifest's `sources[0].tag`, both cargo-sources files, and
  `node-sources.json` all need regenerating together, and §3.3's full
  sandbox privilege-escalation test needs re-running - a Flatpak that
  silently stops being able to write the real `/etc/fstab` after a Rustab
  update is exactly the kind of regression this project's whole safety
  posture exists to prevent.
- **Keep this document in sync with `flatpak/`, not just with itself**: if
  the Trunk offline-build problem (§2.4) gets solved, update both this
  file's §2.4 and the manifest's own build-command comments in the same
  change - don't let one say "solved" while the other still says
  "unverified." The same applies to the privilege-escalation `finish-args`
  in §3.2 once real sandbox testing (§3.3) produces an actual answer.
- **Re-read `docs/API_CONTRACT.md` §4 and `docs/SPEC.md` §6 before
  touching anything privilege-escalation-related here** - this remains
  this project's single highest-scrutiny subsystem, Flatpak packaging or
  not.

---

## Checklist for a future submitter

- [ ] Read `docs/API_CONTRACT.md` §4 (privilege escalation design) and
      `docs/SPEC.md` §6 (atomicity guarantees that must not be compromised)
- [ ] Push this repo to a public GitHub remote (none configured today)
- [ ] Cut a real tagged release via `release-publish.yml` (`docs/SPEC.md` §13)
- [ ] **Hand-rewrite `flatpak/dev.tomsk.Rustab.yml`** - per §1.4, the
      AI-drafted version in this repo must not be submitted as-is
- [ ] Decide on `src-tauri/tauri.conf.json`'s identifier vs. the Flatpak
      app ID mismatch (§1.3) - reconcile or consciously leave as-is
- [x] Take real screenshots and add a real `<screenshots>` block to
      `flatpak/dev.tomsk.Rustab.metainfo.xml` - done, committed at
      `docs/screenshots/`; the remaining step is swapping each `<image>`
      URL's `vX.Y.Z` placeholder for the real tag once one is cut (same
      substitution the manifest's `sources[0].tag` already needs)
- [ ] Add a real `<release>` entry for the actual cut version
- [ ] Run `flatpak-cargo-generator` against **both** `Cargo.lock` files
- [ ] Run `flatpak-node-generator` against `frontend/package-lock.json`
- [ ] Resolve §2.4's Trunk offline-build problem for real, and update this
      doc and the manifest's comments once it's solved
- [ ] Build locally: `flatpak run --command=flathub-build org.flatpak.Builder --install flatpak/dev.tomsk.Rustab.yml`
- [ ] Run `flatpak-builder-lint` against the manifest and the built repo (§5)
- [ ] Test the sandbox target end to end
- [ ] Test real-target writes/restores/backup-delete/backup-create with
      `RUSTAB_TARGET_PATH=/etc/fstab`, confirm `pkexec` reaches polkit, and
      confirm `findmnt --verify` afterward
- [ ] Reboot a test VM to verify the system still boots cleanly
- [ ] Fork `flathub/flathub`, branch off `new-pr` (not `master`), open the
      PR against `new-pr` (§5) - by hand, not via an AI agent (§1.4)
- [ ] Respond to Flathub maintainer feedback
- [ ] After merge: accept the write-access invitation to the new
      `github.com/flathub/dev.tomsk.Rustab` repo within a week, with 2FA on
- [ ] Set up a process to regenerate all vendoring files and re-run §3.3's
      full test on every subsequent Rustab release

---

**Questions?** Refer to the live, authoritative documentation at
[docs.flathub.org](https://docs.flathub.org/) linked throughout this
guide, and reach out to the maintainer at
[opensource@tomsk.dev](mailto:opensource@tomsk.dev).
