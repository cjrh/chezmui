//! Thin wrapper around the `chezmoi` command-line tool.
//!
//! The GUI never spawns `chezmoi` itself; every interaction goes through the
//! handful of functions in this module. Each function maps to one day-to-day
//! chezmoi operation, takes ordinary Rust values, and returns either parsed
//! results or a human-readable error message. All the awkward parts — building
//! argument lists, capturing stdout/stderr, suppressing the pager and colour
//! codes, and turning a non-zero exit status into an error — live here and
//! nowhere else.
//!
//! Every function blocks until `chezmoi` exits. Callers that must not block the
//! UI thread (i.e. all of them) run these inside an `iced` `Task`.
//!
//! ## The two directions
//!
//! chezmoi maintains a *source* state (the tracked copy of your dotfiles) and a
//! *destination* state (the real files in your home directory). The two
//! everyday operations move data in opposite directions, which is the single
//! most confusing thing about the tool:
//!
//! * [`apply`]  writes the source version *onto* the destination (source → home)
//! * [`re_add`] captures destination edits *back into* the source (home → source)
//!
//! Keeping the names straight here means the rest of the app — and the user —
//! can think in terms of "push managed version out" vs "pull my edits in".

use std::path::{Path, PathBuf};
use std::process::Command;

/// One axis of difference that chezmoi reports for a managed entry.
///
/// chezmoi prints a status with two columns (see `chezmoi status --help`); each
/// column holds one of these. The variants mirror chezmoi's own single-letter
/// codes so parsing is a direct mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// No difference on this axis (chezmoi prints a space).
    None,
    /// Entry was/will be created (`A`).
    Added,
    /// Entry was/will be deleted (`D`).
    Deleted,
    /// Entry was/will be modified (`M`).
    Modified,
    /// A script that will be run (`R`, second column only).
    Run,
}

impl Change {
    /// Map one of chezmoi's status characters to a `Change`. Unknown
    /// characters (there shouldn't be any) collapse to `None`, which keeps
    /// parsing total rather than fallible for a cosmetic field.
    fn from_char(c: char) -> Change {
        match c {
            'A' => Change::Added,
            'D' => Change::Deleted,
            'M' => Change::Modified,
            'R' => Change::Run,
            _ => Change::None,
        }
    }

    /// A short word for the UI. Empty for `None` so callers can show nothing.
    pub fn label(self) -> &'static str {
        match self {
            Change::None => "",
            Change::Added => "added",
            Change::Deleted => "deleted",
            Change::Modified => "modified",
            Change::Run => "run",
        }
    }
}

/// One managed path together with chezmoi's view of how it differs.
///
/// The two `Change` fields are kept separate because they drive which actions
/// make sense for the entry:
///
/// * `actual_to_target` (the second column) is what `apply` would change — the
///   gap between the file on disk and the managed target.
/// * `last_to_actual` (the first column) is how the file has drifted since
///   chezmoi last wrote it — a candidate for `re-add`.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Absolute path in the destination directory, e.g. `/home/you/.bashrc`.
    /// This is what gets passed back to per-target chezmoi commands.
    pub path: PathBuf,
    /// `path` with the home directory collapsed to `~`, for display.
    pub display: String,
    /// First column: how the actual state drifted from what chezmoi last wrote.
    pub last_to_actual: Change,
    /// Second column: how the actual state differs from the target — i.e. what
    /// `apply` will do.
    pub actual_to_target: Change,
}

/// Fetch the status of every managed entry that currently differs.
///
/// Equivalent to `chezmoi status`, but with absolute paths so each entry can be
/// fed straight back into [`apply`], [`re_add`], [`diff`], or [`forget`]. The
/// returned list is in chezmoi's own order (lexicographic by path); an empty
/// list means everything is already in sync.
pub fn status() -> Result<Vec<Entry>, String> {
    let out = run(&["status", "--path-style", "absolute"])?;
    Ok(out.lines().filter_map(parse_status_line).collect())
}

