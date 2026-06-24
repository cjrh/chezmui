//! chezmui — a small GUI over the `chezmoi` dotfile manager.
//!
//! The app does one thing well: show what differs between your home directory
//! and the dotfiles chezmoi manages, and let you act on each difference with a
//! single click. It is a thin presentation layer; everything that knows how to
//! talk to chezmoi lives in the [`chezmoi`] module.
//!
//! ## The mental model the UI presents
//!
//! chezmoi's hardest idea is that changes flow in two directions. The UI names
//! them explicitly so the user never has to remember command syntax:
//!
//! * **Apply →**  push the managed version onto your home directory.
//! * **← Re-add** pull your local edits back into chezmoi's source.
//!
//! The left pane lists every differing file; selecting one shows its diff and
//! the per-file actions on the right.

#![windows_subsystem = "windows"]

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{Column, button, column, container, row, scrollable, text};
use iced::{
    Alignment, Border, Color, Element, Font, Length, Size, Task, Theme, color, theme::Palette,
    window,
};
use iced_themer::{ThemeConfig, Themed};

mod chezmoi;
use chezmoi::{Change, Entry};

/// The theme is baked into the binary so the app is styled regardless of the
/// directory it is launched from.
const THEME_TOML: &str = include_str!("../theme.toml");

fn main() -> iced::Result {
    let config = Arc::new(
        THEME_TOML
            .parse::<ThemeConfig>()
            .expect("embedded theme.toml should parse"),
    );

    let font = config.font();
    let boot_cfg = Arc::clone(&config);
    let theme_cfg = Arc::clone(&config);

    let app = iced::application(move || boot(Arc::clone(&boot_cfg)), update, view)
        .title("chezmui")
        .theme(move |_state: &State| theme_cfg.theme())
        .window(window_settings());

    // iced-themer reports a configured font (or None to keep iced's default).
    match font {
        Some(f) => app.default_font(f).run(),
        None => app.run(),
    }
}

fn window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(980.0, 660.0),
        min_size: Some(Size::new(680.0, 440.0)),
        ..window::Settings::default()
    }
}

// ── State ────────────────────────────────────────────────────────────

struct State {
    /// The loaded theme, kept so `view` can read the palette and style panels.
    theme: Arc<ThemeConfig>,
    /// Every managed path that currently differs (from `chezmoi status`).
    entries: Vec<Entry>,
    /// Index into `entries` of the file whose diff is shown, if any.
    selected: Option<usize>,
    /// The diff for the selected entry.
    diff: DiffState,
    /// The most recent operation result, shown as a coloured banner.
    banner: Option<Banner>,
    /// True while a chezmoi command is running; disables the action buttons so
    /// the user can't fire overlapping operations.
    busy: bool,
}

impl State {
    /// The absolute path of the selected entry, if one is selected.
    fn selected_path(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|i| self.entries.get(i))
            .map(|e| e.path.clone())
    }
}

/// The right pane's diff, modelled as a small state machine so the UI can show
/// "loading", an error, or "nothing to show" without special-casing in `view`.
enum DiffState {
    /// No entry selected.
    Empty,
    /// A diff has been requested and is being computed.
    Loading,
    Loaded(String),
    Error(String),
}

struct Banner {
    level: Level,
    message: String,
}

#[derive(Clone, Copy)]
enum Level {
    Success,
    Error,
}

