//! The table as text, for the file and the screen, and as JSON.
use crate::ledger::{Entry, fmt_amount, fmt_col};

/// Cells as text. `marked`: flagged descriptions carry their `**` (the file); the screen
/// shows them bold instead.
pub fn grid(rows: &[Entry], marked: bool) -> Vec<[String; 5]> {
    rows.iter()
        .map(|e| {
            let desc = if marked && e.bold { format!("**{}**", e.desc) } else { e.desc.clone() };
            [e.date.clone(), desc, fmt_col(e.debit), fmt_col(e.credit), fmt_amount(e.balance)]
        })
        .collect()
}

/// Widest cell per column, header included.
pub fn natural_widths(header: &[String], grid: &[[String; 5]]) -> [usize; 5] {
    let width = |i: usize| grid.iter().map(|r| r[i].chars().count()).chain([header[i].chars().count()]).max().unwrap();
    [width(0), width(1), width(2), width(3), width(4)]
}

/// Greedy word wrap to `w` columns; a word longer than `w` is split.
pub fn wrap_words(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let cur_len = cur.chars().count();
            let need = if cur.is_empty() { 0 } else { 1 } + word.len();
            if cur_len + need <= w {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.extend(&word);
                break;
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                continue;
            }
            cur.extend(word.drain(..w));
            lines.push(std::mem::take(&mut cur));
            if word.is_empty() {
                break;
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Table lines with the given column widths, each with the entry it belongs to
/// (None for the header and separator). A description wider than `w[1]` wraps onto
/// continuation lines whose other cells are blank (never happens with natural widths).
/// `foot`: box-drawing borders for the screen instead of GFM's `|` and `---:`, with a
/// top border line first and, after the rows, a separator, the given footer row (the
/// totals) and a bottom border; the column arithmetic is the same since each border
/// is one cell wide.
pub fn render_lines(header: &[String], grid: &[[String; 5]], w: &[usize; 5], foot: Option<&[String; 5]>) -> Vec<(String, Option<usize>)> {
    let boxed = foot.is_some();
    let v = if boxed { "│" } else { "|" };
    let line = |r: [&str; 5]| {
        format!(
            "{v} {:<w0$} {v} {:<w1$} {v} {:>w2$} {v} {:>w3$} {v} {:>w4$} {v}",
            r[0], r[1], r[2], r[3], r[4],
            w0 = w[0], w1 = w[1], w2 = w[2], w3 = w[3], w4 = w[4]
        )
    };
    let mut out = vec![];
    let border = |l: &str, m: &str, r: &str| {
        let d = |n: usize| "─".repeat(n + 2);
        format!("{l}{}{m}{}{m}{}{m}{}{m}{}{r}", d(w[0]), d(w[1]), d(w[2]), d(w[3]), d(w[4]))
    };
    let sep = if boxed {
        out.push((border("┌", "┬", "┐"), None));
        border("├", "┼", "┤")
    } else {
        format!("|{}|{}|{}|{}|{}|", "-".repeat(w[0] + 2), "-".repeat(w[1] + 2), "-".repeat(w[2] + 1) + ":", "-".repeat(w[3] + 1) + ":", "-".repeat(w[4] + 1) + ":")
    };
    out.push((line([&header[0], &header[1], &header[2], &header[3], &header[4]]), None));
    out.push((sep, None));
    for (i, r) in grid.iter().enumerate() {
        let desc = if r[1].chars().count() > w[1] { wrap_words(&r[1], w[1]) } else { vec![r[1].clone()] };
        out.push((line([&r[0], &desc[0], &r[2], &r[3], &r[4]]), Some(i)));
        for d in &desc[1..] {
            out.push((line(["", d, "", "", ""]), Some(i)));
        }
    }
    if let Some(f) = foot {
        out.push((border("├", "┼", "┤"), None));
        out.push((line([&f[0], &f[1], &f[2], &f[3], &f[4]]), None));
        out.push((border("└", "┴", "┘"), None));
    }
    out
}

/// The statement's footer: the sums of the debit and credit columns.
pub fn totals(rows: &[Entry]) -> [String; 5] {
    let sum = |f: fn(&Entry) -> i64| fmt_col(rows.iter().map(f).sum());
    [String::new(), String::new(), sum(|e| e.debit), sum(|e| e.credit), String::new()]
}

pub fn render_cols(header: &[String], grid: &[[String; 5]], w: &[usize; 5]) -> String {
    render_lines(header, grid, w, None).into_iter().map(|(l, _)| l + "\n").collect()
}

pub fn render(header: &[String], rows: &[Entry]) -> String {
    let g = grid(rows, true);
    render_cols(header, &g, &natural_widths(header, &g))
}

pub fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out + "\""
}

pub fn render_json(title: Option<&str>, rows: &[Entry]) -> String {
    let entries: Vec<String> = rows
        .iter()
        .map(|e| {
            format!(
                "  {{\"date\": \"{}\", \"description\": {}, \"debit\": {}, \"credit\": {}, \"balance\": {}{}}}",
                e.date, json_str(&e.desc), fmt_amount(e.debit), fmt_amount(e.credit), fmt_amount(e.balance),
                if e.bold { ", \"bold\": true" } else { "" }
            )
        })
        .collect();
    format!(
        "{{\"title\": {}, \"entries\": [\n{}\n]}}\n",
        title.map_or("null".into(), json_str),
        entries.join(",\n")
    )
}
