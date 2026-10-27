use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mdl-multi-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mdl")).current_dir(&self.0).args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
}

fn document() -> String {
    let table = "| Date | Description | Debit | Credit | Balance |\n|---|---|---:|---:|---:|\n| 2026-10-01 | Opening | 100 | | 100 |\n";
    format!("# USD\n\nShared notes.\n\n## Checking\n\nChecking notes.\n\n{table}\nChecking footer.\n\n## Emergency Fund\n\nSavings notes.\n\n{table}\nSavings footer.\n")
}

#[test]
fn qualified_cli_edits_preserve_siblings_and_support_reports() {
    let f = Fixture::new("cli");
    let original = document();
    fs::write(f.0.join("usd.md"), &original).unwrap();
    f.ok(&["usd:checking", "debit", "--date", "2026-10-02", "25", "Deposit"]);
    let changed = fs::read_to_string(f.0.join("usd.md")).unwrap();
    assert_eq!(changed.split("## Emergency Fund").nth(1), original.split("## Emergency Fund").nth(1));
    assert!(changed.starts_with("# USD\n\nShared notes.\n\n## Checking\n\nChecking notes."));
    f.ok(&["usd.md:emergency-fund", "credit", "--date", "2026-10-02", "10", "Withdrawal"]);
    assert_eq!(f.ok(&["balance", "usd:checking", "usd:emergency-fund", "-q"]), "125.00\n90.00\n215.00\n");
    let combined = f.ok(&["usd:checking", "usd:emergency-fund", "show", "--json"]);
    assert!(combined.contains("usd:checking: Deposit"));
    assert!(combined.contains("usd:emergency-fund: Withdrawal"));
    f.ok(&["usd:checking", "edit", "2", "2026-10-02", "30", "-", "Changed"]);
    f.ok(&["usd:emergency-fund", "delete", "2"]);
    f.ok(&["usd:checking", "lint"]);
    f.ok(&["usd:emergency-fund", "lint"]);
    assert_eq!(f.ok(&["balance", "usd:checking", "usd:emergency-fund", "-q"]), "130.00\n100.00\n230.00\n");
    let bare = f.run(&["usd", "credit", "20", "Ambiguous"]);
    assert!(String::from_utf8_lossy(&bare.stderr).contains("select a specific account"));
    fs::write(f.0.join("chart.md"), "- USD\n  - [Checking](usd:checking)\n  - [Savings](usd.md:emergency-fund)\n").unwrap();
    assert_eq!(f.ok(&["balance", "--total", "-q"]), "230.00\n");
    assert!(f.ok(&["graph", "2026-10", "--monthly"]).contains("230.00"));
    fs::write(f.0.join("chart.md"), "- [A](usd:checking)\n- [B](usd.md:checking)\n").unwrap();
    assert!(String::from_utf8_lossy(&f.run(&["balance"]).stderr).contains("duplicate account"));
}