#[derive(Debug, Clone)]
enum Msg {
    /// Re-run `chezmoi status`.
    Refresh,
    StatusLoaded(Result<Vec<Entry>, String>),
    /// A file row was clicked.
    Select(usize),
    DiffLoaded(Result<String, String>),
    /// Push every change onto the home directory.
    ApplyAll,
    /// Per-selected-entry actions.
    ApplyOne,
    ReAddOne,
    ForgetOne,
    /// `chezmoi update`: pull from the remote and apply.
    Update,
    /// Open a file picker to start managing new files.
    PickFiles,
    FilesPicked(Vec<PathBuf>),
    /// A long-running operation finished. The label names it for the banner.
    OpDone(&'static str, Result<String, String>),
}

fn boot(theme: Arc<ThemeConfig>) -> (State, Task<Msg>) {
    let state = State {
        theme,
        entries: Vec::new(),
        selected: None,
        diff: DiffState::Empty,
        banner: None,
        busy: true,
    };
    (state, Task::perform(async { chezmoi::status() }, Msg::StatusLoaded))
}

// ── Update ───────────────────────────────────────────────────────────

fn update(state: &mut State, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::Refresh => {
            state.busy = true;
            state.banner = None;
            Task::perform(async { chezmoi::status() }, Msg::StatusLoaded)
        }

        Msg::StatusLoaded(Ok(entries)) => {
            state.busy = false;
            // Preserve the selection across a refresh by matching on path, since
            // indices shift as entries appear and disappear. Re-load its diff so
            // the right pane reflects the file's new state after an operation.
            let prev = state
                .selected
                .and_then(|i| state.entries.get(i))
                .map(|e| e.path.clone());
            state.entries = entries;
            match prev.and_then(|p| state.entries.iter().position(|e| e.path == p)) {
                Some(i) => {
                    state.selected = Some(i);
                    state.diff = DiffState::Loading;
                    let path = state.entries[i].path.clone();
                    Task::perform(async move { chezmoi::diff(Some(&path)) }, Msg::DiffLoaded)
                }
                None => {
                    state.selected = None;
                    state.diff = DiffState::Empty;
                    Task::none()
                }
            }
        }
        Msg::StatusLoaded(Err(e)) => {
            state.busy = false;
            state.banner = Some(Banner {
                level: Level::Error,
                message: format!("Couldn't read status: {e}"),
            });
            Task::none()
        }

        Msg::Select(i) => {
            if i >= state.entries.len() {
                return Task::none();
            }
            state.selected = Some(i);
            state.diff = DiffState::Loading;
            let path = state.entries[i].path.clone();
            Task::perform(async move { chezmoi::diff(Some(&path)) }, Msg::DiffLoaded)
        }
        Msg::DiffLoaded(Ok(d)) => {
            state.diff = DiffState::Loaded(d);
            Task::none()
        }
        Msg::DiffLoaded(Err(e)) => {
            state.diff = DiffState::Error(e);
            Task::none()
        }

        Msg::ApplyAll => start_op(state, "Apply all", async {
            chezmoi::apply(&[]).map(|_| "Applied all changes.".to_string())
        }),
        Msg::ApplyOne => match state.selected_path() {
            Some(path) => start_op(state, "Apply", async move {
                chezmoi::apply(&[path]).map(|_| "Applied.".to_string())
            }),
            None => Task::none(),
        },
        Msg::ReAddOne => match state.selected_path() {
            Some(path) => start_op(state, "Re-add", async move {
                chezmoi::re_add(&[path]).map(|_| "Saved your changes into chezmoi.".to_string())
            }),
            None => Task::none(),
        },
        Msg::ForgetOne => match state.selected_path() {
            Some(path) => start_op(state, "Forget", async move {
                chezmoi::forget(&path).map(|_| "Stopped managing that file.".to_string())
            }),
            None => Task::none(),
        },
        Msg::Update => start_op(state, "Update", async { chezmoi::update() }),

        Msg::PickFiles => Task::perform(pick_files(), Msg::FilesPicked),
        Msg::FilesPicked(paths) => {
            if paths.is_empty() {
                return Task::none();
            }
            start_op(state, "Add", async move {
                chezmoi::add(&paths).map(|_| "Added to chezmoi.".to_string())
            })
        }

        Msg::OpDone(label, result) => {
            state.busy = false;
            state.banner = Some(match result {
                Ok(msg) => Banner {
                    level: Level::Success,
                    message: if msg.trim().is_empty() {
                        format!("{label} done.")
                    } else {
                        msg
                    },
                },
                Err(e) => Banner {
                    level: Level::Error,
                    message: format!("{label} failed: {e}"),
                },
            });
            // Any operation changes the on-disk state, so refresh the list.
            Task::perform(async { chezmoi::status() }, Msg::StatusLoaded)
        }
    }
}

