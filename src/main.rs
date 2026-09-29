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
//! the per-file actions on the right. A separate dialog commits and pushes
//! selected files in the chezmoi source repository.

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    Column, button, checkbox, column, container, row, scrollable, text, text_input,
};
use iced::{
    Alignment, Border, Color, Element, Font, Length, Size, Subscription, Task, Theme, color,
    theme::Palette, window,
};
use iced_themer::{ThemeConfig, Themed};

mod chezmoi;
use chezmoi::{Change, Entry, GitEntry, GitStatus};

/// The theme is baked into the binary so the app is styled regardless of the
/// directory it is launched from.
const THEME_TOML: &str = include_str!("../theme.toml");

/// Font sizes kept in one place so the whole UI scales together. These sit
/// above iced's defaults for comfortable reading; adjust here to re-scale.
const SIZE_TITLE: f32 = 30.0;
const SIZE_HEADING: f32 = 18.0;
const SIZE_BODY: f32 = 16.0;
const SIZE_SMALL: f32 = 14.0;
const SIZE_CAPTION: f32 = 13.0;

fn main() -> iced::Result {
    let config = Arc::new(
        THEME_TOML
            .parse::<ThemeConfig>()
            .expect("embedded theme.toml should parse"),
    );

    let font = config.font();
    let boot_cfg = Arc::clone(&config);
    let theme_cfg = Arc::clone(&config);

    let app = iced::daemon(move || boot(Arc::clone(&boot_cfg)), update, view)
        .title(|state: &State, id| {
            if state.git_window == Some(id) {
                "Commit chezmoi changes".into()
            } else {
                "chezmui".into()
            }
        })
        .theme(move |_state: &State, _id| theme_cfg.theme())
        .subscription(|_state| {
            Subscription::batch([
                window::close_events().map(Msg::WindowClosed),
                window::close_requests().map(Msg::WindowCloseRequested),
            ])
        });

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
        // Give the toplevel a stable Wayland app_id so the compositor and dock
        // identify the window (icon, name) and can parent the file-chooser
        // portal dialog to it instead of listing it as a stray window.
        platform_specific: window::settings::PlatformSpecific {
            application_id: "chezmui".to_string(),
            ..Default::default()
        },
        ..window::Settings::default()
    }
}

fn git_window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(1100.0, 740.0),
        min_size: Some(Size::new(740.0, 480.0)),
        position: window::Position::Centered,
        // Closing is handled in update so work cannot be interrupted midway.
        exit_on_close_request: false,
        platform_specific: window::settings::PlatformSpecific {
            application_id: "chezmui".to_string(),
            ..Default::default()
        },
        ..window::Settings::default()
    }
}

// ── State ────────────────────────────────────────────────────────────

struct State {
    /// The loaded theme, kept so `view` can read the palette and style panels.
    theme: Arc<ThemeConfig>,
    main_window: window::Id,
    git_window: Option<window::Id>,
    /// Every managed path that currently differs (from `chezmoi status`).
    entries: Vec<Entry>,
    /// Index into `entries` of the file whose diff is shown, if any.
    selected: Option<usize>,
    /// The diff for the selected entry.
    diff: DiffState,
    /// The most recent operation result, shown as a coloured banner.
    banner: Option<Banner>,
    /// True while a command or file picker is running; disables actions so
    /// the user cannot start overlapping operations.
    busy: bool,
    /// State of the separate Git window, if open.
    git_dialog: Option<GitDialog>,
}

struct GitDialog {
    source: Option<PathBuf>,
    entries: Vec<GitEntry>,
    selected: HashSet<PathBuf>,
    message: String,
    feedback: Option<Banner>,
    preview: DiffState,
    preview_version: u64,
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
    OpenGit,
    CloseGit,
    GitLoaded(Result<GitStatus, String>),
    ToggleGit(PathBuf, bool),
    GitMessageChanged(String),
    RefreshGitPreview,
    GitPreviewLoaded(window::Id, u64, Result<String, String>),
    CommitGit,
    PushAndClose,
    GitDone(GitAction, Result<String, String>),
    WindowCloseRequested(window::Id),
    WindowClosed(window::Id),
}

#[derive(Debug, Clone, Copy)]
enum GitAction {
    Commit,
    PushAndClose,
}

