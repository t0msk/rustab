#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: packaging/sync.sh <link|materialize> [--dry-run] [--force] [target ...]

link         Symlink each target's canonical files, in the destination
             clone, to the real files in this repo - so day-to-day edits
             made while working inside the destination clone (e.g. while
             testing a Flatpak build) land directly in this repo. There is
             only ever one real copy of the content; nothing can drift.

materialize  Replace the destination's symlinks (or any existing file)
             with a real copy of this repo's content. Run this immediately
             before 'git add'/'git commit'/'git push' in the destination
             clone: downstream CI (e.g. Flathub's) checks out that repo
             standalone and cannot follow a symlink pointing back into
             this repo on your machine.

Targets (default: all configured):
  flathub   this repo's flatpak/ dir -> the Flathub submission clone:
            dev.tomsk.Rustab.yml, cargo-sources.json,
            frontend-cargo-sources.json, node-sources.json

Destination root:
  RUSTAB_PACKAGING_ROOT     defaults to "$HOME/rustab-packaging"
  Per-target override:      RUSTAB_PACKAGING_<TARGET>_DIR
                             e.g. RUSTAB_PACKAGING_FLATPAK_DIR=/some/path

Every run also (re)creates packaging/clones in this repo as a symlink to
RUSTAB_PACKAGING_ROOT, so you can e.g. 'cd packaging/clones/flathub'
without remembering the real path. It's gitignored - purely a local,
per-machine convenience, never tracked or pushed.

--force overwrites a destination file that differs from this repo's
version and is not already a symlink to it (link mode only; materialize
always overwrites, that's its job).

Typical workflow:
  packaging/sync.sh link flathub          # once, after cloning the
                                           # downstream repo locally
  ...edit/test directly in the destination clone, or in this repo -
  ...it's the same file either way...
  packaging/sync.sh materialize flathub   # right before committing there
  git -C "$RUSTAB_PACKAGING_ROOT/flathub" add -A
  git -C "$RUSTAB_PACKAGING_ROOT/flathub" commit
  git -C "$RUSTAB_PACKAGING_ROOT/flathub" push
  packaging/sync.sh link flathub          # re-link, so later edits stay
                                           # single-source again
EOF
}

MODE="${1:-}"
case "$MODE" in
  link|materialize) shift ;;
  -h|--help) usage; exit 0 ;;
  *)
    echo "error: first argument must be 'link' or 'materialize'" >&2
    usage >&2
    exit 1
    ;;
esac

DRY_RUN=0
FORCE=0
TARGETS=()

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --force) FORCE=1 ;;
    -h|--help) usage; exit 0 ;;
    *) TARGETS+=("$arg") ;;
  esac
done

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd)"
PACKAGING_ROOT="${RUSTAB_PACKAGING_ROOT:-$HOME/rustab-packaging}"

declare -A TARGET_SRC_DIR
declare -A TARGET_FILES

TARGET_SRC_DIR[flathub]="$REPO_ROOT/flatpak"
TARGET_FILES[flathub]="dev.tomsk.Rustab.yml cargo-sources.json frontend-cargo-sources.json node-sources.json"

if [ "${#TARGETS[@]}" -eq 0 ]; then
  TARGETS=("${!TARGET_FILES[@]}")
fi

ensure_clones_convenience_link() {
  local link="$REPO_ROOT/packaging/clones"

  if [ -L "$link" ]; then
    local current
    current="$(readlink -f -- "$link" 2>/dev/null || true)"
    if [ "$current" = "$(readlink -f -- "$PACKAGING_ROOT" 2>/dev/null || printf '%s' "$PACKAGING_ROOT")" ]; then
      return 0
    fi
    [ "$DRY_RUN" -eq 1 ] && return 0
    ln -sfn "$PACKAGING_ROOT" "$link"
    return 0
  fi

  if [ -e "$link" ]; then
    echo "warn: $link exists and is not a symlink - leaving it alone (packaging/clones is meant to be gitignored and machine-local)" >&2
    return 0
  fi

  [ "$DRY_RUN" -eq 1 ] && return 0
  mkdir -p "$PACKAGING_ROOT"
  ln -s "$PACKAGING_ROOT" "$link"
}