/// Mark the app busy, clear any stale banner, and run a chezmoi operation whose
/// success/failure is reported through [`Msg::OpDone`].
///
/// Centralising this keeps every operation arm in `update` to a single line and
/// guarantees they all share the same busy/banner bookkeeping.
fn start_op<F>(state: &mut State, label: &'static str, fut: F) -> Task<Msg>
where
    F: Future<Output = Result<String, String>> + Send + 'static,
{
    state.busy = true;
    state.banner = None;
    Task::perform(fut, move |r| Msg::OpDone(label, r))
}

/// Show a native multi-select file picker rooted at the home directory.
/// Returns the chosen paths, or an empty vector if the user cancelled.
async fn pick_files() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    match rfd::AsyncFileDialog::new()
        .set_title("Add files to chezmoi")
        .set_directory(home)
        .pick_files()
        .await
    {
        Some(handles) => handles.into_iter().map(|h| h.path().to_path_buf()).collect(),
        None => Vec::new(),
    }
}

// ── View ─────────────────────────────────────────────────────────────

fn view(state: &State) -> Element<'_, Msg> {
    let palette = state.theme.theme().palette();

    let body = row![entry_list(state, &palette), detail_pane(state, &palette)]
        .spacing(16)
        .height(Length::Fill);

    let mut root = column![header(state, &palette)].spacing(12);
    if let Some(banner) = &state.banner {
        root = root.push(banner_view(banner, &palette));
    }
    root = root.push(body);

    container(root)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Title plus the global actions that apply to everything at once.
fn header<'a>(state: &State, palette: &Palette) -> Element<'a, Msg> {
    let titles = column![
        text("chezmui").size(24).color(palette.text),
        text("chezmoi, without the typing")
            .size(12)
            .color(dim(palette.text, 0.5)),
    ]
    .spacing(2)
    .width(Length::Fill);

    let working = if state.busy {
        text("working…").size(12).color(palette.primary)
    } else {
        text("")
    };

    let idle = !state.busy;
    let buttons = row![
        action_button("Refresh", palette.primary, idle.then_some(Msg::Refresh)),
        action_button(
            "Apply all →",
            palette.success,
            (idle && !state.entries.is_empty()).then_some(Msg::ApplyAll),
        ),
        action_button("Update (pull)", palette.primary, idle.then_some(Msg::Update)),
        action_button("Add files…", palette.primary, idle.then_some(Msg::PickFiles)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    row![titles, working, buttons]
        .spacing(16)
        .align_y(Alignment::Center)
        .into()
}

/// A short coloured strip reporting the last operation's outcome.
fn banner_view<'a>(banner: &Banner, palette: &Palette) -> Element<'a, Msg> {
    let accent = match banner.level {
        Level::Success => palette.success,
        Level::Error => palette.danger,
    };
    container(text(banner.message.clone()).size(12).color(palette.text))
        .padding([8, 12])
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(dim(accent, 0.15).into()),
            border: Border {
                radius: 8.0.into(),
                width: 1.0,
                color: dim(accent, 0.4),
            },
            ..container::Style::default()
        })
        .into()
}

/// Left panel: a count and the scrollable list of differing files.
fn entry_list<'a>(state: &'a State, palette: &Palette) -> Element<'a, Msg> {
    let inner: Element<Msg> = if state.entries.is_empty() {
        container(
            column![
                text("✓ Everything is in sync")
                    .size(15)
                    .color(palette.success),
                text("No managed files differ from your home directory.")
                    .size(12)
                    .color(dim(palette.text, 0.5)),
            ]
            .spacing(6),
        )
        .center_x(Length::Fill)
        .padding(24)
        .into()
    } else {
        let rows: Vec<Element<Msg>> = state
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| entry_row(e, i, state.selected == Some(i), palette))
            .collect();
        scrollable(
            Column::with_children(rows)
                .spacing(2)
                .padding([0, 6])
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    };

    let heading = text(format!("Changes ({})", state.entries.len()))
        .size(13)
        .color(dim(palette.text, 0.6));

    container(column![heading, inner].spacing(10).height(Length::Fill))
        .padding(14)
        .width(Length::FillPortion(2))
        .height(Length::Fill)
        .themed(state.theme.container())
        .into()
}

