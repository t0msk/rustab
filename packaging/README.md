# Packaging

Canonical source files for distro packaging live in this repo, next to the
code they package. The actual downstream submissions - the Flathub PR, an
AUR package repo, a Debian/PPA build - are **separate git repositories,
cloned wherever you like on disk, outside this repo**.

`packaging/sync.sh` keeps those clones' packaging files and this repo's
canonical copies as the *same file*, via a symlink, so there is never a
second copy that can silently drift out of sync (which is exactly what
happened before this script existed: real fixes landed directly in a
downstream clone during live testing, and the repo's own draft went
stale). It never commits or pushes anything itself.

## Layout

- `flatpak/` (repo root, not under `packaging/`) - the Flathub manifest and
  generated offline-build sources. Stays at the repo root rather than
  moving under `packaging/` because `docs/FLATPAK.md` and the submitted
  Flathub PR (flathub/flathub#10723) already reference these exact paths
  extensively; relocating it is a deliberate future step, not bundled into
  this reorg.
- `debian/` (repo root, once it exists) - a standard Debian source package
  directory. Debian tooling (`debuild`, `dpkg-buildpackage`) requires
  `debian/` at the root of the tree it builds from, so this can't live
  under `packaging/` either - see `docs/PACKAGING_DISTROS.md` §2.2.
- AUR's `PKGBUILD` is authored directly in its own AUR git clone, not in
  this repo - see `docs/PACKAGING_DISTROS.md` §4.2. Nothing to link/sync
  for it yet.

## Why a symlink here, when a symlink was rejected for the final push

A symlink works for local editing convenience: whether you edit the file
inside this repo or inside the downstream clone, it's the same inode, so
there is exactly one copy and it cannot drift.

It does **not** work for the file you actually commit and push to the
downstream repo. That repo's own CI (e.g. Flathub's `flatpak-builder-lint`)
checks it out standalone, with no access to this repo on your machine - a
symlink pointing back here would just be dangling from its point of view.
So the workflow has two steps: link while you work, materialize (replace
the symlink with a real copy) right before you commit there.

## Usage

```
packaging/sync.sh link flatpak          # once, after cloning the
                                         # downstream repo locally
# ...edit/test directly in the destination clone, or in this repo -
# ...it's the same file either way, nothing to keep in sync...
packaging/sync.sh materialize flatpak   # right before committing there
git -C "$RUSTAB_PACKAGING_ROOT/flatpak" add -A
git -C "$RUSTAB_PACKAGING_ROOT/flatpak" commit
git -C "$RUSTAB_PACKAGING_ROOT/flatpak" push
packaging/sync.sh link flatpak          # re-link, so later edits stay
                                         # single-source again
```

`--dry-run` on either mode shows what would change without touching
anything. `link` refuses to replace a destination file that already
differs from this repo's version, unless you pass `--force` (so a real,
uncommitted difference in the clone is never silently thrown away).

Destination root (one git clone per target underneath it):

```
RUSTAB_PACKAGING_ROOT=/path/to/your/packaging/clones packaging/sync.sh link
```

Defaults to `$HOME/rustab-packaging` if unset. Expected layout under that
root:

```
$RUSTAB_PACKAGING_ROOT/
└── flatpak/   <- clone of flathub/flathub, checked out on the submission branch
```

Per-target override, if a clone lives somewhere that doesn't fit the
`$RUSTAB_PACKAGING_ROOT/<target>` convention:

```
RUSTAB_PACKAGING_FLATPAK_DIR=/wherever/it/actually/is packaging/sync.sh link flatpak
```

Add more targets to `sync.sh`'s `TARGET_FILES`/`TARGET_SRC_DIR` maps once
a channel (Debian, AUR) actually has a canonical source file in this repo
worth linking out.

## `packaging/clones`

Every run of `sync.sh` also (re)creates `packaging/clones` in this repo as
a symlink to `$RUSTAB_PACKAGING_ROOT`, so you can `cd packaging/clones/flatpak`
instead of remembering (or hardcoding anywhere) where the real clones live
on this particular machine. It's listed in `.gitignore` - purely a local,
per-machine convenience, the same reasoning as the file-level symlinks
above: never committed, never pushed, nothing for another machine or CI to
resolve.