dest_dir_for() {
  local name="$1"
  local upper
  upper="$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')"
  local var="RUSTAB_PACKAGING_${upper}_DIR"
  local override="${!var:-}"
  if [ -n "$override" ]; then
    printf '%s' "$override"
  else
    printf '%s/%s' "$PACKAGING_ROOT" "$name"
  fi
}

link_one() {
  local src="$1" dest="$2"

  if [ -L "$dest" ]; then
    local current
    current="$(readlink -f -- "$dest" 2>/dev/null || true)"
    if [ "$current" = "$(readlink -f -- "$src")" ]; then
      echo "  $(basename "$dest"): already linked"
      return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "  $(basename "$dest"): would relink (currently points elsewhere)"
      return 0
    fi
    ln -sf "$src" "$dest"
    echo "  $(basename "$dest"): relinked"
    return 0
  fi

  if [ -e "$dest" ]; then
    if ! diff -q "$dest" "$src" >/dev/null 2>&1 && [ "$FORCE" -ne 1 ]; then
      echo "  warn: $dest exists, is a regular file, and differs from $src - skipping (use --force to replace with a symlink)" >&2
      return 1
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "  $(basename "$dest"): would replace plain file with symlink"
      return 0
    fi
    rm -f "$dest"
    ln -s "$src" "$dest"
    echo "  $(basename "$dest"): linked (replaced plain file)"
    return 0
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  $(basename "$dest"): would link"
    return 0
  fi
  ln -s "$src" "$dest"
  echo "  $(basename "$dest"): linked"
}

materialize_one() {
  local src="$1" dest="$2"

  if [ -L "$dest" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "  $(basename "$dest"): would materialize (currently a symlink)"
      return 0
    fi
    rm -f "$dest"
    cp "$src" "$dest"
    echo "  $(basename "$dest"): materialized"
    return 0
  fi

  if [ -e "$dest" ] && diff -q "$dest" "$src" >/dev/null 2>&1; then
    echo "  $(basename "$dest"): unchanged"
    return 0
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  $(basename "$dest"): would change"
    return 0
  fi
  cp "$src" "$dest"
  echo "  $(basename "$dest"): copied"
}

run_target() {
  local name="$1"
  local src_dir="${TARGET_SRC_DIR[$name]:-}"
  local files="${TARGET_FILES[$name]:-}"

  if [ -z "$src_dir" ] || [ -z "$files" ]; then
    echo "error: unknown target '$name'" >&2
    return 1
  fi

  local dest_dir
  dest_dir="$(dest_dir_for "$name")"

  if [ ! -d "$dest_dir/.git" ]; then
    echo "skip $name: '$dest_dir' is not a git clone yet" >&2
    echo "  clone the real downstream repo there, or set RUSTAB_PACKAGING_$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')_DIR" >&2
    return 1
  fi

  echo "== $name ($MODE): $src_dir <-> $dest_dir =="
  local f status=0
  for f in $files; do
    if [ ! -f "$src_dir/$f" ]; then
      echo "  warn: $src_dir/$f does not exist, skipping" >&2
      continue
    fi
    if [ "$MODE" = "link" ]; then
      link_one "$src_dir/$f" "$dest_dir/$f" || status=1
    else
      materialize_one "$src_dir/$f" "$dest_dir/$f" || status=1
    fi
  done
  return "$status"
}

ensure_clones_convenience_link

status=0
for t in "${TARGETS[@]}"; do
  run_target "$t" || status=1
done

if [ "$DRY_RUN" -eq 0 ] && [ "$status" -eq 0 ] && [ "$MODE" = "materialize" ]; then
  echo
  echo "Done. Review with 'git -C <destination> diff' before committing/pushing there."
fi

exit "$status"