#[test]
fn initialization_and_edit_open_named_documents() {
    let f = Fixture::new("init");
    f.ok(&["init", "usd:checking", "Checking"]);
    f.ok(&["init", "usd:emergency-fund", "Emergency", "Fund"]);
    let saved = fs::read_to_string(f.0.join("usd.md")).unwrap();
    for args in [&["edit", "usd"][..], &["edit", "usd:emergency-fund"], &["usd:checking", "edit"]] {
        let out = f.run(args);
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "not a terminal");
        assert_eq!(fs::read_to_string(f.0.join("usd.md")).unwrap(), saved);
    }
    assert!(!f.run(&["init", "usd:checking"]).status.success());
    assert!(!f.run(&["init", "usd:other", "Mismatched", "title"]).status.success());
    assert_eq!(fs::read_to_string(f.0.join("usd.md")).unwrap(), saved);
    let out = f.run(&["edit", "usd:new"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a terminal"));
    f.ok(&["usd:new", "lint"]);
    assert!(!f.0.join("usd:new.md").exists());
    f.ok(&["init", "legacy"]);
    let legacy = fs::read_to_string(f.0.join("legacy.md")).unwrap();
    assert!(!f.run(&["init", "legacy:new"]).status.success());
    assert_eq!(fs::read_to_string(f.0.join("legacy.md")).unwrap(), legacy);
}

#[test]
fn malformed_sections_and_fenced_examples() {
    let f = Fixture::new("validation");
    let original = document();
    for text in [original.replace("## Emergency Fund", "## Checking"), original.replace("## Emergency Fund", "## Checking!!!")] {
        fs::write(f.0.join("usd.md"), &text).unwrap();
        assert!(String::from_utf8_lossy(&f.run(&["usd:checking", "lint"]).stderr).contains("duplicate ledger"));
        assert_eq!(fs::read_to_string(f.0.join("usd.md")).unwrap(), text);
    }
    let unnamed = original.replace("## Checking", "# Checking").replace("## Emergency Fund", "# Emergency Fund");
    fs::write(f.0.join("usd.md"), format!("{original}\n{unnamed}")).unwrap();
    assert!(String::from_utf8_lossy(&f.run(&["usd:checking", "lint"]).stderr).contains("name every ledger"));
    fs::write(f.0.join("usd.md"), format!("```markdown\n{original}\n```\n{original}")).unwrap();
    f.ok(&["usd:checking", "lint"]);
    let out = f.run(&["usd:missing", "debit", "1", "No"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such ledger"));
    fs::write(f.0.join("usd.md"), original.replace("Savings footer.", "<<<<<<< ours\n=======\n>>>>>>> theirs")).unwrap();
    assert!(String::from_utf8_lossy(&f.run(&["usd:checking", "lint"]).stderr).contains("unresolved merge conflict"));
}

#[test]
fn qualified_print_has_a_distinct_filename() {
    if Command::new("typst").arg("--version").output().is_err() { return; }
    let f = Fixture::new("print");
    let text = document().replace("Checking notes.", "").replace("Checking footer.", "")
        .replace("Savings notes.", "").replace("Savings footer.", "");
    fs::write(f.0.join("usd.md"), text).unwrap();
    f.ok(&["usd:checking", "print"]);
    assert!(Path::new(&f.0.join("usd-checking.pdf")).exists());
    f.ok(&["usd", "print"]);
    assert!(f.0.join("usd-emergency-fund.pdf").exists());
    assert!(!f.run(&["usd", "print", "-o", "all.pdf"]).status.success());
    assert!(!f.0.join("all.pdf").exists());
}

#[test]
fn bare_documents_show_and_balance_every_ledger_without_merging_statements() {
    let f = Fixture::new("all-show");
    fs::write(f.0.join("usd.md"), document()).unwrap();
    let shown = f.ok(&["usd"]);
    assert!(shown.contains("usd:checking\n"));
    assert!(shown.contains("usd:emergency-fund\n"));
    assert!(shown.find("usd:checking").unwrap() < shown.find("usd:emergency-fund").unwrap());
    assert!(!shown.contains("200.00"), "individual statements must not acquire a combined running balance");
    assert_eq!(shown, f.ok(&["usd.md", "show"]));
    let markdown = f.ok(&["usd", "show", "2026-10", "--markdown"]);
    assert!(markdown.contains("## usd:checking"));
    assert!(markdown.contains("## usd:emergency-fund"));
    let json = f.ok(&["usd", "--json"]);
    assert!(json.starts_with("{\"accounts\":["));
    assert_eq!(json.matches("\"title\"").count(), 2);
    assert!(json.contains("\"title\": \"usd:checking\""));
    assert!(json.contains("\"title\": \"usd:emergency-fund\""));
    assert!(json.trim_end().ends_with("]}"));
    let csv = f.ok(&["usd", "--csv"]);
    assert_eq!(csv.lines().count(), 3);
    assert!(csv.starts_with("Account,Date,Description,Debit,Credit,Balance\n"));
    assert!(csv.contains("usd:checking,2026-10-01,Opening,100.00,0.00,100.00"));
    assert!(csv.contains("usd:emergency-fund,2026-10-01,Opening,100.00,0.00,100.00"));
    for args in [&["usd", "balance", "-q"][..], &["balance", "usd.md", "-q"], &["balance", "usd", "usd:checking", "-q"]] {
        assert_eq!(f.ok(args), "100.00\n100.00\n200.00\n");
    }
    assert_eq!(f.ok(&["balance", "usd", "--as-of", "2026-09-30", "-q"]), "0.00\n0.00\n0.00\n");
    let balance = f.ok(&["usd", "balance", "--json"]);
    assert!(balance.contains("\"account\": \"usd:checking\""));
    assert!(balance.contains("\"account\": \"usd:emergency-fund\""));
    let selected = f.ok(&["usd:checking", "show"]);
    assert!(!selected.contains("Emergency"));
}

#[test]
fn document_lint_and_recalc_visit_all_ledgers_and_validate_before_writing() {
    let f = Fixture::new("all-recalc");
    let stale = document().replace("| 100 | | 100 |", "| 100 | | 999 |");
    fs::write(f.0.join("usd.md"), &stale).unwrap();
    for args in [&["usd", "lint"][..], &["usd", "recalc", "--dry-run"]] {
        let out = f.run(args);
        assert!(!out.status.success());
        let errors = String::from_utf8_lossy(&out.stderr);
        assert!(errors.contains("usd:checking:"));
        assert!(errors.contains("usd:emergency-fund:"));
        assert_eq!(fs::read_to_string(f.0.join("usd.md")).unwrap(), stale);
    }
    let (before, after) = stale.split_once("## Emergency Fund").unwrap();
    let broken = format!("{before}## Emergency Fund{}", after.replace("2026-10-01", "invalid-date"));
    fs::write(f.0.join("usd.md"), &broken).unwrap();
    assert!(!f.run(&["usd", "recalc"]).status.success());
    assert_eq!(fs::read_to_string(f.0.join("usd.md")).unwrap(), broken);
    let out = f.run(&["usd", "show"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    fs::write(f.0.join("usd.md"), &stale).unwrap();
    f.ok(&["usd", "recalc"]);
    f.ok(&["usd", "lint"]);
    f.ok(&["usd", "recalc", "--dry-run"]);
    assert_eq!(f.ok(&["usd", "balance", "-q"]), "100.00\n100.00\n200.00\n");
    let saved = fs::read_to_string(f.0.join("usd.md")).unwrap();
    assert!(saved.contains("Shared notes."));
    assert!(saved.contains("Checking footer."));
    assert!(saved.contains("Savings footer."));
}