fn boot(theme: Arc<ThemeConfig>) -> (State, Task<Msg>) {
    let (main_window, open) = window::open(window_settings());
    let state = State {
        theme,
        main_window,
        git_window: None,
        entries: Vec::new(),
        selected: None,
        diff: DiffState::Empty,
        banner: None,
        busy: true,
        git_dialog: None,
    };
    (
        state,
        Task::batch([
            open.discard(),
            Task::perform(async { chezmoi::status() }, Msg::StatusLoaded),
        ]),
    )
}

// ── Update ───────────────────────────────────────────────────────────

fn update(state: &mut State, msg: Msg) -> Task<Msg> {
    // The main window stays visible behind Git, but its controls are inactive.
    if state.git_window.is_some()
        && matches!(
            msg,
            Msg::Refresh
                | Msg::Select(_)
                | Msg::ApplyAll
                | Msg::ApplyOne
                | Msg::ReAddOne
                | Msg::ForgetOne
                | Msg::Update
                | Msg::PickFiles
                | Msg::FilesPicked(_)
        )
    {
        return Task::none();
    }
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

        Msg::PickFiles => {
            state.busy = true;
            Task::perform(pick_files(), Msg::FilesPicked)
        }
        Msg::FilesPicked(paths) => {
            if paths.is_empty() {
                state.busy = false;
                return Task::none();
            }
            start_op(state, "Add", async move {
                chezmoi::add(&paths).map(|_| "Added to chezmoi.".to_string())
            })
        }

        Msg::OpDone(label, result) => {
            state.busy = true;
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

        Msg::OpenGit if !state.busy && state.git_window.is_none() => {
            let (id, open) = window::open(git_window_settings());
            state.git_window = Some(id);
            state.busy = true;
            state.git_dialog = Some(GitDialog {
                source: None,
                entries: Vec::new(),
                selected: HashSet::new(),
                message: String::new(),
                feedback: None,
                preview: DiffState::Empty,
                preview_version: 0,
            });
            Task::batch([
                open.then(window::gain_focus),
                Task::perform(async { chezmoi::git_status() }, Msg::GitLoaded),
            ])
        }
        Msg::CloseGit if !state.busy => close_git(state),
        Msg::WindowCloseRequested(id) if state.git_window == Some(id) && !state.busy => {
            close_git(state)
        }
        Msg::WindowClosed(id) if id == state.main_window => iced::exit(),
        Msg::WindowClosed(id) if state.git_window == Some(id) => {
            state.git_window = None;
            state.git_dialog = None;
            // A compositor can close a window even while Git is working. Keep
            // busy until that task finishes, then report its result in main.
            Task::none()
        }
        Msg::GitLoaded(result) => {
            state.busy = false;
            if let Some(dialog) = &mut state.git_dialog {
                match result {
                    Ok(status) => {
                        dialog.source = Some(status.source);
                        dialog.entries = status.entries;
                        dialog
                            .selected
                            .retain(|path| dialog.entries.iter().any(|e| &e.path == path));
                    }
                    Err(e) => {
                        dialog.source = None;
                        dialog.entries.clear();
                        dialog.selected.clear();
                        dialog.preview = DiffState::Empty;
                        dialog.feedback = Some(Banner {
                            level: Level::Error,
                            message: format!("Couldn't read Git changes: {e}"),
                        });
                    }
                }
                if !dialog.selected.is_empty() {
                    return load_git_preview(state.git_window.unwrap(), dialog);
                }
            }
            Task::none()
        }
        Msg::ToggleGit(path, checked) if !state.busy => {
            let Some(dialog) = &mut state.git_dialog else {
                return Task::none();
            };
            if !dialog.entries.iter().any(|e| e.path == path) {
                return Task::none();
            }
            if checked {
                dialog.selected.insert(path);
            } else {
                dialog.selected.remove(&path);
            }
            load_git_preview(state.git_window.unwrap(), dialog)
        }
        Msg::RefreshGitPreview if !state.busy => match (state.git_window, &mut state.git_dialog) {
            (Some(id), Some(dialog)) => load_git_preview(id, dialog),
            _ => Task::none(),
        },
        Msg::GitPreviewLoaded(id, version, result) => {
            if state.git_window == Some(id)
                && let Some(dialog) = &mut state.git_dialog
                && dialog.preview_version == version
            {
                dialog.preview = match result {
                    Ok(diff) => DiffState::Loaded(diff),
                    Err(e) => DiffState::Error(e),
                };
            }
            Task::none()
        }
        Msg::GitMessageChanged(message) if !state.busy => {
            if let Some(dialog) = &mut state.git_dialog {
                dialog.message = message;
            }
            Task::none()
        }
        Msg::CommitGit if !state.busy => {
            let Some(dialog) = &mut state.git_dialog else {
                return Task::none();
            };
            if dialog.selected.is_empty()
                || dialog.message.trim().is_empty()
                || !matches!(&dialog.preview, DiffState::Loaded(diff) if !diff.trim().is_empty())
                || dialog.source.is_none()
            {
                return Task::none();
            }
            let paths: Vec<_> = dialog.selected.iter().cloned().collect();
            let message = dialog.message.clone();
            dialog.preview_version += 1;
            dialog.feedback = None;
            state.busy = true;
            Task::perform(
                async move { chezmoi::commit_git(&paths, &message) },
                |result| Msg::GitDone(GitAction::Commit, result),
            )
        }
        Msg::PushAndClose if !state.busy => {
            let Some(dialog) = &mut state.git_dialog else {
                return Task::none();
            };
            if dialog.source.is_none() || !dialog.selected.is_empty() {
                return Task::none();
            }
            state.busy = true;
            dialog.feedback = None;
            Task::perform(async { chezmoi::push_git() }, |result| {
                Msg::GitDone(GitAction::PushAndClose, result)
            })
        }
        Msg::GitDone(GitAction::Commit, result) => {
            if let Some(dialog) = &mut state.git_dialog {
                if result.is_ok() {
                    dialog.message.clear();
                    dialog.selected.clear();
                    dialog.preview = DiffState::Empty;
                }
                dialog.feedback = Some(git_feedback(result));
                // Staging may have changed the index even if the commit failed.
                Task::perform(async { chezmoi::git_status() }, Msg::GitLoaded)
            } else {
                state.busy = false;
                state.banner = Some(git_feedback(result));
                Task::none()
            }
        }
        Msg::GitDone(GitAction::PushAndClose, result) => {
            state.busy = false;
            match result {
                Ok(message) => {
                    state.banner = Some(Banner {
                        level: Level::Success,
                        message,
                    });
                    close_git(state)
                }
                Err(e) => {
                    if let Some(dialog) = &mut state.git_dialog {
                        dialog.feedback = Some(Banner {
                            level: Level::Error,
                            message: e,
                        });
                    } else {
                        state.banner = Some(Banner {
                            level: Level::Error,
                            message: e,
                        });
                    }
                    Task::none()
                }
            }
        }
        _ => Task::none(),
    }
}

