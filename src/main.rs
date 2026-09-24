//! mdl: ledgers as Markdown tables. The commands are in USAGE below (`mdl --help`).
//!
//! The ledger is the first 5-column GFM table in the file:
//! `| date | description | debit | credit | balance |`. Header labels are
//! yours (`init` writes them in English, or Spanish with `--lang es`; nothing reads
//! them back) and are preserved on rewrite. Empty amount cells
//! mean 0; a row with neither amount is a note (a dated remark) and leaves the
//! balance as it was. Amounts are i64 cents; balance = Σ debit − Σ credit. A description may
//! start with `#` and a computation (`#1000*40.50 currency exchange`, `#100+200+50 varios`),
//! its value being the row's effect on the balance (negative: a credit); `lint`
//! checks it against the row, and the TUI fills the amount from it.
//!
//! Git: `fetch`, `push` and `sync` work on the current directory (the account tree),
//! scoped to it. With `git config mdl.autocommit true` in that repository every
//! save also commits the file and pushes best-effort. The interactive screen fetches
//! on the way in and syncs on the way out. See git.rs and tui.rs.
mod git;
mod ledger;
mod print;
mod render;
mod tui;

use std::io::{self, IsTerminal};
use std::path::Path;
use std::{env, fs, process};

use ledger::{Entry, add_entry, amount_arg, balance, delete_entry, edit_entry, entry_what, find_table, fmt_amount, lint, load, move_entry, period, push_pending, recalc, recalc_diff, resolve, row_index, save_commit, scoped, today};
use print::print_pdf;
use render::{render, render_csv, render_json, render_pretty};
use tui::{scan_accounts, tui};

const USAGE: &str = "\
usage: mdl [<file>[.md]] [<command> [args...]]

  mdl                                  interactive screen over ./**/*.md
  mdl init <file> [--lang es|en] [title...]
                                       a new account; a file without a table
                                       gets one appended
  mdl --help | --version

