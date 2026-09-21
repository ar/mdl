//! git for the account tree: fetch, fast-forward, merge, commit, push. Ported from
//! matter's gitsync (itself from pgpv), minus anyhow. Accounts are plain text, so git
//! merges them itself: a conflict is an ordinary text conflict fixed by hand, and the
//! stale running balances a merge leaves behind are what `recalc` is for. Everything
//! is scoped to `dir` (`-- .`) so an account tree inside a larger repository leaves
//! the rest of it alone.
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
    /// Local and remote histories diverged; the caller merges.
    Diverged,
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("cannot run git: {e}"))
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

/// `git config mdl.autocommit true` in the account repository: every save commits
/// the file and pushes best-effort.
pub fn autocommit(dir: &Path) -> bool {
    git_ok(dir, &["config", "--get", "--type=bool", "mdl.autocommit"]).is_ok_and(|v| v == "true")
}

/// Fetch and classify local vs upstream. Fast-forwards when safe.
pub fn sync(dir: &Path) -> Result<State, String> {
    if !is_repo(dir) {
        return Ok(State::NoGit);
    }
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

/// The read half: fetch, fast-forward, and merge when the histories diverged. Never
/// pushes. Returns whether files changed, and a one-line note.
pub fn pull(dir: &Path) -> Result<(bool, &'static str), String> {
    Ok(match sync(dir)? {
        State::NoGit => return Err("not a git repository (git init)".into()),
        State::Offline => (false, "fetch failed; working offline"),
        State::NoUpstream => (false, "no upstream branch yet"),
        State::UpToDate => (false, "up to date"),
        State::FastForwarded => (true, "fast-forwarded to the upstream"),
        State::LocalAhead => (false, "local commits not yet pushed"),
        State::Diverged => {
            merge(dir)?;
            (true, "merged the upstream")
        }
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

/// Everything a commit would pick up: modified, deleted and untracked paths under
/// `dir`, relative to it.
pub fn pending(dir: &Path) -> Result<Vec<String>, String> {
    let prefix = git_ok(dir, &["rev-parse", "--show-prefix"])?;
    let out = git(dir, &["status", "--porcelain", "-z", "--untracked-files=all", "--", "."])?;
    if !out.status.success() {
        return Err(format!("git status failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut fields = out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut paths = Vec::new();
    while let Some(field) = fields.next() {
        // "XY path", and for a rename or a copy the source path follows in a field of
        // its own, which is not a change in itself. Paths are repository-relative.
        let entry = String::from_utf8_lossy(field).into_owned();
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        if code.starts_with('R') || code.starts_with('C') {
            fields.next();
        }
        paths.push(path.strip_prefix(&prefix).unwrap_or(path).to_string());
    }
    paths.sort();
    Ok(paths)
}

/// Stage everything under `dir` and commit it. `Ok(false)`: nothing had changed.
pub fn commit_all(dir: &Path, msg: &str) -> Result<bool, String> {
    git_ok(dir, &["add", "-A", "--", "."])?;
    if git(dir, &["diff", "--cached", "--quiet", "--", "."])?.status.success() {
        return Ok(false);
    }
    git_ok(dir, &["commit", "--quiet", "-m", msg])?;
    Ok(true)
}

/// Merge the upstream. Conflicts are an error listing the files; the user resolves
/// them by hand (`mdl <file> lint` catches leftovers).
pub fn merge(dir: &Path) -> Result<(), String> {
    let out = git(dir, &["merge", "--no-edit", "@{u}"])?;
    if out.status.success() {
        return Ok(());
    }
    let conflicts = git(dir, &["diff", "--name-only", "--diff-filter=U"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if conflicts.is_empty() {
        return Err(format!("git merge failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Err(format!(
        "merge conflicts; edit these, then `git add` + `git commit` (and `mdl <file> lint`): {}",
        conflicts.lines().collect::<Vec<_>>().join(", ")
    ))
}

/// Stage one file, commit if it changed, and push. Push failures are reported in the
/// note, not as an error: the commit is safe locally and pushes next time.
pub fn commit_push(dir: &Path, file: &str, msg: &str) -> Result<String, String> {
    git_ok(dir, &["add", "--", file])?;
    if git(dir, &["diff", "--cached", "--quiet"])?.status.success() {
        return Ok("nothing to commit".into());
    }
    git_ok(dir, &["commit", "--quiet", "-m", msg])?;
    Ok(push(dir))
}

pub fn push(dir: &Path) -> String {
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

fn has_remote(dir: &Path) -> bool {
    git(dir, &["remote"]).map(|o| o.status.success() && !o.stdout.is_empty()).unwrap_or(false)
}
