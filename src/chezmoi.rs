//! Thin wrapper around the `chezmoi` and `git` command-line tools.
//!
//! The GUI never spawns either tool itself; every interaction goes through the
//! functions in this module. Each function takes ordinary Rust values and returns parsed
//! results or a human-readable error message. Argument lists, stdout/stderr,
//! and non-zero exit statuses are handled here, not in the UI.
//!
//! Every function blocks until its command exits. Callers run these inside an
//! `iced` `Task` to keep the UI responsive.
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

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// A change in the Git repository that holds chezmoi's source files.
#[derive(Debug, Clone)]
pub struct GitEntry {
    /// A repository-relative path, suitable for `git add -- <path>`.
    pub path: PathBuf,
    pub display: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct GitStatus {
    pub source: PathBuf,
    pub entries: Vec<GitEntry>,
}

/// List changed, deleted, and untracked source files. Disable rename detection
/// so every record in the NUL-delimited output has exactly one path.
pub fn git_status() -> Result<GitStatus, String> {
    let source = git_source()?;
    let output = git(
        &source,
        &[
            "-c",
            "status.renames=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    let entries = parse_git_status(&output);
    Ok(GitStatus { source, entries })
}

fn parse_git_status(output: &[u8]) -> Vec<GitEntry> {
    output
        .split(|&byte| byte == 0)
        .filter_map(|line| {
            if line.len() < 4 || line[2] != b' ' {
                return None;
            }
            let path = PathBuf::from(OsString::from_vec(line[3..].to_vec()));
            Some(GitEntry {
                display: String::from_utf8_lossy(&line[3..])
                    .escape_debug()
                    .to_string(),
                path,
                status: String::from_utf8_lossy(&line[..2]).into_owned(),
            })
        })
        .collect()
}

/// Preview exactly the selected working-tree files without touching the real
/// Git index. A temporary index starts at HEAD, then stages only these paths.
/// This includes new files and staged changes, but excludes other staged files.
pub fn git_preview(paths: &[PathBuf]) -> Result<String, String> {
    let source = git_source()?;
    preview_from(&source, paths)
}

fn preview_from(source: &Path, paths: &[PathBuf]) -> Result<String, String> {
    if paths.is_empty() {
        return Ok(String::new());
    }
    let temp = tempfile::tempdir().map_err(|e| format!("could not create preview index: {e}"))?;
    let index = temp.path().join("index");
    let mut read_tree = Command::new("git");
    read_tree
        .current_dir(source)
        .env("GIT_INDEX_FILE", &index)
        .arg("read-tree");
    if git(source, &["rev-parse", "--verify", "HEAD"]).is_ok() {
        read_tree.arg("HEAD");
    } else {
        read_tree.arg("--empty");
    }
    git_output(read_tree)?;

    let mut add = Command::new("git");
    add.current_dir(source)
        .env("GIT_INDEX_FILE", &index)
        .arg("add")
        .arg("--")
        .args(paths);
    git_output(add)?;

    let mut diff = Command::new("git");
    diff.current_dir(source)
        .env("GIT_INDEX_FILE", &index)
        .args([
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--color=never",
        ]);
    let output = git_output(diff)?;
    let preview = String::from_utf8_lossy(&output);
    let mut lines = preview.lines();
    let limited = lines.by_ref().take(2500).collect::<Vec<_>>().join("\n");
    if lines.next().is_some() {
        Ok(format!("{limited}\n\n[Preview shortened to 2,500 lines.]"))
    } else {
        Ok(limited)
    }
}

/// Stage and commit only the chosen paths. Already-staged changes in other
/// files stay in the index for a later commit.
pub fn commit_git(paths: &[PathBuf], message: &str) -> Result<String, String> {
    if paths.is_empty() || message.trim().is_empty() {
        return Err("select files and enter a commit message".into());
    }
    let source = git_source()?;
    commit_from(&source, paths, message)?;
    Ok("Committed selected files. Continue or Push & Close.".into())
}

fn commit_from(source: &Path, paths: &[PathBuf], message: &str) -> Result<(), String> {
    let mut add = Command::new("git");
    add.current_dir(source).arg("add").arg("--").args(paths);
    git_output(add)?;

    let mut commit = Command::new("git");
    commit
        .current_dir(source)
        .arg("commit")
        .arg("--only")
        .arg("-m")
        .arg(message.trim())
        .arg("--")
        .args(paths);
    git_output(commit).map(drop)
}

/// Push existing commits via Git's configured upstream.
pub fn push_git() -> Result<String, String> {
    let source = git_source()?;
    push_from(&source).map(|_| "Push complete.".into())
}

fn push_from(source: &Path) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(source).arg("push");
    git_output(cmd).map(drop)
}

/// Resolve chezmoi's source directory, not the GUI's working directory. Do
/// not act on a parent Git repository: that could stage unrelated files.
fn git_source() -> Result<PathBuf, String> {
    let raw = run(&["source-path"])?;
    let raw = raw.trim_end_matches(['\r', '\n']);
    if raw.is_empty() {
        return Err("chezmoi returned an empty source directory".into());
    }
    let source = PathBuf::from(raw);
    let source = source
        .canonicalize()
        .map_err(|e| format!("could not find chezmoi source directory: {e}"))?;
    let mut cmd = Command::new("git");
    cmd.current_dir(&source)
        .args(["rev-parse", "--show-toplevel"]);
    let root =
        PathBuf::from(String::from_utf8_lossy(&git_output(cmd)?).trim_end_matches(['\r', '\n']));
    if root
        .canonicalize()
        .map_err(|e| format!("could not find Git root: {e}"))?
        != source
    {
        return Err("chezmoi source directory is not the Git repository root".into());
    }
    Ok(source)
}

fn git(source: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(source).args(args);
    git_output(cmd)
}

fn git_output(mut cmd: Command) -> Result<Vec<u8>, String> {
    // A background Task cannot answer Git's terminal prompts.
    let output = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    command_result(output)
}

fn command_result(output: Output) -> Result<Vec<u8>, String> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        let error = error.trim();
        if error.is_empty() {
            Err(format!("git exited with {}", output.status))
        } else {
            Err(error.into())
        }
    }
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
/// This is the one place a chezmoi subprocess is actually spawned. A failure to launch
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_delimited_git_paths() {
        let entries = parse_git_status(b" M path with spaces\0?? new\nfile\0D  deleted\0");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, " M");
        assert_eq!(entries[0].path, Path::new("path with spaces"));
        assert_eq!(entries[1].display, "new\\nfile");
        assert_eq!(entries[2].path, Path::new("deleted"));
    }

    #[test]
    fn previews_and_commits_selected_paths_then_pushes_separately() {
        let dir = std::env::temp_dir().join(format!(
            "chezmui-git-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = dir.join("source");
        let remote = dir.join("remote.git");
        std::fs::create_dir_all(&source).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "--bare"])
                .arg(&remote)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["init", "-q", "--initial-branch=main"])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        for args in [
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.org"],
            vec!["remote", "add", "origin", remote.to_str().unwrap()],
        ] {
            git(&source, &args).unwrap();
        }
        std::fs::write(source.join("chosen"), "base").unwrap();
        std::fs::write(source.join("other"), "base").unwrap();
        let initial = preview_from(&source, &[PathBuf::from("chosen")]).unwrap();
        assert!(initial.contains("+base"));
        assert!(!initial.contains("other"));
        assert!(
            git(&source, &["diff", "--cached", "--name-only"])
                .unwrap()
                .is_empty()
        );
        git(&source, &["add", "."]).unwrap();
        git(&source, &["commit", "-m", "base"]).unwrap();
        git(&source, &["push", "-u", "origin", "main"]).unwrap();

        std::fs::write(source.join("chosen"), "selected").unwrap();
        std::fs::write(source.join("other"), "staged but not selected").unwrap();
        git(&source, &["add", "other"]).unwrap();
        let preview = preview_from(&source, &[PathBuf::from("chosen")]).unwrap();
        assert!(preview.contains("+selected"));
        assert!(!preview.contains("staged but not selected"));
        assert_eq!(
            git(&source, &["diff", "--cached", "--name-only"]).unwrap(),
            b"other\n"
        );
        commit_from(&source, &[PathBuf::from("chosen")], "selected").unwrap();
        assert_eq!(
            git(&source, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            b"chosen\n"
        );
        assert_eq!(
            git(&source, &["diff", "--cached", "--name-only"]).unwrap(),
            b"other\n"
        );
        // The remote is unchanged until the user pushes.
        assert_eq!(
            git(&remote, &["log", "-1", "--format=%s"]).unwrap(),
            b"base\n"
        );

        std::fs::write(source.join("new file"), "new").unwrap();
        let preview = preview_from(&source, &[PathBuf::from("new file")]).unwrap();
        assert!(preview.contains("+new"));
        assert!(!preview.contains("staged but not selected"));
        commit_from(&source, &[PathBuf::from("new file")], "new file").unwrap();
        assert_eq!(
            git(&source, &["log", "-1", "--format=%s"]).unwrap(),
            b"new file\n"
        );
        assert_eq!(
            git(&remote, &["log", "-1", "--format=%s"]).unwrap(),
            b"base\n"
        );
        push_from(&source).unwrap();
        assert_eq!(
            git(&remote, &["log", "-1", "--format=%s"]).unwrap(),
            b"new file\n"
        );

        git(&source, &["remote", "remove", "origin"]).unwrap();
        assert!(push_from(&source).is_err());
        assert_eq!(
            git(&source, &["log", "-1", "--format=%s"]).unwrap(),
            b"new file\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
