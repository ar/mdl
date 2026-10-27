use std::{fs, path::PathBuf, process::{Command, Output}};

struct Repo(PathBuf);
impl Repo {
    fn new(name: &str) -> Self {
        let r = Self(std::env::temp_dir().join(format!("mdl-git-policy-{name}-{}", std::process::id())));
        fs::create_dir_all(&r.0).unwrap();
        r.git(&["init", "--quiet"]);
        r.git(&["config", "user.name", "Test"]);
        r.git(&["config", "user.email", "test@example.com"]);
        r.git(&["config", "commit.gpgsign", "false"]);
        r
    }
    fn git(&self, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(&self.0).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mdl")).current_dir(&self.0).args(args).output().unwrap()
    }
    fn mdl(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        o
    }
    fn write(&self, file: &str, text: &str) { fs::write(self.0.join(file), text).unwrap(); }
    fn enroll(&self) { self.git(&["add", "."]); self.git(&["commit", "--quiet", "-m", "Initial files"]); }
}
impl Drop for Repo { fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); } }
fn ledger(desc: &str) -> String {
    format!("# Cash\n\n| Date | Description | Debit | Credit | Balance |\n|---|---|---:|---:|---:|\n| 2026-10-01 | {desc} | 1.00 | | 99.00 |\n")
}

#[test]
fn bulk_commit_ignores_unrelated_new_deleted_renamed_and_partially_staged_files() {
    let r = Repo::new("bulk");
    for name in ["eligible.md", "staged.md", "partial.md", "deleted.md", "rename.md", "broken.md", "fenced.md"] {
        r.write(name, &ledger("Opening"));
    }
    r.write("README.md", "Notes\n");
    r.write("code.rs", "original\n");
    r.enroll();
    r.write("eligible.md", &ledger("Unstaged ledger"));
    r.write("staged.md", &ledger("Staged ledger"));
    r.git(&["add", "staged.md"]);
    r.write("partial.md", &ledger("Staged portion"));
    r.git(&["add", "partial.md"]);
    r.write("partial.md", &ledger("Unstaged portion"));
    r.write("new.md", &ledger("New"));
    r.write("added.md", &ledger("Added"));
    r.git(&["add", "added.md"]);
    fs::remove_file(r.0.join("deleted.md")).unwrap();
    r.git(&["mv", "rename.md", "renamed.md"]);
    r.write("broken.md", &ledger("Bad date").replace("2026-10-01", "not-a-date"));
    r.write("fenced.md", &format!("```markdown\n{}\n```\n", ledger("Example")));
    r.write("README.md", "Changed prose\n| A | B | C | D | E |\n|---|---|---|---|---|\n");
    r.write("code.rs", "staged code\n");
    r.git(&["add", "code.rs", "README.md"]);
    let staged_code = r.git(&["show", ":code.rs"]);
    let partial = r.git(&["show", ":partial.md"]);
    r.mdl(&["push"]);
    assert_eq!(r.git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]), "eligible.md\nstaged.md\n");
    assert_eq!(r.git(&["show", ":code.rs"]), staged_code);
    assert_eq!(r.git(&["show", ":partial.md"]), partial);
    assert!(r.git(&["status", "--porcelain"]).contains("?? new.md"));
    assert_eq!(r.git(&["show", "HEAD:code.rs"]), "original\n");
    let before = r.git(&["rev-parse", "HEAD"]);
    r.mdl(&["push"]);
    assert_eq!(r.git(&["rev-parse", "HEAD"]), before);
}

#[test]
fn autosave_keeps_staging_and_requires_manual_first_commit() {
    let r = Repo::new("autosave");
    r.write("cash.md", &ledger("Opening"));
    r.write("code.rs", "original\n");
    r.enroll();
    r.git(&["config", "mdl.autocommit", "true"]);
    r.write("code.rs", "staged code\n");
    r.git(&["add", "code.rs"]);
    let o = r.mdl(&["cash", "debit", "2", "Deposit"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("automatic push disabled"));
    assert_eq!(r.git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]), "cash.md\n");
    assert_eq!(r.git(&["diff", "--cached", "--name-only"]), "code.rs\n");
    let head = r.git(&["rev-parse", "HEAD"]);
    r.mdl(&["init", "new"]);
    r.mdl(&["new", "debit", "1", "New deposit"]);
    assert_eq!(r.git(&["rev-parse", "HEAD"]), head);
    assert!(r.git(&["status", "--porcelain"]).contains("?? new.md"));
    r.write("cash.md", &ledger("Staged cash"));
    r.git(&["add", "cash.md"]);
    let staged = r.git(&["show", ":cash.md"]);
    r.mdl(&["cash", "debit", "1", "Leave staging alone"]);
    assert_eq!(r.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(r.git(&["show", ":cash.md"]), staged);
}

#[test]
fn scoped_commits_preserve_staging_outside_account_directory_and_literal_names() {
    let r = Repo::new("scope");
    fs::create_dir_all(r.0.join("accounts")).unwrap();
    r.write("accounts/a[1].md", &ledger("Opening"));
    r.write("accounts/a1.md", &ledger("Other"));
    r.write("outside.md", &ledger("Outside"));
    r.enroll();
    r.write("outside.md", &ledger("Staged outside"));
    r.git(&["add", "outside.md"]);
    r.write("accounts/a[1].md", &ledger("Changed"));
    let o = Command::new(env!("CARGO_BIN_EXE_mdl")).current_dir(r.0.join("accounts")).arg("push").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(r.git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]), "accounts/a[1].md\n");
    assert_eq!(r.git(&["diff", "--cached", "--name-only"]), "outside.md\n");
    // An ongoing merge must never be completed by an MDL commit.
    let head = r.git(&["rev-parse", "HEAD"]);
    r.write(".git/MERGE_HEAD", &head);
    r.write("accounts/a[1].md", &ledger("During merge"));
    let o = r.run(&["push"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("operation in progress"));
    assert_eq!(r.git(&["rev-parse", "HEAD"]), head);
}
