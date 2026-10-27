//! Selective Git management for previously committed ledger documents.
use std::path::Path;
use std::process::{Command, Output};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// Not a git repository.
    NoGit,
    /// No upstream (or no commits yet); nothing to compare against.
    NoUpstream,
    /// Fetch failed; continuing with the local copy.
    Offline,
    UpToDate,
    FastForwarded,
    /// Local commits not yet on the remote.
    LocalAhead,
    /// Local and remote histories diverged; manual reconciliation is required.
    Diverged,
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git").arg("-C").arg(dir).arg("--literal-pathspecs").args(args).output().map_err(|e| format!("cannot run git: {e}"))
}

/// Run git, failing with its stderr; returns trimmed stdout.
fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(dir, args)?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn rev(dir: &Path, what: &str) -> Option<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", what])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--git-dir"]).map(|o| o.status.success()).unwrap_or(false)
}

/// Only accounts inside this working tree and this working directory participate
/// in its Git operations. Resolve symlinks before checking the boundary.
pub fn account_in_tree(dir: &Path, file: &str) -> bool {
    let (Ok(dir), Ok(file)) = (dir.canonicalize(), Path::new(file).canonicalize()) else { return false };
    if !file.starts_with(&dir) {
        return false;
    }
    let Some(parent) = file.parent() else { return false };
    let root = |p: &Path| git_ok(p, &["rev-parse", "--show-toplevel"])
        .ok().and_then(|s| Path::new(&s).canonicalize().ok());
    match (root(&dir), root(parent)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `git config mdl.autocommit true` in the account repository: every save commits
/// eligible files locally. Automatic pushing is separately opt-in.
pub fn autocommit(dir: &Path) -> bool {
    git_ok(dir, &["config", "--get", "--type=bool", "mdl.autocommit"]).is_ok_and(|v| v == "true")
}

pub fn autopush(dir: &Path) -> bool {
    git_ok(dir, &["config", "--get", "--type=bool", "mdl.autopush"]).is_ok_and(|v| v == "true")
}

fn ensure_idle(dir: &Path) -> Result<(), String> {
    for state in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply", "sequencer"] {
        let path = git_ok(dir, &["rev-parse", "--git-path", state])?;
        if dir.join(path).exists() {
            return Err("Git operation in progress; finish it manually before syncing".into());
        }
    }
    Ok(())
}

/// Fetch and classify local vs upstream. Fast-forwards when safe.
pub fn sync(dir: &Path) -> Result<State, String> {
    if !is_repo(dir) {
        return Ok(State::NoGit);
    }
    ensure_idle(dir)?;
    let fetched = git(dir, &["fetch", "--quiet"]).map(|o| o.status.success()).unwrap_or(false);
    let (local, remote) = match (rev(dir, "HEAD"), rev(dir, "@{u}")) {
        (Some(l), Some(r)) => (l, r),
        _ => return Ok(State::NoUpstream),
    };
    if !fetched {
        return Ok(State::Offline);
    }
    if local == remote {
        return Ok(State::UpToDate);
    }
    let base = git_ok(dir, &["merge-base", "HEAD", "@{u}"])?;
    if base == remote {
        return Ok(State::LocalAhead);
    }
    if base == local {
        git_ok(dir, &["merge", "--ff-only", "--quiet", "@{u}"])?;
        return Ok(State::FastForwarded);
    }
    Ok(State::Diverged)
}

/// The read half: fetch and fast-forward only. Never
/// pushes. Returns whether files changed, and a one-line note.
pub fn pull(dir: &Path) -> Result<(bool, &'static str), String> {
    Ok(match sync(dir)? {
        State::NoGit => return Err("not a git repository (git init)".into()),
        State::Offline => (false, "fetch failed; working offline"),
        State::NoUpstream => (false, "no upstream branch yet"),
        State::UpToDate => (false, "up to date"),
        State::FastForwarded => (true, "fast-forwarded to the upstream"),
        State::LocalAhead => (false, "local commits not yet pushed"),
        State::Diverged => return Err("local and upstream histories diverged; merge or rebase manually".into()),
    })
}

/// Time since the last successful fetch, from the `FETCH_HEAD` git rewrites on each
/// one; None when there has never been one (or it cannot be told).
pub fn fetched_ago(dir: &Path) -> Option<std::time::Duration> {
    let git_dir = git_ok(dir, &["rev-parse", "--absolute-git-dir"]).ok()?;
    let modified = std::fs::metadata(Path::new(&git_dir).join("FETCH_HEAD")).ok()?.modified().ok()?;
    modified.elapsed().ok()
}

pub fn has_upstream(dir: &Path) -> bool {
    rev(dir, "@{u}").is_some()
}

/// Commits on HEAD the upstream does not have.
pub fn unpushed(dir: &Path) -> u32 {
    if !has_upstream(dir) {
        return 0;
    }
    git_ok(dir, &["rev-list", "--count", "@{u}..HEAD"]).ok().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Eligible modified files only. New files (including staged additions), deletes,
/// renames, conflicts, and files with both staged and unstaged edits stay untouched.
pub fn pending(dir: &Path) -> Result<Vec<String>, String> {
    let prefix = git_ok(dir, &["rev-parse", "--show-prefix"])?;
    let out = git(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=no", "--renames", "--", "."])?;
    if !out.status.success() {
        return Err(format!("git status failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut fields = out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut paths = Vec::new();
    while let Some(field) = fields.next() {
        if field.len() < 4 { continue; }
        let code = &field[..2];
        if code.contains(&b'R') || code.contains(&b'C') { fields.next(); }
        if code != b" M" && code != b"M " { continue; }
        let Ok(path) = std::str::from_utf8(&field[3..]) else { continue };
        let Some(relative) = path.strip_prefix(&prefix) else { continue };
        let file = dir.join(relative);
        if !std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) { continue; }
        if !account_in_tree(dir, &file.to_string_lossy()) { continue; }
        let tracked = git(dir, &["ls-tree", "-z", "HEAD", "--", relative])?;
        if !tracked.status.success() || !tracked.stdout.starts_with(b"100") { continue; }
        if !crate::ledger::contains_ledger(&file.to_string_lossy()) { continue; }
        paths.push(relative.to_string());
    }
    paths.sort();
    Ok(paths)
}

/// Commit exactly these eligible files. --only leaves unrelated staged changes
/// in the index, and avoids staging anything if the commit fails.
fn commit_paths(dir: &Path, paths: &[String], msg: &str) -> Result<bool, String> {
    ensure_idle(dir)?;
    let eligible = pending(dir)?;
    let paths: Vec<&str> = paths.iter().filter(|p| eligible.contains(p)).map(String::as_str).collect();
    if paths.is_empty() { return Ok(false); }
    let mut args = vec!["commit", "--only", "--quiet", "-m", msg, "--"];
    args.extend(paths);
    git_ok(dir, &args)?;
    Ok(true)
}

pub fn commit_all(dir: &Path, msg: &str) -> Result<bool, String> {
    commit_paths(dir, &pending(dir)?, msg)
}

/// Save-time commit of one eligible account; pushing requires mdl.autopush.
pub fn commit_push(dir: &Path, file: &str, msg: &str) -> Result<String, String> {
    let target = Path::new(file).canonicalize().map_err(|e| e.to_string())?;
    let paths: Vec<String> = pending(dir)?.into_iter().filter(|p|
        dir.join(p).canonicalize().is_ok_and(|p| p == target)).collect();
    if !commit_paths(dir, &paths, msg)? {
        return Ok("saved locally; no eligible Git changes".into());
    }
    if autopush(dir) { Ok(push(dir)) }
    else { Ok("committed locally; automatic push disabled".into()) }
}

pub fn push(dir: &Path) -> String {
    if let Err(e) = ensure_idle(dir) { return format!("push failed ({e})"); }
    let args: &[&str] = if rev(dir, "@{u}").is_some() {
        &["push", "--quiet"]
    } else if has_remote(dir) {
        // First push on a fresh branch/clone: create the upstream.
        &["push", "--quiet", "-u", "origin", "HEAD"]
    } else {
        return "no remote configured".into();
    };
    match git(dir, args) {
        Ok(o) if o.status.success() => "pushed".into(),
        Ok(o) => format!("push failed ({})", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("push failed ({e})"),
    }
}

pub fn has_remote(dir: &Path) -> bool {
    git(dir, &["remote"]).map(|o| o.status.success() && !o.stdout.is_empty()).unwrap_or(false)
}
