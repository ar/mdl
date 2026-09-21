//! Interactive data entry: raw mode, keys and mouse, the screen, the state machine.
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;
use std::{fs, process};

use crate::git;
use crate::ledger::{Doc, Entry, add_entry, delete_entry, desc_expr, edit_entry, entry_what, fmt_amount, fmt_col, insert_entry, load, move_entry, parse_amount, push_pending, recalc, recalc_diff, save_commit, today};
use crate::render::{grid, natural_widths, render_lines, totals};

fn stty(args: &[&str]) -> Option<String> {
    let o = process::Command::new("stty").args(args).stdin(process::Stdio::inherit()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Raw terminal with xterm mouse reporting (clicks, drags, wheel; SGR encoded); restores
/// the saved mode, turns the mouse off and clears the screen on drop, including panics.
struct Raw(String);

impl Raw {
    fn enter() -> Option<Raw> {
        let saved = stty(&["-g"])?;
        stty(&["raw", "-echo", "min", "0", "time", "1"])?;
        print!("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        io::stdout().flush().ok();
        Some(Raw(saved))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        print!("\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[2J\x1b[H");
        io::stdout().flush().ok();
        stty(&[&self.0]);
    }
}

/// (rows, cols), defaulting to 24x80 when unknown or absurd.
fn term_size() -> (usize, usize) {
    let s = stty(&["size"]).unwrap_or_default();
    let mut it = s.split_whitespace().map(|n| n.parse::<usize>().ok());
    let rows = it.next().flatten().filter(|&n| n > 3).unwrap_or(24);
    let cols = it.next().flatten().filter(|&n| n > 20).unwrap_or(80);
    (rows, cols)
}

#[derive(Debug, PartialEq)]
enum Key {
    Char(char),
    Enter,
    Backspace,
    Next,
    Prev,
    Esc,
    /// Left button press at 1-based (column, row).
    Click(usize, usize),
    /// Motion with the left button held.
    Drag(usize, usize),
    /// Left button release.
    Release(usize, usize),
    /// Wheel: -1 up, 1 down.
    Wheel(i32),
    PageUp,
    PageDown,
    /// Delete key or Ctrl-D.
    Delete,
    /// Shift-Up / Shift-Down: move the selected entry.
    MoveUp,
    MoveDown,
    /// Ctrl-S
    Sync,
    /// Ctrl-L: back to an empty account field.
    Clear,
    /// Cursor movement within a field; Home/End are also Ctrl-A/Ctrl-E.
    Left,
    Right,
    Home,
    End,
    /// Nothing typed for a while (the read timed out): the loop can look at
    /// background work and redraw.
    Idle,
}

/// `[<b;x;yM` (press / motion) or `[<b;x;ym` (release) after the ESC: SGR mouse report.
/// Only the left button and the wheel count.
fn parse_mouse(seq: &[u8]) -> Option<Key> {
    let (body, release) = match seq.strip_prefix(b"[<")? {
        s if s.ends_with(b"M") => (&s[..s.len() - 1], false),
        s if s.ends_with(b"m") => (&s[..s.len() - 1], true),
        _ => return None,
    };
    let mut it = std::str::from_utf8(body).ok()?.split(';').map(|n| n.parse::<usize>().ok());
    let (b, x, y) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    match (b, release) {
        (0, false) => Some(Key::Click(x, y)),
        (0, true) => Some(Key::Release(x, y)),
        (32, false) => Some(Key::Drag(x, y)),
        (64, false) => Some(Key::Wheel(-1)),
        (65, false) => Some(Key::Wheel(1)),
        _ => None,
    }
}

/// One read; with `min 0 time 1` it returns None after 100ms of silence.
fn read_byte(stdin: &mut impl Read) -> Option<u8> {
    let mut b = [0u8; 1];
    (stdin.read(&mut b).ok()? == 1).then_some(b[0])
}

fn read_key(stdin: &mut impl Read) -> Key {
    loop {
        let Some(b) = read_byte(stdin) else { return Key::Idle };
        return match b {
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            b'\t' => Key::Next,
            0x03 => Key::Esc,
            0x04 => Key::Delete,
            0x13 => Key::Sync,
            0x0c => Key::Clear,
            0x01 => Key::Home,
            0x05 => Key::End,
            0x1b => {
                let mut seq = vec![];
                while let Some(c) = read_byte(stdin) {
                    seq.push(c);
                    if (0x40..=0x7e).contains(&c) && c != b'[' && c != b'O' {
                        break;
                    }
                }
                match seq.as_slice() {
                    b"" => Key::Esc,
                    b"[A" | b"OA" => Key::Prev,
                    b"[B" | b"OB" => Key::Next,
                    b"[Z" => Key::Prev,
                    b"[C" | b"OC" => Key::Right,
                    b"[D" | b"OD" => Key::Left,
                    b"[H" | b"OH" | b"[1~" | b"[7~" => Key::Home,
                    b"[F" | b"OF" | b"[4~" | b"[8~" => Key::End,
                    b"[3~" => Key::Delete,
                    b"[1;2A" => Key::MoveUp,
                    b"[1;2B" => Key::MoveDown,
                    b"[5~" => Key::PageUp,
                    b"[6~" => Key::PageDown,
                    s => match parse_mouse(s) {
                        Some(k) => k,
                        None => continue,
                    },
                }
            }
            0x20..=0x7e => Key::Char(b as char),
            0xc0.. => {
                let n = if b >= 0xf0 { 3 } else if b >= 0xe0 { 2 } else { 1 };
                let mut buf = vec![b];
                for _ in 0..n {
                    buf.push(read_byte(stdin).unwrap_or(0));
                }
                match String::from_utf8(buf) {
                    Ok(s) => Key::Char(s.chars().next().unwrap()),
                    Err(_) => continue,
                }
            }
            _ => continue,
        };
    }
}

/// All `*.md` files under `dir`, as relative paths, skipping hidden entries, `target`
/// and any `Attic` directory (closed accounts kept out of the way).
pub fn scan_accounts(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" || name.eq_ignore_ascii_case("attic") {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            scan_accounts(&p, out);
        } else if name.ends_with(".md") {
            out.push(p.strip_prefix("./").unwrap_or(&p).to_string_lossy().into_owned());
        }
    }
}

/// Search: the query is a case-insensitive substring of the date or the description, or
/// the leading digits of an amount (debit, credit or balance; sign ignored): `150` finds
/// 150.00 and 1500.00, not 2150.00.
fn entry_matches(e: &Entry, q: &str) -> bool {
    let ql = q.to_lowercase();
    if e.date.contains(q) || e.desc.to_lowercase().contains(&ql) {
        return true;
    }
    let digits = q.trim_start_matches('-');
    !digits.is_empty()
        && [fmt_col(e.debit), fmt_col(e.credit), fmt_amount(e.balance)]
            .iter()
            .any(|s| !s.is_empty() && s.trim_start_matches('-').starts_with(digits))
}

/// Fuzzy match, the way `sk` and `fzf` do it: every char of `q` in `s`, in order, case
/// ignored. The score rewards a match at the start of the name or of a word (after `/`,
/// `-`, `_`, `.`, a space, or a lower-to-upper step), one right after the previous, and
/// a short name; a gap between matches costs. None when `q` is not in `s`.
fn fuzzy_score(s: &str, q: &str) -> Option<i64> {
    let (sc, qc): (Vec<char>, Vec<char>) = (s.chars().collect(), q.chars().collect());
    if qc.is_empty() {
        return Some(-(sc.len() as i64));
    }
    let boundary = |i: usize| i == 0 || "/-_. ".contains(sc[i - 1]) || (sc[i - 1].is_lowercase() && sc[i].is_uppercase());
    // greedy left to right, retrying from each possible start of the first char
    let mut best: Option<i64> = None;
    for start in 0..sc.len() {
        if !sc[start].eq_ignore_ascii_case(&qc[0]) && sc[start].to_lowercase().ne(qc[0].to_lowercase()) {
            continue;
        }
        let (mut score, mut i, mut prev) = (0i64, start, None::<usize>);
        for c in &qc {
            let Some(at) = (i..sc.len()).find(|&j| sc[j].to_lowercase().eq(c.to_lowercase())) else { return best };
            score += 1;
            if boundary(at) {
                score += 8;
            }
            match prev {
                Some(p) if p + 1 == at => score += 4,
                Some(p) => score -= ((at - p - 1) as i64).min(6),
                None => score -= (at as i64).min(6),
            }
            prev = Some(at);
            i = at + 1;
        }
        score -= sc.len() as i64 / 8;
        best = Some(best.map_or(score, |b| b.max(score)));
    }
    best
}

/// The accounts `q` fuzzy-matches, best first (ties: shorter, then alphabetical), as
/// names without `.md`.
fn account_matches(q: &str, accounts: &[String]) -> Vec<String> {
    let mut hits: Vec<(i64, &String)> = accounts.iter().filter_map(|p| fuzzy_score(p.trim_end_matches(".md"), q.trim()).map(|s| (s, p))).collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    hits.into_iter().map(|(_, p)| p.trim_end_matches(".md").to_string()).collect()
}

/// Map what was typed in the account field to a file from `accounts`: an exact path
/// (with or without `.md`), else the one file whose name matches, else the best fuzzy
/// match (see `fuzzy_score`). Several files with the same name are an error listing them.
fn resolve_account(id: &str, accounts: &[String]) -> Result<String, String> {
    let with_md = format!("{id}.md");
    if let Some(p) = accounts.iter().find(|p| **p == with_md || **p == id) {
        return Ok(p.clone());
    }
    let stem = |p: &String| p.rsplit('/').next().unwrap_or(p).trim_end_matches(".md").to_string();
    let named: Vec<&String> = accounts.iter().filter(|p| stem(p) == id).collect();
    match named.as_slice() {
        [one] => return Ok((*one).clone()),
        [_, ..] => return Err(format!("ambiguous: {}", named.iter().map(|p| p.trim_end_matches(".md")).collect::<Vec<_>>().join(", "))),
        [] => {}
    }
    match account_matches(id, accounts).first() {
        Some(name) => Ok(format!("{name}.md")),
        None => Err(format!("{id}: no such account")),
    }
}

/// Screen column widths: the date column is wide enough for account names (the account
/// field sits under it), amounts wide enough to type into, and the description gets
/// what is left of the terminal, at least DESC_MIN when that fits.
pub fn tui_widths(header: &[String], grid: &[[String; 5]], cols: usize, account_w: usize) -> [usize; 5] {
    const DESC_MIN: usize = 48;
    let mut w = natural_widths(header, grid);
    w[0] = w[0].max(10).max(account_w);
    w[2] = w[2].max(10);
    w[3] = w[3].max(10);
    let fixed = 16 + w[0] + w[2] + w[3] + w[4]; // borders and gaps: "| " + 4 * " | " + " |"
    let room = cols.saturating_sub(fixed).max(header[1].chars().count()).max(8);
    w[1] = w[1].max(DESC_MIN).min(room);
    w
}

/// 0-based screen column where table cell `i` starts.
pub fn cell_start(w: &[usize; 5], i: usize) -> usize {
    2 + w[..i].iter().map(|c| c + 3).sum::<usize>()
}

/// The tail of `s` that fits in a `w` wide field with the cursor after it: (text, cursor offset).
/// A text field as shown: the tail that fits (one cell left for the cursor), scrolled
/// left when the cursor (a char index, clamped) would be off screen. Returns the text
/// and the cursor offset.
pub fn field_view(s: &str, w: usize, cur: usize) -> (String, usize) {
    let n = s.chars().count();
    let cur = cur.min(n);
    let skip = n.saturating_sub(w.saturating_sub(1)).min(cur);
    (s.chars().skip(skip).take(w).collect(), cur - skip)
}

/// Byte offset of char `i` in `s` (its length past the end).
pub fn byte_at(s: &str, i: usize) -> usize {
    s.char_indices().nth(i).map_or(s.len(), |(b, _)| b)
}

/// Load an account for the screen: the doc plus, when balances are stale, the prompt
/// offering a recalc.
fn load_for_tui(path: &str) -> Result<(Doc, Option<String>), String> {
    let doc = load(path)?;
    let diffs = recalc_diff(&doc.rows);
    let offer = diffs.first().map(|first| {
        let more = if diffs.len() > 1 { format!(" (+{} more)", diffs.len() - 1) } else { String::new() };
        format!("{first}{more}. Enter: recalc and save, any other key: leave as is")
    });
    Ok((doc, offer))
}

/// An amount field as shown: right-aligned when not focused; while typing, left-aligned
/// until the `.` is pressed, then aligned on the decimal point (third column from the
/// right). Returns the text padded to `w` and the offset of the cursor (a char index,
/// clamped).
fn amount_view(s: &str, w: usize, focused: bool, cur: usize) -> (String, usize) {
    let n = s.chars().count();
    if !focused {
        let shown: String = s.chars().skip(n.saturating_sub(w)).collect();
        let c = shown.chars().count();
        return (format!("{shown:>w$}"), c);
    }
    let cur = cur.min(n);
    match s.split_once('.') {
        Some((int, dec)) => {
            let iw = w.saturating_sub(3);
            let ni = int.chars().count();
            let skipped = ni.saturating_sub(iw);
            let int: String = int.chars().skip(skipped).collect();
            let shown = int.chars().count();
            let off = if cur <= ni { (iw - shown + cur.saturating_sub(skipped)).min(iw) } else { iw + (cur - ni) };
            (format!("{int:>iw$}.{dec:<dw$}", dw = w - iw - 1), off)
        }
        None => {
            let (t, c) = field_view(s, w, cur);
            (format!("{t:<w$}"), c)
        }
    }
}

/// Status line right now, ahead of a call that will block (a fetch).
fn status_now(s: &str) {
    print!("\r\x1b[1A\x1b[2K{s}\x1b[1B");
    io::stdout().flush().ok();
}

// ponytail: raw mode via `stty` + ANSI escapes; no crates. The screen is a viewport
// over the statement (bottom-aligned, wheel / PgUp / PgDn scroll it; it ends with the
// debit and credit totals between two rules) with a status
// row and the entry fields on the last two rows; the whole frame is redrawn in place
// on every key. The fields are one more table row on the same column grid; long
// input scrolls within its field.
//
// Form: Enter next field / submit, Tab|Down next, Shift-Tab|Up prev, Left/Right move
// within the field, Home/End (or Ctrl-A/Ctrl-E) to its ends, Backspace and Delete remove
// the character before and under the cursor, Esc quits. The balance column is a fifth
// field, reached by Enter through an empty debit and credit (or a click, never Tab): a
// balance typed there makes the row the debit or credit that gets there from the
// previous balance (the last row's, or for an edit or `n` the row above's). A
// click on a field focuses it. The amount fields take digits, one `.` and a leading
// `-`, Clipper style: left-aligned while typing, aligned on the point once it is
// pressed, right-aligned and normalised (`1234.50`) once left; the second decimal
// leaves the field as Enter would. Enter on a filled debit submits (the credit is
// then empty by definition); on an empty one it moves to the credit. Coming back to
// an amount with a value, Backspace clears it whole and typing replaces it. A
// description starting with `#` and a computation (`#1000*40.50 currency exchange`,
// `#100+200 varios`) fills the amount field entered next, a negative one the credit;
// the fill follows the focus to the other amount field until it is typed over, and is
// recomputed when the description is left again. After a save the account field stays filled but
// "selected": typing replaces it, Enter keeps it. Opening an account with stale
// balances offers a recalc on the status line first.
//
// Account field: what is typed is matched fuzzily against the accounts (`sk` style: the
// chars in order, word starts and runs scoring higher) and the matches are listed on the
// status line, the first one highlighted; Tab (or Down) moves the highlight, Shift-Tab
// (or Up, while no account is open) moves it back, Enter opens the highlighted one; a
// lone match is completed into the field by Tab. A name that is exactly an account
// needs no picking: Tab then goes on to the description as usual.
//
// Statement: a click on an entry, or Up from the account field, selects it; Up/Down
// move the selection, Enter edits it in the fields (date in the first one), `n` starts a
// new entry below it in the fields (same date to begin with; saved, it is selected so
// `n` chains), Space flags it (bold here, `**bold**` in the file), Ctrl-D or
// Delete removes it at once (git has the history), Shift-Up/Shift-Down move it (crossing
// into another day adopts that day), and so does dragging it with the mouse (saved on
// release, Esc cancels), Esc or Down past the last entry return to the form.
//
// Search: `/` (from the statement, an empty description or an amount field) opens a
// prompt on the status line; typing selects the nearest match at or above the selection
// (the newest entries are at the bottom), Enter or Up the next older one, Down the next
// newer one, wrapping around. Text matches the date or the description (case ignored),
// digits the start of the debit, credit or balance (sign ignored). Esc closes the prompt
// and keeps the selection.
//
// Ctrl-L closes the account: empty fields, the account list rescanned, cursor in the
// account field.
//
// Git: the screen starts a fetch in the background (unless fetched within 15 minutes)
// and opening an account waits for it, so the account read is the current one; Ctrl-S
// fetches + commits + pushes, and with `mdl.autocommit` every save does.
struct Tui {
    h: usize,
    cols: usize,
    /// Statement rows on screen: everything but the status and field rows.
    view: usize,
    w: [usize; 5],
    account_w: usize,
    accounts: Vec<String>,
    /// Account (date while editing), description, debit, credit, and the balance to
    /// reach: filled instead of an amount, the row becomes whatever takes the previous
    /// balance there.
    fields: [String; 5],
    focus: usize,
    /// Cursor in the focused field, a char index; usize::MAX (any value past the end)
    /// means the end, so a refilled field needs no bookkeeping.
    cur: usize,
    /// The focused field's value is "selected": Backspace clears it, typing replaces
    /// it. Set on the account field after a save and on an amount field re-entered
    /// with a value.
    select: bool,
    account: Option<(String, Doc)>,
    /// Rendered statement, each line with the entry it belongs to.
    lines: Vec<(String, Option<usize>)>,
    /// First statement line on screen.
    top: usize,
    /// Selected entry (statement mode).
    sel: Option<usize>,
    /// Entry being edited in the fields; with `insert`, the fields are a new entry to
    /// go right below it instead.
    editing: Option<usize>,
    insert: bool,
    /// An amount field holding the description's computation, untouched so far: it
    /// follows the focus into the other amount field.
    prefill: Option<usize>,
    /// Mouse button down on an entry: where it was (index, date) when pressed. The
    /// entry follows the pointer in memory; the release saves.
    drag: Option<(usize, String)>,
    msg: String,
    /// The search prompt, while open.
    search: Option<String>,
    /// The selection when the search began: where retyping the query searches from.
    search_start: Option<usize>,
    /// The open account has stale balances; Enter fixes them.
    offer_recalc: bool,
    dir: &'static Path,
    in_repo: bool,
    last_pull: Option<Instant>,
    /// The pull running in the background, started when the screen opens (and by
    /// `pull_if_due`); opening an account joins it first.
    pull_job: Option<std::thread::JoinHandle<Result<(bool, &'static str), String>>>,
    /// The highlighted account match: for which text in the account field, and its index
    /// in `account_matches`; any other text means the first match.
    pick: (String, usize),
}

impl Tui {
    fn new() -> Tui {
        let (h, cols) = term_size();
        let mut accounts = vec![];
        scan_accounts(Path::new("."), &mut accounts);
        accounts.sort();
        let account_w = accounts.iter().map(|p| p.trim_end_matches(".md").chars().count()).max().unwrap_or(0).min(24);
        let header: Vec<String> = ["date", "description", "debit", "credit", "balance"].iter().map(|s| s.to_string()).collect();
        let dir = Path::new(".");
        Tui {
            h,
            cols,
            view: h - 2,
            w: tui_widths(&header, &[], cols, account_w),
            account_w,
            accounts,
            fields: Default::default(),
            focus: 0,
            cur: usize::MAX,
            select: false,
            account: None,
            lines: vec![],
            top: 0,
            sel: None,
            editing: None,
            insert: false,
            prefill: None,
            drag: None,
            msg: String::new(),
            search: None,
            search_start: None,
            offer_recalc: false,
            dir,
            in_repo: git::is_repo(dir),
            last_pull: None,
            pull_job: None,
            pick: (String::new(), 0),
        }
    }

    fn rows(&self) -> &[Entry] {
        self.account.as_ref().map_or(&[], |(_, d)| &d.rows)
    }

    fn stem(&self) -> String {
        self.account.as_ref().map(|(p, _)| p.trim_end_matches(".md").to_string()).unwrap_or_default()
    }

    // ---- statement viewport ------------------------------------------------

    /// Re-render the statement after the account or its rows changed.
    fn rebuild(&mut self) {
        let mut lines = vec![];
        if let Some((path, doc)) = &self.account {
            let g = grid(&doc.rows, false);
            let foot = totals(&doc.rows);
            self.w = tui_widths(&doc.header, &[g.as_slice(), std::slice::from_ref(&foot)].concat(), self.cols, self.account_w);
            let title = doc.title().map_or(path.as_str(), |t| t[2..].trim());
            lines.push((title.to_string(), None));
            lines.extend(render_lines(&doc.header, &g, &self.w, Some(&foot)));
        }
        self.lines = lines;
        match self.sel {
            Some(i) => self.show_row(i),
            None => self.top = usize::MAX, // bottom
        }
        self.clamp_top();
    }

    fn clamp_top(&mut self) {
        self.top = self.top.min(self.lines.len().saturating_sub(self.view));
    }

    /// Scroll just enough for entry `i` (all its lines) to be on screen.
    fn show_row(&mut self, i: usize) {
        let idx: Vec<usize> = self.lines.iter().enumerate().filter(|(_, (_, e))| *e == Some(i)).map(|(n, _)| n).collect();
        if let (Some(&first), Some(&last)) = (idx.first(), idx.last()) {
            if first < self.top {
                self.top = first;
            } else if last >= self.top + self.view {
                self.top = last + 1 - self.view;
            }
        }
        self.clamp_top();
    }

    fn select_row(&mut self, i: usize) {
        self.sel = Some(i);
        self.show_row(i);
    }

    /// The entry drawn on screen row `y` (1-based), if any.
    fn entry_at(&self, y: usize) -> Option<usize> {
        let pad = self.view.saturating_sub(self.lines.len());
        if y == 0 || y > self.view || y <= pad {
            return None;
        }
        self.lines.get(self.top + y - 1 - pad).and_then(|(_, e)| *e)
    }

    /// Drag in progress: walk the selected entry to `target`, in memory only.
    fn drag_to(&mut self, target: usize) {
        let Some(mut i) = self.sel else { return };
        let Some((_, doc)) = self.account.as_mut() else { return };
        while i != target {
            match move_entry(&mut doc.rows, i, target < i) {
                Ok(j) => i = j,
                Err(_) => break,
            }
        }
        self.select_row(i);
        self.rebuild();
    }

    /// Esc during a drag: put the entry back (nothing was saved) and keep it selected.
    fn cancel_drag(&mut self) {
        let Some((from, _)) = self.drag.take() else { return };
        let Some((path, doc)) = self.account.as_mut() else { return };
        match load(path) {
            Ok(d) => {
                *doc = d;
                self.select_row(from);
                self.rebuild();
                self.msg = "drag cancelled".into();
            }
            Err(e) => self.msg = e,
        }
    }

    /// Button up (or any key that interrupts a drag): save if the entry moved.
    fn end_drag(&mut self) {
        let Some((from, date0)) = self.drag.take() else { return };
        let Some(j) = self.sel else { return };
        if j == from {
            return;
        }
        match self.save(&format!("move row {} to row {}", from + 1, j + 1)) {
            Ok(note) => {
                let after = &self.rows()[j].date;
                let day = if *after == date0 { String::new() } else { format!(", now {after}") };
                self.msg = format!("row {} moved to row {}{day}{}", from + 1, j + 1, if note.is_empty() { String::new() } else { format!("; {note}") });
            }
            Err(e) => self.msg = e,
        }
    }

    fn draw(&self) -> String {
        let mut out = String::from("\x1b[?25l\x1b[H");
        let pad = self.view.saturating_sub(self.lines.len());
        for r in 0..self.view {
            out += "\x1b[2K";
            if r >= pad {
                if let Some((text, entry)) = self.lines.get(self.top + r - pad) {
                    let hl = entry.is_some() && *entry == self.sel;
                    let bold = entry.is_some_and(|i| self.rows()[i].bold);
                    if bold {
                        out += "\x1b[1m";
                    }
                    if hl {
                        out += "\x1b[7m";
                    }
                    out.extend(text.chars().take(self.cols));
                    if hl || bold {
                        out += "\x1b[0m";
                    }
                }
            }
            out += "\r\n";
        }
        let status = if let Some(q) = &self.search {
            let hint = if self.msg.is_empty() { "Enter/Up older, Down newer, Esc done" } else { self.msg.as_str() };
            format!("/{q}   {hint}")
        } else if !self.msg.is_empty() {
            self.msg.clone()
        } else if let Some(i) = self.editing {
            let what = if self.insert { "new row below row" } else { "editing row" };
            format!("{what} {}: Enter saves, Esc cancels", i + 1)
        } else if let Some(i) = self.sel {
            format!("row {}: Enter edit, n new below, Space flag, Ctrl-D delete, Shift-Up/Down move, Esc back", i + 1)
        } else if self.focus == 4 {
            "balance to reach: Enter adds the debit or credit that gets there (empty: a note)".into()
        } else if let Some(strip) = self.completion_strip() {
            strip
        } else if self.pull_job.is_some() {
            "syncing with the remote in the background…".into()
        } else {
            String::new()
        };
        out += "\x1b[2K";
        // the completion strip carries its own highlight escapes and is already cut to fit
        if status.contains('\x1b') {
            out += &status;
        } else {
            out.extend(status.chars().take(self.cols));
        }
        out += "\r\n\x1b[2K│ ";
        let view = |i: usize| {
            let (f, w) = (&self.fields[i], self.w[i]);
            let cur = if i == self.focus { self.cur() } else { usize::MAX };
            if i >= 2 { amount_view(f, w, i == self.focus, cur) } else { field_view(f, w, cur) }
        };
        for i in 0..5 {
            let style = if i == self.focus { "\x1b[47;30m" } else { "\x1b[100m" };
            let (shown, _) = view(i);
            out += &format!("{style}{shown:<width$}\x1b[0m │{}", if i < 4 { " " } else { "" }, width = self.w[i]);
        }
        if let Some(q) = &self.search {
            out += &format!("\x1b[{};{}H\x1b[?25h", self.view + 1, 2 + q.chars().count());
        } else if self.sel.is_none() || self.editing.is_some() {
            let col = cell_start(&self.w, self.focus) + view(self.focus).1;
            out += &format!("\r\x1b[{col}C\x1b[?25h");
        }
        out
    }

    // ---- fields ------------------------------------------------------------

    /// Move focus; an amount field left behind is normalised (`1234.50`, empty for 0),
    /// and one entered with a value in it is "selected": Backspace clears it, typing
    /// replaces it.
    fn set_focus(&mut self, f: usize) {
        let from = self.focus;
        let mut f = f;
        if from >= 2 && from != f {
            let s = &mut self.fields[from];
            if let Ok(v) = parse_amount(s.trim()) {
                *s = fmt_col(v);
            }
        }
        // A computation at the start of the description fills the amount field entered
        // from it, a negative one the credit (the value is the row's effect on the
        // balance); leaving the description again recomputes an untouched fill. An
        // untouched fill follows the focus into the other amount field.
        if from == 1 && f >= 2 {
            if let Some(k) = self.prefill.take() {
                self.fields[k].clear();
            }
            if self.fields[2].is_empty() && self.fields[3].is_empty() {
                match desc_expr(self.fields[1].trim()) {
                    Some(Ok(v)) if v != 0 => {
                        if v < 0 {
                            f = 3;
                        }
                        self.fields[f] = fmt_col(v.abs());
                        self.prefill = Some(f);
                    }
                    Some(Err(e)) => self.msg = e,
                    _ => {}
                }
            }
        } else if self.prefill == Some(from) && (2..=3).contains(&f) && f != from && self.fields[f].is_empty() {
            self.fields[f] = std::mem::take(&mut self.fields[from]);
            self.prefill = Some(f);
        }
        self.focus = f;
        self.cur = usize::MAX;
        self.select = f >= 2 && !self.fields[f].is_empty();
    }

    // ---- account completion ------------------------------------------------

    /// The accounts the account field's text matches, best first; empty when the field
    /// is empty or in use as a date (an edit).
    fn completions(&self) -> Vec<String> {
        let q = self.fields[0].trim();
        if self.editing.is_some() || q.is_empty() {
            return vec![];
        }
        account_matches(q, &self.accounts)
    }

    /// The account field's text names an account outright (a path or a file name).
    fn exact_account(&self) -> bool {
        let id = self.fields[0].trim();
        let with_md = format!("{id}.md");
        self.accounts.iter().any(|p| *p == with_md || *p == id || p.rsplit('/').next().unwrap_or(p).trim_end_matches(".md") == id)
    }

    /// Index of the highlighted match for the field's current text.
    fn pick(&self) -> usize {
        if self.pick.0 == self.fields[0] { self.pick.1 } else { 0 }
    }

    /// Tab / Shift-Tab in the account field: move the highlight when there is a list to
    /// move it on; a lone match is completed into the field instead (the next Tab then
    /// goes on, as for any exact name). False when the key should do what it normally
    /// does.
    fn cycle_pick(&mut self, forward: bool) -> bool {
        if self.focus != 0 || self.editing.is_some() || self.exact_account() {
            return false;
        }
        let names = self.completions();
        match names.as_slice() {
            [] => false,
            [one] => {
                self.fields[0] = one.clone();
                self.cur = usize::MAX;
                true
            }
            _ => {
                let (i, n) = (self.pick(), names.len());
                self.pick = (self.fields[0].clone(), if forward { (i + 1) % n } else { (i + n - 1) % n });
                true
            }
        }
    }

    /// The status line while typing an account name: the matches, the picked one in
    /// reverse video, as many as fit the width. None when there is nothing to show.
    fn completion_strip(&self) -> Option<String> {
        if self.focus != 0 || self.editing.is_some() || self.sel.is_some() {
            return None;
        }
        let names = self.completions();
        if names.is_empty() || (names.len() == 1 && self.exact_account()) {
            return None;
        }
        let (pick, mut used, mut out) = (self.pick(), 0usize, String::new());
        for (i, name) in names.iter().enumerate() {
            let w = name.chars().count();
            if used + w + if used > 0 { 2 } else { 0 } > self.cols.saturating_sub(2) {
                out += "  …";
                break;
            }
            if used > 0 {
                out += "  ";
                used += 2;
            }
            if i == pick {
                out += &format!("\x1b[7m{name}\x1b[0m");
            } else {
                out += name;
            }
            used += w;
        }
        Some(out)
    }

    /// Enter (or the second decimal of an amount): next field, or submit. A filled debit
    /// submits too, since the credit is then meant to be empty; when it is not (an edit
    /// turning a credit into a debit) Enter goes there so the conflict is in view. An
    /// empty credit after an empty debit goes on to the balance field (Tab never does).
    fn enter(&mut self) {
        let amount = |s: &str| parse_amount(s.trim()).unwrap_or(0);
        let debit_done = self.focus == 2 && amount(&self.fields[2]) != 0 && amount(&self.fields[3]) == 0;
        let no_amount = amount(&self.fields[2]) == 0 && amount(&self.fields[3]) == 0;
        match (self.editing.is_some(), self.focus) {
            (false, 0) => self.open(),
            (_, 3) if no_amount => self.set_focus(4),
            (false, 3 | 4) => self.add(),
            (true, 3 | 4) => self.submit_edit(),
            (false, 2) if debit_done => self.add(),
            (true, 2) if debit_done => self.submit_edit(),
            (_, f) => self.set_focus(f + 1),
        }
    }

    /// The debit and credit the fields describe: as typed, or, with a balance to reach
    /// in the last field, whatever takes the balance there from `from` (the balance of
    /// the row before the one being saved).
    fn amounts(&self, from: i64) -> Result<(i64, i64), String> {
        let d = parse_amount(self.fields[2].trim())?;
        let c = parse_amount(self.fields[3].trim())?;
        let target = self.fields[4].trim();
        if target.is_empty() {
            return Ok((d, c));
        }
        if d != 0 || c != 0 {
            return Err("an amount or a balance to reach, not both".into());
        }
        let diff = parse_amount(target)? - from;
        if diff == 0 {
            return Err(format!("the balance is {} already", fmt_amount(from)));
        }
        Ok((diff.max(0), (-diff).max(0)))
    }

    /// Cursor position in the focused field, clamped to its length.
    fn cur(&self) -> usize {
        self.cur.min(self.fields[self.focus].chars().count())
    }

    /// A key in a text field: inserted at the cursor.
    fn insert_char(&mut self, c: char) {
        let cur = self.cur();
        let f = &mut self.fields[self.focus];
        f.insert(byte_at(f, cur), c);
        self.cur = cur + 1;
    }

    /// Remove the char before (Backspace) or under (Delete) the cursor.
    fn erase(&mut self, before: bool) {
        let cur = self.cur();
        let f = &mut self.fields[self.focus];
        let at = if before { cur.checked_sub(1) } else { Some(cur).filter(|&c| c < f.chars().count()) };
        if let Some(at) = at {
            f.remove(byte_at(f, at));
            self.cur = at;
        }
    }

    /// A key in an amount field, at the cursor: digits, one `.` (with a 0 in front when
    /// nothing is), a leading `-`; the integer part stops at what fits before the point,
    /// the decimals at two. The second decimal typed at the end leaves the field as
    /// Enter would.
    fn type_amount(&mut self, c: char) {
        let w = self.w[self.focus];
        let mut cur = self.cur();
        let mut t: Vec<char> = self.fields[self.focus].chars().collect();
        let at_end = cur == t.len();
        match c {
            '0'..='9' => t.insert(cur, c),
            '.' if !t.contains(&'.') => {
                if t[..cur].iter().all(|c| *c == '-') {
                    t.insert(cur, '0');
                    cur += 1;
                }
                t.insert(cur, '.');
            }
            '-' if t.is_empty() => t.push('-'),
            _ => return,
        }
        let s: String = t.iter().collect();
        let (int, dec) = s.split_once('.').unwrap_or((&s, ""));
        let (ni, nd) = (int.trim_start_matches('-').chars().count(), dec.chars().count());
        if nd > 2 || ni > w.saturating_sub(3) {
            return;
        }
        self.fields[self.focus] = s;
        self.cur = cur + 1;
        if at_end && nd == 2 {
            self.enter();
        }
    }

    // ---- actions -----------------------------------------------------------

    pub fn save(&self, what: &str) -> Result<String, String> {
        let (path, doc) = self.account.as_ref().ok_or("account?")?;
        save_commit(path, doc, what)
    }

    /// Start a pull in the background, unless one is running, the tree was fetched (by
    /// anything, in any session) less than SYNC_EVERY ago, or this session already tried
    /// recently and failed.
    fn start_pull(&mut self) {
        const SYNC_EVERY: u64 = 15 * 60;
        let tried = self.last_pull.is_some_and(|t| t.elapsed().as_secs() < SYNC_EVERY);
        let fetched = git::fetched_ago(self.dir).is_some_and(|d| d.as_secs() < SYNC_EVERY);
        if !self.in_repo || self.pull_job.is_some() || tried || fetched {
            return;
        }
        self.last_pull = Some(Instant::now());
        let dir = self.dir;
        self.pull_job = Some(std::thread::spawn(move || git::pull(dir)));
    }

    /// The background pull has ended (the idle loop asks, to show its note).
    fn pull_done(&self) -> bool {
        self.pull_job.as_ref().is_some_and(|j| j.is_finished())
    }

    /// Wait for the background pull, if any, and return its note.
    fn finish_pull(&mut self) -> Result<String, String> {
        let Some(job) = self.pull_job.take() else { return Ok(String::new()) };
        if !job.is_finished() {
            status_now("syncing…");
        }
        match job.join() {
            Ok(r) => r.map(|(_, n)| n.to_string()),
            Err(_) => Err("sync failed".into()),
        }
    }

    /// Fetch before editing (see `start_pull` for when), waiting for it to end.
    fn pull_if_due(&mut self) -> Result<String, String> {
        self.start_pull();
        self.finish_pull()
    }

    fn open(&mut self) {
        let id = self.fields[0].trim().to_string();
        let mut opened = self.account.is_some();
        if !id.is_empty() && self.stem() != id {
            opened = false;
            // the highlighted match when there is a list, else the name as typed
            let target = match self.completions().get(self.pick()) {
                Some(name) if !self.exact_account() => Ok(format!("{name}.md")),
                _ => resolve_account(&id, &self.accounts),
            };
            match target {
                Ok(path) => {
                    let pulled = self.pull_if_due();
                    match load_for_tui(&path) {
                        Ok((doc, offer)) => {
                            self.fields[0] = path.trim_end_matches(".md").to_string();
                            self.offer_recalc = offer.is_some();
                            self.msg = offer.unwrap_or_else(|| pulled.clone().unwrap_or_else(|e| e));
                            self.account = Some((path, doc));
                            self.sel = None;
                            self.editing = None;
                            self.rebuild();
                            opened = true;
                        }
                        // a failed merge leaves markers the parser refuses: say why
                        Err(e) => self.msg = pulled.err().unwrap_or(e),
                    }
                }
                Err(e) => self.msg = e,
            }
        }
        if opened {
            self.focus = 1;
            self.cur = usize::MAX;
        }
    }

    fn add(&mut self) {
        let amounts = self.amounts(self.rows().last().map_or(0, |e| e.balance));
        let Some((_, doc)) = self.account.as_mut() else {
            self.msg = "account?".into();
            self.focus = 0;
            self.cur = usize::MAX;
            return;
        };
        let desc = self.fields[1].trim().to_string();
        let r = amounts.and_then(|(d, c)| add_entry(&mut doc.rows, today(), d, c, desc.clone()).map(|_| (d, c)));
        let r = r.and_then(|(d, c)| self.save(&entry_what(d, c, &desc)));
        match r {
            Ok(note) => {
                self.msg = note;
                self.fields[1..].iter_mut().for_each(String::clear);
                self.focus = 0;
                self.cur = usize::MAX;
                self.select = true;
                self.sel = None;
                self.prefill = None;
                self.rebuild();
            }
            Err(e) => self.msg = e,
        }
    }

    fn start_edit(&mut self, i: usize) {
        let e = &self.rows()[i];
        self.fields = [e.date.clone(), e.desc.clone(), fmt_col(e.debit), fmt_col(e.credit), String::new()];
        self.editing = Some(i);
        self.focus = 1;
        self.cur = usize::MAX;
    }

    /// `n` on a row: the fields become a new entry to go right below it, with its date.
    fn start_insert(&mut self, i: usize) {
        self.fields = [self.rows()[i].date.clone(), String::new(), String::new(), String::new(), String::new()];
        self.editing = Some(i);
        self.insert = true;
        self.focus = 1;
        self.cur = usize::MAX;
    }

    fn cancel_edit(&mut self) {
        self.editing = None;
        self.insert = false;
        self.fields = [self.stem(), String::new(), String::new(), String::new(), String::new()];
        self.focus = 0;
        self.cur = usize::MAX;
        self.prefill = None;
    }

    fn submit_edit(&mut self) {
        let Some(i) = self.editing else { return };
        let insert = self.insert;
        let before = if insert { Some(i) } else { i.checked_sub(1) };
        let amounts = self.amounts(before.map_or(0, |j| self.rows()[j].balance));
        let Some((_, doc)) = self.account.as_mut() else { return };
        let desc = self.fields[1].trim().to_string();
        let date = self.fields[0].trim().to_string();
        let r = amounts.and_then(|(d, c)| {
                if insert {
                    insert_entry(&mut doc.rows, i, date, d, c, desc.clone()).map(|_| (d, c))
                } else {
                    edit_entry(&mut doc.rows, i, date, d, c, desc.clone()).map(|_| (d, c))
                }
            });
        let r = r.and_then(|(d, c)| {
            let what = if insert {
                format!("{} after row {}", entry_what(d, c, &desc), i + 1)
            } else {
                format!("edit row {}: {desc}", i + 1)
            };
            self.save(&what)
        });
        match r {
            Ok(note) => {
                let (j, verb) = if insert { (i + 1, "added") } else { (i, "updated") };
                self.msg = if note.is_empty() { format!("row {} {verb}", j + 1) } else { format!("row {} {verb}; {note}", j + 1) };
                self.cancel_edit();
                if insert {
                    self.sel = Some(j);
                }
                self.rebuild();
            }
            Err(e) => self.msg = e,
        }
    }

    fn delete(&mut self, i: usize) {
        let Some((_, doc)) = self.account.as_mut() else { return };
        let r = delete_entry(&mut doc.rows, i).and_then(|e| self.save(&format!("delete row {}: {}", i + 1, e.desc)));
        match r {
            Ok(note) => {
                self.msg = if note.is_empty() { format!("row {} deleted", i + 1) } else { format!("row {} deleted; {note}", i + 1) };
                let n = self.rows().len();
                self.sel = if n == 0 { None } else { Some(i.min(n - 1)) };
                self.rebuild();
            }
            Err(e) => self.msg = e,
        }
    }

    fn shift(&mut self, i: usize, up: bool) {
        let Some((_, doc)) = self.account.as_mut() else { return };
        let dir = if up { "up" } else { "down" };
        let before = doc.rows[i].date.clone();
        let r = move_entry(&mut doc.rows, i, up).and_then(|j| self.save(&format!("move row {} {dir}", i + 1)).map(|note| (j, note)));
        match r {
            Ok((j, note)) => {
                let after = &self.rows()[j].date;
                let day = if *after == before { String::new() } else { format!(", now {after}") };
                self.msg = format!("row {} moved {dir} to row {}{day}{}", i + 1, j + 1, if note.is_empty() { String::new() } else { format!("; {note}") });
                self.select_row(j);
                self.rebuild();
            }
            Err(e) => self.msg = e,
        }
    }

    /// Space: toggle the flag on row `i`.
    fn flag(&mut self, i: usize) {
        let Some((_, doc)) = self.account.as_mut() else { return };
        doc.rows[i].bold = !doc.rows[i].bold;
        let on = doc.rows[i].bold;
        let verb = if on { "flag" } else { "unflag" };
        self.msg = match self.save(&format!("{verb} row {}", i + 1)) {
            Ok(note) if note.is_empty() => format!("row {} {verb}ged", i + 1),
            Ok(note) => format!("row {} {verb}ged; {note}", i + 1),
            Err(e) => e,
        };
        self.rebuild();
    }

    /// Reload the open account from disk (after a sync); the recalc offer applies.
    fn reload(&mut self) {
        let Some((path, doc)) = self.account.as_mut() else { return };
        match load_for_tui(path) {
            Ok((d, offer)) => {
                *doc = d;
                if let Some(o) = offer {
                    self.msg = o;
                    self.offer_recalc = true;
                }
                self.editing = None;
                self.fields = [self.stem(), String::new(), String::new(), String::new(), String::new()];
                let n = self.rows().len();
                self.sel = self.sel.filter(|_| n > 0).map(|i| i.min(n - 1));
                self.rebuild();
            }
            Err(e) => self.msg = e,
        }
    }

    fn sync(&mut self) {
        if !self.in_repo {
            self.msg = "not a git repository".into();
            return;
        }
        status_now("syncing…");
        let dir = self.dir;
        self.msg = match git::pull(dir).and_then(|(_, n)| push_pending(dir, "").map(|(_, p)| format!("{n}; {p}"))) {
            Ok(m) => m,
            Err(e) => e,
        };
        self.last_pull = Some(Instant::now());
        self.reload();
    }

    fn accept_recalc(&mut self, key: &Key) {
        self.offer_recalc = false;
        if let (Key::Enter, Some((_, doc))) = (key, self.account.as_mut()) {
            recalc(&mut doc.rows);
            self.msg = match self.save("recalc") {
                Ok(note) if note.is_empty() => "balances recalculated".into(),
                Ok(note) => format!("balances recalculated; {note}"),
                Err(e) => e,
            };
            self.rebuild();
        } else {
            self.msg = "balances left as is".into();
        }
    }

    /// Ctrl-L: close the account and start over with an empty account field; the
    /// account list is rescanned on the way.
    fn clear(&mut self) {
        self.account = None;
        self.fields = Default::default();
        self.focus = 0;
        self.cur = usize::MAX;
        self.select = false;
        self.sel = None;
        self.editing = None;
        self.prefill = None;
        self.search = None;
        self.offer_recalc = false;
        self.accounts.clear();
        scan_accounts(self.dir, &mut self.accounts);
        self.accounts.sort();
        self.rebuild();
    }

    fn start_search(&mut self) {
        if self.rows().is_empty() {
            self.msg = "account?".into();
            return;
        }
        self.search = Some(String::new());
        self.search_start = self.sel;
    }

    /// The nearest match from `from` (inclusive) going up (older) or down, wrapping.
    fn find(&self, from: usize, up: bool) -> Option<usize> {
        let n = self.rows().len();
        let q = self.search.as_deref().unwrap_or("");
        if n == 0 || q.is_empty() {
            return None;
        }
        (0..n).map(|k| if up { (from + n - k) % n } else { (from + k) % n }).find(|&i| entry_matches(&self.rows()[i], q))
    }

    /// A key while the search prompt is open; false means it is not a search key: the
    /// prompt closes and the key is handled as usual.
    fn search_key(&mut self, key: &Key) -> bool {
        let n = self.rows().len();
        let (from, up) = match key {
            Key::Char(c) => {
                self.search.as_mut().unwrap().push(*c);
                (self.search_start.unwrap_or(n - 1), true)
            }
            Key::Backspace => {
                if self.search.as_mut().unwrap().pop().is_none() {
                    self.search = None;
                    return true;
                }
                (self.search_start.unwrap_or(n - 1), true)
            }
            Key::Enter | Key::Prev => (self.sel.map_or(n - 1, |i| (i + n - 1) % n), true),
            Key::Next => (self.sel.map_or(0, |i| (i + 1) % n), false),
            Key::Esc => {
                self.search = None;
                return true;
            }
            _ => {
                self.search = None;
                return false;
            }
        };
        match self.find(from, up) {
            Some(i) => {
                if if up { i > from } else { i < from } {
                    self.msg = "wrapped".into();
                }
                self.select_row(i);
            }
            None if self.search.as_deref() == Some("") => self.sel = self.search_start,
            None => self.msg = "no match".into(),
        }
        true
    }

    // ---- keys --------------------------------------------------------------

    /// One key; false means quit.
    fn handle(&mut self, key: Key) -> bool {
        if key == Key::Idle {
            return true;
        }
        if self.offer_recalc {
            self.accept_recalc(&key);
            return true;
        }
        if self.search.is_some() && self.search_key(&key) {
            return true;
        }
        if !matches!(key, Key::Char(_) | Key::Backspace) {
            self.select = false;
        }
        if let Some((from, _)) = &self.drag {
            let moved = self.sel != Some(*from);
            match key {
                Key::Drag(..) | Key::Release(..) => {}
                Key::Esc if moved => {
                    self.cancel_drag();
                    return true;
                }
                _ => self.end_drag(),
            }
        }
        let n = self.rows().len();
        let account_before = self.fields[0].clone();
        match key {
            Key::Esc => {
                if self.editing.is_some() {
                    self.cancel_edit();
                } else if self.sel.is_some() {
                    self.sel = None;
                } else {
                    return false;
                }
            }
            Key::Char(' ') if self.sel.is_some() && self.editing.is_none() => self.flag(self.sel.unwrap()),
            Key::Char('n') if self.sel.is_some() && self.editing.is_none() => self.start_insert(self.sel.unwrap()),
            Key::Char('/') if self.editing.is_none() && (self.sel.is_some() || self.focus >= 2 || (self.focus == 1 && self.fields[1].is_empty())) => {
                self.start_search()
            }
            Key::Char(c) => {
                if self.editing.is_none() {
                    self.sel = None;
                }
                if self.select {
                    self.fields[self.focus].clear();
                    self.cur = usize::MAX;
                }
                self.select = false;
                if self.prefill == Some(self.focus) {
                    self.prefill = None;
                }
                if self.focus >= 2 {
                    self.type_amount(c);
                } else {
                    self.insert_char(c);
                }
            }
            Key::Backspace => {
                if self.editing.is_none() && self.sel.is_some() {
                    self.sel = None;
                } else {
                    if self.select {
                        self.fields[self.focus].clear();
                        self.cur = usize::MAX;
                        self.select = false;
                    } else {
                        self.erase(true);
                    }
                    if self.prefill == Some(self.focus) {
                        self.prefill = None;
                    }
                }
            }
            Key::Sync => self.sync(),
            Key::Clear => self.clear(),
            Key::Prev => {
                if self.editing.is_some() {
                    self.set_focus(self.focus.saturating_sub(1));
                } else if let Some(i) = self.sel {
                    self.select_row(i.saturating_sub(1));
                } else if self.focus == 0 && self.account.is_none() && self.cycle_pick(false) {
                } else if self.focus > 0 {
                    self.set_focus(self.focus - 1);
                } else if n > 0 {
                    self.select_row(n - 1);
                }
            }
            Key::Next => {
                if self.editing.is_some() {
                    self.set_focus((self.focus + 1).min(3));
                } else if let Some(i) = self.sel {
                    if i + 1 < n {
                        self.select_row(i + 1);
                    } else {
                        self.sel = None;
                    }
                } else if self.cycle_pick(true) {
                } else {
                    self.set_focus((self.focus + 1).min(3));
                }
            }
            Key::PageUp => self.top = self.top.saturating_sub(self.view),
            Key::PageDown => {
                self.top += self.view;
                self.clamp_top();
            }
            Key::Wheel(d) => {
                self.top = (self.top as i64 + 3 * d as i64).max(0) as usize;
                self.clamp_top();
            }
            Key::Delete => {
                if let (Some(i), None) = (self.sel, self.editing) {
                    self.delete(i);
                } else {
                    self.erase(false);
                    if self.prefill == Some(self.focus) {
                        self.prefill = None;
                    }
                }
            }
            Key::Left | Key::Right | Key::Home | Key::End => {
                if self.sel.is_none() || self.editing.is_some() {
                    let n = self.fields[self.focus].chars().count();
                    self.cur = match key {
                        Key::Left => self.cur().saturating_sub(1),
                        Key::Right => (self.cur() + 1).min(n),
                        Key::Home => 0,
                        _ => usize::MAX,
                    };
                }
            }
            Key::MoveUp | Key::MoveDown => {
                if let (Some(i), None) = (self.sel, self.editing) {
                    self.shift(i, key == Key::MoveUp);
                }
            }
            Key::Click(x, y) => {
                if y == self.h {
                    // the fields, on the table's column grid
                    for i in 0..5 {
                        let start = cell_start(&self.w, i);
                        if x > start && x <= start + self.w[i] {
                            self.set_focus(i);
                        }
                    }
                    if self.editing.is_none() {
                        self.sel = None;
                    }
                } else if let Some(i) = self.entry_at(y) {
                    if self.editing.is_some() {
                        self.cancel_edit();
                    }
                    self.select_row(i);
                    self.drag = Some((i, self.rows()[i].date.clone()));
                }
            }
            Key::Drag(_, y) => {
                if self.drag.is_some() {
                    if let Some(target) = self.entry_at(y) {
                        self.drag_to(target);
                    }
                }
            }
            Key::Release(..) => self.end_drag(),
            Key::Enter => match (self.editing, self.sel) {
                (None, Some(i)) => self.start_edit(i),
                _ => self.enter(),
            },
            Key::Idle => {}
        }
        if self.fields[0] != account_before {
            self.pick = (String::new(), 0); // new text: the first match is highlighted
        }
        true
    }
}

pub fn tui() -> Result<(), String> {
    let _raw = Raw::enter().ok_or("not a terminal")?;
    let mut stdin = io::stdin().lock();
    let mut t = Tui::new();
    t.start_pull();
    print!("\x1b[2J\x1b[3J");
    loop {
        print!("{}", t.draw());
        io::stdout().flush().ok();
        t.msg.clear();
        // wait for a key; while waiting, a finished background pull gets its note shown
        let key = loop {
            match read_key(&mut stdin) {
                Key::Idle if t.pull_done() => {
                    t.msg = t.finish_pull().unwrap_or_else(|e| e);
                    break None;
                }
                Key::Idle => {}
                k => break Some(k),
            }
        };
        if let Some(k) = key {
            if !t.handle(k) {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{cash_head, lint};
    use crate::render::{render, render_cols, wrap_words};

    #[test]
    fn account_resolution() {
        let a: Vec<String> = ["cash/ABC.md", "bank/XYZ.md", "bank/XYZ2.md", "caja.md"].iter().map(|s| s.to_string()).collect();
        assert_eq!(resolve_account("ABC", &a).unwrap(), "cash/ABC.md");
        assert_eq!(resolve_account("cash/ABC", &a).unwrap(), "cash/ABC.md");
        assert_eq!(resolve_account("cash/ABC.md", &a).unwrap(), "cash/ABC.md");
        assert_eq!(resolve_account("caja", &a).unwrap(), "caja.md");
        assert_eq!(resolve_account("XYZ", &a).unwrap(), "bank/XYZ.md"); // exact stem beats prefix
        assert_eq!(resolve_account("XYZ2", &a).unwrap(), "bank/XYZ2.md");
        assert_eq!(resolve_account("AB", &a).unwrap(), "cash/ABC.md"); // prefix
        assert_eq!(resolve_account("XY", &a).unwrap(), "bank/XYZ.md"); // both match: the shorter
        assert_eq!(resolve_account("bz2", &a).unwrap(), "bank/XYZ2.md"); // fuzzy: b, z, 2 in order
        assert_eq!(resolve_account("nope", &a).unwrap_err(), "nope: no such account");
        let twice: Vec<String> = ["a/caja.md", "b/caja.md"].iter().map(|s| s.to_string()).collect();
        assert_eq!(resolve_account("caja", &twice).unwrap_err(), "ambiguous: a/caja, b/caja");
    }

    #[test]
    fn fuzzy_ranking() {
        let a: Vec<String> = ["cash/ABC.md", "bank/XYZ.md", "bank/XYZ2.md", "caja.md", "notes/acu.md"].iter().map(|s| s.to_string()).collect();
        assert_eq!(account_matches("ca", &a), ["caja", "cash/ABC"]); // both at the start; the shorter first
        assert_eq!(account_matches("ab", &a), ["cash/ABC"]); // a word start beats nothing
        assert_eq!(account_matches("xyz", &a), ["bank/XYZ", "bank/XYZ2"]);
        assert_eq!(account_matches("z2", &a), ["bank/XYZ2"]);
        assert_eq!(account_matches("na", &a), ["notes/acu"]); // n, then an a after it: not bank
        assert_eq!(account_matches("an", &a), ["bank/XYZ", "bank/XYZ2"]); // a run inside a word
        assert!(account_matches("q", &a).is_empty());
        assert!(fuzzy_score("cash/ABC", "cA").unwrap() > fuzzy_score("cash/ABC", "sA").unwrap()); // the start of the name scores
        assert!(fuzzy_score("cash/ABC", "ABC").unwrap() > fuzzy_score("cash/ABC", "AC").unwrap()); // a run beats a gap
    }

    /// Typing in the account field lists the matches on the status line; Tab moves the
    /// highlight, Enter opens the highlighted one, and an exact name needs no picking.
    #[test]
    fn tui_completes_the_account() {
        let mut t = Tui::new();
        t.in_repo = false;
        t.cols = 60;
        t.accounts = ["bank/main.md", "bank/savings.md", "cash/drawer.md"].iter().map(|s| s.to_string()).collect();
        let type_all = |t: &mut Tui, s: &str| s.chars().for_each(|c| assert!(t.handle(Key::Char(c))));

        // nothing typed: no strip; "sa" lists savings (a word start) before cash/drawer (s..a inside)
        assert_eq!(t.completion_strip(), None);
        type_all(&mut t, "sa");
        assert_eq!(t.completions(), ["bank/savings", "cash/drawer"]);
        assert_eq!(t.completion_strip().unwrap(), "\x1b[7mbank/savings\x1b[0m  cash/drawer");
        assert!(t.draw().contains("\x1b[7mbank/savings\x1b[0m  cash/drawer"));

        // Tab moves the highlight and wraps; Shift-Tab goes back; the field keeps its text
        assert!(t.handle(Key::Next));
        assert_eq!((t.pick(), t.focus, t.fields[0].as_str()), (1, 0, "sa"));
        assert_eq!(t.completion_strip().unwrap(), "bank/savings  \x1b[7mcash/drawer\x1b[0m");
        t.handle(Key::Next);
        assert_eq!(t.pick(), 0);
        t.handle(Key::Prev);
        assert_eq!(t.pick(), 1);
        // typing again starts over at the first match; Tab on a lone match completes it
        type_all(&mut t, "v");
        assert_eq!((t.pick(), t.completions().len()), (0, 1));
        t.handle(Key::Next);
        assert_eq!((t.fields[0].as_str(), t.focus), ("bank/savings", 0));
        t.handle(Key::Next);
        assert_eq!(t.focus, 1); // exact now: Tab went on to the description
        t.set_focus(0);
        t.fields[0] = "sa".into();
        t.handle(Key::Next);
        assert_eq!(t.pick(), 1);

        // a strip too wide for the screen is cut with an ellipsis
        t.cols = 20;
        assert_eq!(t.completion_strip().unwrap(), "bank/savings  …");
        t.cols = 60;

        // an exact name: no strip, and Tab goes on to the description, as before
        t.fields[0] = "drawer".into();
        assert_eq!(t.completion_strip(), None);
        t.handle(Key::Next);
        assert_eq!(t.focus, 1);
        t.set_focus(0);

        // no match: Enter says so and nothing opens
        t.fields[0] = "zzz".into();
        assert_eq!(t.completion_strip(), None);
        t.handle(Key::Enter);
        assert_eq!((t.msg.as_str(), t.account.is_none(), t.fields[0].as_str()), ("zzz: no such account", true, "zzz"));

        // Enter opens the highlighted match: real files this time
        let dir = std::env::temp_dir().join(format!("mdl-tui-complete-{}", std::process::id()));
        for d in ["bank", "cash"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        let files = ["bank/main.md", "bank/savings.md", "cash/drawer.md"];
        for f in files {
            fs::write(dir.join(f), cash_head(3)).unwrap();
        }
        t.accounts = files.iter().map(|f| dir.join(f).to_str().unwrap().to_string()).collect();
        t.fields[0].clear();
        type_all(&mut t, "dr"); // the word start in cash/drawer outranks any d..r in the directory names
        assert!(t.completions()[0].ends_with("cash/drawer"));
        t.handle(Key::Enter);
        assert_eq!((t.account.as_ref().map(|(p, _)| p.as_str()), t.focus, t.rows().len()), (Some(dir.join("cash/drawer.md").to_str().unwrap()), 1, 3));
        assert!(t.fields[0].ends_with("cash/drawer"));
        assert_eq!(t.completion_strip(), None); // the name is exact: nothing to pick
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn click_parsing() {
        assert_eq!(parse_mouse(b"[<0;30;24M"), Some(Key::Click(30, 24)));
        assert_eq!(parse_mouse(b"[<0;30;24m"), Some(Key::Release(30, 24)));
        assert!(parse_mouse(b"[<2;30;24M").is_none()); // right button
        assert_eq!(parse_mouse(b"[<32;30;24M"), Some(Key::Drag(30, 24)));
        assert!(parse_mouse(b"[<34;30;24M").is_none()); // right-button drag
        assert_eq!(parse_mouse(b"[<64;30;24M"), Some(Key::Wheel(-1)));
        assert_eq!(parse_mouse(b"[<65;30;24M"), Some(Key::Wheel(1)));
        assert!(parse_mouse(b"[A").is_none());
    }

    /// The TUI state machine, headless, on a scratch copy of the first rows of cash.md.
    #[test]
    fn tui_select_edit_delete() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.fields[0] = "cash".into();
        t.rebuild();
        assert_eq!(t.rows().len(), 5);
        assert_eq!(t.lines.len(), 1 + 3 + 5 + 3); // title, top border, header, separator, rows, separator, totals, bottom border
        assert_eq!(t.top, 0); // fits: nothing to scroll

        // Up from the account field selects the last entry; Up/Down walk; Down past the end returns
        t.handle(Key::Prev);
        assert_eq!(t.sel, Some(4));
        t.handle(Key::Prev);
        assert_eq!(t.sel, Some(3));
        assert!(t.draw().contains("\x1b[7m│ 2026-09-04 │ Counter sale"));
        assert!(t.draw().contains("row 4: Enter edit"));
        t.handle(Key::Next);
        t.handle(Key::Next);
        assert_eq!(t.sel, None);

        // a click on an entry line selects it: pad is 22 - 12 = 10 blank rows, entries start at row 15
        t.handle(Key::Click(5, 15));
        assert_eq!(t.sel, Some(0));
        t.handle(Key::Click(5, 17));
        assert_eq!(t.sel, Some(2));
        t.handle(Key::Click(5, 12)); // the title row: no entry
        assert_eq!(t.sel, Some(2));

        // Enter edits: date lands in the first field; Esc cancels and restores the account name
        t.handle(Key::Enter);
        assert_eq!((t.editing, t.focus), (Some(2), 1));
        assert_eq!(t.fields, ["2026-09-03", "Coffee", "", "12.00", ""]);
        t.handle(Key::Esc);
        let stem = path.trim_end_matches(".md").to_string();
        assert_eq!((t.editing, t.sel, t.fields[0].as_str()), (None, Some(2), stem.as_str()));

        // edit for real: change the credit, submit from the last field
        t.handle(Key::Enter);
        t.fields[3] = "11.00".into();
        t.focus = 3;
        t.handle(Key::Enter);
        assert_eq!(t.editing, None);
        assert_eq!(t.msg, "row 3 updated");
        assert_eq!(t.rows()[4].balance, 27800);
        assert!(fs::read_to_string(&file).unwrap().contains("|  278.00 |"));

        // a date out of order is refused and stays in edit
        t.handle(Key::Enter);
        t.fields[0] = "2026-09-20".into();
        t.focus = 3;
        t.handle(Key::Enter);
        assert_eq!(t.editing, Some(2));
        assert_eq!(t.msg, "date 2026-09-20 is after row 4 (2026-09-04)");
        t.handle(Key::Esc);

        // delete at once; the selection stays on the same row number
        t.handle(Key::Delete);
        assert_eq!(t.msg, "row 3 deleted");
        assert_eq!((t.rows().len(), t.sel), (4, Some(2)));
        assert!(!fs::read_to_string(&file).unwrap().contains("Coffee"));
        // typing leaves statement mode and goes to the focused field
        t.handle(Key::Char('x'));
        assert_eq!((t.sel, t.fields[0].as_str()), (None, format!("{stem}x").as_str()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_viewport_scrolls() {
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 12;
        t.view = 10;
        let mut rows = vec![];
        for i in 0..30 {
            rows.push(Entry { date: format!("2026-09-{:02}", i % 28 + 1), desc: format!("fila {i}"), debit: 100, credit: 0, balance: 100 * (i + 1), bold: false });
        }
        let doc = Doc { lines: vec![], start: 0, end: 0, header: ["F", "D", "De", "H", "S"].iter().map(|s| s.to_string()).collect(), rows };
        t.account = Some(("x.md".into(), doc));
        t.rebuild();
        assert_eq!(t.lines.len(), 37);
        assert_eq!(t.top, 27); // bottom-aligned
        assert!(t.draw().contains("fila 29"));
        t.handle(Key::Wheel(-1));
        assert_eq!(t.top, 24);
        t.handle(Key::PageUp);
        assert_eq!(t.top, 14);
        t.handle(Key::PageUp);
        t.handle(Key::PageUp);
        assert_eq!(t.top, 0);
        t.handle(Key::PageDown);
        t.handle(Key::PageDown);
        t.handle(Key::PageDown);
        assert_eq!(t.top, 27);
        // selecting a row off screen scrolls to it
        t.handle(Key::Prev); // selects the last row (on screen)
        for _ in 0..20 {
            t.handle(Key::Prev);
        }
        assert_eq!(t.sel, Some(9));
        assert!(t.top <= 4 + 9 && t.top + t.view > 4 + 9);
        // a click on the visible row maps through `top`
        let y = 4 + 9 - t.top + 1;
        t.handle(Key::Click(3, y + 1));
        assert_eq!(t.sel, Some(10));
    }

    #[test]
    fn tui_moves_the_selection() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-move-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        t.handle(Key::Prev); // Supplier payment, row 5
        t.handle(Key::MoveUp);
        assert_eq!(t.msg, "row 5 moved up to row 4, now 2026-09-04");
        assert_eq!(t.sel, Some(3));
        assert!(fs::read_to_string(&file).unwrap().contains("| 2026-09-04 | Supplier payment"));
        t.handle(Key::MoveDown); // same day both ways now: no date note
        assert_eq!(t.msg, "row 4 moved down to row 5");
        t.handle(Key::MoveDown);
        assert_eq!(t.msg, "row 5: already last");
        t.handle(Key::Enter); // editing: Shift keys do nothing
        t.handle(Key::MoveUp);
        assert_eq!((t.editing, t.sel), (Some(4), Some(4)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_sequences() {
        let mut input: &[u8] = b"\x1b[A\x1b[1;2A\x1b[1;2B\x1b[3~\x1b[5~\x1b[<0;5;20M\x1b";
        assert_eq!(read_key(&mut input), Key::Prev);
        assert_eq!(read_key(&mut input), Key::MoveUp);
        assert_eq!(read_key(&mut input), Key::MoveDown);
        assert_eq!(read_key(&mut input), Key::Delete);
        assert_eq!(read_key(&mut input), Key::PageUp);
        assert_eq!(read_key(&mut input), Key::Click(5, 20));
        assert_eq!(read_key(&mut input), Key::Esc);
        assert_eq!(read_key(&mut input), Key::Idle); // nothing more to read
    }

    #[test]
    fn tui_drags_an_entry() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-drag-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        // 10 blank rows, title 11, blank 12, header 13, separator 14, entries 15..19, totals below
        assert_eq!(t.entry_at(14), None);
        assert_eq!(t.entry_at(19), Some(4));
        t.handle(Key::Click(5, 19)); // press on Supplier payment
        assert_eq!(t.drag.as_ref(), Some(&(4, "2026-09-05".to_string())));
        t.handle(Key::Drag(5, 18));
        t.handle(Key::Drag(5, 17)); // up to row 3
        assert_eq!(t.sel, Some(2));
        assert_eq!(t.rows()[2].desc, "Supplier payment");
        assert_eq!(t.rows()[2].date, "2026-09-03");
        assert!(fs::read_to_string(&file).unwrap().contains("| 2026-09-05 | Supplier payment")); // not saved yet
        t.handle(Key::Drag(5, 13)); // the header: ignored
        assert_eq!(t.sel, Some(2));
        t.handle(Key::Release(5, 17));
        assert_eq!(t.msg, "row 5 moved to row 3, now 2026-09-03");
        assert!(t.drag.is_none());
        assert!(fs::read_to_string(&file).unwrap().contains("| 2026-09-03 | Supplier payment"));
        assert!(lint(t.rows()).is_empty());
        // press and release in place: a plain selection, nothing written
        let before = fs::metadata(&file).unwrap().modified().unwrap();
        t.msg.clear(); // the loop does this before every key
        t.handle(Key::Click(5, 15));
        t.handle(Key::Release(5, 15));
        assert_eq!((t.sel, t.msg.as_str()), (Some(0), ""));
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), before);
        // Esc during a drag puts the entry back and keeps it selected; nothing written
        let disk = fs::read_to_string(&file).unwrap();
        t.handle(Key::Click(5, 15));
        t.handle(Key::Drag(5, 16));
        assert_eq!(t.rows()[1].desc, "Opening balance");
        t.handle(Key::Esc);
        assert_eq!((t.msg.as_str(), t.sel, t.drag.is_none()), ("drag cancelled", Some(0), true));
        assert_eq!(t.rows()[0].desc, "Opening balance");
        assert_eq!(fs::read_to_string(&file).unwrap(), disk);
        // Esc on a drag that has not moved is an ordinary Esc: back to the form
        t.handle(Key::Click(5, 15));
        t.handle(Key::Esc);
        assert_eq!((t.sel, t.drag.is_none()), (None, true));
        // any other key finishes the drag and saves it
        t.handle(Key::Click(5, 15));
        t.handle(Key::Drag(5, 16));
        t.handle(Key::Next);
        assert!(t.msg.starts_with("row 1 moved to row 2"));
        assert_ne!(fs::read_to_string(&file).unwrap(), disk);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_space_flags_a_row() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-flag-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        assert_eq!(t.lines[0].0, "Cash"); // title without the `# `
        t.handle(Key::Prev);
        t.handle(Key::Char(' '));
        assert_eq!(t.msg, "row 5 flagged");
        assert!(t.rows()[4].bold);
        assert!(t.draw().contains("\x1b[1m\x1b[7m│ 2026-09-05 │ Supplier payment"));
        assert!(fs::read_to_string(&file).unwrap().contains("| **Supplier payment**"));
        // editing keeps the flag; a second Space clears it
        t.handle(Key::Enter);
        t.focus = 3;
        t.handle(Key::Enter);
        assert!(t.rows()[4].bold);
        t.handle(Key::Char(' '));
        assert_eq!(t.msg, "row 5 unflagged");
        assert!(!fs::read_to_string(&file).unwrap().contains("**"));
        // in the form, Space is just a space
        t.handle(Key::Esc);
        t.handle(Key::Char(' '));
        assert!(t.fields[0].ends_with(' '));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_enter_with_no_amount_adds_a_note() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-note-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        let stem = path.trim_end_matches(".md").to_string();
        // Enter through the empty debit, credit and balance: a note, the balance unchanged
        t.fields = [stem.clone(), "Reconciled with the bank".into(), String::new(), String::new(), String::new()];
        t.focus = 2;
        t.handle(Key::Enter);
        t.handle(Key::Enter);
        assert_eq!(t.focus, 4);
        t.handle(Key::Enter);
        let e = t.rows().last().unwrap();
        assert_eq!((t.rows().len(), e.debit, e.credit, e.balance, e.desc.as_str()), (6, 0, 0, 27700, "Reconciled with the bank"));
        assert!(lint(t.rows()).is_empty());
        assert!(fs::read_to_string(&path).unwrap().contains("| Reconciled with the bank |        |        |  277.00 |"));
        // editing it into a payment, and an insert of a note below row 2
        t.sel = Some(5);
        t.handle(Key::Enter);
        assert_eq!(t.editing, Some(5));
        t.fields[2] = "10".into();
        t.focus = 2;
        t.handle(Key::Enter);
        assert_eq!((t.rows()[5].debit, t.rows()[5].balance, t.editing), (1000, 28700, None));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn amount_views() {
        assert_eq!(amount_view("150", 10, true, usize::MAX), ("150       ".to_string(), 3));
        assert_eq!(amount_view("150.", 10, true, usize::MAX), ("    150.  ".to_string(), 8));
        assert_eq!(amount_view("150.5", 10, true, usize::MAX), ("    150.5 ".to_string(), 9));
        assert_eq!(amount_view("-0.5", 10, true, usize::MAX), ("     -0.5 ".to_string(), 9));
        assert_eq!(amount_view("150.50", 10, false, usize::MAX), ("    150.50".to_string(), 6));
        assert_eq!(amount_view("", 10, false, usize::MAX), ("          ".to_string(), 0));
        assert_eq!(amount_view("", 10, true, usize::MAX), ("          ".to_string(), 0));
        // the cursor inside: before the 3rd digit, on the point, on the 1st decimal
        assert_eq!(amount_view("150.50", 10, true, 2).1, 6);
        assert_eq!(amount_view("150.50", 10, true, 3).1, 7);
        assert_eq!(amount_view("150.50", 10, true, 4).1, 8);
        assert_eq!(amount_view("150", 10, true, 1).1, 1);
    }

    #[test]
    fn tui_balance_to_reach() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-balance-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        let stem = path.trim_end_matches(".md").to_string();
        let type_all = |t: &mut Tui, s: &str| s.chars().for_each(|c| assert!(t.handle(Key::Char(c))));
        assert_eq!(t.rows().last().unwrap().balance, 27700);

        // Enter through an empty debit and credit reaches the balance field; Tab never does
        t.fields = [stem.clone(), "Reconciled".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Next);
        t.handle(Key::Next);
        t.handle(Key::Next);
        assert_eq!(t.focus, 3);
        t.handle(Key::Enter);
        assert_eq!(t.focus, 4);
        assert!(t.draw().contains("balance to reach: Enter adds"));
        // a lower balance is a credit of the difference
        type_all(&mut t, "270");
        t.handle(Key::Enter);
        let e = t.rows().last().unwrap();
        assert_eq!((t.rows().len(), e.debit, e.credit, e.balance, e.desc.as_str()), (6, 0, 700, 27000, "Reconciled"));
        assert_eq!((t.focus, t.fields[4].as_str()), (0, ""));

        // a higher one a debit; the second decimal submits from there too
        t.fields = [stem.clone(), "Reconciled 2".into(), String::new(), String::new(), String::new()];
        t.focus = 2;
        t.handle(Key::Enter);
        t.handle(Key::Enter);
        assert_eq!(t.focus, 4);
        type_all(&mut t, "300.25");
        let e = t.rows().last().unwrap();
        assert_eq!((t.rows().len(), e.debit, e.credit, e.balance), (7, 3025, 0, 30025));

        // the same balance, or an amount as well, is refused
        t.fields = [stem.clone(), "x".into(), String::new(), String::new(), "300.25".into()];
        t.focus = 4;
        t.handle(Key::Enter);
        assert_eq!((t.rows().len(), t.msg.as_str()), (7, "the balance is 300.25 already"));
        t.fields = [stem.clone(), "x".into(), "1.00".into(), String::new(), "5".into()];
        t.focus = 4;
        t.handle(Key::Enter);
        assert_eq!((t.rows().len(), t.msg.as_str()), (7, "an amount or a balance to reach, not both"));

        // a click on the balance cell focuses it
        t.handle(Key::Click(cell_start(&t.w, 4) + 1, t.h));
        assert_eq!(t.focus, 4);

        // editing row 3 (Coffee, after a balance of 150.00): a balance of 100 makes it a credit of 50
        t.fields = [stem.clone(), String::new(), String::new(), String::new(), String::new()];
        t.focus = 0;
        for _ in 0..5 {
            t.handle(Key::Prev);
        }
        assert_eq!(t.sel, Some(2));
        t.handle(Key::Enter);
        t.fields[3].clear();
        t.fields[4] = "100".into();
        t.focus = 4;
        t.handle(Key::Enter);
        let e = &t.rows()[2];
        assert_eq!((e.debit, e.credit, e.balance, t.editing), (0, 5000, 10000, None));

        // `n` below row 3: a balance of 130 is a debit of 30
        t.handle(Key::Char('n'));
        t.fields[1] = "Adjustment".into();
        t.fields[4] = "130".into();
        t.focus = 4;
        t.handle(Key::Enter);
        let e = &t.rows()[3];
        assert_eq!((e.desc.as_str(), e.debit, e.balance, t.sel), ("Adjustment", 3000, 13000, Some(3)));
        assert_eq!(lint(t.rows()), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_cursor_in_fields() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-cursor-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        t.fields[0] = path.trim_end_matches(".md").to_string();
        t.focus = 1;
        let type_all = |t: &mut Tui, s: &str| s.chars().for_each(|c| assert!(t.handle(Key::Char(c))));

        // text: Left, insert, Home, End, Backspace and Delete at the cursor (chars, not bytes)
        type_all(&mut t, "Vnta cfé");
        for _ in 0..7 {
            t.handle(Key::Left);
        }
        t.handle(Key::Char('e'));
        assert_eq!((t.fields[1].as_str(), t.cur()), ("Venta cfé", 2));
        t.handle(Key::End);
        t.handle(Key::Left);
        t.handle(Key::Left);
        t.handle(Key::Char('a'));
        assert_eq!((t.fields[1].as_str(), t.cur()), ("Venta café", 8));
        t.handle(Key::Home);
        t.handle(Key::Delete);
        t.handle(Key::Right);
        t.handle(Key::Backspace);
        assert_eq!((t.fields[1].as_str(), t.cur()), ("nta café", 0));
        t.handle(Key::Backspace); // nothing before the cursor
        assert_eq!(t.fields[1], "nta café");
        t.handle(Key::End);
        assert!(t.draw().contains("nta café"));

        // amounts: fix a digit without retyping; the second decimal submits only at the end
        t.handle(Key::Next);
        assert_eq!((t.focus, t.cur()), (2, 0));
        type_all(&mut t, "1234.5");
        for _ in 0..3 {
            t.handle(Key::Left);
        }
        t.handle(Key::Backspace);
        t.handle(Key::Char('9'));
        assert_eq!((t.fields[2].as_str(), t.cur(), t.focus), ("1294.5", 3, 2));
        t.handle(Key::Home);
        t.handle(Key::Char('7'));
        assert_eq!((t.fields[2].as_str(), t.cur()), ("71294.5", 1));
        t.handle(Key::Delete);
        assert_eq!(t.fields[2], "7294.5");
        t.handle(Key::End);
        t.handle(Key::Char('0')); // second decimal at the end: submits
        assert_eq!((t.rows().len(), t.rows()[5].debit, t.focus), (6, 729450, 0));

        // a point with nothing in front gets its 0; too many decimals are refused
        t.fields[1] = "x".into();
        t.focus = 2;
        t.cur = usize::MAX;
        type_all(&mut t, "50");
        t.handle(Key::Home);
        t.handle(Key::Char('.'));
        assert_eq!((t.fields[2].as_str(), t.cur()), ("0.50", 2));
        t.handle(Key::Char('1'));
        assert_eq!(t.fields[2], "0.50");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_amount_entry() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-amount-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        let stem = path.trim_end_matches(".md").to_string();
        t.fields[0] = stem.clone();
        t.fields[1] = "d1".into();
        t.focus = 2;
        let type_all = |t: &mut Tui, s: &str| s.chars().for_each(|c| assert!(t.handle(Key::Char(c))));

        type_all(&mut t, "150");
        assert_eq!((t.fields[2].as_str(), t.focus), ("150", 2));
        type_all(&mut t, ".5");
        assert_eq!((t.fields[2].as_str(), t.focus), ("150.5", 2));
        type_all(&mut t, "0"); // second decimal of a debit: the entry is submitted
        assert_eq!((t.rows().len(), t.rows()[5].debit, t.focus), (6, 15050, 0));
        assert!(fs::read_to_string(&file).unwrap().contains("| d1 "));

        // Enter on an empty debit goes to the credit; on a filled one it submits
        t.fields = [stem.clone(), "d2".into(), String::new(), String::new(), String::new()];
        t.focus = 2;
        t.handle(Key::Enter);
        assert_eq!(t.focus, 3);
        t.focus = 2;
        type_all(&mut t, "3");
        t.handle(Key::Enter);
        assert_eq!((t.rows().len(), t.rows()[6].debit, t.focus), (7, 300, 0));

        // coming back to a filled amount: Backspace clears it whole, typing replaces it
        t.fields = [stem.clone(), "d3".into(), "150.50".into(), "12.00".into(), String::new()];
        t.set_focus(3);
        assert!(t.select);
        t.handle(Key::Backspace);
        assert_eq!(t.fields[3], "");
        t.handle(Key::Backspace); // nothing left: harmless
        t.set_focus(2);
        type_all(&mut t, "15");
        assert_eq!(t.fields[2], "15");
        t.handle(Key::Backspace); // after typing, Backspace is one character again
        assert_eq!(t.fields[2], "1");
        t.fields[2] = "150.50".into();
        t.set_focus(3);
        t.set_focus(2);
        assert!(t.select);
        t.handle(Key::Next); // moving away disarms without touching the value
        assert_eq!(t.fields[2], "150.50");
        t.set_focus(2);
        // only digits, one point, a leading minus; the typed 9 replaces the old value
        type_all(&mut t, "9x.-");
        assert_eq!(t.fields[2], "9.");
        t.fields[2].clear();
        type_all(&mut t, "-.5");
        assert_eq!(t.fields[2], "-0.5"); // a leading point gets its zero
        type_all(&mut t, "0"); // submit attempt: a negative debit is refused, focus stays
        assert_eq!((t.fields[2].as_str(), t.focus), ("-0.50", 2));
        assert!(t.msg.contains("positive"));

        // leaving by Tab normalises too, and 0 clears
        t.fields[2].clear();
        type_all(&mut t, "7");
        t.handle(Key::Next);
        assert_eq!((t.fields[2].as_str(), t.focus), ("7.00", 3));
        t.handle(Key::Prev); // back on the filled debit: armed, the 0 replaces it
        type_all(&mut t, "0");
        assert_eq!(t.fields[2], "0");
        t.handle(Key::Next); // and 0 normalises to empty
        assert_eq!((t.fields[2].as_str(), t.focus), ("", 3));
        assert!(t.draw().contains("\x1b[100m          \x1b[0m")); // empty debit, right-aligned view

        // the second decimal in the credit field submits, like Enter
        type_all(&mut t, "12.34");
        let last = t.rows().last().unwrap();
        assert_eq!((t.rows().len(), last.credit, last.desc.as_str(), t.focus), (8, 1234, "d3", 0));

        // editing a credit row into a debit: Enter on the debit shows the credit first
        t.handle(Key::Prev); // select the last row (credit 12.34)
        t.handle(Key::Enter); // edit; focus on the description
        t.set_focus(2);
        type_all(&mut t, "5");
        t.handle(Key::Enter);
        assert_eq!((t.editing, t.focus, t.fields[3].as_str()), (Some(7), 3, "12.34"));
        t.fields[3].clear();
        t.handle(Key::Prev);
        t.handle(Key::Enter); // credit now empty: submits the edit
        assert_eq!((t.editing, t.rows()[7].debit, t.rows()[7].credit), (None, 500, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_skips_attic() {
        let dir = std::env::temp_dir().join(format!("mdl-scan-{}", std::process::id()));
        for d in ["cash", "Attic/old", "attic", ".hidden", "target"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["cash/ABC.md", "Attic/old/X.md", "Attic/Y.md", "attic/Z.md", ".hidden/H.md", "target/T.md", "top.md", "notes.txt"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let mut found = vec![];
        scan_accounts(&dir, &mut found);
        let mut names: Vec<String> = found.iter().map(|p| p.strip_prefix(dir.to_str().unwrap()).unwrap().trim_start_matches('/').to_string()).collect();
        names.sort();
        assert_eq!(names, ["cash/ABC.md", "top.md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_inserts_below() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-insert-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.fields[0] = "cash".into();
        t.rebuild();
        for _ in 0..4 {
            t.handle(Key::Prev);
        }
        assert_eq!(t.sel, Some(1)); // Counter sale, 2026-09-02

        // `n`: the fields hold a new entry with that date, cursor in the description
        t.handle(Key::Char('n'));
        assert_eq!((t.editing, t.insert, t.focus), (Some(1), true, 1));
        assert_eq!(t.fields, ["2026-09-02", "", "", "", ""]);
        assert!(t.draw().contains("new row below row 2: Enter saves"));
        for c in "Extra".chars() {
            t.handle(Key::Char(c));
        }
        t.handle(Key::Next);
        t.handle(Key::Char('1'));
        t.handle(Key::Char('0'));
        t.handle(Key::Enter); // a filled debit submits
        let rows = t.rows();
        assert_eq!((rows.len(), rows[2].desc.as_str(), rows[2].date.as_str(), rows[2].debit, rows[2].balance, rows[3].balance), (6, "Extra", "2026-09-02", 1000, 16000, 14800));
        assert_eq!((t.editing, t.insert, t.sel, t.msg.as_str()), (None, false, Some(2), "row 3 added"));
        assert!(fs::read_to_string(&file).unwrap().contains("| 2026-09-02 | Extra"));

        // the new row is selected, so `n` chains; a date out of order is refused, Esc cancels
        t.handle(Key::Char('n'));
        assert_eq!((t.editing, t.fields[0].as_str()), (Some(2), "2026-09-02"));
        t.fields = ["2026-09-06".into(), "Late".into(), String::new(), "5.00".into(), String::new()];
        t.focus = 3;
        t.handle(Key::Enter);
        assert_eq!((t.editing, t.msg.as_str()), (Some(2), "date 2026-09-06 is after row 4 (2026-09-03)"));
        t.handle(Key::Esc);
        assert_eq!((t.editing, t.insert, t.sel, t.rows().len()), (None, false, Some(2), 6));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_matches() {
        let rows = load("cash.md").unwrap().rows;
        let sale = &rows[1]; // 2026-09-02 Counter sale 150.00 | 150.00
        assert!(entry_matches(sale, "150"));
        assert!(entry_matches(sale, "15"));
        assert!(!entry_matches(sale, "50"));
        assert!(entry_matches(sale, "COUNTER"));
        assert!(entry_matches(sale, "09-02"));
        assert!(!entry_matches(sale, "x"));
        assert!(entry_matches(sale, "-")); // a date
        let e = Entry { date: "2026-01-01".into(), desc: "x".into(), debit: 0, credit: 8000, balance: -8000, bold: false };
        assert!(entry_matches(&e, "80"));
        assert!(entry_matches(&e, "-80"));
        assert!(!entry_matches(&e, "0."));
    }

    #[test]
    fn tui_clears_and_searches() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-search-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.h = 24;
        t.view = 22;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.fields = ["cash".into(), "something".into(), "1.00".into(), String::new(), String::new()];
        t.focus = 2;
        t.rebuild();
        t.handle(Key::Prev);
        t.handle(Key::Prev);
        assert_eq!((t.sel, t.focus), (None, 0));
        t.handle(Key::Prev);
        assert_eq!(t.sel, Some(4));

        // Ctrl-L: no account, empty fields, cursor in the account field
        t.handle(Key::Clear);
        assert_eq!((t.account.is_none(), t.sel, t.focus, t.lines.len()), (true, None, 0, 0));
        assert_eq!(t.fields, ["", "", "", "", ""]);
        t.draw();

        // `/` in a description with text is a character; on an empty one it opens the prompt
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.fields[0] = "cash".into();
        t.rebuild();
        t.focus = 1;
        t.fields[1] = "a".into();
        t.handle(Key::Char('/'));
        assert_eq!((t.search.is_none(), t.fields[1].as_str()), (true, "a/"));
        t.fields[1].clear();
        t.handle(Key::Char('/'));
        assert_eq!((t.search.as_deref(), t.sel), (Some(""), None));
        assert!(t.draw().contains("\x1b[23;2H"));

        // typing searches upward from the bottom: "sa" is the Counter sale of row 4, then row 2
        t.handle(Key::Char('s'));
        t.handle(Key::Char('a'));
        assert_eq!(t.sel, Some(3));
        assert!(t.draw().contains("/sa   Enter/Up older"));
        t.handle(Key::Enter);
        assert_eq!((t.sel, t.msg.as_str()), (Some(1), ""));
        t.handle(Key::Enter);
        assert_eq!((t.sel, t.msg.as_str()), (Some(3), "wrapped"));
        t.msg.clear();
        t.handle(Key::Next);
        assert_eq!((t.sel, t.msg.as_str()), (Some(1), "wrapped"));
        t.msg.clear();
        t.handle(Key::Next);
        assert_eq!((t.sel, t.msg.as_str()), (Some(3), ""));

        // no match keeps the selection; an empty query restores the one the search began with
        t.handle(Key::Char('z'));
        assert_eq!((t.sel, t.msg.as_str()), (Some(3), "no match"));
        t.msg.clear();
        for _ in 0..3 {
            t.handle(Key::Backspace);
        }
        assert_eq!((t.search.as_deref(), t.sel), (Some(""), None));

        // amounts: "35" is the balance after row 4; Esc keeps it selected, Enter then edits
        t.handle(Key::Char('3'));
        t.handle(Key::Char('5'));
        assert_eq!(t.sel, Some(3));
        t.handle(Key::Esc);
        assert_eq!((t.search.is_none(), t.sel), (true, Some(3)));
        t.handle(Key::Enter);
        assert_eq!(t.editing, Some(3));
        t.handle(Key::Esc);
        assert_eq!((t.editing, t.sel), (None, Some(3)));

        // a search from a selected row starts there; Backspace on an empty prompt closes it
        t.handle(Key::Char('/'));
        t.handle(Key::Char('1'));
        t.handle(Key::Char('5'));
        assert_eq!(t.sel, Some(1));
        t.handle(Key::Backspace);
        t.handle(Key::Backspace);
        t.handle(Key::Backspace);
        assert_eq!((t.search.is_none(), t.sel), (true, Some(3)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_fills_the_amount_from_the_description() {
        let dir = std::env::temp_dir().join(format!("mdl-tui-expr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cash.md");
        fs::write(&file, cash_head(5)).unwrap();
        let path = file.to_str().unwrap().to_string();
        let mut t = Tui::new();
        t.in_repo = false;
        t.account = Some((path.clone(), load(&path).unwrap()));
        t.rebuild();
        let stem = path.trim_end_matches(".md").to_string();
        let type_all = |t: &mut Tui, s: &str| s.chars().for_each(|c| assert!(t.handle(Key::Char(c))));

        // Enter from the description fills the debit, selected; Enter again submits it
        t.fields = [stem.clone(), String::new(), String::new(), String::new(), String::new()];
        t.focus = 1;
        type_all(&mut t, "#1000*40.50 cambio");
        t.handle(Key::Enter);
        assert_eq!((t.focus, t.fields[2].as_str(), t.prefill, t.select), (2, "40500.00", Some(2), true));
        t.handle(Key::Enter);
        let e = t.rows().last().unwrap();
        assert_eq!((t.rows().len(), e.debit, e.desc.as_str()), (6, 4050000, "#1000*40.50 cambio"));
        assert!(lint(t.rows()).is_empty());

        // the fill follows Tab into the credit, and submits from there
        t.fields = [stem.clone(), "#100+200 varios".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        t.handle(Key::Next);
        assert_eq!((t.focus, t.fields[2].as_str(), t.fields[3].as_str(), t.prefill), (3, "", "300.00", Some(3)));
        t.handle(Key::Prev); // and back
        assert_eq!((t.focus, t.fields[2].as_str(), t.fields[3].as_str()), (2, "300.00", ""));
        t.handle(Key::Enter); // a positive computation is a debit
        assert_eq!((t.rows().len(), t.rows()[6].debit), (7, 30000));

        // typing over the fill makes it the user's: it no longer travels
        t.fields = [stem.clone(), "#2*5 x".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        type_all(&mut t, "7");
        assert_eq!((t.fields[2].as_str(), t.prefill), ("7", None));
        t.handle(Key::Next);
        assert_eq!((t.fields[2].as_str(), t.fields[3].as_str()), ("7.00", ""));

        // back to the description with a new formula: the untouched fill is recomputed,
        // and one that no longer computes is cleared
        t.fields = [stem.clone(), "#2*5 x".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        assert_eq!(t.fields[2], "10.00");
        t.handle(Key::Prev);
        t.fields[1] = "#3*5 x".into();
        t.handle(Key::Enter);
        assert_eq!((t.fields[2].as_str(), t.prefill), ("15.00", Some(2)));
        t.handle(Key::Next); // travels to the credit
        t.handle(Key::Prev);
        t.handle(Key::Prev); // back to the description from the debit
        t.fields[1] = "plain text".into();
        t.handle(Key::Enter);
        assert_eq!((t.fields[2].as_str(), t.fields[3].as_str(), t.prefill), ("", "", None));

        // a negative computation fills the credit and goes there: formula, Enter, Enter
        t.fields = [stem.clone(), "#-1000*40.50 venta USD".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        assert_eq!((t.focus, t.fields[2].as_str(), t.fields[3].as_str(), t.prefill), (3, "", "40500.00", Some(3)));
        t.handle(Key::Enter);
        let e = t.rows().last().unwrap();
        assert_eq!((e.credit, e.debit, e.desc.as_str()), (4050000, 0, "#-1000*40.50 venta USD"));
        assert!(lint(t.rows()).is_empty());

        // a bad `#` computation is reported, no fill; prose fills nothing
        t.fields = [stem.clone(), "#10/0 x".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        assert_eq!((t.fields[2].as_str(), t.msg.as_str()), ("", "bad expression `10/0`: division by zero"));
        t.fields = [stem.clone(), "1000*40.50 cambio".into(), String::new(), String::new(), String::new()];
        t.focus = 1;
        t.handle(Key::Enter);
        assert_eq!((t.fields[2].as_str(), t.prefill), ("", None));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrapping_and_layout() {
        assert_eq!(wrap_words("Pago proveedor de larga descripción", 14), ["Pago proveedor", "de larga", "descripción"]);
        assert_eq!(wrap_words("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_words("", 4), [""]);

        let header: Vec<String> = ["Fecha", "Descripción", "Debe", "Haber", "Saldo"].iter().map(|s| s.to_string()).collect();
        let rows = vec![
            Entry { date: "2026-09-01".into(), desc: "Saldo  inicial".into(), debit: 0, credit: 0, balance: 0, bold: false },
            Entry { date: "2026-09-03".into(), desc: "Venta mostrador con una descripción bastante larga".into(), debit: 15000, credit: 0, balance: 15000, bold: false },
        ];
        // natural widths never wrap, and keep internal spacing (file round-trip)
        assert!(render(&header, &rows).contains("| Saldo  inicial "));
        assert_eq!(render(&header, &rows).lines().count(), 4);

        let g = grid(&rows, false);
        let w = tui_widths(&header, &g, 80, 8);
        assert_eq!(w, [10, 28, 10, 10, 6]); // 16 + 10 + 10 + 10 + 6 = 52 fixed, 28 left
        let t = render_cols(&header, &g, &w);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l.chars().count() == 80));
        assert!(lines[3].starts_with("| 2026-09-03 | Venta mostrador con una      |"));
        assert!(lines[4].starts_with("|            | descripción bastante larga   |"));
        assert_eq!(cell_start(&w, 1), 15);
        assert_eq!(cell_start(&w, 2), 46);

        // wide terminal: the description fits its longest entry, and is at least DESC_MIN
        assert_eq!(tui_widths(&header, &g, 200, 8)[1], 50);
        assert_eq!(tui_widths(&header, &g[..1], 200, 8)[1], 48);

        assert_eq!(field_view("hello", 10, usize::MAX), ("hello".into(), 5));
        assert_eq!(field_view("hello world", 6, usize::MAX), ("world".into(), 5));
        assert_eq!(field_view("hello world", 6, 7), ("world".into(), 1));
        assert_eq!(field_view("hello world", 6, 0), ("hello ".into(), 0)); // scrolled to the cursor
        assert_eq!(field_view("hello world", 6, 3), ("lo wor".into(), 0));
        assert_eq!(byte_at("añb", 2), 3);
        assert_eq!(byte_at("añb", 9), 4);
    }
}
