# Flatpak Packaging Guide for Rustab

**Status: planning document.** This is guidance for a future contributor or the maintainer to follow when preparing Rustab for Flathub submission. No Flatpak packaging work has been completed yet in v1.

---

## Overview

Rustab is a Tauri 2 application built with Rust (backend) and Yew (frontend). Packaging it as a Flatpak for distribution via Flathub is straightforward in some respects but has one critical safety consideration: Rustab's privilege-escalation mechanism (`pkexec`-based writes to `/etc/fstab`) must interact safely with Flatpak's sandbox. This guide walks through the process and flags the testing requirements upfront.

## 1. Why Tauri apps on Linux need extra Flatpak considerations

### WebKitGTK and runtime dependencies

Tauri 2 on Linux uses WebKitGTK (via `libwebkit2gtk-4.1`) to render the frontend, rather than bundling Chromium. This has important implications for Flatpak:

- **No bundled browser engine**: unlike Electron apps, you don't ship a copy of Chromium inside the bundle. Instead, Flatpak's base runtime (e.g., `org.freedesktop.Platform`) provides system libraries.
- **WebKitGTK availability**: WebKitGTK must come from the Flatpak runtime you choose. Flathub's standard approach is to use the GNOME runtime (e.g., `org.gnome.Platform` version 46) or the freedesktop runtime (`org.freedesktop.Platform` version 23), both of which include WebKitGTK.
- **SDK requirement**: building Rustab's Rust binary inside the Flatpak sandbox requires the matching SDK (e.g., `org.gnome.Sdk` or `org.freedesktop.Sdk`), which includes Rust, Cargo, and build tools.