fn close_git(state: &mut State) -> Task<Msg> {
    let Some(id) = state.git_window.take() else {
        return Task::none();
    };
    state.git_dialog = None;
    state.busy = false;
    window::close(id)
}

fn load_git_preview(id: window::Id, dialog: &mut GitDialog) -> Task<Msg> {
    dialog.preview_version += 1;
    if dialog.selected.is_empty() {
        dialog.preview = DiffState::Empty;
        return Task::none();
    }
    let version = dialog.preview_version;
    let paths: Vec<_> = dialog.selected.iter().cloned().collect();
    dialog.preview = DiffState::Loading;
    Task::perform(async move { chezmoi::git_preview(&paths) }, move |result| {
        Msg::GitPreviewLoaded(id, version, result)
    })
}

fn git_feedback(result: Result<String, String>) -> Banner {
    match result {
        Ok(message) => Banner {
            level: Level::Success,
            message,
        },
        Err(message) => Banner {
            level: Level::Error,
            message,
        },
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
        Some(handles) => handles
            .into_iter()
            .map(|h| h.path().to_path_buf())
            .collect(),
        None => Vec::new(),
    }
}

// ── View ─────────────────────────────────────────────────────────────

fn view(state: &State, id: window::Id) -> Element<'_, Msg> {
    let palette = state.theme.theme().palette();
    if state.git_window == Some(id) {
        return state.git_dialog.as_ref().map_or_else(
            || text("").into(),
            |dialog| git_dialog_view(state, dialog, &palette),
        );
    }

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
    let working = if state.busy {
        text("working…").size(SIZE_SMALL).color(palette.primary)
    } else {
        text("")
    };
    let titles = column![
        text("chezmui").size(SIZE_TITLE).color(palette.text),
        text("chezmoi, without the typing")
            .size(SIZE_SMALL)
            .color(dim(palette.text, 0.5)),
        working,
    ]
    .spacing(2)
    .width(Length::Fill);

    let idle = !state.busy && state.git_window.is_none();
    let remote_actions = column![
        text("REMOTE / GIT")
            .size(SIZE_CAPTION)
            .color(dim(palette.warning, 0.8)),
        row![
            action_button(
                "Update (pull)",
                palette.warning,
                idle.then_some(Msg::Update)
            ),
            action_button(
                "Commit & push…",
                palette.warning,
                idle.then_some(Msg::OpenGit)
            ),
        ]
        .spacing(10),
    ]
    .spacing(5);
    let local_actions = row![
        action_button("Refresh", palette.primary, idle.then_some(Msg::Refresh)),
        action_button(
            "Apply all →",
            palette.success,
            (idle && !state.entries.is_empty()).then_some(Msg::ApplyAll),
        ),
        action_button(
            "Add files…",
            palette.primary,
            idle.then_some(Msg::PickFiles)
        ),
    ]
    .spacing(10);

    column![
        row![titles, remote_actions]
            .spacing(16)
            .align_y(Alignment::Center),
        local_actions,
    ]
    .spacing(10)
    .into()
}

