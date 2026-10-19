#!/usr/bin/env bash
#
# Generates the three offline-vendoring source files Flathub's manifest
# (flatpak/dev.tomsk.Rustab.yml) needs for its no-network build
# (docs/FLATPAK.md §2.2/2.3):
#
#   flatpak/cargo-sources.json          <- Cargo.lock (fstab-core + src-tauri)
#   flatpak/frontend-cargo-sources.json <- frontend/Cargo.lock
#   flatpak/node-sources.json           <- frontend/package-lock.json
#
# Uses the two generators from flatpak/flatpak-builder-tools (upstream
# Flathub project), cloned into a local, gitignored cache on first run and
# reused (not re-cloned) on later runs. Requires network access and
# Python 3.10+ (the node generator's own minimum) - both only for running
# this script, never for the actual Flatpak build.
#
# Usage: flatpak/generate-sources.sh
# Re-run this every time Cargo.lock, frontend/Cargo.lock, or
# frontend/package-lock.json changes, and always right before a Flathub
# submission or update (docs/FLATPAK.md §5) - a stale output here is a
# silently broken offline build, not a warning.

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
repo_root="$(cd -- "$script_dir/.." &>/dev/null && pwd)"
cache_dir="$script_dir/.cache"
tools_dir="$cache_dir/flatpak-builder-tools"
venv_dir="$cache_dir/venv"

log() { printf '==> %s\n' "$1"; }
die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

command -v python3 &>/dev/null || die "python3 not found - required to run the generators."
command -v git &>/dev/null || die "git not found - required to fetch flatpak-builder-tools."

py_minor="$(python3 -c 'import sys; print(sys.version_info[1])')"
if [[ "$py_minor" -lt 10 ]]; then
    die "python3 is too old ($(python3 --version)) - flatpak-node-generator needs 3.10+."
fi

mkdir -p "$cache_dir"

if [[ -d "$tools_dir/.git" ]]; then
    log "Updating cached flatpak-builder-tools checkout..."
    git -C "$tools_dir" fetch --depth=1 origin master
    git -C "$tools_dir" reset --hard origin/master
else
    log "Cloning flatpak-builder-tools (first run only, cached after this)..."
    git clone --depth=1 https://github.com/flatpak/flatpak-builder-tools.git "$tools_dir"
fi

if [[ ! -d "$venv_dir" ]]; then
    log "Creating a venv for the generators' own dependencies..."
    python3 -m venv "$venv_dir"
fi

log "Installing/upgrading generator dependencies into the venv..."
"$venv_dir/bin/pip" install -q --upgrade pip
# Versions match what each tool's own pyproject.toml currently pins
# (checked against the cloned copy) - re-check there if either generator's
# own requirements change upstream and this install starts failing.
"$venv_dir/bin/pip" install -q "aiohttp>=3.9,<4.0" "PyYAML>=6.0,<7.0" "tomlkit>=0.13,<1.0"

cargo_generator="$tools_dir/cargo/flatpak-cargo-generator.py"
[[ -f "$cargo_generator" ]] || die "flatpak-cargo-generator.py not found at $cargo_generator"
[[ -d "$tools_dir/node/flatpak_node_generator" ]] || die "flatpak_node_generator package not found under $tools_dir/node"

run_cargo_generator() {
    local lockfile="$1" output="$2"
    log "Generating $(basename "$output") from $(realpath --relative-to="$repo_root" "$lockfile")..."
    "$venv_dir/bin/python" "$cargo_generator" "$lockfile" -o "$output"
}

run_cargo_generator "$repo_root/Cargo.lock" "$script_dir/cargo-sources.json"
run_cargo_generator "$repo_root/frontend/Cargo.lock" "$script_dir/frontend-cargo-sources.json"

log "Generating node-sources.json from frontend/package-lock.json..."
# Tailwind v4's platform-specific optional dependencies (@tailwindcss/
# oxide-*, lightningcss-*, @parcel/watcher-*) must be included, not just
# the pure-JS tree (docs/FLATPAK.md §2.3's wrinkle) - the default run
# verified against this project's real lockfile (2026-10-19) does include
# them with no extra flag, but if a future flatpak-node-generator version
# changes that default, re-check the output for those packages before
# trusting it.
PYTHONPATH="$tools_dir/node" "$venv_dir/bin/python" -m flatpak_node_generator \
    npm "$repo_root/frontend/package-lock.json" \
    -o "$script_dir/node-sources.json"

log "Done. Generated:"
printf '    %s\n' \
    "$script_dir/cargo-sources.json" \
    "$script_dir/frontend-cargo-sources.json" \
    "$script_dir/node-sources.json"
log "These are gitignored here (see .gitignore) - their long-term home is the Flathub-org repo once a submission is accepted (docs/FLATPAK.md §5.1)."