/// One clickable file row: a two-character status badge and the path.
fn entry_row<'a>(entry: &Entry, index: usize, selected: bool, palette: &Palette) -> Element<'a, Msg> {
    let code = format!(
        "{}{}",
        code_char(entry.last_to_actual),
        code_char(entry.actual_to_target),
    );
    let badge = text(code)
        .font(Font::MONOSPACE)
        .size(13)
        .color(change_color(primary_change(entry), palette));
    let path = text(entry.display.clone()).size(13).color(palette.text);

    let content = row![badge, path]
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let primary = palette.primary;
    let text_color = palette.text;
    button(content)
        .on_press(Msg::Select(index))
        .width(Length::Fill)
        .padding([7, 10])
        .style(move |_theme, status| {
            let background = if selected {
                dim(primary, 0.18)
            } else if matches!(status, button::Status::Hovered) {
                Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.06,
                }
            } else {
                Color::TRANSPARENT
            };
            let border = Border {
                radius: 8.0.into(),
                width: if selected { 1.0 } else { 0.0 },
                color: dim(primary, 0.4),
            };
            button::Style {
                background: Some(background.into()),
                text_color,
                border,
                ..button::Style::default()
            }
        })
        .into()
}

/// Right panel: the selected file's path, its per-file actions, and the diff.
fn detail_pane<'a>(state: &'a State, palette: &Palette) -> Element<'a, Msg> {
    let inner: Element<Msg> = match state.selected {
        None => container(
            text("Select a file on the left to see what changed.")
                .size(13)
                .color(dim(palette.text, 0.5)),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into(),
        Some(i) => {
            let entry = &state.entries[i];
            let idle = !state.busy;

            let change = primary_change(entry);
            let title = row![
                text(entry.display.clone()).size(15).color(palette.text),
                text(change.label())
                    .size(11)
                    .color(change_color(change, palette)),
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            let actions = row![
                labeled_action(
                    "Apply →",
                    "Overwrite your file with the managed version.",
                    palette.success,
                    idle.then_some(Msg::ApplyOne),
                    palette,
                ),
                labeled_action(
                    "← Re-add",
                    "Save your file's current contents into chezmoi.",
                    palette.primary,
                    idle.then_some(Msg::ReAddOne),
                    palette,
                ),
                labeled_action(
                    "Forget",
                    "Stop managing this file. Your file is left untouched.",
                    palette.danger,
                    idle.then_some(Msg::ForgetOne),
                    palette,
                ),
            ]
            .spacing(12);

            column![title, actions, diff_view(&state.diff, palette)]
                .spacing(14)
                .height(Length::Fill)
                .into()
        }
    };

    container(inner)
        .padding(16)
        .width(Length::FillPortion(3))
        .height(Length::Fill)
        .themed(state.theme.container())
        .into()
}

/// An action button stacked above a one-line caption explaining what it does.
fn labeled_action<'a>(
    label: &str,
    caption: &str,
    accent: Color,
    on_press: Option<Msg>,
    palette: &Palette,
) -> Element<'a, Msg> {
    column![
        action_button(label, accent, on_press),
        text(caption.to_string())
            .size(10)
            .color(dim(palette.text, 0.5)),
    ]
    .spacing(4)
    .max_width(160)
    .into()
}