For current details on Tauri's own recommendations for Linux packaging, including Flatpak specifics, see [Tauri's official Linux packaging documentation](https://v2.tauri.app/distribute/linux/).

## 2. Writing the Flatpak manifest

### Manifest structure and app ID

Create a file named `dev.tomsk.Rustab.yml` (or similar; the exact name is a convention, not a hard requirement for Flathub submission). The reverse-DNS app ID **`dev.tomsk.Rustab`** is derived from the project contact domain (`rustab@tomsk.dev`) and must be distinct from the GitHub repository name ("rustab").

A minimal manifest skeleton:

```yaml
app-id: dev.tomsk.Rustab
runtime: org.gnome.Platform
runtime-version: '46'
sdk: org.gnome.Sdk
build-options:
  append-path: /usr/lib/sdk/rust/bin

modules:
  - name: rustab
    buildsystem: cargo
    build-options:
      append-path: /usr/lib/sdk/cargo/bin
    sources:
      # Cargo dependencies vendoring (see §2.2 below)
      - type: file
        path: generated-sources.json
        dest-filename: generated-sources.json
      # The application source
      - type: git
        url: https://github.com/t0msk/rustab.git
        branch: main
    build-commands:
      - cargo --offline build --release
    install-commands:
      - install -Dm755 target/release/rustab /app/bin/rustab
      - install -Dm644 resources/dev.tomsk.Rustab.desktop /app/share/applications/dev.tomsk.Rustab.desktop
      - install -Dm644 resources/dev.tomsk.Rustab.appdata.xml /app/share/appdata/dev.tomsk.Rustab.appdata.xml
      # Icon (if present in the repo)
      - install -Dm644 resources/icon.png /app/share/icons/hicolor/512x512/apps/dev.tomsk.Rustab.png

finish-args:
  # Filesystem access
  - --filesystem=host
  - --share=network
  - --share=ipc
  - --socket=wayland
  - --socket=x11
```

### Vendoring Cargo dependencies for offline builds

Flathub's build environment has **no network access**. To provide dependencies offline:

1. **Run `flatpak-cargo-generator`** on your development machine (with network access):
   ```bash
   python3 flatpak-cargo-generator.py Cargo.lock --output generated-sources.json
   ```
   This tool (part of the `flatpak-builder` tools ecosystem) reads your `Cargo.lock` and generates a JSON file listing every dependency with its exact version and source hash.

2. **Include `generated-sources.json` in the manifest** (as shown above under `sources`). The Flatpak build system uses this to pre-fetch and cache all dependencies before the build runs.

3. **Re-generate before every submission**: if you update dependencies (e.g., a Tauri bugfix), re-run the generator and commit the updated `generated-sources.json` to the submission branch.

For current tooling and procedures, see the [Flatpak Cargo tools documentation](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).

### Rust features and build flags

Ensure the Rust build uses the same flags as the production release build:

```bash
cargo --offline build --release
```

This matches the normal `cargo tauri build` invocation. If you have feature flags (e.g., `--features some-feature`), include them in the manifest's `build-commands`.

## 3. Privilege escalation under Flatpak: the pkexec challenge

This is **the most critical and uncertain part** of Rustab's Flatpak packaging. Read this section carefully and understand the testing requirements before attempting a submission.

### Why pkexec doesn't "just work" in a sandbox

Rustab's architecture (see `docs/API_CONTRACT.md` §4) uses a re-exec mechanism: when saving to the real `/etc/fstab`, the unprivileged GUI process re-executes its own binary via `pkexec`, passing the candidate fstab content as JSON on stdin. This re-exec calls `write_document` with full atomicity guarantees.

In a normal (non-sandboxed) Linux system, `pkexec` can:
- Access the host's polkit daemon to prompt for authentication
- Execute the elevated process with the caller's original environment
- Write to the host filesystem

**Inside a Flatpak sandbox**, the application runs in an isolated namespace with limited access:
- The sandbox may not have access to the host's D-Bus session or system bus where polkit listens
- The filesystem is partially restricted (only mounted paths are visible)
- The process cannot arbitrarily spawn host tools or access host resources

### Possible approaches

There is **no single "correct" answer yet**; the following are known approaches, each with trade-offs:

#### Option A: Host filesystem access + implicit policy

Grant the Flatpak broad filesystem and D-Bus access so `pkexec` can reach the host's polkit daemon and write to `/etc/fstab`:

```yaml
finish-args:
  - --filesystem=host               # Full host filesystem
  - --system-bus-talk=org.freedesktop.PolicyKit1
  - --system-bus-talk=org.freedesktop.DBus.Properties
```

**Advantage**: the elevated write path works unchanged.
**Disadvantage**: defeats much of Flatpak's sandboxing. The app has full host access, which is not ideal for an application that exists to protect a critical system file.

#### Option B: Restricted filesystem access + explicit policy

Allow access only to `/etc/fstab` and the backup directory, and assume the host's polkit daemon is reachable:

```yaml
finish-args:
  - --filesystem=/etc/fstab:rw
  - --filesystem=~/.local/share/rustab:rw
  - --system-bus-talk=org.freedesktop.PolicyKit1
```

**Advantage**: narrower sandbox breach. The app can only touch `/etc/fstab` and its own data directory.
**Disadvantage**: still grants system bus access to polkit, and assumes polkit is configured and reachable from the sandbox.

#### Option C: Portal-based approach (future)

Use Flatpak's [file portal system](https://flatpak.org/documentation/portal-docs/index.html) to request host system write access through a documented, more tightly controlled mechanism. This is newer and less common in current Flathub apps.

**Advantage**: more principled, tighter security.
**Disadvantage**: requires Flatpak/portal updates and is not well-established for `pkexec` workflows yet.

### The testing requirement: do not assume any of these will work

**This is non-negotiable**: before submitting to Flathub, any Flatpak manifest must be built and tested on an actual Flatpak-enabled system:

1. Build the Flatpak locally using `flatpak-builder`:
   ```bash
   flatpak-builder --user --install-deps-from=flathub build-dir dev.tomsk.Rustab.yml
   flatpak run --devel dev.tomsk.Rustab
   ```

2. **Test the sandbox target first** (the default): confirm that the app opens, loads a fstab file, and can edit the sandbox target without requiring elevation. This should work regardless of option chosen.

3. **Test the real-target writes** by running with `RUSTAB_TARGET_PATH=/etc/fstab`:
   - Does `pkexec` appear at all?
   - If yes, can you successfully authenticate and complete a write?
   - Does `findmnt --verify` confirm the written file is valid?
   - Can you restore a backup afterward?

4. **Check the system state afterward**:
   - Is the real `/etc/fstab` intact and bootable?
   - Run `findmnt --verify /etc/fstab` manually to confirm integrity.
   - If you have a test VM, reboot it to be absolutely sure.

**If any of steps 3-4 fail**, do not proceed to Flathub. Rustab's whole purpose is *never* corrupting `/etc/fstab`. A sandboxed build that breaks the privilege escalation is worse than no Flatpak at all; it is a serious safety regression.

## 4. Resources and existing examples

For current, authoritative information on Flatpak manifest syntax and Flathub's exact requirements:

- **[Flatpak Application Manifest Reference](https://docs.flatpak.org/en/latest/manifests.html)** — detailed manifest format
- **[Flathub Guidelines](https://github.com/flathub/flathub/blob/master/CONTRIBUTING.md)** — submission requirements, what Flathub reviewers look for
- **[Flatpak Cargo Tools](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)** — current vendoring approach
- **Existing Tauri apps on Flathub** — search Flathub's repository for `buildsystem: cargo` examples to see how other Rust/Tauri apps are packaged

## 5. Submitting to Flathub

Once the local build and safety testing (§3) are complete and successful:

1. **Fork** [flathub/flathub](https://github.com/flathub/flathub) on GitHub.

2. **Add your manifest** to the fork in the appropriate directory structure. Flathub expects:
   ```
   <repo-fork>/
   ├── dev/tomsk/Rustab/
   │   └── dev.tomsk.Rustab.yml
   ```

3. **Create a pull request** from your fork to the upstream flathub/flathub `master` branch. Include:
   - A clear title: "Add Rustab (dev.tomsk.Rustab)"
   - A description explaining:
     - What Rustab does
     - Why the chosen filesystem/D-Bus access model is necessary and tested
     - Any security considerations and how they were addressed
     - A link to the upstream GitHub repository

4. **Flathub's CI/review process**:
   - A bot (currently `flathubbot` or similar) will automatically test the build
   - Flathub maintainers will review the manifest and the security implications
   - They may request changes (e.g., narrower filesystem access, better justification for privileges)
   - Once approved and merged, the app becomes available on Flathub

5. **After merge: ongoing maintenance**:
   - When Rustab updates, re-generate `generated-sources.json` if dependencies change
   - If a Tauri, Rust, or system library update requires manifest changes, prepare and open a new PR
   - Keep the manifest's runtime version reasonably current (e.g., don't let the GNOME Platform version fall more than one major release behind)

## 6. Addressing the elevated write in the manifest

The current Flatpak manifest template above assumes the sandboxed process can call `pkexec` directly. If testing (§3) reveals that `pkexec` cannot reach the host's polkit daemon, you have two options:

1. **Update `finish-args`** to grant the necessary D-Bus or filesystem access (see §3 options A or B).
2. **Verify that the real `/etc/fstab` write still succeeds** after any such change by re-testing steps 3-4 of §3.

If neither works, and a portal-based approach is not yet viable, it may be necessary to defer Flathub submission until Flatpak's privilege-escalation story is clearer. A Flatpak that silently fails on real-target saves is a worse outcome than no Flatpak at all.

## Checklist for a future submitter

- [ ] Read `docs/API_CONTRACT.md` §4 (privilege escalation design)
- [ ] Read `docs/SPEC.md` §6 (atomicity guarantees that must not be compromised)
- [ ] Write or adapt the manifest (start from the template in §2)
- [ ] Run `flatpak-cargo-generator` on your machine and commit `generated-sources.json`
- [ ] Build locally: `flatpak-builder --user --install-deps-from=flathub build-dir dev.tomsk.Rustab.yml`
- [ ] Test sandbox target: create/edit a fstab entry, verify it saves locally
- [ ] Test real-target writes: `RUSTAB_TARGET_PATH=/etc/fstab flatpak run dev.tomsk.Rustab`
  - Try to edit a benign entry (e.g., add a comment)
  - Verify `pkexec` prompts and authentication succeeds
  - Verify the write completes and `findmnt --verify` confirms it
- [ ] Reboot a test VM to verify the system still boots cleanly
- [ ] Fork flathub/flathub and open a PR with the manifest and detailed justification
- [ ] Respond to Flathub maintainer feedback (may request security adjustments)
- [ ] After merge, set up a process to regenerate `generated-sources.json` on each Rustab release

---

**Questions?** Refer to the authoritative documentation linked in §4, and reach out to the maintainer at [rustab@tomsk.dev](mailto:rustab@tomsk.dev).