/// Parse one line of `chezmoi status --path-style absolute`.
///
/// The format is two status characters, a space, then the path:
/// `MM /home/you/.bashrc`. Returning `Option` (rather than erroring) lets a
/// stray blank or malformed line be skipped instead of failing the whole list.
fn parse_status_line(line: &str) -> Option<Entry> {
    // Need at least "XY p": two codes, the separating space, and one path char.
    if line.len() < 4 {
        return None;
    }
    let bytes = line.as_bytes();
    let first = Change::from_char(bytes[0] as char);
    let second = Change::from_char(bytes[1] as char);
    // The path is everything after the two codes and their trailing space.
    // Byte index 3 is always a char boundary because indices 0..3 are ASCII.
    let path = PathBuf::from(line[3..].to_string());
    Some(Entry {
        display: collapse_home(&path),
        path,
        last_to_actual: first,
        actual_to_target: second,
    })
}

/// Render a path for display with the home directory shown as `~`.
fn collapse_home(path: &Path) -> String {
    let text = path.to_string_lossy();
    match home_dir() {
        Some(home) => match text.strip_prefix(&*home.to_string_lossy()) {
            Some(rest) => format!("~{rest}"),
            None => text.into_owned(),
        },
        None => text.into_owned(),
    }
}

/// The unified diff between the target and destination state.
///
/// Pass `Some(path)` for a single entry or `None` for the whole tree. The
/// output is plain (no pager, no colour) so it can be rendered directly. This
/// is the same text `chezmoi diff` would print to a terminal.
pub fn diff(target: Option<&Path>) -> Result<String, String> {
    let mut args = vec!["diff", "--no-pager", "--color=false"];
    let target_str;
    if let Some(path) = target {
        target_str = path.to_string_lossy().into_owned();
        args.push(&target_str);
    }
    run(&args)
}

/// Write the managed (source) version onto the destination: **source → home**.
///
/// With no targets, applies everything; otherwise only the given paths. Runs
/// with `--force` so it never blocks waiting for a terminal confirmation — the
/// GUI is the place the user already confirmed.
pub fn apply(targets: &[PathBuf]) -> Result<(), String> {
    run_targets("apply", targets).map(drop)
}

/// Capture destination edits back into the source state: **home → source**.
///
/// The inverse of [`apply`]. With no targets, re-adds every modified file.
pub fn re_add(targets: &[PathBuf]) -> Result<(), String> {
    run_targets("re-add", targets).map(drop)
}

/// Start managing one or more existing files (`chezmoi add`).
pub fn add(targets: &[PathBuf]) -> Result<(), String> {
    if targets.is_empty() {
        return Err("nothing selected to add".into());
    }
    run_targets("add", targets).map(drop)
}

/// Stop managing a path, removing it from the source state (`chezmoi forget`).
///
/// This does not touch the real file in the home directory, only chezmoi's
/// tracked copy.
pub fn forget(target: &Path) -> Result<(), String> {
    let path = target.to_string_lossy();
    run(&["forget", "--force", &path]).map(drop)
}

/// Pull the latest source state from the remote and apply it (`chezmoi update`).
///
/// Returns chezmoi's output so the UI can show what changed.
pub fn update() -> Result<String, String> {
    run(&["update", "--force"])
}

/// Run a verb that takes an optional list of target paths, with `--force`.
fn run_targets(verb: &str, targets: &[PathBuf]) -> Result<String, String> {
    let mut args = vec![verb.to_string(), "--force".to_string()];
    args.extend(targets.iter().map(|p| p.to_string_lossy().into_owned()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs)
}

/// Invoke `chezmoi` with `args`, returning stdout on success.
///
/// This is the one place a subprocess is actually spawned. A failure to launch
/// the binary at all, or any non-zero exit, becomes an `Err` carrying the most
/// useful message available (chezmoi writes errors to stderr). `--no-tty`
/// guarantees chezmoi never tries to grab a terminal we don't have.
fn run(args: &[&str]) -> Result<String, String> {
    let output = Command::new("chezmoi")
        .arg("--no-tty")
        .args(args)
        .output()
        .map_err(|e| format!("could not run chezmoi: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            Err(format!("chezmoi exited with {}", output.status))
        } else {
            Err(msg.to_string())
        }
    }
}

/// `$HOME` as a path, if set. Used only to prettify paths for display.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
