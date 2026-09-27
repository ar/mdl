use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn mdl(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mdl"))
        .current_dir(dir).args(args).output().unwrap()
}

#[test]
fn explicit_edit_initializes_and_preserves_accounts() {
    let dir = std::env::temp_dir().join(format!("mdl-edit-cli-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for args in [vec!["edit", "new"], vec!["alias", "edit"], vec!["edit", "named.md"]] {
        // With piped stdin, reaching the TUI produces this error after preparation.
        let result = mdl(&dir, &args);
        assert_eq!(String::from_utf8_lossy(&result.stderr).trim(), "not a terminal");
    }
    for name in ["new.md", "alias.md", "named.md"] {
        let text = fs::read_to_string(dir.join(name)).unwrap();
        assert!(text.contains("Opening balance"));
        assert!(mdl(&dir, &[name, "lint"]).status.success());
    }
    let notes = "# Notes\n\nKeep this text exactly.";
    for name in ["notes.md", "extensionless"] {
        fs::write(dir.join(name), notes).unwrap();
        let result = mdl(&dir, &["edit", name]);
        assert!(String::from_utf8_lossy(&result.stderr).contains("not a terminal"));
        let text = fs::read_to_string(dir.join(name)).unwrap();
        assert!(text.starts_with(&format!("{notes}\n\n")));
        assert!(text.contains("Opening balance"));
        mdl(&dir, &[name, "edit"]);
        assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), text);
    }
    // The old row-edit form keeps its meaning.
    let result = mdl(&dir, &["new", "edit", "1", "2026-09-27", "12", "-", "Updated"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let text = fs::read_to_string(dir.join("new.md")).unwrap();
    assert!(text.contains("Updated"));
    mdl(&dir, &["edit", "new"]);
    assert_eq!(fs::read_to_string(dir.join("new.md")).unwrap(), text);
    // Invalid ledgers and merge conflicts are never treated as uninitialized files.
    for text in [text.replace("12.00", "oops"), "<<<<<<< ours\nnotes\n=======\nother\n>>>>>>> theirs\n".into()] {
        fs::write(dir.join("broken.md"), &text).unwrap();
        let result = mdl(&dir, &["edit", "broken"]);
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("not a terminal"));
        assert_eq!(fs::read_to_string(dir.join("broken.md")).unwrap(), text);
    }
    for args in [vec!["edit"], vec!["edit", "one", "two"]] {
        let result = mdl(&dir, &args);
        assert_eq!(result.status.code(), Some(2));
    }
    assert!(!dir.join("one.md").exists());
    fs::remove_dir_all(dir).unwrap();
}