/// The diff for the selected file, syntax-coloured line by line.
fn diff_view<'a>(diff: &DiffState, palette: &Palette) -> Element<'a, Msg> {
    let content: Element<Msg> = match diff {
        DiffState::Empty => text("").into(),
        DiffState::Loading => text("Loading diff…")
            .size(12)
            .color(dim(palette.text, 0.6))
            .into(),
        DiffState::Error(e) => text(e.clone()).size(12).color(palette.danger).into(),
        DiffState::Loaded(d) if d.trim().is_empty() => text("No differences to show.")
            .size(12)
            .color(dim(palette.text, 0.6))
            .into(),
        DiffState::Loaded(d) => {
            let lines: Vec<Element<Msg>> = d
                .lines()
                .map(|line| {
                    text(line.to_string())
                        .font(Font::MONOSPACE)
                        .size(12)
                        .color(diff_line_color(line, palette))
                        .into()
                })
                .collect();
            scrollable(Column::with_children(lines).padding(10).width(Length::Fill))
                .height(Length::Fill)
                .into()
        }
    };

    // An inset, slightly darker well so the diff reads as a distinct region.
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(
                Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.18,
                }
                .into(),
            ),
            border: Border {
                radius: 8.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

// ── Small view helpers ───────────────────────────────────────────────

/// A filled accent button. With `on_press == None` the button renders as
/// disabled (iced treats a missing handler as non-interactive), which is how we
/// grey out actions while an operation is running.
fn action_button<'a>(label: &str, accent: Color, on_press: Option<Msg>) -> Element<'a, Msg> {
    let mut b = button(text(label.to_string()).size(13))
        .padding([8, 14])
        .style(move |_theme, status| {
            let background = match status {
                button::Status::Hovered => scale(accent, 1.12),
                button::Status::Pressed => scale(accent, 0.85),
                button::Status::Disabled => dim(accent, 0.25),
                button::Status::Active => accent,
            };
            let text_color = match status {
                button::Status::Disabled => dim(accent, 0.5),
                // Dark text reads clearly on every accent in the palette.
                _ => color!(0x14161f),
            };
            button::Style {
                background: Some(background.into()),
                text_color,
                border: Border {
                    radius: 8.0.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        });
    if let Some(msg) = on_press {
        b = b.on_press(msg);
    }
    b.into()
}

/// The change that best describes an entry's actionable state: prefer the
/// second column (what `apply` would do) and fall back to the first.
fn primary_change(e: &Entry) -> Change {
    if e.actual_to_target != Change::None {
        e.actual_to_target
    } else {
        e.last_to_actual
    }
}

/// chezmoi's single-letter code for a change; a middle dot for "no change" so
/// the two-character badge stays aligned.
fn code_char(c: Change) -> char {
    match c {
        Change::None => '·',
        Change::Added => 'A',
        Change::Deleted => 'D',
        Change::Modified => 'M',
        Change::Run => 'R',
    }
}

fn change_color(c: Change, palette: &Palette) -> Color {
    match c {
        Change::Added => palette.success,
        Change::Deleted => palette.danger,
        Change::Modified | Change::Run => palette.primary,
        Change::None => dim(palette.text, 0.4),
    }
}

/// Colour a single diff line by its leading character, the way a terminal pager
/// would: additions green, removals red, hunk headers in the accent, file
/// headers dimmed, and context in normal (slightly muted) text.
fn diff_line_color(line: &str, palette: &Palette) -> Color {
    if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        dim(palette.text, 0.45)
    } else if line.starts_with("@@") {
        palette.primary
    } else if line.starts_with('+') {
        palette.success
    } else if line.starts_with('-') {
        palette.danger
    } else {
        dim(palette.text, 0.85)
    }
}

/// The same colour at a different opacity — used for tints, dim text, and
/// faint borders so the whole UI stays derived from the theme palette.
fn dim(c: Color, alpha: f32) -> Color {
    Color { a: alpha, ..c }
}

/// Multiply a colour's brightness, clamped — the basis for hover/pressed tints.
fn scale(c: Color, factor: f32) -> Color {
    Color {
        r: (c.r * factor).clamp(0.0, 1.0),
        g: (c.g * factor).clamp(0.0, 1.0),
        b: (c.b * factor).clamp(0.0, 1.0),
        a: c.a,
    }
}
