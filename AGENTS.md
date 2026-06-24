# chezmui

A small desktop GUI front-end for the [chezmoi](https://www.chezmoi.io/) dotfile
manager. It shows the diff between your source and destination state and lets you
apply or re-add changes with one click. Personal binary, never published.

## Layout

- `src/main.rs` — the iced app (state / update / view) plus all UI.
- `src/chezmoi.rs` — the only place that shells out to the `chezmoi` CLI. Every
  chezmoi interaction goes through these blocking functions, run off the UI
  thread inside iced `Task`s. The module docstring explains the source ↔
  destination direction model.

## Dependencies & build

- `iced-themer` is a **local path dependency** (`../iced-themer`), not from
  crates.io — it must be checked out as a sibling directory.
- The iced `tiny-skia` **CPU renderer is deliberate** (see `Cargo.toml`): it
  avoids GPU context loss across suspend/resume on Linux. Don't switch to wgpu.
- Theme/colours come from `theme.toml` via iced-themer.

## iced gotchas (re-derived more than once — don't repeat the investigation)

- **Text size is `f32`, via `impl Into<Pixels>`** — the `SIZE_*` constants in
  `main.rs` must be `f32`, not `u16`. Using `u16` is a type error at `.size()`.
- **Size is separate from `Font`.** iced's `Font` only carries
  family/weight/style/stretch, so iced-themer's `[font]` table cannot include a
  size. Set sizing globally via `Settings::default_text_size` or per-widget with
  `.size()` — which is why sizes live as constants in `main.rs`.
- **Wayland file picker:** `rfd` delegates to xdg-desktop-portal's FileChooser,
  so a portal subprocess (e.g. a "KWin dialog helper") appears in the dock while
  `pick_files()` is open. This is expected — it is not a second app window.