/// The separate Git window keeps the changed paths and their selected diff
/// visible while writing a commit message.
fn git_dialog_view<'a>(
    state: &'a State,
    dialog: &'a GitDialog,
    palette: &Palette,
) -> Element<'a, Msg> {
    let busy = state.busy;
    let heading = row![
        text("Commit chezmoi changes")
            .size(SIZE_HEADING)
            .color(palette.text)
            .width(Length::Fill),
        text(if busy { "Working…" } else { "" })
            .size(SIZE_SMALL)
            .color(palette.warning),
        action_button("Close", palette.primary, (!busy).then_some(Msg::CloseGit)),
    ]
    .align_y(Alignment::Center);

    let source = dialog.source.as_ref().map_or_else(
        || "Finding chezmoi's Git repo…".into(),
        |path| format!("Repository: {}", path.display()),
    );
    let mut content = column![
        heading,
        text(source).size(SIZE_SMALL).color(dim(palette.text, 0.6)),
        text("Select the files to commit. Other staged files will not be committed.")
            .size(SIZE_SMALL)
            .color(dim(palette.text, 0.6)),
    ]
    .spacing(12);

    if let Some(feedback) = &dialog.feedback {
        content = content.push(banner_view(feedback, palette));
    }

    let files: Element<Msg> = if dialog.source.is_none() {
        text(if busy {
            "Loading Git changes…"
        } else {
            "Git changes unavailable."
        })
        .size(SIZE_BODY)
        .into()
    } else if dialog.entries.is_empty() {
        text("No changed files in the chezmoi Git repo.")
            .size(SIZE_BODY)
            .color(dim(palette.text, 0.6))
            .into()
    } else {
        let items: Vec<Element<Msg>> = dialog
            .entries
            .iter()
            .map(|entry| {
                let path = entry.path.clone();
                let selected = dialog.selected.contains(&path);
                let label = format!("{}  {}", entry.status, entry.display);
                checkbox(selected)
                    .label(label)
                    .size(20)
                    .text_size(SIZE_BODY)
                    .on_toggle(move |checked| Msg::ToggleGit(path.clone(), checked))
                    .into()
            })
            .collect();
        scrollable(Column::with_children(items).spacing(8).padding(8))
            .height(Length::Fill)
            .into()
    };

    let list = column![
        text(format!("Changed files ({})", dialog.entries.len()))
            .size(SIZE_BODY)
            .color(palette.text),
        inset_panel(files),
    ]
    .spacing(8)
    .width(Length::FillPortion(2))
    .height(Length::Fill);

    let preview_content: Element<Msg> = if dialog.selected.is_empty() {
        text("Select files to preview the commit.")
            .size(SIZE_SMALL)
            .color(dim(palette.text, 0.6))
            .into()
    } else {
        diff_view(&dialog.preview, palette)
    };
    let preview = column![
        row![
            text("Selected changes")
                .size(SIZE_BODY)
                .color(palette.text)
                .width(Length::Fill),
            action_button(
                "Refresh diff",
                palette.primary,
                (!busy && !dialog.selected.is_empty()).then_some(Msg::RefreshGitPreview),
            ),
        ]
        .align_y(Alignment::Center),
        inset_panel(preview_content),
    ]
    .spacing(8)
    .width(Length::FillPortion(3))
    .height(Length::Fill);
    content = content.push(row![list, preview].spacing(14).height(Length::Fill));

    let can_commit = !busy
        && dialog.source.is_some()
        && !dialog.selected.is_empty()
        && !dialog.message.trim().is_empty()
        && matches!(&dialog.preview, DiffState::Loaded(diff) if !diff.trim().is_empty());
    content = content
        .push(text(format!("Selected: {} · Push & Close sends commits only; it does not commit selected files.", dialog.selected.len())).size(SIZE_SMALL))
        .push(text_input("Commit message", &dialog.message)
            .on_input(Msg::GitMessageChanged)
            .on_submit(Msg::CommitGit)
            .size(SIZE_BODY)
            .padding(10))
        .push(row![
            action_button("Commit", palette.warning, can_commit.then_some(Msg::CommitGit)),
            action_button("Push & Close", palette.warning,
                (!busy && dialog.source.is_some() && dialog.selected.is_empty())
                    .then_some(Msg::PushAndClose)),
        ].spacing(10));

    container(content)
        .padding(18)
        .width(Length::Fill)
        .height(Length::Fill)
        .themed(state.theme.container())
        .into()
}

