use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mdl-chart-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("charts")).unwrap();
        fs::write(dir.join("bank.md"), "| Date | Description | Debit | Credit | Balance |\n| --- | --- | --- | --- | --- |\n| 2024-02-28 | Opening | 100 | | 100 |\n| 2024-02-29 | Deposit | 25 | | 125 |\n| 2024-02-29 | Fee | | 5 | 120 |\n| 2024-03-01 | Deposit | 80 | | 200 |\n").unwrap();
        fs::write(dir.join("card.md"), "| Date | Description | Debit | Credit | Balance |\n| --- | --- | --- | --- | --- |\n| 2024-02-28 | Purchase | | 30 | -30 |\n").unwrap();
        fs::write(dir.join("charts/usd.md"), "# USD balances\n\nCurrency: USD\n\n- Assets\n  - Bank\n    - [Checking](../bank.md)\n  - Investments\n- Liabilities\n  - [Card](../card.md)\n").unwrap();
        fs::write(
            dir.join("chart.md"),
            "Currency: UYU\n- Cash\n  - [Bank](bank.md)\n",
        )
        .unwrap();
        Self(dir)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mdl"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn output(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn reject(&self, args: &[&str], error: &str) {
        let output = self.run(args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn hierarchy_totals_and_reverse_amount_indentation() {
    let f = Fixture::new();
    let text = f.output(&[
        "balance",
        "--chart",
        "charts/usd.md",
        "--as-of",
        "2024-02-29",
    ]);
    assert!(text.starts_with("USD balances\n\nCurrency: USD\n\nAs of 2024-02-29\n"));
    let row = |label: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .unwrap()
    };
    assert!(row("Assets").ends_with("120.00"));
    assert!(row("Bank").ends_with("120.00"));
    assert!(row("Checking").ends_with("120.00"));
    assert!(row("Investments").ends_with("0.00"));
    assert!(row("Liabilities").ends_with("-30.00"));
    assert!(row("Total").ends_with("90.00"));
    assert_eq!(row("Assets").len(), row("Bank").len() + 2);
    assert_eq!(row("Bank").len(), row("Checking").len() + 2);
    assert_eq!(row("Total").len(), row("Assets").len());
    assert!(row("Bank").starts_with("  Bank"));
    assert!(row("Checking").starts_with("    Checking"));
    assert!(!text.contains('\x1b'));
    let md = f.output(&[
        "balance",
        "--chart",
        "charts/usd.md",
        "--as-of",
        "2024-02-29",
        "--markdown",
    ]);
    assert_eq!(
        md.replace("```text\n", "").replace("```\n", "").replacen(
            "# USD balances",
            "USD balances",
            1
        ),
        text
    );
    assert_eq!(
        f.output(&[
            "balance",
            "--chart",
            "charts/usd.md",
            "--as-of",
            "2024-02-29",
            "--total",
            "-q"
        ]),
        "90.00\n"
    );
}

#[test]
fn snapshots_default_chart_and_explicit_accounts() {
    let f = Fixture::new();
    for (date, expected) in [
        ("2024-02-27", "0.00\n"),
        ("2024-02-28", "100.00\n"),
        ("2024-02-29", "120.00\n"),
        ("2025-01-01", "200.00\n"),
    ] {
        assert_eq!(
            f.output(&["balance", "bank", "--as-of", date, "--total", "-q"]),
            expected
        );
        assert_eq!(
            f.output(&["bank", "balance", "--as-of", date, "--total", "-q"]),
            expected
        );
        assert_eq!(
            f.output(&["balance", "--as-of", date, "--total", "-q"]),
            expected
        );
    }
    assert_eq!(f.output(&["balance", "--total", "-q"]), "200.00\n");
    assert_eq!(
        f.output(&["balance", "bank", "card", "--total", "-q"]),
        "170.00\n"
    );
    // Selecting another currency/report is independent of the default chart.
    assert!(f
        .output(&["balance", "--chart", "charts/usd.md"])
        .contains("Currency: USD"));
    assert!(f.output(&["balance"]).contains("Currency: UYU"));
    fs::remove_file(f.0.join("chart.md")).unwrap();
    f.reject(&["balance"], "balance needs");
}

#[test]
fn structured_exports_preserve_hierarchy_and_metadata() {
    let f = Fixture::new();
    let json = f.output(&[
        "balance",
        "--chart",
        "charts/usd.md",
        "--json",
        "--as-of",
        "2024-02-29",
    ]);
    assert!(json.contains("\"as_of\": \"2024-02-29\""));
    assert!(!json.contains("\"currency\":"));
    assert!(json
        .contains("\"name\": \"Assets\", \"balance\": 120.00, \"children\": [{\"name\": \"Bank\""));
    assert!(
        json.contains("\"name\": \"Checking\", \"balance\": 120.00, \"account\": \"../bank.md\"")
    );
    assert!(json.ends_with("\"total\": 90.00}\n"));
    let csv = f.output(&["balance", "--chart", "charts/usd.md", "--csv"]);
    assert!(csv.contains("Assets / Bank / Checking,account,200.00,\n"));
    assert!(csv.contains("Assets,group,200.00,\n"));
    assert!(csv.ends_with("Total,total,170.00,\n"));
    assert_eq!(
        f.output(&["balance", "--chart", "charts/usd.md", "-q"]),
        "200.00\n200.00\n200.00\n0.00\n-30.00\n-30.00\n170.00\n"
    );
    assert!(f
        .output(&["balance", "--json", "--total"])
        .contains("\"nodes\": []"));
}

