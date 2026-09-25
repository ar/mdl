use std::process::{Command, Output};

fn mdl(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mdl"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args).output().unwrap()
}

fn output(args: &[&str]) -> String {
    let result = mdl(args);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn quiet_and_total_in_both_command_orders() {
    for args in [
        vec!["balance", "-q", "cash", "cash"],
        vec!["cash", "cash", "balance", "--quiet"],
        vec!["balance", "cash", "--quiet", "cash"],
    ] {
        assert_eq!(output(&args), "331.65\n331.65\n663.30\n");
    }
    assert_eq!(output(&["cash", "balance", "-q"]), "331.65\n331.65\n");
    assert_eq!(output(&["balance", "--total", "cash", "cash", "-q"]), "663.30\n");
}

#[test]
fn formats_and_validation() {
    let pretty = output(&["balance", "cash"]);
    assert!(pretty.starts_with('┌'));
    assert!(pretty.contains("│ Account"));
    assert!(pretty.contains("│ Total"));
    assert!(!pretty.contains('\x1b'));
    assert_eq!(pretty, output(&["cash", "balance", "--pretty"]));
    assert_eq!(output(&["balance", "cash", "--csv"]), "Account,Balance\ncash.md,331.65\nTotal,331.65\n");
    assert_eq!(output(&["cash", "balance", "--json"]), "{\"accounts\": [\n    {\"account\": \"cash.md\", \"balance\": 331.65}\n], \"total\": 331.65}\n");
    let markdown = output(&["cash", "balance", "--markdown"]);
    assert!(markdown.starts_with("| Account"));
    assert!(markdown.contains("| Total"));
    for args in [
        vec!["balance", "-q"],
        vec!["cash", "balance", "--typo"],
        vec!["balance", "cash", "--json", "--csv"],
        vec!["cash", "balance", "-q", "--markdown"],
        vec!["balance", "cash", "missing-account", "-q"],
    ] {
        let result = mdl(&args);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
}
