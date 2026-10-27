//! mdl: ledgers as Markdown tables. The commands are in USAGE below (`mdl --help`).
//!
//! An unnamed ledger is the first 5-column GFM table in the file. Named ledgers
//! use `##` sections addressed as `file:ledger`. Each table has this shape:
//! `| date | description | debit | credit | balance |`. Header labels are
//! yours (`init` writes them in English, or Spanish with `--lang es`) and are preserved
//! on rewrite. Transfers use `Fecha` to choose Spanish description suffixes. Empty amount cells
//! mean 0; a row with neither amount is a note (a dated remark) and leaves the
//! balance as it was. Amounts are i64 cents; balance = Σ debit − Σ credit. Descriptions
//! contain plain text. Amount inputs accept arithmetic as a calculation aid; only
//! the resulting numbers are stored.
//!
//! Git: `fetch`, `push` and `sync` work on the current directory (the account tree),
//! scoped to it. With `git config mdl.autocommit true` in that repository every
//! save commits eligible tracked ledgers; `mdl.autopush` separately enables pushes.
//! The account picker fetches
//! on the way in and syncs on the way out; `edit` opens locally and leaves locally.
//! See git.rs and tui.rs.
#[cfg(test)]
use ledger::balance;

mod account_chart;
mod evolution_plot;
mod git;
mod chart;
mod ledger;
mod print;
mod render;
mod tui;

use std::io::{self, IsTerminal};
use std::path::Path;
use std::{env, fs, process};

use ledger::{Entry, add_entry, amount_arg, delete_entry, edit_entry, entry_what, find_table, lint, load, move_entry, period, push_pending, recalc, recalc_diff, resolve, row_index, save_commit, scoped, today};
use print::{graph_png, graph_png_accounts, print_pdf};
use render::{render, render_csv, render_json, render_pretty};
use tui::{scan_accounts, tui};

const USAGE: &str = "\
usage: mdl [<file>[.md][:ledger]] [<command> [args...]]

  Named ledgers: ## Checking in usd.md is usd:checking (or usd.md:checking).
  Use lowercase heading words joined by hyphens: Emergency Fund -> emergency-fund.
  A bare document shows, checks, recalculates, or reports all its ledgers.
  Entry changes require file:ledger; balances stay independent.
  mdl init usd:checking Checking        append a named ledger (or create its file)
  mdl edit usd                         open the document with ledger tabs
  mdl edit usd:checking                open with Checking selected
  Tabs: Ctrl-P/N, Ctrl-Left/Right, or click; finish/cancel entries before switching.

  mdl                                  interactive screen over ./**/*.md
  mdl init <file> [--lang es|en] [title...]
                                       a new account; a file without a table
                                       gets one appended
  mdl --help | --version

