# Rustab

![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white) [![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-db61a2?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/t0msk) [![X](https://img.shields.io/badge/X-Follow-000000?logo=x&logoColor=white)](https://x.com/TomskDEV)

A modern, native Linux desktop GUI for viewing, editing, adding, and removing entries in `/etc/fstab`, built with stability and data safety as the top priority.

Rustab combines a clean, intuitive interface with a powerful safety-first architecture:
- **Full fstab syntax support**: UUIDs, LABELs, device paths, network sources, bind mounts, swap entries, systemd-generator options (`x-systemd.*`), comments - everything fstab and systemd support, round-tripping perfectly.
- **Atomic writes & automatic backups**: Every save uses write-ahead semantics with mandatory `findmnt --verify` validation, atomic filesystem rename, and automatic timestamped backups.
- **Clear target indicator & mandatory diff review**: A prominent real-vs-sandbox banner and a required diff-before-save step ensure you always know what you're changing and can review every modification before it persists.
- **Dev sandbox by default**: Rustab defaults to a safe per-user sandbox file; pointing at the real `/etc/fstab` is an explicit, auditable choice.

![Discord Badge](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white) · [GitHub Sponsors](https://github.com/sponsors/t0msk) · [X / Twitter](https://x.com/TomskDEV)

## Contents

- [Features](#features)
- [Screenshot Placeholder](#screenshot-placeholder)
- [Safety Notes](#safety-notes)
- [Prerequisites](#prerequisites)
- [Building & Running](#building--running)
- [Testing](#testing)
- [License](#license)
- [AI Use Disclosure](#ai-use-disclosure)
- [Contributing](#contributing)
- [Contact & Support](#contact--support)

## Features

- **View and edit fstab entries** with a clean, modern table interface
- **Add, modify, or delete entries** with a structured form and inline validation feedback
- **Support for all fstab device identifiers**: raw device paths, UUID, LABEL, PARTUUID, PARTLABEL, and network sources
- **Advanced option handling**: toggles for common options, checklist/tag input for custom options, first-class support for `x-systemd.*` options
- **Device and mount-point pickers** for easier entry creation (via `lsblk`/`findmnt`; soft failure if tools unavailable)
- **Mandatory diff review before saving** - see exactly what will change in a side-by-side Summary or unified Raw format
- **Automatic backup management** - every save creates a timestamped backup; restore any previous state with one click
- **Comment and blank-line preservation** - your existing comments and formatting survive edits and round-trip without loss
- **Real vs. sandbox visibility** - a clear, unmissable banner indicates whether you're viewing the real system file or a sandbox copy
- **Privilege escalation gating** - when editing `/etc/fstab`, an explicit confirmation checkbox and danger banner appear; sandbox edits never require elevation

## Screenshot Placeholder

<!-- TODO: screenshot/GIF demonstrating the main table view, add entry form, diff review, and backup panel -->

## Safety Notes

**Sandbox-by-default behavior:**

By design, Rustab **never touches the real `/etc/fstab` by default**, even in release builds. Instead, it reads and writes a per-user sandbox file at:

```
$XDG_DATA_HOME/rustab/fstab.sandbox
```

(falling back to `$HOME/.local/share/rustab/fstab.sandbox` if `$XDG_DATA_HOME` is not set).

To point Rustab at the real system file, set the environment variable `RUSTAB_TARGET_PATH=/etc/fstab` before starting the application. This is the **only** way to enable real-file editing. When enabled:

- A red danger banner is always visible
- An extra confirmation checkbox appears before any save
- The save is routed through a single privilege-escalated (`pkexec`) helper call

See `.env.example` in the repository root for environment variable details and an example of how to configure it for development testing.

## Prerequisites

You need:

- **Rust toolchain** (1.77.2 or later)
  - WASM target: `rustup target add wasm32-unknown-unknown`
- **Trunk** - build tool for Yew/WASM: `cargo install trunk` (verified: 0.21.14+)
- **Tauri CLI** - available as `cargo tauri` or via `cargo install tauri-cli` (verified: 2.2.7+)
- **Node.js** (dev-time only) - used by Trunk's pre-build hook to run Tailwind CSS via `npx`; nothing from `node_modules` ships in the final app or is needed at runtime
- **Linux system dependencies** for Tauri - see [Tauri's official Linux prerequisites documentation](https://v2.tauri.app/start/prerequisites/#linux); notably:
  - `libwebkit2gtk-4.1` and build tools (on Debian/Ubuntu: `libwebkit2gtk-4.1-dev`)
  - `libssl-dev`, `libgtk-3-dev`, and others (exact packages depend on your distro)
- **`pkexec` and polkit** - required only for writing to the real `/etc/fstab`; not needed for sandbox-target saves

## Building & Running

### Development

From the `src-tauri/` directory:

```bash
cargo tauri dev
```

This command automatically starts Trunk's dev server (which watches `frontend/src/` for changes and rebuilds on every save). The Tauri window will open with the compiled Yew frontend.

The workspace layout:
- `crates/fstab-core/` - pure Rust parser, validator, writer (zero GUI dependencies)
- `src-tauri/` - Tauri 2 application shell, command IPC layer, privilege escalation
- `frontend/` - Yew/WASM user interface (compiled via Trunk, not a workspace member due to target conflicts)

### Release Build

From the `src-tauri/` directory:

```bash
cargo tauri build
```

This invokes `trunk build --release` to compile the frontend, then bundles everything into a single native Linux executable. Build artifacts are placed in `src-tauri/target/release/bundle/`.

### Tailwind CSS Build Step

Tailwind is run as a CLI tool (Node.js runtime only) via a pre-build hook in `frontend/Trunk.toml`. The compiled CSS is written to `frontend/styles/tailwind.css` and included in the final bundle. This step is automatic - no manual invocation needed.

## Testing

The repository includes an automated test suite covering the parser, validator, atomic writer, and frontend components:

**Workspace (Rust backend):**

```bash
cargo test --workspace
```

Runs 160 tests across:
- `crates/fstab-core`: 105 unit tests + 4 fixture-based integration tests
- `src-tauri/rustab_lib`: 51 command and privilege-escalation tests

**Frontend (Yew/WASM):**

```bash
cd frontend && cargo test
```

Runs 9 tests for components and diff logic (native Rust target, not WASM, so tests run on the host).

**Total:** 169 automated tests, all passing.

Run `cargo fmt --check` and `cargo clippy -- -D warnings` locally to catch style issues before committing. Both are gated in CI.

## License

Rustab is dual-licensed under the MIT License or the Apache License 2.0 - your choice. See `LICENSE-MIT` and `LICENSE-APACHE` at the repository root for full terms.

## AI Use Disclosure

Rustab's development made use of AI code assistance (Claude Code) to accelerate initial implementation. This is disclosed openly: the maintainer is a senior software developer with 10+ years of production experience, and personally code-reviewed and tested every change before merging. The codebase is backed by a comprehensive automated test suite (227 tests, all passing) in addition to manual review and repeated live testing of the real application through its documented entrypoint (`cargo tauri dev`). AI assistance was a development tool, not a replacement for professional engineering judgment or testing discipline.

## Contributing

See `CONTRIBUTING.md` for guidelines on setting up the dev environment, running tests, code style expectations, and the PR process. Contributors are asked to add themselves to `CONTRIBUTORS.md` in their PR.

## Contact & Support

- **Email**: [rustab@tomsk.dev](mailto:rustab@tomsk.dev)
- **Discord**: [Join the dev server](https://discord.gg/vymDkeQnpj)
- **X / Twitter**: [@TomskDEV](https://x.com/TomskDEV)
- **Sponsor**: [Support on GitHub Sponsors](https://github.com/sponsors/t0msk)

Have a question, found a bug, or want to contribute? Reach out on Discord or email - all are welcome.
