# Contributing to Rustab

Thank you for your interest in contributing to Rustab. This guide covers the development setup, code quality expectations, and the PR workflow.

## Development environment setup

### Rust toolchain

Install a recent Rust toolchain via [rustup](https://rustup.rs/), then add the WebAssembly target for the Yew frontend:

```sh
rustup target add wasm32-unknown-unknown
```

### Required tools

- **Trunk** (frontend build tool): `cargo install trunk`
- **Tauri CLI**: Available as `cargo-tauri` in the Cargo registry. The project uses `cargo tauri dev` and `cargo tauri build` commands, which work if `cargo-tauri` is installed:
  ```sh
  cargo install tauri-cli
  ```
- **Node.js** (dev-time only): Required solely to run the Tailwind CSS CLI via `npx` in the build process. No `node_modules` dependency ships with the final application - Node is never needed to run the built app.

### System dependencies (Linux)

Tauri 2 on Linux requires several system packages. See [Tauri's official Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux) for the complete, up-to-date list. A quick reference for Ubuntu:

```sh
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf build-essential curl wget file
```

### Optional: `pkexec` and polkit

Only needed if you plan to test against a real `/etc/fstab` (which you should almost never do - see the safety section below). On most Linux distributions, these are already installed. If not:

```sh
sudo apt-get install policykit-1  # or equivalent on your distro
```

### Running the app

From the `src-tauri/` directory:

```sh
cargo tauri dev
```

This automatically:
1. Runs `trunk serve` for the frontend (hot-reload dev server on `http://localhost:8080`).
2. Launches the Tauri window connected to that dev server.
3. Watches Rust source files in `src-tauri/` and `crates/fstab-core/` for changes.

By default, the app targets a **sandbox file** (`$XDG_DATA_HOME/rustab/fstab.sandbox`) for safety - see "Safety-first contribution notes" below.

## Safety-first contribution notes

Rustab edits `/etc/fstab`, a critical system file. Preventing data corruption is the project's top non-negotiable priority. Please follow these rules:

### Never test against the real `/etc/fstab` unless you deliberately choose to

- By default, in both debug and release builds, Rustab uses a **per-user sandbox file** (`$XDG_DATA_HOME/rustab/fstab.sandbox`). This is deliberate and safe.
- Automated tests run against synthetic sample files in `tests/fixtures/` - never against a real `/etc/fstab` or any personal copy.
- `dev-data/` is in `.gitignore` and is for manual smoke testing only. Its contents must never be committed and must never be read by automated tests.

### To point at `/etc/fstab` (only when you explicitly mean to)

The **only** way to make Rustab target the real system file is to explicitly export the environment variable before running:

```sh
RUSTAB_TARGET_PATH=/etc/fstab cargo tauri dev
```

This:
- Turns on the UI's danger banner and extra save confirmation.
- Activates the real `pkexec`-elevated write path.
- **Does not** happen by accident. If you run this, you understand you are editing the actual system file.

### Use synthetic test fixtures

For safe, reproducible testing, use the hand-written sample fstab files in `tests/fixtures/`. These include a variety of realistic entries (UUID/LABEL sources, bind mounts, swap, systemd options, escaped paths, malformed lines) and are safe to commit and share.

## Running tests

### Native workspace (fstab-core + src-tauri)

```sh
cargo test --workspace
```

This runs **160 tests** (109 in `fstab-core` - 105 unit tests plus 4
parser round-trip integration tests - and 51 in the `src-tauri`/`rustab`
crate), covering:
- `fstab-core` parser, validator, and atomic writer logic.
- Atomicity and failure-injection tests.
- Tauri command handlers and target resolution.
- Privilege escalation and elevated writer behavior.

### Frontend (Yew)

```sh
cd frontend && cargo test
```

This runs **9 tests** for the Yew component logic (Rust compiling to native `x86_64` - the test code itself does not need WebAssembly).

## Code style and quality gates

Before opening a PR, ensure all of these pass locally:

### Formatting

```sh
cargo fmt --check
cd frontend && cargo fmt --check
```

### Linting

```sh
cargo clippy --all-targets -- -D warnings
cd frontend && cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
```

(The frontend targets `wasm32-unknown-unknown` because it compiles to WebAssembly; clippy must check that specific target.)

### Tests

```sh
cargo test --workspace
cd frontend && cargo test
```

### Documentation

Public items in `fstab-core` must have `///` rustdoc comments explaining their contract and invariants - not just restating the function signature. For example:

```rust
/// Atomically replaces `target` with `candidate`, restoring from `backup` if the write fails.
/// If this function returns `Ok`, the file at `target` is guaranteed to match `candidate`
/// byte-for-byte; if it returns `Err`, the file is guaranteed to be unchanged.
pub fn write_document(target: &Path, candidate: &Document) -> Result<WriteOutcome, WriteError> {
    // ...
}
```

This is a requirement in the project spec and is checked during code review.

## Pull request workflow

1. **Fork the repository** and create a branch with a clear, descriptive name:
   ```sh
   git checkout -b feat/your-feature
   ```

2. **Keep changes scoped.** One feature or bug fix per PR; avoid mixing refactoring and new functionality.

3. **Run the code quality gates locally** (fmt, clippy, tests) before pushing. CI will also run them, but it's faster to catch issues first.

4. **Push your branch and open a PR** with a clear description of what you changed and why.

### Commit authorship

This repository has an explicit policy on commit authorship: commits carry only the contributor's own authorship. There are no AI co-author trailers in commit messages or PR descriptions. This is a deliberate choice by the project maintainer. If you use AI assistance in your own work, follow the same convention for PRs in this repository.

## Contributing to CONTRIBUTORS.md

In your PR, please add yourself to the `CONTRIBUTORS.md` file in the same format as existing entries. This is a simple way to acknowledge that you've contributed.

## Questions or issues?

Reach out to the maintainer at **opensource@tomsk.dev** for anything related to contributing, development setup, or the contribution workflow.

## Key references

- **Project specification:** `docs/SPEC.md` - covers atomicity requirements, safety design, fstab domain coverage, and project non-goals.
- **API contract:** `docs/API_CONTRACT.md` - documents the Tauri command boundary, wire format, and privilege escalation design.
- **Tauri prerequisites:** [v2.tauri.app/start/prerequisites](https://v2.tauri.app/start/prerequisites/) - official Tauri docs for system dependencies.