/// Dark well used for the Git file list and the preview.
fn inset_panel<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    container(content)
        .padding(10)
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

/// A short coloured strip reporting the last operation's outcome.
fn banner_view<'a>(banner: &Banner, palette: &Palette) -> Element<'a, Msg> {
    let accent = match banner.level {
        Level::Success => palette.success,
        Level::Error => palette.danger,
    };
    container(
        text(banner.message.clone())
            .size(SIZE_SMALL)
            .color(palette.text),
    )
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
                    .size(SIZE_HEADING)
                    .color(palette.success),
                text("No managed files differ from your home directory.")
                    .size(SIZE_SMALL)
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
            .map(|(i, e)| {
                entry_row(
                    e,
                    i,
                    state.selected == Some(i),
                    state.git_window.is_none(),
                    palette,
                )
            })
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
        .size(SIZE_BODY)
        .color(dim(palette.text, 0.6));

    container(column![heading, inner].spacing(10).height(Length::Fill))
        .padding(14)
        .width(Length::FillPortion(2))
        .height(Length::Fill)
        .themed(state.theme.container())
        .into()
}

/// One clickable file row: a two-character status badge and the path.
fn entry_row<'a>(
    entry: &Entry,
    index: usize,
    selected: bool,
    enabled: bool,
    palette: &Palette,
) -> Element<'a, Msg> {
    let code = format!(
        "{}{}",
        code_char(entry.last_to_actual),
        code_char(entry.actual_to_target),
    );
    let badge = text(code)
        .font(Font::MONOSPACE)
        .size(SIZE_BODY)
        .color(change_color(primary_change(entry), palette));
    let path = text(entry.display.clone())
        .size(SIZE_BODY)
        .color(palette.text);

    let content = row![badge, path]
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let primary = palette.primary;
    let text_color = palette.text;
    let mut row = button(content);
    if enabled {
        row = row.on_press(Msg::Select(index));
    }
    row.width(Length::Fill)
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
                .size(SIZE_BODY)
                .color(dim(palette.text, 0.5)),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into(),
        Some(i) => {
            let entry = &state.entries[i];
            let idle = !state.busy && state.git_window.is_none();

            let change = primary_change(entry);
            let title = row![
                text(entry.display.clone())
                    .size(SIZE_HEADING)
                    .color(palette.text),
                text(change.label())
                    .size(SIZE_SMALL)
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
            .size(SIZE_CAPTION)
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
            .size(SIZE_SMALL)
            .color(dim(palette.text, 0.6))
            .into(),
        DiffState::Error(e) => text(e.clone())
            .size(SIZE_SMALL)
            .color(palette.danger)
            .into(),
        DiffState::Loaded(d) if d.trim().is_empty() => text("No differences to show.")
            .size(SIZE_SMALL)
            .color(dim(palette.text, 0.6))
            .into(),
        DiffState::Loaded(d) => {
            let lines: Vec<Element<Msg>> = d
                .lines()
                .map(|line| {
                    text(line.to_string())
                        .font(Font::MONOSPACE)
                        .size(SIZE_SMALL)
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
    let mut b = button(text(label.to_string()).size(SIZE_BODY))
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
