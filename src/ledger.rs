//! The ledger: entries and the document around the table, amounts, dates, periods,
//! `#` expressions, lint, recalc, and saving (with a commit when the repository asks).
use std::time::{SystemTime, UNIX_EPOCH};
use std::fs;
use std::path::Path;

use crate::git;
use crate::render::render;

#[derive(Clone)]
pub struct Entry {
    pub date: String,
    pub desc: String,
    pub debit: i64,
    pub credit: i64,
    pub balance: i64,
    /// Flagged: `**description**` in the file, bold on screen. An eye-catcher for
    /// reconciliation (say, where the balance matched the bank's).
    pub bold: bool,
}

pub fn parse_amount(s: &str) -> Result<i64, String> {
    if s.is_empty() || s == "-" {
        return Ok(0);
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let digits = int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit());
    if int.is_empty() || frac.len() > 2 || !digits {
        return Err(format!("bad amount `{s}`"));
    }
    let cents = int.parse::<i64>().unwrap() * 100 + format!("{frac:0<2}").parse::<i64>().unwrap();
    Ok(if neg { -cents } else { cents })
}

pub fn fmt_amount(c: i64) -> String {
    format!("{}{}.{:02}", if c < 0 { "-" } else { "" }, c.abs() / 100, c.abs() % 100)
}

pub fn fmt_col(c: i64) -> String {
    if c == 0 { String::new() } else { fmt_amount(c) }
}

fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && d.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && (1..=12).contains(&d[5..7].parse::<u8>().unwrap())
        && (1..=31).contains(&d[8..10].parse::<u8>().unwrap())
}

/// `YYYY-MM`.
fn valid_month(m: &str) -> bool {
    m.len() == 7 && valid_date(&format!("{m}-01"))
}

/// `ym` shifted by `n` months.
pub fn month_add(ym: &str, n: i64) -> String {
    let t = ym[..4].parse::<i64>().unwrap() * 12 + ym[5..7].parse::<i64>().unwrap() - 1 + n;
    format!("{:04}-{:02}", t.div_euclid(12), t.rem_euclid(12) + 1)
}

/// The months a statement covers, from the words after the command: none (the whole
/// ledger), `YYYY-MM`, `YYYY-MM YYYY-MM` (inclusive), `this`, `last`, or `last N` (the N
/// months before the current one).
pub fn period(words: &[String], today: &str) -> Result<Option<(String, String)>, String> {
    let this = &today[..7];
    let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    Ok(Some(match w[..] {
        [] => return Ok(None),
        ["this"] => (this.to_string(), this.to_string()),
        ["last"] => (month_add(this, -1), month_add(this, -1)),
        ["last", n] => {
            let n: i64 = n.parse().ok().filter(|n| *n > 0).ok_or(format!("bad month count `{n}`"))?;
            (month_add(this, -n), month_add(this, -1))
        }
        [a] if valid_month(a) => (a.to_string(), a.to_string()),
        [a, b] if valid_month(a) && valid_month(b) && a <= b => (a.to_string(), b.to_string()),
        _ => return Err(format!("bad period `{}`: YYYY-MM [YYYY-MM] | this | last [N]", w.join(" "))),
    }))
}

/// `from` alone, or `from – to`.
pub fn period_label(from: &str, to: &str) -> String {
    if from == to { from.to_string() } else { format!("{from} – {to}") }
}

/// The document narrowed to the months `from..=to`: their rows behind an opening row
/// dated the first of `from`, described by the balance column's label, carrying the
/// balance before the period (rows are in date order, as lint requires).
pub fn scoped(doc: Doc, p: &Option<(String, String)>) -> Doc {
    let Some((from, to)) = p else { return doc };
    let before = doc.rows.iter().take_while(|e| &e.date[..7] < from.as_str()).count();
    let balance = before.checked_sub(1).map_or(0, |i| doc.rows[i].balance);
    let opening = Entry { date: format!("{from}-01"), desc: doc.header[4].clone(), debit: 0, credit: 0, balance, bold: false };
    let rows = std::iter::once(opening).chain(doc.rows[before..].iter().filter(|e| &e.date[..7] <= to.as_str()).cloned()).collect();
    Doc { rows, ..doc }
}

// ponytail: UTC date, no local tz in stdlib; use --date to override.
pub fn today() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 / 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

// ponytail: no `\|` escaping; descriptions can't contain a pipe.
fn cells(line: &str) -> Option<Vec<String>> {
    let l = line.trim();
    let l = l.strip_prefix('|')?;
    let l = l.strip_suffix('|').unwrap_or(l);
    Some(l.split('|').map(|c| c.trim().to_string()).collect())
}

fn is_separator(line: &str) -> bool {
    cells(line).is_some_and(|c| !c.is_empty() && c.iter().all(|c| c.trim_matches(':').chars().all(|ch| ch == '-') && c.contains('-')))
}

