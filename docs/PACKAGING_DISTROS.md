# Distro Packaging Guide: Ubuntu PPA, Debian, AUR, Fedora

**Status: planning document, not yet executed.** Nothing in this guide has been
run against a real Launchpad, AUR, or Copr account in this session - no package
was actually uploaded or built on any of those services. Every step that
requires an account, a GPG/SSH key, or a push to an external service is
real follow-up work for the maintainer, not something already done. This
guide exists to lay out *exactly* what to do and in what order, using facts
read directly from this repository (package name, bundle identifier,
license, existing CI) rather than generic advice.

Companion document: [`FLATPAK.md`](FLATPAK.md) already solved the same
underlying problem once (getting a Rust+Tauri app into a sandboxed,
no-network build) for Flathub. Several points below reuse that lesson
instead of re-deriving it.

## 0. Facts this guide is built on (read from the repo, not assumed)

- Package/binary name: `rustab` (`src-tauri/Cargo.toml`).
- Bundle identifier: `dev.tomsk.Rustab` (`src-tauri/tauri.conf.json`), unified
  with the Flatpak app ID in T44 - see `docs/FLATPAK.md` §1.3.
- Current version: `0.0.0` - this project has not cut a real release yet.
  All four channels below need an actual tagged version; see §1.
- License: dual `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`).
- GitHub Sponsors / X link reference the handle `t0msk`; `docs/FLATPAK.md`
  already names the target repo as `https://github.com/t0msk/rustab` and
  confirmed live (via `curl`) that it returns **404 today** - it does not
  exist publicly yet. This repo's local `git remote -v` is empty.
- CI already builds `.deb`, `.rpm`, and `.AppImage` via
  `cargo tauri build --ci --bundles deb,rpm,appimage`
  (`.github/workflows/release-build.yml`), triggered against a `release`
  branch tag cut by `.github/workflows/release-publish.yml`. That workflow
  does **not exist as a running thing yet** either - the `release` branch
  itself doesn't exist yet per that workflow's own comments.
- The app is two Cargo workspaces: the native one (`src-tauri/`,
  `crates/fstab-core/`) and a `wasm32-unknown-unknown` frontend
  (`frontend/`) built by Trunk, pulled in via `tauri.conf.json`'s
  `beforeBuildCommand`. Any from-source build job for any of the four
  channels below needs *both* toolchains (`rustup target add
  wasm32-unknown-unknown`, `trunk`, Node/npm for Tailwind) - not just a
  plain `cargo build`.

## 1. Prerequisite for all four channels: a real public release

None of PPA, a Debian repo, AUR, or Fedora Copr can package "whatever is on
your laptop." Every one of them packages *a specific, publicly fetchable
source snapshot* - a git tag, or a tarball at a stable URL. Concretely,
before touching any of the sections below:

1. **[MAINTAINER ACTION]** Create the public GitHub repo at
   `github.com/t0msk/rustab` (or wherever you actually want it) and push
   this repository's history to it. `FLATPAK.md` already flagged this as
   a shared prerequisite for Flathub too - do it once, it unblocks both.