Read
  mdl <file> [show] [--pretty|--markdown|--json|--csv] [period]
                                       the statement, with borders and totals
                                       by default; --markdown uses a GFM table
  mdl <file> print [-o <pdf>] [--graph [balance|debit|credit]...] [period]
                                       typeset to <file>.pdf, or <pdf>; --graph
                                       charts the balance, or each entry's amounts
  mdl <file> lint                      check dates, balances and #expressions
  mdl <file> balance                   the closing balance
  mdl <file> <file>... show|print ...  several accounts as one (bank/*.md):
                                       merged by date, each description led
                                       by its account, one running balance
  mdl balance [--total] <file>...      several accounts, and their sum

Write
  mdl <file> debit  [--date D] <amount> <description...>
  mdl <file> credit [--date D] <amount> <description...>
  mdl <file> note   [--date D] <description...>
                                       a dated remark; the balance stays
  mdl <file> edit <row> <date> <debit|-> <credit|-> <description...>
  mdl <file> move <row> up|down        swap with the row above or below
  mdl <file> flag <row>                toggle bold
  mdl <file> delete <row>
  mdl <file> recalc [--dry-run]        recompute the balances; --dry-run
                                       only reports the stale ones

Git (the current directory is the account tree)
  mdl fetch                            bring the tree up to date; no commit
  mdl push [message...]                commit everything pending and push
  mdl sync [message...]                fetch, then push
  git config mdl.autocommit true       every save commits and pushes
  (leaving the interactive screen syncs whatever is pending)

period      YYYY-MM [YYYY-MM] | this [N] | last [N]: months; this N ends with
            the current month, last N with the previous one; the statement
            opens with the balance before them
--date D    D is YYYY-MM-DD, today by default; a date before the last row
            inserts the entry in date order, after the rows on that day
amount      1234.50, or a computation: 100+200+50, 1000*40.50
#expr       a description may start with one (#1000*40.50 exchange): its
            value is the row's amount, negative for a credit; lint checks
            it, and the interactive screen fills the amount from it
";

/// A command line that does not parse: one line saying what is missing, and where the
/// full usage is. Exit status 2.
fn bad(what: &str) -> ! {
    eprintln!("mdl: {what} (see mdl --help)");
    process::exit(2)
}

/// `mdl init <file[.md]> [--lang es|en] [title...]`: a new account from the template,
/// titled after the file when no title is given. An existing file keeps everything it
/// has and gets the table appended (the title is then not used); one that already has
/// a table is refused. With `mdl.autocommit`, commits and pushes it like any other save.
fn cmd_init(args: &[String]) -> Result<(), String> {
    let mut lang = "en".to_string();
    let mut words = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lang" => lang = it.next().cloned().unwrap_or_else(|| bad("init: --lang needs es or en")),
            _ => words.push(a.clone()),
        }
    }
    let [file, title @ ..] = &words[..] else { bad("init needs a file name") };
    let file = if file.ends_with(".md") { file.clone() } else { format!("{file}.md") };
    let stem = Path::new(&file).file_stem().map_or(file.clone(), |s| s.to_string_lossy().into_owned());
    let title = if title.is_empty() { stem.clone() } else { title.join(" ") };
    let existing = match fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{file}: {e}")),
    };
    let text = init_text(existing.as_deref(), &title, &lang, &today()).map_err(|e| format!("{file}: {e}"))?;
    fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    println!("{} {file}", if existing.is_some() { "added the table to" } else { "wrote" });
    let dir = Path::new(".");
    if git::is_repo(dir) && git::autocommit(dir) {
        let note = git::commit_push(dir, &file, &format!("mdl: {stem} init"))?;
        if note != "pushed" {
            eprintln!("{note}");
        }
    }
    Ok(())
}

/// The file `init` writes: the template for a new one, or `existing` with the table
/// appended after a blank line when it has no table yet.
fn init_text(existing: Option<&str>, title: &str, lang: &str, today: &str) -> Result<String, String> {
    let Some(text) = existing else { return template(title, lang, today) };
    let lines: Vec<&str> = text.lines().collect();
    if find_table(&lines).is_ok() {
        return Err("already has a table".into());
    }
    let (_, _, table) = parts(title, lang, today)?;
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    Ok(out + &table)
}

/// A new account: the title as a heading, a line of prose, and the table.
fn template(title: &str, lang: &str, today: &str) -> Result<String, String> {
    let (_, prose, table) = parts(title, lang, today)?;
    Ok(format!("# {title}\n\n{prose}\n\n{table}"))
}

/// The pieces of a new account in `lang` (`es` or `en`): the header labels, a line of
/// prose about `title`, and the rendered table with an opening row at 0.00 dated `today`.
fn parts(title: &str, lang: &str, today: &str) -> Result<(Vec<String>, String, String), String> {
    let (prose, header, opening) = match lang {
        "es" => (
            format!("Libro mayor de la cuenta {title}. Saldo = Σ debe − Σ haber."),
            ["Fecha", "Descripción", "Debe", "Haber", "Saldo"],
            "Saldo inicial",
        ),
        "en" => (
            format!("{title} ledger. Balance = Σ debit − Σ credit."),
            ["Date", "Description", "Debit", "Credit", "Balance"],
            "Opening balance",
        ),
        _ => return Err(format!("bad language `{lang}`: es | en")),
    };
    let header: Vec<String> = header.iter().map(|s| s.to_string()).collect();
    let rows = [Entry { date: today.into(), desc: opening.into(), debit: 0, credit: 0, balance: 0, bold: false }];
    let table = render(&header, &rows);
    Ok((header, prose, table))
}

fn repo_dir() -> Result<&'static Path, String> {
    let dir = Path::new(".");
    if git::is_repo(dir) { Ok(dir) } else { Err("not a git repository (git init)".into()) }
}

/// After a fast-forward or merge: lint every account and say what needs a recalc.
fn check_accounts() {
    let mut accounts = vec![];
    scan_accounts(Path::new("."), &mut accounts);
    accounts.sort();
    let mut stale = false;
    for a in &accounts {
        match load(a) {
            Ok(doc) => {
                for e in lint(&doc.rows) {
                    eprintln!("{a}: {e}");
                }
                stale |= !recalc_diff(&doc.rows).is_empty();
            }
            Err(e) => eprintln!("{e}"),
        }
    }
    if stale {
        eprintln!("stale balances: run `mdl <account> recalc`");
    }
}

/// `mdl fetch`: bring the tree up to date with the remote. No push, no commit.
fn cmd_fetch() -> Result<(), String> {
    let (changed, note) = git::pull(repo_dir()?)?;
    eprintln!("{note}");
    if changed {
        check_accounts();
    }
    Ok(())
}

/// `mdl push [message...]`: commit everything pending under the tree and push it.
/// Fails when the push itself fails: an explicit push that did not push is an error.
fn cmd_push(msg: &[String]) -> Result<(), String> {
    let dir = repo_dir()?;
    let (pending, note) = push_pending(dir, &msg.join(" "))?;
    for p in pending.iter().take(10) {
        eprintln!("  {p}");
    }
    if pending.len() > 10 {
        eprintln!("  … and {} more", pending.len() - 10);
    }
    eprintln!("{note}");
    if note.contains("push failed") {
        return Err("the remote may have moved on; run `mdl fetch` and push again".into());
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return tui();
    }
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--version") {
        println!("mdl {} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"));
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "balance") {
        let (only_total, files) = match &args[1..] {
            [f, r @ ..] if f == "--total" => (true, r),
            r => (false, r),
        };
        if files.is_empty() {
            bad("balance needs at least one file")
        }
        let mut total = 0;
        for f in files {
            let f = &resolve(f);
            let b = balance(&load(f)?.rows);
            total += b;
            if !only_total {
                println!("{f}: {}", fmt_amount(b));
            }
        }
        println!("{}{}", if only_total { "" } else { "total: " }, fmt_amount(total));
        return Ok(());
    }
    match args.as_slice() {
        [c] if c == "fetch" => return cmd_fetch(),
        [c, m @ ..] if c == "push" => return cmd_push(m),
        [c, r @ ..] if c == "init" => return cmd_init(r),
        [c, m @ ..] if c == "sync" => {
            let (changed, note) = git::pull(repo_dir()?)?;
            eprintln!("{note}");
            if changed {
                check_accounts();
            }
            return cmd_push(m);
        }
        _ => {}
    }
    // More accounts may follow the first (a shell glob, say): every word up to the
    // command that names an existing file. show and print combine them.
    const COMMANDS: [&str; 12] = ["show", "print", "lint", "balance", "recalc", "debit", "credit", "note", "edit", "move", "flag", "delete"];
    let n = 1 + args[1..].iter().take_while(|a| !COMMANDS.contains(&a.as_str()) && Path::new(&resolve(a)).is_file()).count();
    let files: Vec<String> = args[..n].iter().map(|f| resolve(f)).collect();
    // Display flags and period selectors imply `show`; validation stays in the
    // same period parser used by `print`.
    let show = "show".to_string();
    let (cmd, rest): (&String, &[String]) = match &args[n..] {
        [] => (&show, &[]),
        [first, ..] if ["--json", "--csv", "--pretty", "--markdown", "this", "last"].contains(&first.as_str())
            || first.as_bytes().first().is_some_and(u8::is_ascii_digit) => (&show, &args[n..]),
        [cmd, rest @ ..] => (cmd, rest),
    };
    let combined;
    let file = if files.len() == 1 {
        &files[0]
    } else if cmd == "show" || cmd == "print" {
        // ponytail: the PDF is named after every stem; a long glob makes a long name
        let stems: Vec<String> = files.iter().map(|f| Path::new(f).file_stem().unwrap_or_default().to_string_lossy().into_owned()).collect();
        combined = Path::new(&files[0]).with_file_name(stems.join("+")).with_extension("md").to_string_lossy().into_owned();
        &combined
    } else {
        bad(&format!("{cmd} takes one account; only show and print combine several"))
    };
    let mut doc = if files.len() == 1 { load(file)? } else { ledger::combine(&files)? };
    let what: String;

    match cmd.as_str() {
        "lint" => {
            let errs = lint(&doc.rows);
            return if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) };
        }
        "show" => {
            let json = rest.iter().any(|a| a == "--json");
            let csv = rest.iter().any(|a| a == "--csv");
            let markdown = rest.iter().any(|a| a == "--markdown");
            let explicit_pretty = rest.iter().any(|a| a == "--pretty");
            if json as u8 + csv as u8 + markdown as u8 + explicit_pretty as u8 > 1 {
                bad("show: choose only one of --pretty, --markdown, --json, or --csv");
            }
            let pretty = !json && !csv && !markdown;
            let words: Vec<String> = rest.iter().filter(|a| !["--json", "--csv", "--pretty", "--markdown"].contains(&a.as_str())).cloned().collect();
            let doc = scoped(doc, &period(&words, &today())?);
            if json {
                print!("{}", render_json(doc.title().map(|t| t[2..].trim()), &doc.rows));
            } else if csv {
                print!("{}", render_csv(&doc.header, &doc.rows));
            } else {
                if let Some(t) = doc.title() {
                    println!("{}\n", if pretty { t[2..].trim() } else { t });
                }
                print!("{}", if pretty {
                    render_pretty(&doc.header, &doc.rows, io::stdout().is_terminal())
                } else {
                    render(&doc.header, &doc.rows)
                });
            }
            return Ok(());
        }
        "print" => {
            // --graph [balance|debit|credit]...: the series words anywhere, balance alone by default
            // -o <file>: where the PDF goes, instead of the name print_pdf derives
            let mut rest = rest.to_vec();
            let out = match rest.iter().position(|w| w == "-o") {
                Some(i) if i + 1 < rest.len() => Some(rest.drain(i..i + 2).nth(1).unwrap()),
                Some(_) => bad("print: -o needs a file name"),
                None => None,
            };
            let (flags, words): (Vec<String>, Vec<String>) = rest.into_iter().partition(|w| ["--graph", "balance", "debit", "credit"].contains(&w.as_str()));
            let mut series: Vec<String> = flags.iter().filter(|w| *w != "--graph").cloned().collect();
            if series.is_empty() && !flags.is_empty() {
                series.push("balance".into());
            }
            let p = period(&words, &today())?;
            let doc = scoped(doc, &p);
            println!("{}", print_pdf(&doc, file, &p, &series, out.as_deref())?);
            return Ok(());
        }
        "balance" => {
            println!("{}", fmt_amount(balance(&doc.rows)));
            return Ok(());
        }
        "recalc" => match rest {
            [] => {
                recalc(&mut doc.rows);
                what = "recalc".into();
            }
            [d] if d == "--dry-run" => {
                let diffs = recalc_diff(&doc.rows);
                return if diffs.is_empty() { Ok(()) } else { Err(diffs.join("\n")) };
            }
            _ => bad("recalc takes --dry-run and nothing else"),
        },
        "debit" | "credit" | "note" => {
            let (date, rest) = match rest {
                [f, d, r @ ..] if f == "--date" => (d.clone(), r),
                _ => (today(), rest),
            };
            let (debit, credit, desc) = match (cmd.as_str(), rest) {
                ("note", desc) => (0, 0, desc),
                ("debit", [amount, desc @ ..]) => (amount_arg(amount)?, 0, desc),
                ("credit", [amount, desc @ ..]) => (0, amount_arg(amount)?, desc),
                _ => bad(&format!("{cmd} needs an amount and a description")),
            };
            if desc.is_empty() {
                bad(&format!("{cmd} needs a description"))
            }
            let i = add_entry(&mut doc.rows, date, debit, credit, desc.join(" "))?;
            if i + 1 < doc.rows.len() {
                println!("inserted as row {} (before {})", i + 1, doc.rows[i + 1].date);
            }
            what = entry_what(debit, credit, &desc.join(" "));
        }
        "edit" => {
            let [n, date, debit, credit, desc @ ..] = rest else { bad("edit needs <row> <date> <debit|-> <credit|-> <description...>") };
            if desc.is_empty() {
                bad("edit needs a description")
            }
            let i = row_index(n, doc.rows.len())?;
            edit_entry(&mut doc.rows, i, date.clone(), amount_arg(debit)?, amount_arg(credit)?, desc.join(" "))?;
            what = format!("edit row {}: {}", i + 1, desc.join(" "));
        }
        "move" => {
            let [n, dir] = rest else { bad("move needs <row> up|down") };
            let up = match dir.as_str() {
                "up" => true,
                "down" => false,
                _ => bad(&format!("move: `{dir}` is not up or down")),
            };
            let i = row_index(n, doc.rows.len())?;
            let j = move_entry(&mut doc.rows, i, up)?;
            println!("row {} moved {dir} to row {} ({})", i + 1, j + 1, doc.rows[j].date);
            what = format!("move row {} {dir}", i + 1);
        }
        "flag" => {
            let [n] = rest else { bad("flag needs a row number") };
            let i = row_index(n, doc.rows.len())?;
            doc.rows[i].bold = !doc.rows[i].bold;
            what = format!("{} row {}", if doc.rows[i].bold { "flag" } else { "unflag" }, i + 1);
        }
        "delete" => {
            let [n] = rest else { bad("delete needs a row number") };
            let i = row_index(n, doc.rows.len())?;
            let e = delete_entry(&mut doc.rows, i)?;
            what = format!("delete row {}: {}", i + 1, e.desc);
        }
        _ => bad(&format!("`{cmd}` is not a command")),
    }
    let note = save_commit(file, &doc, &what)?;
    if !note.is_empty() && note != "pushed" {
        eprintln!("{note}");
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{load_str, DOC};

    #[test]
    fn init_appends_to_a_file_without_a_table() {
        let table = "| Fecha      | Descripción   | Debe | Haber | Saldo |\n|------------|---------------|-----:|------:|------:|\n| 2026-09-18 | Saldo inicial |      |       |  0.00 |\n";
        // notes above stay, a blank line separates them from the table, whatever the ending
        let notes = "# Caja\n\nCta 123\n- 2026-09-01 algo";
        for (text, sep) in [(notes, "\n\n"), (&format!("{notes}\n"), "\n"), (&format!("{notes}\n\n"), "")] {
            assert_eq!(init_text(Some(text), "Caja", "es", "2026-09-18").unwrap(), format!("{text}{sep}{table}"));
        }
        assert_eq!(init_text(Some(""), "Caja", "es", "2026-09-18").unwrap(), table);
        assert_eq!(init_text(Some(DOC), "Caja", "es", "2026-09-18").unwrap_err(), "already has a table");
        assert_eq!(init_text(None, "Caja", "es", "2026-09-18").unwrap(), template("Caja", "es", "2026-09-18").unwrap());
        // the result is an account with the notes as its prose
        let out = init_text(Some(notes), "Caja", "es", "2026-09-18").unwrap();
        let (header, rows) = load_str(&out);
        assert_eq!((header[2].as_str(), rows.len(), rows[0].desc.as_str()), ("Debe", 1, "Saldo inicial"));
    }

    #[test]
    fn combine_merges_by_date() {
        let dir = std::env::temp_dir().join(format!("mdl-combine-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bank = dir.join("bank.md");
        fs::write(&bank, "# Bank\n\n| D | Desc | Dr | Cr | Bal |\n|---|---|--:|--:|--:|\n| 2026-09-02 | Deposit | 10.00 | | 10.00 |\n| 2026-09-20 | Fee | | 1.00 | 9.00 |\n").unwrap();
        let files = ["cash.md".to_string(), bank.to_str().unwrap().to_string()];
        let doc = ledger::combine(&files).unwrap();
        assert_eq!(doc.title(), Some("# Cash + Bank"));
        assert_eq!(doc.header[4], "Balance");
        let cash = load("cash.md").unwrap();
        assert_eq!(doc.rows.len(), cash.rows.len() + 2);
        // same day: the accounts' order; the balance runs over both
        assert_eq!((doc.rows[1].desc.as_str(), doc.rows[2].desc.as_str()), ("cash: Counter sale", "bank: Deposit"));
        assert_eq!(doc.rows[2].balance, 15000 + 1000);
        assert_eq!(balance(&doc.rows), balance(&cash.rows) + 900);
        assert!(lint(&doc.rows).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn init_template() {
        let es = template("Caja", "es", "2026-09-18").unwrap();
        assert!(es.starts_with("# Caja\n\nLibro mayor de la cuenta Caja. Saldo = Σ debe − Σ haber.\n\n| Fecha      | Descripción   | Debe | Haber | Saldo |\n"));
        assert!(es.ends_with("| 2026-09-18 | Saldo inicial |      |       |  0.00 |\n"));
        let en = template("Petty cash", "en", "2026-09-18").unwrap();
        assert!(en.contains("# Petty cash\n\nPetty cash ledger. Balance = Σ debit − Σ credit.\n\n| Date       | Description     | Debit | Credit | Balance |\n"));
        assert!(en.ends_with("| 2026-09-18 | Opening balance |       |        |    0.00 |\n"));
        assert_eq!(template("x", "fr", "2026-09-18").unwrap_err(), "bad language `fr`: es | en");

        // the template is a valid, clean account in either language, and its labels
        // are the ones a period statement's opening row and a rewrite keep
        let dir = std::env::temp_dir().join(format!("mdl-init-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, &en).unwrap();
        let path = file.to_str().unwrap();
        let mut doc = load(path).unwrap();
        assert_eq!(doc.header, ["Date", "Description", "Debit", "Credit", "Balance"]);
        assert!(lint(&doc.rows).is_empty());
        add_entry(&mut doc.rows, "2026-10-02".into(), 1000, 0, "Sale".into()).unwrap();
        doc.save(path).unwrap();
        let doc = load(path).unwrap();
        assert_eq!((doc.rows.len(), doc.rows[1].balance), (2, 1000));
        let d = scoped(doc, &Some(("2026-10".into(), "2026-10".into())));
        assert_eq!((d.rows[0].desc.as_str(), d.rows[0].balance, d.rows.len()), ("Balance", 0, 2));
        assert!(render(&d.header, &d.rows).starts_with("| Date "));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Two clones of a bare remote: push, fast-forward, merge, conflict.
    #[test]
    fn git_round_trip() {
        use std::process::Command;
        let sh = |dir: &Path, args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
            assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
        };
        let base = std::env::temp_dir().join(format!("mdl-git-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (remote, a, b) = (base.join("remote.git"), base.join("a"), base.join("b"));
        fs::create_dir_all(&remote).unwrap();
        sh(&remote, &["-c", "init.defaultBranch=main", "init", "--bare", "--quiet"]);
        let clone = |dst: &Path| {
            sh(&base, &["clone", "--quiet", remote.to_str().unwrap(), dst.to_str().unwrap()]);
            sh(dst, &["config", "user.email", "t@example.com"]);
            sh(dst, &["config", "user.name", "t"]);
            sh(dst, &["config", "commit.gpgsign", "false"]);
        };
        clone(&a);
        assert!(git::fetched_ago(&a).is_none()); // a clone has not fetched yet
        assert_eq!(git::sync(&a).unwrap(), git::State::NoUpstream);
        assert!(git::fetched_ago(&a).is_some_and(|d| d.as_secs() < 60)); // sync fetched, even with no upstream

        let table = "# Caja\n\n| Fecha | Descripción | Debe | Haber | Saldo |\n|---|---|--:|--:|--:|\n| 2026-09-01 | inicial | 1.00 | | 1.00 |\n";
        fs::write(a.join("caja.md"), table).unwrap();
        assert_eq!(git::pending(&a).unwrap(), ["caja.md"]);
        let (_, note) = push_pending(&a, "").unwrap();
        assert_eq!(note, "committed: mdl: update caja; pushed"); // first push creates the upstream
        assert_eq!(git::sync(&a).unwrap(), git::State::UpToDate);

        clone(&b);
        fs::write(a.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n")).unwrap();
        assert_eq!(push_pending(&a, "from a").unwrap().1, "committed: from a; pushed");
        assert_eq!(git::pull(&b).unwrap(), (true, "fast-forwarded to the upstream"));
        assert!(fs::read_to_string(b.join("caja.md")).unwrap().contains("| a |"));

        // divergence on different files merges; stale balances are what recalc is for
        fs::write(a.join("banco.md"), table).unwrap();
        push_pending(&a, "").unwrap();
        fs::write(b.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n| 2026-09-03 | b | | 0.50 | 1.50 |\n")).unwrap();
        assert!(git::commit_all(&b, "from b").unwrap());
        assert_eq!(git::sync(&b).unwrap(), git::State::Diverged);
        assert_eq!(git::pull(&b).unwrap(), (true, "merged the upstream"));
        assert!(b.join("banco.md").exists());
        assert_eq!(git::push(&b), "pushed");

        // divergence at the same tail conflicts, and says so
        fs::write(a.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n| 2026-09-04 | x | 1.00 | | 3.00 |\n")).unwrap();
        assert!(git::commit_all(&a, "a again").unwrap());
        assert_eq!(git::sync(&a).unwrap(), git::State::Diverged);
        let err = git::pull(&a).unwrap_err();
        assert!(err.starts_with("merge conflicts;") && err.ends_with("caja.md"), "{err}");
        assert!(load(a.join("caja.md").to_str().unwrap()).is_err()); // markers trip the parser
        sh(&a, &["merge", "--abort"]);
        assert!(!git::autocommit(&a));
        sh(&a, &["config", "mdl.autocommit", "true"]);
        assert!(git::autocommit(&a));
        let _ = fs::remove_dir_all(&base);
    }
}