#[test]
fn rejects_invalid_configuration_and_dates_without_partial_output() {
    let f = Fixture::new();
    for date in [
        "2023-02-29",
        "1900-02-29",
        "2024-04-31",
        "2024-00-01",
        "2024-01-00",
        "2024-1-01",
        "0000-01-01",
        "abcdefghij",
    ] {
        f.reject(&["balance", "--as-of", date], "invalid date");
    }
    assert_eq!(
        f.output(&["balance", "--as-of", "2000-02-29", "--total", "-q"]),
        "0.00\n"
    );
    for args in [
        vec!["balance", "--as-of"],
        vec!["balance", "--chart"],
        vec!["balance", "--chart", "--json"],
    ] {
        f.reject(&args, "needs a value");
    }
    f.reject(
        &["balance", "bank", "--chart", "chart.md"],
        "cannot be combined",
    );
    f.reject(&["balance", "--chart", "missing.md"], "missing.md");
    f.reject(
        &["balance", "--as-of", "2024-01-01", "--as-of", "2024-02-01"],
        "repeated",
    );
    for (body, error) in [
        (
            "- [One](bank.md)\n- [Two](./bank.md)\n",
            "duplicate account",
        ),
        ("- [Missing](missing.md)\n", "missing.md"),
        ("  - Assets\n", "top level"),
        ("- Assets\n    - Bank\n", "indentation level"),
        ("- Assets\n   - Bank\n", "two spaces"),
        ("- [Bank](bank.md)\n  - Child\n", "children require a group"),
        ("- [Broken](bank.md\n", "expected [name](path)"),
        ("Unexpected text\n", "chart needs a list"),
    ] {
        fs::write(f.0.join("bad.md"), format!("Currency: USD\n{body}")).unwrap();
        f.reject(&["balance", "--chart", "bad.md"], error);
    }
    fs::write(f.0.join("bad.md"), "- Assets\n").unwrap();
    assert_eq!(
        f.output(&["balance", "--chart", "bad.md", "--total", "-q"]),
        "0.00\n"
    );
    fs::write(f.0.join("bank.md"), "not a ledger").unwrap();
    f.reject(&["balance"], "bank.md");
}

#[test]
fn snapshot_rejects_out_of_order_ledger_dates() {
    let f = Fixture::new();
    let path = f.0.join("bank.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(path, text.replace("2024-03-01", "2024-02-01")).unwrap();
    f.reject(
        &["balance", "--as-of", "2024-02-29"],
        "dates are not in order",
    );
}

#[cfg(unix)]
#[test]
fn duplicate_symlink_is_detected() {
    let f = Fixture::new();
    std::os::unix::fs::symlink(Path::new("bank.md"), f.0.join("alias.md")).unwrap();
    fs::write(
        f.0.join("chart.md"),
        "Currency: USD\n- [Bank](bank.md)\n- [Alias](alias.md)\n",
    )
    .unwrap();
    f.reject(&["balance"], "duplicate account");
}

#[test]
fn currency_is_ordinary_prose_and_surrounding_markdown_is_preserved() {
    let f = Fixture::new();
    let before = "# Personal accounts\n\nBalances exclude **retirement**.\n\nCurrency: dollars, without conversion\nOwner: Me\n\n";
    let after = "## Notes\n\nSee [details](notes.md).\n\n- This is a note, not an account.\n";
    fs::write(
        f.0.join("chart.md"),
        format!("{before}- Assets\n  - [Checking](bank.md)\n\n{after}"),
    )
    .unwrap();
    let md = f.output(&["balance", "--markdown"]);
    assert!(md.starts_with(before));
    assert!(md.ends_with(after));
    assert!(md.contains("200.00"));
    assert_eq!(md.matches("# Personal accounts").count(), 1);
    let pretty = f.output(&["balance"]);
    assert!(pretty.starts_with("Personal accounts\n\nBalances exclude **retirement**."));
    assert!(pretty.contains("Currency: dollars, without conversion\nOwner: Me"));
    assert!(pretty
        .ends_with("Notes\n\nSee [details](notes.md).\n\n- This is a note, not an account.\n"));

    // No metadata at all is required, and no default title or currency is invented.
    fs::write(f.0.join("chart.md"), "- Assets\n  - [Checking](bank.md)\n").unwrap();
    let plain = f.output(&["balance"]);
    assert!(plain.starts_with("Account"));
    assert_eq!(f.output(&["balance", "--total", "-q"]), "200.00\n");
    let json = f.output(&["balance", "--json"]);
    assert!(json.contains("\"title\": null"));
}

#[test]
fn fenced_lists_remain_report_text() {
    let f = Fixture::new();
    let preamble = "# Report\n\n```markdown\n- [Example](missing.md)\n```\n\n";
    fs::write(
        f.0.join("chart.md"),
        format!("{preamble}- [Bank](bank.md)\n"),
    )
    .unwrap();
    assert!(f.output(&["balance", "--markdown"]).starts_with(preamble));
    assert_eq!(f.output(&["balance", "--total", "-q"]), "200.00\n");
}
