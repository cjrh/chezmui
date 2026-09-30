# chezmui

A small desktop GUI over the [`chezmoi`](https://www.chezmoi.io/) dotfile manager —
*chezmoi, without the typing*.

chezmui does one thing well: it shows what differs between your home directory and
the dotfiles chezmoi manages, and lets you act on each difference with a single
click. No need to remember command syntax or which direction a change flows.

![chezmui screenshot](screenshot.png)

## What it does

The left pane lists every managed file that currently differs (this is
`chezmoi status`). Select one to see its diff on the right, along with the actions
that make sense for it.

chezmoi's trickiest idea is that changes flow in two directions. chezmui names them
explicitly so you never have to keep them straight:

- **Apply →** — overwrite your file with the managed version (source → home).
- **← Re-add** — save your file's current contents back into chezmoi (home → source).
- **Forget** — stop managing a file. Your real file is left untouched.

The top-left actions work with local files:

- **Refresh** — re-read the status.
- **Apply all →** — write every managed source change onto your home files.
- **Add files…** — open a native file picker to start managing new files.

The amber **Remote / Git** actions are on the right:

- **Update (pull)** — `chezmoi update`: pull from the remote **and apply to home**.
- **Commit & push…** — open a separate Git window for chezmoi's source
  repository. Select files to see the exact diff for the next commit, enter a
  message, then use **Commit**. Make more commits in the same window, then use
  **Push & Close** to push existing commits. It does not commit selected files.
  Only selected files go into each commit, even if other files are staged.
  A failed push leaves the window open for a retry. Git errors are shown there.
  iced cannot parent this second window as a native modal on Linux; the main
  window remains visible, but its actions are disabled until you close Git.

Diffs are syntax-coloured the way a terminal pager would render them: additions in
green, removals in red, hunk headers in the accent colour.

## Install on Linux

Download the x86_64 AppImage and `.sha256` file from
[GitHub Releases](https://github.com/cjrh/chezmui/releases). Then run:

```sh
# Replace VERSION with the release version, for example 0.1.1.
sha256sum --check --ignore-missing chezmui-vVERSION-x86_64.sha256
chmod +x chezmui-vVERSION-x86_64.AppImage
./chezmui-vVERSION-x86_64.AppImage
```

If FUSE is unavailable, use
`./chezmui-vVERSION-x86_64.AppImage --appimage-extract-and-run`.
A `.tar.gz` archive with the binary is also available.

Release builds use Ubuntu 22.04 and require glibc 2.35 or newer. Use a Linux
desktop with Wayland or X11, system fonts, and XKB keyboard data. Native file
pickers need `xdg-desktop-portal` and a portal backend for your desktop.
The AppImage includes the keyboard and Wayland client libraries, but not
chezmoi or Git. No Rust toolchain or sibling checkout is needed to run it.

## Requirements

- The [`chezmoi`](https://www.chezmoi.io/install/) CLI, installed and on your `PATH`.
  chezmui uses it for dotfile operations and to locate its source directory.
- Git, installed and on your `PATH`, and a Git repository at the root of the
  chezmoi source directory. Commit and push use your existing Git configuration.

## Building and running

To build from source, install the stable Rust toolchain and check out
[`iced-themer`](https://github.com/cjrh/iced-themer) at `../iced-themer`.
It is a local path dependency. The release workflow pins its commit so CI does
not depend on later changes to that repository.

```sh
cargo run --release
```

Or build the binary and run it from anywhere:

```sh
cargo build --release
./target/release/chezmui
```

The theme is embedded into the binary at compile time, so the app looks the same
no matter which directory you launch it from.

## How it's built

- **[iced](https://iced.rs/) 0.14** for the UI, using the **tiny-skia** CPU
  renderer rather than a GPU backend. This is deliberate: it avoids GPU context
  loss across suspend/resume on Linux.
- Wayland and X11 are both supported.
- The UI is a thin presentation layer. Everything that knows how to talk to
  chezmoi lives in [`src/chezmoi.rs`](src/chezmoi.rs); the iced application
  (state, update, view) lives in [`src/main.rs`](src/main.rs).

## Theming

Colours, fonts, and panel styling come from [`theme.toml`](theme.toml), which is
consumed by the `iced-themer` crate and embedded at build time. Edit the
`[variables]` and `[palette]` sections to recolour the app, then rebuild.

## Releases

Install [cargo-release](https://github.com/crate-ci/cargo-release) once:

```sh
cargo install cargo-release --locked
```

Commit the release setup before the first release. From a clean `main` branch:

```sh
cargo test --locked
cargo release patch            # Preview; no changes, tag, or push.
cargo release patch --execute  # Bump, commit, tag vX.Y.Z, and push to origin.
```

Use `minor` or `major` instead of `patch` when needed. `publish = false` prevents
crates.io publication. `release.toml` enables Git tags and pushes independently.

A pushed version tag starts [the release workflow](.github/workflows/release.yml).
It checks that the tag matches `Cargo.toml`, runs tests, and builds:

- `chezmui-vX.Y.Z-x86_64.AppImage`
- `chezmui-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `chezmui-vX.Y.Z-x86_64.sha256`

Only a successful build can publish the GitHub release. Release notes are
created from GitHub history. Tags with a prerelease suffix produce prereleases.
A retry replaces assets on the same release. The workflow uses the built-in
`GITHUB_TOKEN`; no extra release secret is needed.

Use **Actions → Release → Run workflow** to check the build without publishing.
Download its `linux-x86_64` artifact to inspect the files. GitHub's workflow
artifact ZIP does not preserve executable permissions; run `chmod +x` on its
AppImage after extraction.

### Build release artifacts locally

Install `linuxdeploy` on `PATH`. The script uses system `patchelf` instead of
linuxdeploy's bundled copy, which can damage newer shared libraries.
On Ubuntu, also install:

```sh
sudo apt-get install python3 pkg-config patchelf libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev
./build-appimage.sh
```

The script writes all three files to `target/dist/` (or `$CARGO_TARGET_DIR/dist/`).
The workflow records the tested linuxdeploy version and checksum. Build on
Ubuntu 22.04 for the same glibc baseline; an AppImage built on a newer system
can require a newer glibc.

When changing `iced-themer`, update its commit in the workflow and verify
`cargo test --locked` with that clean checkout. Local uncommitted sibling
changes are not part of a release build.

## Status

A personal tool. It is not published to crates.io and carries no stability
guarantees.

## Licence

Licensed under the GNU Affero General Public License, version 3 or (at your
option) any later version (`AGPL-3.0-or-later`). See [LICENSE](LICENSE).