Read
  mdl <file> [show] [--pretty|--markdown|--json|--csv] [period]
                                       the statement, with borders and totals
                                       by default; --markdown uses a GFM table
  mdl <file> show --graph [balance|debit|credit]... [period]
                                       show the statement and an inline chart
  mdl <file> graph [-o <png>] [balance|debit|credit]... [period]
                                       show or save the same chart as in the PDF
  mdl <file> print [-o <pdf>] [--graph [balance|debit|credit]...] [period]
                                       typeset to <file>.pdf, or <pdf>; --graph
                                       charts the balance, or each entry's amounts
  mdl <file> lint                      check dates and balances
  mdl <file> balance [options]         the closing balance and total
  mdl <file> <file>... show|print ...  several accounts as one (bank/*.md):
                                       merged by date, each description led
                                       by its account, one running balance
  mdl balance [options] <file>...      several accounts, and their sum
  mdl balance [--chart <chart.md>]     hierarchy; defaults to ./chart.md
             [--as-of YYYY-MM-DD]     closing balances on or before this date
                                       --total: only the total
                                       -q|--quiet: amounts only, one per line
                                       --pretty|--markdown|--json|--csv

Write
  mdl edit <file>                      interactive screen with this account open;
                                       creates the file or appends a table if needed;
                                       no automatic fetch or exit sync
  mdl <file> edit                      alias for mdl edit <file>
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
  mdl push [message...]                commit eligible tracked ledgers and push
  mdl sync [message...]                fetch, then push
  git config mdl.autocommit true       commit eligible ledgers on save
  git config mdl.autopush true         also push automatically (default: off)
  (exit commits eligible ledgers; push requires mdl.autopush)
  New files need a manual first commit. Non-ledgers, deletions, renames, and
  partially staged files are skipped. Pulls only fast-forward.

period      YYYY-MM [YYYY-MM] | this [N] | last [N]: months; this N ends with
            the current month, last N with the previous one; the statement
            opens with the balance before them
--date D    D is YYYY-MM-DD, today by default; a date before the last row
            inserts the entry in date order, after the rows on that day
amount      1234.50, or a computation: 100+200+50, 1000*40.50
            in the interactive screen, calculate directly in Debit, Credit or
            Balance; Enter evaluates and advances/saves, Tab evaluates and moves
";

const GRAPH_USAGE: &str = "\
Graphs
  mdl graph [period] [options]         chart total, groups, and child accounts
                                       defaults to chart.md and its full history
  --chart <path>                       select a different account chart
  --from YYYY-MM[-DD]                  inclusive start; default: first recorded month
  --to YYYY-MM[-DD]                    inclusive end; default: last recorded month
  --monthly                           month-end snapshots (daily closes by default)
                                       an exact --to also includes that day's close
  --total                             plot only the overall total
  -o <png>                            save the graph as a PNG
  mdl graph --help                     show this graph help

  period: YYYY-MM [YYYY-MM] | this [N] | last [N]
  Use either a period or --from/--to. Either bound may be omitted.
  Inline graphics use Ghostty/Kitty; other terminals show a snapshot table.
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
/// a table is refused. Git eligibility and automatic push settings apply to saves.
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
    if ledger::account_parts(file).1.is_some() {
        return init_ledger(file, title, &lang);
    }
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

/// Append an explicitly named ledger, without converting an unnamed ledger silently.
fn init_ledger(account: &str, title: &[String], lang: &str) -> Result<(), String> {
    let (path, id) = ledger::account_parts(account);
    let id = id.ok_or("expected file:ledger")?;
    if id.is_empty() || ledger::ledger_id(id) != id {
        return Err("ledger identifiers must use lowercase words separated by hyphens".into());
    }
    let title = if title.is_empty() { id.to_string() } else { title.join(" ") };
    if ledger::ledger_id(&title) != id {
        return Err(format!("heading `{title}` must produce the identifier `{id}`"));
    }
    let mut path = resolve(path);
    if !Path::new(&path).exists() && !path.ends_with(".md") { path.push_str(".md"); }
    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{path}: {e}")),
    };
    if existing.lines().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ")) {
        return Err(format!("{path}: unresolved merge conflict"));
    }
    if Path::new(&path).exists() {
        let tabs = ledger::ledger_tabs(&path)?;
        if tabs.iter().any(|(a, _)| ledger::account_parts(a).1 == Some(id)) {
            return Err(format!("{account}: ledger already exists"));
        }
        if tabs.is_empty() && find_table(&existing.lines().collect::<Vec<_>>()).is_ok() {
            return Err("add a ## heading to the existing ledger before adding another".into());
        }
    }
    let (_, _, table) = parts(&title, lang, &today())?;
    let mut text = existing;
    if !text.is_empty() {
        if !text.ends_with('\n') { text.push('\n'); }
        if !text.ends_with("\n\n") { text.push('\n'); }
    }
    text += &format!("## {title}\n\n{table}");
    fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
    println!("added {id} to {path}");
    let dir = Path::new(".");
    if git::account_in_tree(dir, &path) && git::autocommit(dir) {
        let note = git::commit_push(dir, &path, &format!("mdl: {}:{id} init", path.trim_end_matches(".md")))?;
        if note != "pushed" { eprintln!("{note}"); }
    }
    Ok(())
}

/// Prepare an explicit interactive account, preserving existing prose and tables.
fn prepare_edit(file: &str) -> Result<String, String> {
    if ledger::account_parts(file).1.is_some() {
        let resolved = resolve(file);
        let (path, id) = ledger::account_parts(&resolved);
        let exists = Path::new(path).exists() && ledger::ledger_tabs(path)?.iter()
            .any(|(name, _)| ledger::account_parts(name).1 == id);
        if !exists { init_ledger(file, &[], "en")?; }
        load(&resolved)?;
        return Ok(resolve(file));
    }
    let mut file = resolve(file);
    if !Path::new(&file).exists() && !file.ends_with(".md") {
        file.push_str(".md");
    }
    let existing = match fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{file}: {e}")),
    };
    if let Some(text) = &existing {
        let lines: Vec<&str> = text.lines().collect();
        if lines.iter().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ")) {
            return Err(format!("{file}: unresolved merge conflict"));
        }
        if find_table(&lines).is_ok() {
            // A malformed ledger is an error, not a reason to append another table.
            load(&ledger::first_account(&file)?)?;
            return Ok(file);
        }
    }
    let title = Path::new(&file).file_stem().unwrap_or_default().to_string_lossy();
    let text = init_text(existing.as_deref(), &title, "en", &today())?;
    fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    Ok(file)
}

fn cmd_edit(file: &str) -> Result<(), String> {
    let file = prepare_edit(file)?;
    tui(Some(&file))
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
    let mut accounts: Vec<String> = accounts.into_iter().flat_map(|a| {
        ledger::ledger_tabs(&a).ok().filter(|t| !t.is_empty()).map_or_else(|| vec![a], |t| t.into_iter().map(|(id, _)| id).collect())
    }).collect();
    accounts.sort();
    accounts.dedup();
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

/// `mdl push [message...]`: commit eligible ledger changes under the tree and push.
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

/// Both command orders share option parsing and rendering.
fn cmd_balance(initial_files: &[String], args: &[String]) -> Result<(), String> {
    let mut files = initial_files.to_vec();
    let mut format = None;
    let mut only_total = false;
    let mut positional = false;
    let mut chart = None;
    let mut as_of = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if positional {
            files.push(resolve(arg));
            continue;
        }
        let selected = match arg.as_str() {
            "--" => { positional = true; continue; }
            "--total" => { only_total = true; continue; }
            "--chart" | "--as-of" => {
                let value = args.next().filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("balance: {arg} needs a value"))?;
                let slot = if arg == "--chart" { &mut chart } else { &mut as_of };
                if slot.replace(value.as_str()).is_some() {
                    return Err(format!("balance: repeated {arg}"));
                }
                continue;
            }
            "-q" | "--quiet" => "quiet",
            "--pretty" => "pretty",
            "--markdown" => "markdown",
            "--json" => "json",
            "--csv" => "csv",
            _ if arg.starts_with('-') => return Err(format!("balance: unknown option {arg}")),
            _ => { files.push(resolve(arg)); continue; }
        };
        if format.is_some_and(|f| f != selected) {
            return Err("balance: choose only one of --quiet, --pretty, --markdown, --json, or --csv".into());
        }
        format = Some(selected);
    }
    if let Some(date) = as_of { account_chart::validate_date(date)?; }
    if chart.is_some() && !files.is_empty() {
        return Err("balance: --chart cannot be combined with account files".into());
    }
    if files.is_empty() {
        let file = chart.or_else(|| Path::new("chart.md").exists().then_some("chart.md"))
            .ok_or("balance needs account files or a chart.md (or --chart path)")?;
        let chart = account_chart::load_chart(file, as_of)?;
        print!("{}", account_chart::render(&chart, as_of, format.unwrap_or("pretty"), only_total, io::stdout().is_terminal()));
        return Ok(());
    }
    let files = ledger::expand_accounts(&files)?;
    let accounts = files.iter().map(|f| Ok((if ledger::account_parts(f).1.is_some() { ledger::account_name(f) } else { f.clone() }, account_chart::account_balance(f, as_of)?)))
        .collect::<Result<Vec<_>, String>>()?;
    print!("{}", render::render_balances(&accounts, format.unwrap_or("pretty"), only_total, io::stdout().is_terminal()));
    Ok(())
}

/// Chart-level evolution, distinct from the existing per-ledger graph command.
fn cmd_chart_graph(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{GRAPH_USAGE}");
        return Ok(());
    }
    let mut file = None;
    let mut out = None;
    let mut from = None;
    let mut to = None;
    let mut monthly = false;
    let mut only_total = false;
    let mut words = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--chart" | "-o" | "--from" | "--to" => {
                let value = args.next().filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("graph: {arg} needs a value"))?;
                let slot = match arg.as_str() {
                    "--chart" => &mut file,
                    "--from" => &mut from,
                    "--to" => &mut to,
                    _ => &mut out,
                };
                if slot.replace(value.as_str()).is_some() { return Err(format!("graph: repeated {arg}")); }
            }
            "--monthly" => monthly = true,
            "--total" => only_total = true,
            _ if arg.starts_with('-') => return Err(format!("graph: unknown option {arg}")),
            _ => words.push(arg.clone()),
        }
    }
    if !words.is_empty() && (from.is_some() || to.is_some()) {
        return Err("graph: use either a period or --from/--to".into());
    }
    let period = period(&words, &today())?;
    let chart = account_chart::load_chart(file.unwrap_or("chart.md"), None)?;
    let data = account_chart::evolution(&chart, &period, (from, to), monthly, only_total)?;
    if out.is_some() || chart::supported() {
        let png = evolution_plot::png(&data)?;
        if let Some(path) = out {
            fs::write(path, &png).map_err(|e| format!("{path}: {e}"))?;
            println!("wrote {path}");
        }
        if chart::supported() { chart::display(&png).map_err(|e| format!("graph: {e}"))?; }
    } else {
        print!("{}", account_chart::render_evolution(&data));
    }
    Ok(())
}

/// Keep each account's own running balance when charting a combined statement.
fn chart_accounts(files: &[String], p: &Option<(String, String)>) -> Result<Vec<(String, ledger::Doc)>, String> {
    if files.len() < 2 {
        return Ok(vec![]);
    }
    files.iter().map(|file| {
        let name = ledger::account_name(file);
        Ok((name, scoped(load(file)?, p)))
    }).collect()
}

/// Display document children independently; explicit account lists retain combined statements.
fn cmd_show(files: &[String], rest: &[String], separate: bool) -> Result<(), String> {
    let graph = rest.iter().any(|a| a == "--graph");
    let series: Vec<String> = if graph { rest.iter().filter(|a| ["balance", "debit", "credit"].contains(&a.as_str())).cloned().collect() } else { vec![] };
    let json = rest.iter().any(|a| a == "--json");
    let csv = rest.iter().any(|a| a == "--csv");
    let markdown = rest.iter().any(|a| a == "--markdown");
    let explicit_pretty = rest.iter().any(|a| a == "--pretty");
    if json as u8 + csv as u8 + markdown as u8 + explicit_pretty as u8 > 1 {
        bad("show: choose only one of --pretty, --markdown, --json, or --csv");
    }
    if graph && (json || csv || markdown) {
        bad("show: --graph requires the default or --pretty output");
    }
    let pretty = !json && !csv && !markdown;
    let words: Vec<String> = rest.iter().filter(|a| {
        !["--json", "--csv", "--pretty", "--markdown"].contains(&a.as_str())
            && !(graph && ["--graph", "balance", "debit", "credit"].contains(&a.as_str()))
    }).cloned().collect();
    let p = period(&words, &today())?;
    // Parse all ledgers before producing output, including structured exports.
    let docs = if separate {
        files.iter().map(|f| load(f).map(|d| scoped(d, &p))).collect::<Result<Vec<_>, _>>()?
    } else {
        vec![scoped(if files.len() == 1 { load(&files[0])? } else { ledger::combine(files)? }, &p)]
    };
    if separate && json { print!("{{\"accounts\":["); }
    for (index, doc) in docs.into_iter().enumerate() {
        let file = &files[index];
        if json {
            let title = if separate { Some(ledger::account_name(file)) } else { doc.title().map(|t| t[2..].trim().to_string()) };
            if separate && index > 0 { print!(","); }
            print!("{}", render_json(title.as_deref(), &doc.rows));
        } else if csv {
            let output = render_csv(&doc.header, &doc.rows);
            if separate {
                if index == 0 { println!("Account,{}", output.lines().next().unwrap_or_default()); }
                let name = render::csv_str(&ledger::account_name(file));
                for line in output.lines().skip(1) { println!("{name},{line}"); }
            } else { print!("{output}"); }
        } else {
            if separate {
                if index > 0 { println!(); }
                let name = ledger::account_name(file);
                println!("{}{name}\n", if pretty { "" } else { "## " });
            } else if let Some(t) = doc.title() {
                println!("{}\n", if pretty { t[2..].trim() } else { t });
            }
            print!("{}", if pretty {
                render_pretty(&doc.header, &doc.rows, io::stdout().is_terminal())
            } else {
                render(&doc.header, &doc.rows)
            });
        }
        if graph && chart::supported() {
            let series = if series.is_empty() { vec!["balance".to_string()] } else { series.clone() };
            let accounts = if separate { vec![] } else { chart_accounts(files, &p)? };
            let png = if accounts.is_empty() { graph_png(&doc, &p, &series)? } else { graph_png_accounts(&doc, &p, &series, &accounts)? };
            if let Some(png) = png {
                chart::display(&png).map_err(|e| format!("graph: {e}"))?;
            }
        }
    }
    if separate && json { println!("]}}"); }
    Ok(())
}

/// Lint every account, or recalculate each ledger independently after parsing all targets.
fn cmd_check(files: &[String], cmd: &str, rest: &[String]) -> Result<(), String> {
    if (cmd == "lint" && !rest.is_empty()) || (cmd == "recalc" && !rest.is_empty() && rest != ["--dry-run"]) {
        return Err(format!("{cmd}: unexpected arguments"));
    }
    let mut docs = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        match load(file) {
            Ok(doc) => {
                let issues = if cmd == "lint" { lint(&doc.rows) }
                    else if !rest.is_empty() { recalc_diff(&doc.rows) } else { vec![] };
                errors.extend(issues.into_iter().map(|e| format!("{}: {e}", ledger::account_name(file))));
                docs.push((file, doc));
            }
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() { return Err(errors.join("\n")); }
    if cmd == "lint" || !rest.is_empty() { return Ok(()); }
    for (file, doc) in &mut docs {
        recalc(&mut doc.rows);
        doc.save(file)?;
    }
    let mut saved = std::collections::HashSet::new();
    for (file, _) in docs {
        let path = ledger::account_parts(file).0;
        if saved.insert(path) && git::account_in_tree(Path::new("."), path) && git::autocommit(Path::new(".")) {
            let note = git::commit_push(Path::new("."), path, &format!("mdl: {} recalc", ledger::account_name(path)))?;
            if note != "pushed" { eprintln!("{note}"); }
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return tui(None);
    }
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        print!("{USAGE}\n{GRAPH_USAGE}");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--version") {
        println!("mdl {} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"));
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "balance") {
        return cmd_balance(&[], &args[1..]);
    }
    if args.first().is_some_and(|a| a == "graph") {
        return cmd_chart_graph(&args[1..]);
    }
    match args.as_slice() {
        [c] if c == "fetch" => return cmd_fetch(),
        [c, m @ ..] if c == "push" => return cmd_push(m),
        [c, r @ ..] if c == "init" => return cmd_init(r),
        [c, rest @ ..] if c == "edit" => {
            let [file] = rest else { bad("edit needs exactly one file name") };
            return cmd_edit(file);
        }
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
    // command that names an existing file. show, print and graph combine them.
    const COMMANDS: [&str; 13] = ["show", "print", "graph", "lint", "balance", "recalc", "debit", "credit", "note", "edit", "move", "flag", "delete"];
    let n = 1 + args[1..].iter().take_while(|a| !COMMANDS.contains(&a.as_str()) && Path::new(ledger::account_parts(&resolve(a)).0).is_file()).count();
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
    if cmd == "balance" {
        return cmd_balance(&files, rest);
    }
    if cmd == "edit" && rest.is_empty() && files.len() == 1 { return cmd_edit(&files[0]); }
    let expanded = ledger::expand_accounts(&files)?;
    let document = expanded != files;
    if cmd == "show" { return cmd_show(&expanded, rest, document); }
    if cmd == "lint" || cmd == "recalc" { return cmd_check(&expanded, cmd, rest); }
    if document && (cmd == "print" || cmd == "graph") {
        if expanded.len() > 1 && rest.iter().any(|a| a == "-o") {
            return Err(format!("{cmd}: select a specific account when using -o with a multi-ledger document"));
        }
        // Validate all targets before creating any output files.
        for file in &expanded { load(file)?; }
        for file in &expanded { run_command(std::slice::from_ref(file), cmd, rest)?; }
        return Ok(());
    }
    if document {
        return Err(format!("{cmd}: select a specific account: {}", expanded.iter().map(|f| ledger::account_name(f)).collect::<Vec<_>>().join(", ")));
    }
    run_command(&files, cmd, rest)
}

fn run_command(files: &[String], cmd: &str, rest: &[String]) -> Result<(), String> {
    let combined;
    let file = if files.len() == 1 {
        &files[0]
    } else if cmd == "show" || cmd == "print" || cmd == "graph" {
        // ponytail: the PDF is named after every stem; a long glob makes a long name
        let stems: Vec<String> = files.iter().map(|f| {
            let (path, id) = ledger::account_parts(f);
            let stem = Path::new(path).file_stem().unwrap_or_default().to_string_lossy();
            id.map_or_else(|| stem.to_string(), |id| format!("{stem}-{id}"))
        }).collect();
        combined = Path::new(ledger::account_parts(&files[0]).0).with_file_name(stems.join("+")).with_extension("md").to_string_lossy().into_owned();
        &combined
    } else {
        bad(&format!("{cmd} takes one account; only show, print and graph combine several"))
    };
    if cmd == "edit" && rest.is_empty() {
        return cmd_edit(file);
    }
    let mut doc = if files.len() == 1 { load(file)? } else { ledger::combine(&files)? };
    let what: String;

    match cmd {
        "graph" => {
            let mut rest = rest.to_vec();
            let out = match rest.iter().position(|w| w == "-o") {
                Some(i) if i + 1 < rest.len() => Some(rest.drain(i..i + 2).nth(1).unwrap()),
                Some(_) => bad("graph: -o needs a PNG file name"),
                None => None,
            };
            let series: Vec<String> = rest.iter().filter(|w| ["balance", "debit", "credit"].contains(&w.as_str())).cloned().collect();
            let words: Vec<String> = rest.into_iter().filter(|w| !["balance", "debit", "credit"].contains(&w.as_str())).collect();
            let p = period(&words, &today())?;
            let doc = scoped(doc, &p);
            if out.is_some() || chart::supported() {
                let series = if series.is_empty() { vec!["balance".to_string()] } else { series };
                let accounts = chart_accounts(&files, &p)?;
                let png = if accounts.is_empty() { graph_png(&doc, &p, &series)? } else { graph_png_accounts(&doc, &p, &series, &accounts)? };
                if let Some(png) = png {
                    if let Some(path) = out {
                        fs::write(&path, &png).map_err(|e| format!("{path}: {e}"))?;
                        println!("wrote {path}");
                    }
                    if chart::supported() {
                        chart::display(&png).map_err(|e| format!("graph: {e}"))?;
                    }
                } else {
                    return Err("no graph data for the selected period and series".into());
                }
            } else {
                println!("Inline graphics need Ghostty or Kitty; use -o chart.png to save the graph.");
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
            let accounts = if series.is_empty() { vec![] } else { chart_accounts(&files, &p)? };
            println!("{}", print_pdf(&doc, file, &p, &series, &accounts, out.as_deref())?);
            return Ok(());
        }
        "debit" | "credit" | "note" => {
            let (date, rest) = match rest {
                [f, d, r @ ..] if f == "--date" => (d.clone(), r),
                _ => (today(), rest),
            };
            let (debit, credit, desc) = match (cmd, rest) {
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
    // Rust ignores SIGPIPE at startup, so `mdl ... | head` panics with
    // "failed printing to stdout". Restore the Unix default: die quietly.
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        signal(13 /* SIGPIPE */, 0 /* SIG_DFL */);
    }
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

    /// Two clones of a bare remote: explicit enrollment, push, fast-forward, divergence.
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
        assert!(git::pending(&a).unwrap().is_empty());
        sh(&a, &["add", "caja.md"]);
        sh(&a, &["commit", "--quiet", "-m", "Enroll caja"]);
        let (_, note) = push_pending(&a, "").unwrap();
        assert_eq!(note, "pushed"); // first push creates the upstream
        assert_eq!(git::sync(&a).unwrap(), git::State::UpToDate);

        clone(&b);
        fs::write(a.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n")).unwrap();
        assert_eq!(push_pending(&a, "from a").unwrap().1, "committed: from a; pushed");
        assert_eq!(git::pull(&b).unwrap(), (true, "fast-forwarded to the upstream"));
        assert!(fs::read_to_string(b.join("caja.md")).unwrap().contains("| a |"));

        // Divergence on different files requires a manual merge.
        fs::write(a.join("banco.md"), table).unwrap();
        sh(&a, &["add", "banco.md"]);
        sh(&a, &["commit", "--quiet", "-m", "Enroll banco"]);
        push_pending(&a, "").unwrap();
        fs::write(b.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n| 2026-09-03 | b | | 0.50 | 1.50 |\n")).unwrap();
        assert!(git::commit_all(&b, "from b").unwrap());
        assert_eq!(git::sync(&b).unwrap(), git::State::Diverged);
        assert!(git::pull(&b).unwrap_err().contains("diverged"));
        assert!(!b.join("banco.md").exists());
        sh(&b, &["merge", "--no-edit", "@{u}"]);
        assert!(b.join("banco.md").exists());
        assert_eq!(git::push(&b), "pushed");

        // Divergence at the same tail also leaves the working tree untouched.
        fs::write(a.join("caja.md"), format!("{table}| 2026-09-02 | a | 1.00 | | 2.00 |\n| 2026-09-04 | x | 1.00 | | 3.00 |\n")).unwrap();
        assert!(git::commit_all(&a, "a again").unwrap());
        assert_eq!(git::sync(&a).unwrap(), git::State::Diverged);
        let err = git::pull(&a).unwrap_err();
        assert!(err.contains("diverged"), "{err}");
        assert!(load(a.join("caja.md").to_str().unwrap()).is_ok());
        assert!(!a.join(".git/MERGE_HEAD").exists());
        assert!(!git::autocommit(&a));
        sh(&a, &["config", "mdl.autocommit", "true"]);
        assert!(git::autocommit(&a));
        let _ = fs::remove_dir_all(&base);
    }
}