2. **[MAINTAINER ACTION]** Run the existing `release-publish.yml` workflow
   once a `release` branch exists, to cut a real `vX.Y.Z` tag and GitHub
   Release (it currently bumps from `0.0.0`; `patch` gets you `0.0.1` -
   `minor` or `major` might be more honest for a first public release,
   that's your call, not something to decide mechanically here).
3. Every section below assumes a tagged source tarball exists at
   `https://github.com/t0msk/rustab/archive/refs/tags/vX.Y.Z.tar.gz` (GitHub
   generates this automatically for any tag, no extra step needed) and/or
   a GitHub Release with the three `release-build.yml` binary artifacts
   attached. AUR and Copr can use either; a PPA/Debian source package needs
   the source tarball, not the prebuilt binaries (Launchpad rebuilds from
   source itself).

## 2. Ubuntu PPA (Launchpad)

A PPA is a personal package archive hosted by Launchpad, which **rebuilds
your package from source on its own build farm** for every architecture it
offers (amd64, arm64, etc.) - it does not accept a `.deb` you built
yourself. This has one consequence that matters a lot for this project:
**Launchpad's builders have no network access during the actual package
build** (same constraint `FLATPAK.md` §2.2 already hit for Flathub). A
plain `cargo build` or `trunk build` that tries to download crates/npm
packages at build time will fail there. You must vendor dependencies into
the source package itself.

### 2.1 Account and key setup [MAINTAINER ACTION, one-time]

1. Create an Ubuntu One account, then a Launchpad account at
   `launchpad.net` (if you don't already have one).
2. Generate (if you don't have one) a GPG key:
   `gpg --full-generate-key` (RSA 4096, no expiry or a long one - this key
   signs every upload, losing it means re-doing this step and re-signing
   future uploads with a new key).
3. Upload the public key to a keyserver Launchpad polls, e.g.:
   `gpg --keyserver keyserver.ubuntu.com --send-keys <KEYID>`
4. In your Launchpad account, go to "OpenPGP keys" and import/confirm the
   same key (Launchpad emails a confirmation that you decrypt with
   `gpg --decrypt`).
5. Create an SSH key if you don't have one and add it to Launchpad too
   (`~/.ssh/id_ed25519.pub` -> Launchpad "SSH keys") - needed for
   `dput`/Launchpad git access in some flows, and generally for `bzr`/`git`
   interactions with Launchpad.
6. Create the PPA itself: Launchpad -> your account -> "Create a new PPA".
   Give it a name, e.g. `rustab` - its address becomes
   `ppa:t0msk/rustab` and users add it with
   `sudo add-apt-repository ppa:t0msk/rustab`.

### 2.2 Build the Debian source package locally

1. Install packaging tooling:
   `sudo apt install devscripts debhelper dput build-essential`
2. Create a `debian/` directory at the repo root (Tauri's own `.deb`
   bundler does **not** produce one - it generates a binary `.deb`
   directly via its own internal control-file generation, which is a
   different thing from a source package Launchpad can rebuild). Minimum
   files:
   - `debian/control` - `Source: rustab`, `Maintainer: <you>`,
     `Build-Depends:` listing `debhelper-compat (= 13)`, `cargo`,
     `rustc`, `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
     `libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf`, `nodejs`,
     `npm` (the same native dependency list `release-build.yml` already
     installs via `apt-get` - reuse it verbatim, it's already verified to
     be sufficient), plus a `Package: rustab` binary stanza with
     `Architecture: any` and `Depends: ${shlibs:Depends}, ${misc:Depends}`.
   - `debian/rules` - a minimal `dh $@` target set, with an `override_dh_auto_build`
     step that runs the real build: install the `wasm32-unknown-unknown`
     target, run `trunk build --release` in `frontend/`, then
     `cargo build --release` (or drive the whole thing through
     `cargo tauri build --bundles deb` if you'd rather keep one build
     entrypoint - but note that re-invokes Tauri's own bundler *inside*
     the Debian build, which is unusual and not the normal Debian
     packaging shape; a plain `cargo build --release` plus a `dh_install`
     step placing the binary/desktop file/icons is the more idiomatic
     path and easier to review).
   - `debian/copyright` - machine-readable copyright file listing both
     `LICENSE-MIT` and `LICENSE-APACHE` under the `MIT OR Apache-2.0`
     SPDX expression.
   - `debian/changelog` - created/bumped with `dch` (see below), drives
     the version Launchpad builds.
3. **Vendor all dependencies before building** - this is the step that is
   easy to skip and will bite you on Launchpad's actual builders even if
   it works fine on your own machine (which has network access):
   - `cargo vendor vendor/` (run from the repo root) pulls every crate
     dependency into `vendor/` and prints the `[source]` block to add to
     `.cargo/config.toml` so `cargo` reads from it instead of crates.io.
     Do this for **both** Cargo workspaces (root + `frontend/`).
   - `npm ci` then package `frontend/node_modules` into the source tarball
     too (or switch `package-lock.json`'s install to `npm ci --offline`
     once `node_modules` is vendored) - Launchpad has no npm registry
     access either.
   - This inflates the source package considerably (vendored crates for a
     GTK/WebKit Tauri app are not small). That's normal and expected for
     this kind of upload, not a mistake.
4. Set the changelog entry: `dch --create -v 0.1.0-1 "Initial release."`
   (match the version to the real tag from §1), then
   `dch --release` when the entry is final.
5. Build and sign the source package:
   `debuild -S -sa` (the `-sa` forces inclusion of the original source
   tarball on the first upload of a given upstream version; later
   `-N`/Debian-revision-only uploads can drop it). This produces a
   `.dsc`/`.changes` pair in the parent directory, GPG-signed with the key
   from §2.1.
6. Sanity-check it locally first with `sbuild` or `pbuilder` in a clean
   chroot (optional but strongly recommended - it reproduces the
   no-network, minimal-environment constraint locally before you spend a
   Launchpad build slot finding out the same thing).

### 2.3 Upload

1. `dput ppa:t0msk/rustab ../rustab_0.1.0-1_source.changes`
2. Launchpad emails you the build result per architecture. A failure here
   is normal on a first attempt for a project this size (missing
   `Build-Depends`, a vendoring gap, a path assumption that doesn't hold in
   the clean chroot) - fix and re-upload a new Debian revision
   (`0.1.0-2`), you cannot overwrite a version that already built.
3. Once it builds, users install with:
   ```
   sudo add-apt-repository ppa:t0msk/rustab
   sudo apt update
   sudo apt install rustab
   ```

## 3. Debian

Launchpad PPAs are **Ubuntu-specific** - they are not a channel Debian
users consume by default, and Launchpad does not build for plain Debian.
There is no direct "personal Debian PPA" equivalent hosted by Debian
itself for an outside maintainer; the two realistic options are:

### Option A (recommended for this project): self-hosted APT repository

You already produce a `.deb` via `cargo tauri build --bundles deb` in CI.
Serve it from a repository you control, using `reprepro` or `aptly` to
generate the `Packages`/`Release` index files and sign them with the same
GPG key from §2.1 (or a dedicated one).

1. `sudo apt install reprepro`
2. Create a repo layout, e.g. `apt-repo/conf/distributions`:
   ```
   Origin: Rustab
   Label: Rustab
   Codename: stable
   Architectures: amd64 arm64
   Components: main
   Description: Rustab APT repository
   SignWith: <GPG KEYID>
   ```
3. `reprepro -b apt-repo includedeb stable target/release/bundle/deb/*.deb`
4. Host the `apt-repo/` directory over HTTPS (this is genuinely just a
   static file server - a Forgejo "pages"-style static host, S3, or a
   plain nginx vhost on infrastructure you already run all work equally
   well; pick whichever one you already operate).
5. Publish your GPG public key at a stable URL and document the two
   commands users run once:
   ```
   curl -fsSL https://<your-host>/rustab.gpg | sudo tee /etc/apt/trusted.gpg.d/rustab.gpg
   echo "deb https://<your-host>/apt-repo stable main" | sudo tee /etc/apt/sources.list.d/rustab.list
   sudo apt update && sudo apt install rustab
   ```
6. On every new release, re-run `reprepro includedeb` with the new `.deb`
   and re-sync the directory - this is the actual "update channel" for
   Debian users, and it's a cron job or a release-pipeline step, not a
   manual one-off.

This also happens to work for Ubuntu users who don't want to use the PPA,
with the same repo, which is a nice side effect.

### Option B: the official Debian archive (ITP + sponsor)

The "proper" way into Debian is filing an Intent to Package (`ITP`) bug
against `wnpp`, getting the package reviewed by a Debian Developer
sponsor, and going through `mentors.debian.net`. This is a legitimate path
but is a multi-month process with real technical review (licensing of
every vendored dependency gets scrutinized individually, no bundled/
vendored crates are normally accepted - Debian wants each Rust crate
dependency packaged *separately* as its own `librust-*-dev` package,
which for a dependency tree this size is a substantial undertaking on its
own). Mentioned for completeness; **Option A is the realistic path** for
"let people update the app," which is what you asked for.

### Option C: Open Build Service (OBS)

`build.opensuse.org` (or a self-hosted OBS instance) accepts one source
recipe and builds `.deb` for Debian/Ubuntu *and* `.rpm` for Fedora/openSUSE
from the same project, which is worth knowing about if maintaining two
completely separate packaging recipes (this guide's §2/§3 Debian bits and
§5 Fedora bits) ever feels like too much duplication. Not pursued in
detail here since Copr (§5) is more idiomatically "the Fedora PPA" and
your own self-hosted repo already covers Debian/Ubuntu in one place.

## 4. AUR (Arch Linux User Repository)

This is the simplest of the four, for one structural reason: **the AUR
hosts no binaries at all.** You upload a `PKGBUILD` recipe (a shell
script), and every user's own `makepkg` builds the package *on their own
machine*, with their own network access - so the vendoring concern from
§2/§3 does not apply here at all. Arch also already ships a Tauri/WebKit
dependency chain in its own official repos, so build-deps are simpler to
express.

### 4.1 Account setup [MAINTAINER ACTION, one-time]

1. Register an account at `aur.archlinux.org`.
2. Generate an SSH key (`ssh-keygen -t ed25519`) if you don't have one,
   and add the **public** key to your AUR account settings - this is how
   you push to AUR's git, there is no password auth.

### 4.2 Write the PKGBUILD

Create it in its own empty directory (not inside this repo's tree - AUR
packages live in their own tiny git repo, one per package name):

```bash
pkgname=rustab
pkgver=0.1.0
pkgrel=1
pkgdesc="A modern, native Linux desktop GUI for safely editing /etc/fstab"
arch=('x86_64' 'aarch64')
url="https://github.com/t0msk/rustab"
license=('MIT' 'Apache-2.0')
depends=('webkit2gtk-4.1' 'gtk3' 'libayatana-appindicator')
makedepends=('rust' 'cargo' 'trunk' 'npm' 'nodejs')
source=("$pkgname-$pkgver.tar.gz::https://github.com/t0msk/rustab/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('SKIP')  # replace with the real checksum before publishing, see 4.3

build() {
  cd "$pkgname-$pkgver"
  rustup target add wasm32-unknown-unknown 2>/dev/null || true
  (cd frontend && npm ci && trunk build --release)
  cargo build --release --manifest-path src-tauri/Cargo.toml
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"
  install -Dm644 LICENSE-MIT "$pkgdir/usr/share/licenses/$pkgname/LICENSE-MIT"
  install -Dm644 LICENSE-APACHE "$pkgdir/usr/share/licenses/$pkgname/LICENSE-APACHE"
  # .desktop file and icon installs go here once the desktop entry this
  # project's flatpak/ directory already has (flatpak/dev.tomsk.Rustab.desktop)
  # has a non-Flatpak-specific counterpart to install from - check that
  # file's actual Exec= line before copying it verbatim into a non-sandboxed
  # package, since Flatpak desktop entries sometimes assume the Flatpak
  # launcher wrapper.
}
```

Notes on real gaps left deliberately visible rather than papered over:
- `sha256sums=('SKIP')` is a placeholder. **Never actually publish a
  PKGBUILD with `SKIP`** - it disables integrity checking entirely. Once a
  real tagged tarball exists (§1), compute it for real:
  `makepkg -g` (run inside the package directory, after setting
  `source=`) prints the correct line to paste in.
- The exact `depends=()` array above is a reasonable first guess based on
  the native dependency list `release-build.yml` already installs on
  Ubuntu, translated to Arch package names - it is **not verified against
  a real Arch system** in this session (no Arch box was available to
  test `makepkg` against). Confirm the real runtime `.so` dependencies
  with `namcap` or `ldd` against an actual built binary before publishing.
- `arch=('x86_64' 'aarch64')` assumes the build works on both; drop
  `aarch64` if you haven't actually verified a build on that architecture.

### 4.3 Publish

```bash
makepkg --printsrcinfo > .SRCINFO   # AUR requires this file to be present and in sync
git init
git remote add origin ssh://aur@aur.archlinux.org/rustab.git
git add PKGBUILD .SRCINFO
git commit -m "Initial import: rustab 0.1.0"
git push origin master
```

Users then install with any AUR helper, e.g. `paru -S rustab` or
`yay -S rustab`, or manually via `git clone` + `makepkg -si`. **Every
version bump is just**: edit `pkgver`/`pkgrel` in `PKGBUILD`, regenerate
`.SRCINFO`, commit, push - no account/review step gates an update the way
Launchpad or Copr's first build does.

## 5. Fedora (Copr)

Copr (`copr.fedorainfracloud.org`) is Fedora's own hosted build service and
is the closest real equivalent of a Launchpad PPA for Fedora - a personal
repo, one `dnf copr enable` away, that rebuilds from a `.spec` on Fedora's
own builders.

### 5.1 Account setup [MAINTAINER ACTION, one-time]

1. Log into `copr.fedorainfracloud.org` with a Fedora account (FAS -
   Fedora Account System; sign-up is free, no sponsorship needed for a
   personal Copr project, unlike the official Fedora repos).
2. Install the CLI locally if you want to build/push from a terminal
   instead of the web UI: `sudo dnf install copr-cli` (or via `pip` on a
   non-Fedora machine), then `copr-cli` walks you through saving an API
   token to `~/.config/copr`.
3. Create the project: `copr-cli create rustab --chroot fedora-rawhide-x86_64 --chroot fedora-40-x86_64`
   (pick the actual Fedora releases you want to support; add/adjust
   chroots later with `copr-cli edit-chroot`).

### 5.2 Get a real `.spec` file

Tauri's own `rpm` bundler (already used in `release-build.yml`) generates
a working `.spec` internally, but it's generated on the fly at build time
and not checked into this repo as a reusable file. Two ways to get a spec
Copr can build from:

- **Easiest**: run `cargo tauri build --bundles rpm` once locally (this
  repo's Ubuntu CI already proves this command works end to end), then
  look in `target/release/bundle/rpm/` - Tauri leaves the generated
  `.spec` alongside the `.rpm` it built from it. Copy that out as a
  starting point and commit it (e.g. to a new `rpm/rustab.spec`), then
  adjust `Source0`/`%prep` to pull from the real tagged GitHub tarball
  (§1) instead of a local path, since Copr builds from source, not from
  a path on your machine.
- **More control**: hand-write the spec following the standard Rust
  packaging guidelines Fedora documents at
  `docs.fedoraproject.org` ("Packaging:Rust"), which has its own
  conventions for vendoring crate dependencies into an RPM
  (`cargo-vendor-filterer` + a bundled `vendor.tar.xz` + specific
  `%generate_buildrequires`/`%cargo_prep` macros from the
  `rust-packaging` RPM macro set) - more idiomatic for an eventual
  official Fedora submission, more work up front.

Given the goal here is "let people update via Copr," not "get into the
official Fedora repos," the first option is the pragmatic starting point;
revisit the second if you ever pursue an official Fedora package instead
of a personal Copr repo.

### 5.3 Network access during the Copr build

Unlike Launchpad's and Debian's strict no-network build policy (§2, §3),
Fedora's own Rust packaging guidelines assume vendored sources are bundled
into the source RPM rather than fetched live - **verify Copr's current
mock-chroot network policy before relying on either assumption**; this
guide deliberately does not assert a specific current behavior here
because it was not checked live against Copr in this session (unlike the
Flathub/Launchpad no-network points above, which are either this
project's own verified CI behavior or `FLATPAK.md`'s already-live-checked
finding). Vendoring the same way as §2.3 is the safe default regardless of
what you find.

### 5.4 Build and publish

```bash
copr-cli build rustab rpm/rustab.spec   # or a SRPM path, or a direct URL to one
```

or point the Copr project at this GitHub repo directly (Copr supports
"Build from SCM," watching a repo/tag and rebuilding automatically on new
tags - set this up once in the web UI under the project's "Builds" ->
"New Build" -> "SCM" tab, pointing at the spec file's path once it's
committed).

Users then install with:
```
sudo dnf copr enable t0msk/rustab
sudo dnf install rustab
```

## 6. Summary: what's actually required of you vs. automated

| Channel | One-time account/key setup | Per-release work |
|---|---|---|
| Ubuntu PPA | Launchpad account, GPG key, SSH key, create PPA | `dch` bump, vendor deps, `debuild -S -sa`, `dput` |
| Debian (self-hosted) | Set up `reprepro`/`aptly` + hosting once | `reprepro includedeb` + sync host, each release |
| AUR | AUR account, SSH key | Bump `pkgver`, regenerate `.SRCINFO`, `git push` |
| Fedora Copr | FAS account, create Copr project | `copr-cli build`, or automatic via SCM watch |

None of these four is a one-time job overall - each needs the version
bumped and re-pushed/re-built on every release, same as the GitHub Release
flow this repo already automates in `release-publish.yml`. If this
repeated work becomes annoying once the first real release is out, the
next reasonable step is wiring `dput`/`reprepro`/AUR-push/`copr-cli` into
a new CI job triggered by the same tag `release-publish.yml` already
creates - not attempted in this guide since none of the four accounts/keys
exist yet for it to use.