/// (header_line, end_line_exclusive, header_labels) of the first 5-column table.
pub fn find_table(lines: &[&str]) -> Result<(usize, usize, Vec<String>), String> {
    let start = lines
        .windows(2)
        .position(|w| cells(w[0]).is_some_and(|c| c.len() == 5) && is_separator(w[1]))
        .ok_or("no 5-column table found")?;
    let end = lines[start + 2..].iter().position(|l| cells(l).is_none()).map_or(lines.len(), |n| start + 2 + n);
    Ok((start, end, cells(lines[start]).unwrap()))
}

/// `first_line` is the 1-based file line number of `lines[0]`, for messages.
fn parse_rows(lines: &[&str], first_line: usize) -> Result<Vec<Entry>, String> {
    let mut rows = vec![];
    for (i, raw) in lines.iter().enumerate() {
        let ln = first_line + i;
        let c = cells(raw).filter(|c| c.len() == 5).ok_or(format!("line {ln}: expected 5 columns"))?;
        if !valid_date(&c[0]) {
            return Err(format!("line {ln}: bad date `{}`", c[0]));
        }
        if c[1].is_empty() {
            return Err(format!("line {ln}: missing description"));
        }
        let amt = |t: &str| parse_amount(t).map_err(|e| format!("line {ln}: {e}"));
        let (desc, bold) = match c[1].strip_prefix("**").and_then(|d| d.strip_suffix("**")) {
            Some(d) if !d.is_empty() => (d.to_string(), true),
            _ => (c[1].clone(), false),
        };
        rows.push(Entry { date: c[0].clone(), desc, debit: amt(&c[2])?, credit: amt(&c[3])?, balance: amt(&c[4])?, bold });
    }
    Ok(rows)
}

/// `+ - * /`, parentheses, unary minus, decimals: the value in cents, rounded half away
/// from zero. f64 is exact for ledger-sized sums and products; only division rounds.
fn eval_expr(s: &str) -> Result<i64, String> {
    struct P {
        c: Vec<char>,
        i: usize,
    }
    impl P {
        fn peek(&mut self) -> Option<char> {
            while self.c.get(self.i) == Some(&' ') {
                self.i += 1;
            }
            self.c.get(self.i).copied()
        }
        fn expr(&mut self) -> Result<f64, String> {
            let mut v = self.term()?;
            loop {
                match self.peek() {
                    Some('+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some('-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn term(&mut self) -> Result<f64, String> {
            let mut v = self.factor()?;
            loop {
                match self.peek() {
                    Some('*') => {
                        self.i += 1;
                        v *= self.factor()?;
                    }
                    Some('/') => {
                        self.i += 1;
                        let d = self.factor()?;
                        if d == 0.0 {
                            return Err("division by zero".into());
                        }
                        v /= d;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn factor(&mut self) -> Result<f64, String> {
            match self.peek() {
                Some('-') => {
                    self.i += 1;
                    Ok(-self.factor()?)
                }
                Some('(') => {
                    self.i += 1;
                    let v = self.expr()?;
                    if self.peek() != Some(')') {
                        return Err("missing `)`".into());
                    }
                    self.i += 1;
                    Ok(v)
                }
                Some(c) if c.is_ascii_digit() || c == '.' => {
                    let start = self.i;
                    while self.c.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                        self.i += 1;
                    }
                    let t: String = self.c[start..self.i].iter().collect();
                    t.parse::<f64>().map_err(|_| format!("bad number `{t}`"))
                }
                Some(c) => Err(format!("unexpected `{c}`")),
                None => Err("missing operand".into()),
            }
        }
    }
    let mut p = P { c: s.chars().collect(), i: 0 };
    let v = p.expr()?;
    if let Some(c) = p.peek() {
        return Err(format!("unexpected `{c}`"));
    }
    if !v.is_finite() {
        return Err("not a number".into());
    }
    let cents = v * 100.0;
    Ok((cents + cents.signum() * 1e-6).round() as i64)
}

/// The computation a description starts with: a `#` and an expression with at least one
/// operator, e.g. `#1000*40.50 currency exchange` or `#100+200+50 varios`. Its value is
/// the row's effect on the balance: positive is a debit, negative (`#-1000*40.50 venta
/// USD`) a credit. Without the `#` a description is prose, however it looks (`2-3
/// people`, `1000*40.50 cambio`), and so is `#123 invoice`: a `#` with just a number.
/// None: no computation. Some(Err): a computation that does not evaluate.
pub fn desc_expr(desc: &str) -> Option<Result<i64, String>> {
    let body = desc.strip_prefix('#')?;
    let expr: String = body.chars().take_while(|c| c.is_ascii_digit() || " .+-*/()".contains(*c)).collect();
    let expr = expr.trim();
    if expr.is_empty() || !expr.starts_with(|c: char| c.is_ascii_digit() || c == '(' || c == '-') || !expr.contains(['+', '-', '*', '/', '(']) {
        return None;
    }
    Some(eval_expr(expr).map_err(|e| format!("bad expression `{expr}`: {e}")))
}

/// An amount as typed, or a computation (`1000*40.50`).
pub fn amount_arg(s: &str) -> Result<i64, String> {
    if s != "-" && s.contains(['+', '*', '/', '(']) || s.trim_start_matches('-').contains('-') {
        eval_expr(s)
    } else {
        parse_amount(s)
    }
}

pub fn lint(rows: &[Entry]) -> Vec<String> {
    let mut errs = vec![];
    let mut bal = 0;
    for (i, e) in rows.iter().enumerate() {
        let n = i + 1;
        if i > 0 && e.date < rows[i - 1].date {
            errs.push(format!("row {n}: date {} before previous row", e.date));
        }
        if e.debit != 0 && e.credit != 0 {
            errs.push(format!("row {n}: both debit and credit set"));
        }
        if e.debit < 0 || e.credit < 0 {
            errs.push(format!("row {n}: negative debit/credit"));
        }
        bal += e.debit - e.credit;
        if e.balance != bal {
            errs.push(format!("row {n}: balance {} should be {}", fmt_amount(e.balance), fmt_amount(bal)));
        }
        match desc_expr(&e.desc) {
            Some(Ok(v)) if v != e.debit - e.credit => {
                errs.push(format!("row {n}: description computes {} but the row is {}", fmt_amount(v), fmt_amount(e.debit - e.credit)));
            }
            Some(Err(err)) => errs.push(format!("row {n}: {err}")),
            _ => {}
        }
    }
    errs
}

/// What `recalc` would change: one message per row whose balance is stale.
pub fn recalc_diff(rows: &[Entry]) -> Vec<String> {
    let mut bal = 0;
    let mut out = vec![];
    for (i, e) in rows.iter().enumerate() {
        bal += e.debit - e.credit;
        if e.balance != bal {
            out.push(format!("row {}: balance {} should be {}", i + 1, fmt_amount(e.balance), fmt_amount(bal)));
        }
    }
    out
}

pub fn recalc(rows: &mut [Entry]) {
    let mut bal = 0;
    for e in rows {
        bal += e.debit - e.credit;
        e.balance = bal;
    }
}

pub struct Doc {
    pub lines: Vec<String>,
    pub start: usize,
    pub end: usize,
    pub header: Vec<String>,
    pub rows: Vec<Entry>,
}

/// `foo` resolves to `foo.md` when `foo` does not exist but `foo.md` does.
pub fn resolve(file: &str) -> String {
    let with_md = format!("{file}.md");
    if !Path::new(file).exists() && Path::new(&with_md).exists() {
        with_md
    } else {
        file.to_string()
    }
}

pub fn load(file: &str) -> Result<Doc, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    // a marker inside the table would otherwise just end it, silently dropping rows
    if lines.iter().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ")) {
        return Err(format!("{file}: unresolved merge conflict"));
    }
    let (start, end, header) = find_table(&lines).map_err(|e| format!("{file}: {e}"))?;
    let rows = parse_rows(&lines[start + 2..end], start + 3).map_err(|e| format!("{file}: {e}"))?;
    Ok(Doc { lines: lines.iter().map(|l| l.to_string()).collect(), start, end, header, rows })
}

impl Doc {
    pub fn title(&self) -> Option<&str> {
        self.lines.iter().find(|l| l.starts_with("# ")).map(|l| l.as_str())
    }

    pub fn save(&self, file: &str) -> Result<(), String> {
        let mut out = self.lines[..self.start].join("\n");
        if self.start > 0 {
            out += "\n";
        }
        out += &render(&self.header, &self.rows);
        out += &self.lines[self.end..].join("\n");
        out += "\n";
        fs::write(file, out).map_err(|e| format!("{file}: {e}"))
    }
}

/// Save; with `mdl.autocommit` set in the repository, also commit the file and push
/// best-effort. Returns git's note, or "" when git is not involved.
pub fn save_commit(file: &str, doc: &Doc, what: &str) -> Result<String, String> {
    doc.save(file)?;
    let dir = Path::new(".");
    if !git::is_repo(dir) || !git::autocommit(dir) {
        return Ok(String::new());
    }
    git::commit_push(dir, file, &format!("mdl: {} {what}", file.trim_end_matches(".md")))
}

/// What to call a commit nobody named.
pub fn default_commit_message(paths: &[String]) -> String {
    let accounts: Vec<&str> = paths.iter().filter_map(|p| p.strip_suffix(".md")).collect();
    match (accounts.len(), paths.len()) {
        (0, 0) => "mdl: no changes".into(),
        (1, _) => format!("mdl: update {}", accounts[0]),
        (0, 1) => format!("mdl: update {}", paths[0]),
        (0, n) => format!("mdl: update {n} files"),
        (n, _) => format!("mdl: update {n} accounts"),
    }
}

/// Commit whatever is pending under `dir` (with `msg`, or a generated one) and push.
/// Returns the pending paths and a note; the note contains "push failed" when it did.
pub fn push_pending(dir: &Path, msg: &str) -> Result<(Vec<String>, String), String> {
    let pending = git::pending(dir)?;
    if pending.is_empty() && git::has_upstream(dir) && git::unpushed(dir) == 0 {
        return Ok((pending, "nothing to push".into()));
    }
    let mut note = String::new();
    if !pending.is_empty() {
        let msg = if msg.trim().is_empty() { default_commit_message(&pending) } else { msg.trim().to_string() };
        if git::commit_all(dir, &msg)? {
            note = format!("committed: {msg}; ");
        }
    }
    note += &git::push(dir);
    Ok((pending, note))
}

pub fn balance(rows: &[Entry]) -> i64 {
    rows.last().map_or(0, |e| e.balance)
}

fn check_entry(date: &str, debit: i64, credit: i64, desc: &str) -> Result<(), String> {
    if !valid_date(date) {
        return Err(format!("bad date `{date}`"));
    }
    if desc.is_empty() {
        return Err("missing description".into());
    }
    if desc.contains('|') {
        return Err("description can't contain `|`".into());
    }
    if debit < 0 || credit < 0 || (debit > 0 && credit > 0) {
        return Err("one positive amount, debit or credit (or neither for a note)".into());
    }
    Ok(())
}

/// What a save of this entry is called in a commit message and on screen: `debit 1.00
/// desc`, `credit 1.00 desc`, or `note desc`.
pub fn entry_what(debit: i64, credit: i64, desc: &str) -> String {
    match (debit, credit) {
        (0, 0) => format!("note {desc}"),
        (d, 0) => format!("debit {} {desc}", fmt_amount(d)),
        (_, c) => format!("credit {} {desc}", fmt_amount(c)),
    }
}

/// Add the entry in date order: at the end, or, dated before the last row, after the
/// last row on or before its date (a payment or a note recorded late). The balances
/// from there on are recomputed. Returns its 0-based row.
pub fn add_entry(rows: &mut Vec<Entry>, date: String, debit: i64, credit: i64, desc: String) -> Result<usize, String> {
    check_entry(&date, debit, credit, &desc)?;
    let i = rows.iter().rposition(|e| e.date <= date).map_or(0, |i| i + 1);
    rows.insert(i, Entry { date, desc, debit, credit, balance: 0, bold: false });
    recalc(rows);
    Ok(i)
}

/// Replace row `i` (0-based). The date has to keep the order with its neighbours;
/// moving is what `move` is for.
pub fn edit_entry(rows: &mut [Entry], i: usize, date: String, debit: i64, credit: i64, desc: String) -> Result<(), String> {
    if i >= rows.len() {
        return Err(format!("row {}: no such row", i + 1));
    }
    check_entry(&date, debit, credit, &desc)?;
    if i > 0 && date < rows[i - 1].date {
        return Err(format!("date {date} is before row {} ({})", i, rows[i - 1].date));
    }
    if i + 1 < rows.len() && date > rows[i + 1].date {
        return Err(format!("date {date} is after row {} ({})", i + 2, rows[i + 1].date));
    }
    let bold = rows[i].bold;
    rows[i] = Entry { date, desc, debit, credit, balance: 0, bold };
    recalc(rows);
    Ok(())
}

/// Insert a new row right below row `i` (0-based). Its date has to keep the order with
/// rows `i` and `i + 1`.
pub fn insert_entry(rows: &mut Vec<Entry>, i: usize, date: String, debit: i64, credit: i64, desc: String) -> Result<(), String> {
    if i >= rows.len() {
        return Err(format!("row {}: no such row", i + 1));
    }
    check_entry(&date, debit, credit, &desc)?;
    if date < rows[i].date {
        return Err(format!("date {date} is before row {} ({})", i + 1, rows[i].date));
    }
    if i + 1 < rows.len() && date > rows[i + 1].date {
        return Err(format!("date {date} is after row {} ({})", i + 2, rows[i + 1].date));
    }
    rows.insert(i + 1, Entry { date, desc, debit, credit, balance: 0, bold: false });
    recalc(rows);
    Ok(())
}

/// Swap row `i` (0-based) with the one above (`up`) or below. Crossing into a different
/// day adopts that row's date, so the order stays valid. Returns the new index.
pub fn move_entry(rows: &mut [Entry], i: usize, up: bool) -> Result<usize, String> {
    if i >= rows.len() {
        return Err(format!("row {}: no such row", i + 1));
    }
    let j = match up {
        true => i.checked_sub(1).ok_or(format!("row {}: already first", i + 1))?,
        false if i + 1 < rows.len() => i + 1,
        false => return Err(format!("row {}: already last", i + 1)),
    };
    rows[i].date = rows[j].date.clone();
    rows.swap(i, j);
    recalc(rows);
    Ok(j)
}

/// Remove row `i` (0-based) and return it.
pub fn delete_entry(rows: &mut Vec<Entry>, i: usize) -> Result<Entry, String> {
    if i >= rows.len() {
        return Err(format!("row {}: no such row", i + 1));
    }
    let e = rows.remove(i);
    recalc(rows);
    Ok(e)
}

/// A 1-based row number as typed, checked against the table.
pub fn row_index(n: &str, len: usize) -> Result<usize, String> {
    let n: usize = n.parse().map_err(|_| format!("bad row `{n}`"))?;
    if n == 0 || n > len {
        return Err(format!("row {n}: no such row (1..={len})"));
    }
    Ok(n - 1)
}

/// `cash.md`, the sample account, cut to its first `n` rows: the shape the interactive
/// tests work on (five rows, all before today, so an entry dated today lands last).
#[cfg(test)]
pub fn cash_head(n: usize) -> String {
    let mut rows = 0;
    std::fs::read_to_string("cash.md")
        .unwrap()
        .lines()
        .filter(|l| {
            if !l.starts_with("| 20") {
                return true;
            }
            rows += 1;
            rows <= n
        })
        .fold(String::new(), |s, l| s + l + "\n")
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::render::*;
    use crate::tui::tui_widths;

    pub const DOC: &str = "# Caja\n\n| Fecha | Descripción | Debe | Haber | Saldo |\n|---|---|--:|--:|--:|\n| 2026-09-01 | Saldo inicial | | | 0.00 |\n| 2026-09-03 | Venta mostrador | 150.00 | | 150.00 |\n| 2026-09-05 | Pago proveedor | | 80.00 | 70.00 |\n\ntail\n";

    /// The header and rows of a document held in a string.
    pub fn load_str(text: &str) -> (Vec<String>, Vec<Entry>) {
        let lines: Vec<&str> = text.lines().collect();
        let (s, e, header) = find_table(&lines).unwrap();
        (header, parse_rows(&lines[s + 2..e], s + 3).unwrap())
    }

    #[test]
    fn notes_have_no_amount() {
        let mut rows = load_str(DOC).1;
        add_entry(&mut rows, "2026-09-06".into(), 0, 0, "Arqueo: coincide con el banco".into()).unwrap();
        assert_eq!((rows.len(), rows[3].debit, rows[3].credit, rows[3].balance), (4, 0, 0, 7000));
        assert!(lint(&rows).is_empty());
        // rendered with empty amount cells, and read back as such
        let header: Vec<String> = ["Fecha", "Descripción", "Debe", "Haber", "Saldo"].iter().map(|s| s.to_string()).collect();
        let text = render(&header, &rows);
        assert!(text.contains("| 2026-09-06 | Arqueo: coincide con el banco |        |       |  70.00 |"));
        let again = parse_rows(&text.lines().skip(2).collect::<Vec<_>>(), 3).unwrap();
        assert_eq!((again[3].debit, again[3].credit, again[3].balance), (0, 0, 7000));
        // an edit can turn a payment into a note and back; both amounts is still refused
        edit_entry(&mut rows, 2, "2026-09-05".into(), 0, 0, "Pago proveedor (anulado)".into()).unwrap();
        assert_eq!((rows[2].balance, rows[3].balance), (15000, 15000));
        assert!(edit_entry(&mut rows, 2, "2026-09-05".into(), 1, 1, "x".into()).unwrap_err().contains("one positive amount"));
        // a late entry slots in after the rows on or before its date; balances follow
        let mut rows = load_str(DOC).1;
        assert_eq!(add_entry(&mut rows, "2026-09-03".into(), 0, 0, "Mismo día, después".into()).unwrap(), 2);
        assert_eq!(add_entry(&mut rows, "2026-09-02".into(), 0, 1000, "Antes".into()).unwrap(), 1);
        assert_eq!(add_entry(&mut rows, "2020-01-01".into(), 0, 0, "Primera".into()).unwrap(), 0);
        let got: Vec<(&str, i64)> = rows.iter().map(|e| (e.desc.as_str(), e.balance)).collect();
        assert_eq!(got, [("Primera", 0), ("Saldo inicial", 0), ("Antes", -1000), ("Venta mostrador", 14000), ("Mismo día, después", 14000), ("Pago proveedor", 6000)]);
        assert!(lint(&rows).is_empty());
        assert_eq!(entry_what(0, 0, "a"), "note a");
        assert_eq!(entry_what(150, 0, "a"), "debit 1.50 a");
        assert_eq!(entry_what(0, 150, "a"), "credit 1.50 a");
    }

    #[test]
    fn roundtrip_and_lint() {
        assert_eq!(parse_amount("150").unwrap(), 15000);
        assert_eq!(parse_amount("150.5").unwrap(), 15050);
        assert_eq!(parse_amount("").unwrap(), 0);
        assert_eq!(parse_amount("-0.07").unwrap(), -7);
        assert!(parse_amount("1.234").is_err());
        assert!(parse_amount("abc").is_err());
        assert_eq!(fmt_amount(-7), "-0.07");
        assert!(valid_date("2026-09-17"));
        assert!(!valid_date("2026-13-01"));
        assert_eq!(today().len(), 10);

        let lines: Vec<&str> = DOC.lines().collect();
        let (s, e, header) = find_table(&lines).unwrap();
        assert_eq!((s, e), (2, 7));
        assert_eq!(header[1], "Descripción");
        let mut rows = parse_rows(&lines[s + 2..e], s + 3).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(lint(&rows).is_empty());

        rows[2].balance = 1;
        assert_eq!(lint(&rows).len(), 1);
        recalc(&mut rows);
        assert_eq!(rows[2].balance, 7000);

        let rendered = render(&header, &rows);
        let rl: Vec<&str> = rendered.lines().collect();
        let (s2, e2, h2) = find_table(&rl).unwrap();
        assert_eq!(h2, header);
        let again = parse_rows(&rl[s2 + 2..e2], 1).unwrap();
        assert_eq!(again.len(), 3);
        assert!(lint(&again).is_empty());
        assert_eq!(balance(&again), 7000);
        assert_eq!(balance(&[]), 0);

        let mut rows = again;
        assert!(add_entry(&mut rows, "2026-09-06".into(), 0, 0, "".into()).is_err()); // a note still needs its text
        assert!(add_entry(&mut rows, "2026-09-06".into(), 5, 5, "x".into()).is_err());
        assert!(add_entry(&mut rows, "2026-09-06".into(), 500, 0, "".into()).is_err());
        assert_eq!(add_entry(&mut rows, "2026-09-06".into(), 500, 0, "x".into()).unwrap(), 3);
        assert_eq!(balance(&rows), 7500);
        let again = rows;

        assert_eq!(json_str("a\"b\\c\n"), r#""a\"b\\c\n""#);
        let j = render_json(Some("Caja"), &again);
        assert!(j.starts_with("{\"title\": \"Caja\", \"entries\": [\n"));
        assert!(j.contains("\"description\": \"Pago proveedor\", \"debit\": 0.00, \"credit\": 80.00, \"balance\": 70.00}"));
    }

    #[test]
    fn recalc_dry_run() {
        let mut rows = vec![
            Entry { date: "2026-09-01".into(), desc: "a".into(), debit: 100, credit: 0, balance: 100, bold: false },
            Entry { date: "2026-09-02".into(), desc: "b".into(), debit: 0, credit: 30, balance: 80, bold: false },
            Entry { date: "2026-09-03".into(), desc: "c".into(), debit: 0, credit: 0, balance: 80, bold: false },
        ];
        assert_eq!(recalc_diff(&rows), ["row 2: balance 0.80 should be 0.70", "row 3: balance 0.80 should be 0.70"]);
        recalc(&mut rows);
        assert!(recalc_diff(&rows).is_empty());
    }

    #[test]
    fn periods_scope_the_statement() {
        let w = |s: &str| -> Vec<String> { s.split_whitespace().map(|x| x.to_string()).collect() };
        let p = |s: &str| period(&w(s), "2026-02-15");
        let m = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        assert_eq!(p(""), Ok(None));
        assert_eq!(p("this"), Ok(m("2026-02", "2026-02")));
        assert_eq!(p("last"), Ok(m("2026-01", "2026-01")));
        assert_eq!(p("last 3"), Ok(m("2025-11", "2026-01")));
        assert_eq!(p("2026-09"), Ok(m("2026-09", "2026-09")));
        assert_eq!(p("2026-07 2026-09"), Ok(m("2026-07", "2026-09")));
        assert!(p("2026-13").is_err() && p("2026-09 2026-07").is_err() && p("last 0").is_err() && p("last x").is_err());
        assert_eq!(month_add("2026-01", 11), "2026-12");
        assert_eq!(month_add("2026-01", 12), "2027-01");
        assert_eq!(period_label("2026-09", "2026-09"), "2026-09");

        let doc = load("cash.md").unwrap();
        // September has everything: the opening row carries the balance before it, 0
        let d = scoped(doc, &m("2026-09", "2026-09"));
        assert_eq!(d.rows.len(), 23);
        assert_eq!((d.rows[0].date.as_str(), d.rows[0].desc.as_str(), d.rows[0].balance), ("2026-09-01", "Balance", 0));
        assert_eq!(d.rows[22].desc, "Coffee");
        // a later month: nothing but the opening row with the closing balance
        let d = scoped(load("cash.md").unwrap(), &m("2026-10", "2026-12"));
        assert_eq!(d.rows.len(), 1);
        assert_eq!((d.rows[0].date.as_str(), d.rows[0].balance), ("2026-10-01", 33165));
        assert_eq!(render(&d.header, &d.rows).lines().count(), 3);
        // the whole ledger untouched without a period
        assert_eq!(scoped(load("cash.md").unwrap(), &None).rows.len(), 22);
    }

    fn sample() -> Vec<Entry> {
        vec![
            Entry { date: "2026-09-01".into(), desc: "a".into(), debit: 100, credit: 0, balance: 100, bold: false },
            Entry { date: "2026-09-03".into(), desc: "b".into(), debit: 0, credit: 30, balance: 70, bold: false },
            Entry { date: "2026-09-05".into(), desc: "c".into(), debit: 10, credit: 0, balance: 80, bold: false },
        ]
    }

    #[test]
    fn edit_and_delete() {
        let mut rows = sample();
        assert_eq!(row_index("2", 3).unwrap(), 1);
        assert_eq!(row_index("0", 3).unwrap_err(), "row 0: no such row (1..=3)");
        assert_eq!(row_index("x", 3).unwrap_err(), "bad row `x`");
        // the date must stay between the neighbours
        assert_eq!(edit_entry(&mut rows, 1, "2026-08-31".into(), 0, 30, "b".into()).unwrap_err(), "date 2026-08-31 is before row 1 (2026-09-01)");
        assert_eq!(edit_entry(&mut rows, 1, "2026-09-06".into(), 0, 30, "b".into()).unwrap_err(), "date 2026-09-06 is after row 3 (2026-09-05)");
        assert!(edit_entry(&mut rows, 1, "2026-09-04".into(), 0, 50, "b2".into()).is_ok());
        assert_eq!((rows[1].date.as_str(), rows[1].credit, rows[2].balance), ("2026-09-04", 50, 60));
        assert_eq!(edit_entry(&mut rows, 5, "2026-09-04".into(), 0, 50, "b2".into()).unwrap_err(), "row 6: no such row");
        let e = delete_entry(&mut rows, 0).unwrap();
        assert_eq!((e.desc.as_str(), rows.len(), rows[0].balance, rows[1].balance), ("a", 2, -50, -40));
        assert!(delete_entry(&mut rows, 2).is_err());
    }

    #[test]
    fn moving_adopts_the_day() {
        let mut rows = sample();
        assert_eq!(move_entry(&mut rows, 0, true).unwrap_err(), "row 1: already first");
        assert_eq!(move_entry(&mut rows, 2, false).unwrap_err(), "row 3: already last");
        // c (09-05) moves up above b (09-03): it becomes 09-03, order holds, balances follow
        assert_eq!(move_entry(&mut rows, 2, true).unwrap(), 1);
        let dates: Vec<&str> = rows.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, ["2026-09-01", "2026-09-03", "2026-09-03"]);
        assert_eq!(rows.iter().map(|e| e.desc.as_str()).collect::<Vec<_>>(), ["a", "c", "b"]);
        assert_eq!(rows.iter().map(|e| e.balance).collect::<Vec<_>>(), [100, 110, 80]);
        assert!(lint(&rows).is_empty());
        // same day: date untouched
        assert_eq!(move_entry(&mut rows, 1, false).unwrap(), 2);
        assert_eq!(rows[2].desc, "c");
        assert!(lint(&rows).is_empty());
    }

    #[test]
    fn flags_are_bold_in_the_file() {
        let doc = DOC.replace("| Venta mostrador |", "| **Venta mostrador** |");
        let lines: Vec<&str> = doc.lines().collect();
        let (s, e, header) = find_table(&lines).unwrap();
        let rows = parse_rows(&lines[s + 2..e], s + 3).unwrap();
        assert_eq!((rows[1].desc.as_str(), rows[1].bold, rows[0].bold), ("Venta mostrador", true, false));
        assert!(render(&header, &rows).contains("| **Venta mostrador** |"));
        let j = render_json(None, &rows);
        assert!(j.contains("\"description\": \"Venta mostrador\", \"debit\": 150.00, \"credit\": 0.00, \"balance\": 150.00, \"bold\": true}"));
        assert!(j.contains("\"balance\": 0.00}")); // unflagged rows are unchanged
        // `**` alone or empty is a literal description, not a flag
        let odd = DOC.replace("| Venta mostrador |", "| **** |");
        let lines: Vec<&str> = odd.lines().collect();
        assert!(!parse_rows(&lines[s + 2..e], s + 3).unwrap()[1].bold);

        // the screen: box borders, no markers, bold escape
        let g = grid(&rows, false);
        let w = tui_widths(&header, &g, 80, 8);
        let foot = totals(&rows);
        let l = render_lines(&header, &g, &w, Some(&foot));
        assert!(l[0].0.starts_with("┌──────") && l[0].0.ends_with("┐"));
        assert!(l[1].0.starts_with("│ Fecha"));
        assert!(l[2].0.starts_with("├──────") && l[2].0.ends_with("┤"));
        assert!(l[4].0.contains("│ Venta mostrador "));
        assert!(l.iter().all(|(t, _)| t.chars().count() == 80));
        // separator, the totals, bottom border
        let n = l.len();
        assert!(l[n - 3].0.starts_with("├──────") && l[n - 3].0.ends_with("┤"));
        let d: i64 = rows.iter().map(|e| e.debit).sum();
        let c: i64 = rows.iter().map(|e| e.credit).sum();
        assert!(l[n - 2].0.contains(&format!(" {} │", fmt_amount(d))) && l[n - 2].0.contains(&format!(" {} │", fmt_amount(c))));
        assert!(l[n - 2].1.is_none());
        assert!(l[n - 1].0.starts_with("└──────") && l[n - 1].0.ends_with("┘"));
    }

    #[test]
    fn expressions() {
        assert_eq!(eval_expr("1000*40.50").unwrap(), 4050000);
        assert_eq!(eval_expr("1 + 2 * 3").unwrap(), 700);
        assert_eq!(eval_expr("(1 + 2) * 3").unwrap(), 900);
        assert_eq!(eval_expr("100/3").unwrap(), 3333);
        assert_eq!(eval_expr("200/3").unwrap(), 6667);
        assert_eq!(eval_expr("-2.5*2").unwrap(), -500);
        assert_eq!(eval_expr("40.505*1").unwrap(), 4051); // half away from zero, no f64 drift
        assert_eq!(eval_expr("0.1*3").unwrap(), 30);
        assert_eq!(eval_expr("10 - 2 - 3").unwrap(), 500);
        assert_eq!(eval_expr("1/0").unwrap_err(), "division by zero");
        assert_eq!(eval_expr("1 +").unwrap_err(), "missing operand");
        assert_eq!(eval_expr("(1").unwrap_err(), "missing `)`");
        assert_eq!(eval_expr("2 x").unwrap_err(), "unexpected `x`");
        assert_eq!(eval_expr("1.2.3").unwrap_err(), "bad number `1.2.3`");

        assert_eq!(desc_expr("#1000*40.50 currency exchange"), Some(Ok(4050000)));
        assert_eq!(desc_expr("1000*40.50 currency exchange"), None); // no `#`: prose
        assert_eq!(desc_expr("#(10+5)*2 lotes"), Some(Ok(3000)));
        assert_eq!(desc_expr("#3/4 pantalón"), Some(Ok(75)));
        assert_eq!(desc_expr("#2-3 people"), Some(Ok(-100)));
        assert_eq!(desc_expr("2-3 people"), None);
        assert_eq!(desc_expr("3 cafés"), None);
        assert_eq!(desc_expr("Venta mostrador"), None);
        assert_eq!(desc_expr("#100+200+50 varios"), Some(Ok(35000)));
        assert_eq!(desc_expr("#123 invoice"), None);
        assert_eq!(desc_expr("#100/0 x"), Some(Err("bad expression `100/0`: division by zero".into())));
        assert_eq!(desc_expr("2 (dos) cafés"), None); // not an expression: ignored
        assert_eq!(desc_expr("#-1000*40.50 venta USD"), Some(Ok(-4050000)));
        assert_eq!(desc_expr("-1000*40.50 venta USD"), None);
        assert_eq!(desc_expr("#-3 people"), Some(Ok(-300))); // a signed number is a computation
        assert_eq!(desc_expr("#1 +"), Some(Err("bad expression `1 +`: missing operand".into())));

        assert_eq!(amount_arg("150.50").unwrap(), 15050);
        assert_eq!(amount_arg("-").unwrap(), 0);
        assert_eq!(amount_arg("1000*40.50").unwrap(), 4050000);
        assert!(amount_arg("abc").is_err());

        let mut rows = sample();
        rows[1].desc = "#-(10+20) b".into(); // computes -30.00, the credit row is -0.30
        rows[2].desc = "#10*1 c".into(); // 10.00 vs debit 0.10
        assert_eq!(lint(&rows), ["row 2: description computes -30.00 but the row is -0.30", "row 3: description computes 10.00 but the row is 0.10"]);
        rows[1].credit = 3000;
        rows[2].debit = 1000;
        recalc(&mut rows);
        assert!(lint(&rows).is_empty());
        rows[1].desc = "#10+20 b".into(); // a positive computation on a credit row is wrong
        assert_eq!(lint(&rows), ["row 2: description computes 30.00 but the row is -30.00"]);
    }

    #[test]
    fn insert_below() {
        let mut rows = load("cash.md").unwrap().rows;
        assert_eq!(insert_entry(&mut rows, 1, "2026-09-01".into(), 100, 0, "x".into()).unwrap_err(), "date 2026-09-01 is before row 2 (2026-09-02)");
        assert_eq!(insert_entry(&mut rows, 1, "2026-09-04".into(), 100, 0, "x".into()).unwrap_err(), "date 2026-09-04 is after row 3 (2026-09-03)");
        assert!(insert_entry(&mut rows, 30, "2026-09-03".into(), 100, 0, "x".into()).is_err());
        insert_entry(&mut rows, 1, "2026-09-02".into(), 1000, 0, "Extra".into()).unwrap();
        assert_eq!((rows.len(), rows[2].desc.as_str(), rows[2].balance, rows[3].balance), (23, "Extra", 16000, 14800));
        // the last row takes any later date
        insert_entry(&mut rows, 22, "2026-12-31".into(), 0, 50, "y".into()).unwrap();
        assert_eq!((rows.len(), rows[23].balance), (24, 34115));
    }

    #[test]
    fn commit_messages() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_commit_message(&v(&[])), "mdl: no changes");
        assert_eq!(default_commit_message(&v(&["cash/ABC.md"])), "mdl: update cash/ABC");
        assert_eq!(default_commit_message(&v(&["cash/ABC.md", "bank/XYZ.md"])), "mdl: update 2 accounts");
        assert_eq!(default_commit_message(&v(&["notes.txt"])), "mdl: update notes.txt");
        assert_eq!(default_commit_message(&v(&["a.txt", "b.txt"])), "mdl: update 2 files");
    }
}
