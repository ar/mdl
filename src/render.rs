//! The table as text, for the file and the screen, and as JSON or CSV.
use crate::ledger::{Entry, fmt_amount, fmt_col};

/// Cells as text. `marked`: flagged descriptions carry their `**` (the file); the screen
/// shows them bold instead.
/// The rows as cell text. `marked` is the file's form: a flagged description between
/// `**`, and any `|` in one escaped `\|` so it cannot end the cell (GFM reads it as a
/// pipe); the screen, the PDF and JSON take the plain text.
pub fn grid(rows: &[Entry], marked: bool) -> Vec<[String; 5]> {
    rows.iter()
        .map(|e| {
            let desc = match (marked, e.bold) {
                (false, _) => e.desc.clone(),
                (true, false) => e.desc.replace('|', "\\|"),
                (true, true) => format!("**{}**", e.desc.replace('|', "\\|")),
            };
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

/// A CLI statement using the TUI's borders and totals. Include the totals in the
/// width calculation so a sum wider than any entry still fits its column. Styling
/// is optional so redirected output contains only the readable Unicode table.
pub fn render_pretty(header: &[String], rows: &[Entry], styled: bool) -> String {
    let g = grid(rows, false);
    let foot = totals(rows);
    let w = natural_widths(header, &[g.as_slice(), std::slice::from_ref(&foot)].concat());
    let lines = render_lines(header, &g, &w, Some(&foot));
    let total_line = lines.len() - 2;
    lines.into_iter().enumerate().map(|(i, (line, entry))| {
        if styled && (i == 1 || i == total_line || entry.is_some_and(|j| rows[j].bold)) {
            format!("\x1b[1m{line}\x1b[0m\n")
        } else {
            line + "\n"
        }
    }).collect()
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

/// RFC 4180: a cell is quoted when it holds a comma, a quote or a line break, with
/// quotes doubled.
fn csv_str(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// The account's own column labels as the header row, one row per entry; amounts as
/// in JSON, so an empty debit or credit reads 0.00.
pub fn render_csv(header: &[String], rows: &[Entry]) -> String {
    let line = |cells: [String; 5]| cells.iter().map(|c| csv_str(c)).collect::<Vec<_>>().join(",") + "\n";
    let mut out = line([header[0].clone(), header[1].clone(), header[2].clone(), header[3].clone(), header[4].clone()]);
    for e in rows {
        out += &line([e.date.clone(), e.desc.clone(), fmt_amount(e.debit), fmt_amount(e.credit), fmt_amount(e.balance)]);
    }
    out
}

/// Closing balances with a final total; quiet output is suitable for pipelines.
pub fn render_balances(accounts: &[(String, i64)], format: &str, only_total: bool, styled: bool) -> String {
    let total = fmt_amount(accounts.iter().map(|(_, amount)| amount).sum());
    let rows: Vec<_> = accounts.iter().filter(|_| !only_total)
        .map(|(account, amount)| (account.as_str(), fmt_amount(*amount))).collect();
    if format == "quiet" {
        return rows.iter().map(|(_, amount)| amount.clone() + "\n").collect::<String>() + &total + "\n";
    }
    if format == "json" {
        let entries = rows.iter().map(|(account, amount)| {
            format!("    {{\"account\": {}, \"balance\": {amount}}}", json_str(account))
        }).collect::<Vec<_>>().join(",\n");
        return format!("{{\"accounts\": [\n{entries}\n], \"total\": {total}}}\n");
    }
    if format == "csv" {
        let mut out = String::from("Account,Balance\n");
        for (account, amount) in &rows {
            out += &format!("{},{amount}\n", csv_str(account));
        }
        return out + &format!("Total,{total}\n");
    }
    let markdown = format == "markdown";
    let rows: Vec<_> = rows.iter().map(|(account, amount)| {
        let account = account.replace('\r', "\\r").replace('\n', "\\n").replace('\t', "\\t");
        (if markdown { account.replace('|', "\\|") } else { account }, amount)
    }).collect();
    let aw = rows.iter().map(|(a, _)| a.chars().count()).chain([7]).max().unwrap();
    let bw = rows.iter().map(|(_, b)| b.len()).chain([7, total.len()]).max().unwrap();
    let v = if markdown { "|" } else { "│" };
    let line = |a: &str, b: &str| format!("{v} {a:<aw$} {v} {b:>bw$} {v}\n");
    let border = |l, m, r| format!("{l}{}{m}{}{r}\n", "─".repeat(aw + 2), "─".repeat(bw + 2));
    let emphasize = |s: String| if styled && !markdown { format!("\x1b[1m{}\x1b[0m\n", s.trim_end()) } else { s };
    let mut out = if markdown { String::new() } else { border("┌", "┬", "┐") };
    out += &emphasize(line("Account", "Balance"));
    out += &if markdown {
        format!("|{}|{}:|\n", "-".repeat(aw + 2), "-".repeat(bw + 1))
    } else { border("├", "┼", "┤") };
    for (account, amount) in &rows {
        out += &line(account, amount);
    }
    if !markdown && !rows.is_empty() {
        out += &border("├", "┼", "┤");
    }
    out += &emphasize(line("Total", &total));
    if !markdown {
        out += &border("└", "┴", "┘");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_formats_escape_names_and_handle_zero_and_negative_amounts() {
        let accounts = vec![("Café | \"bank\",\nnext".into(), -125), ("empty".into(), 0)];
        assert_eq!(render_balances(&accounts, "quiet", false, true), "-1.25\n0.00\n-1.25\n");
        assert_eq!(render_balances(&[], "quiet", false, false), "0.00\n");
        assert!(render_balances(&accounts, "json", false, true).contains("Café | \\\"bank\\\",\\nnext"));
        assert!(render_balances(&accounts, "csv", false, true).contains("\"Café | \"\"bank\"\",\nnext\",-1.25\n"));
        assert!(render_balances(&accounts, "markdown", false, true).contains("Café \\|"));
        let pretty = render_balances(&accounts, "pretty", false, false);
        let width = pretty.lines().next().unwrap().chars().count();
        assert!(pretty.lines().all(|l| l.chars().count() == width));
        assert!(render_balances(&accounts, "pretty", false, true).contains("\x1b[1m│ Total"));
        assert_eq!(render_balances(&accounts, "csv", true, false), "Account,Balance\nTotal,-1.25\n");
    }

    #[test]
    fn pretty_totals_fit_and_descriptions_remain_literal() {
        let header = ["Fecha", "Descripción", "D", "C", "Saldo"].map(String::from);
        let rows: Vec<Entry> = (1..=2).map(|day| Entry {
            date: format!("2026-09-{day:02}"),
            desc: "Café | caja".into(),
            debit: 99999,
            credit: 0,
            balance: day * 99999,
            bold: day == 1,
        }).collect();
        let plain = render_pretty(&header, &rows, false);
        let width = plain.lines().next().unwrap().chars().count();
        assert!(plain.lines().all(|l| l.chars().count() == width));
        assert!(plain.contains("1999.98"));
        assert!(plain.contains("Café | caja"));
        assert!(!plain.contains("\\|"));
        assert!(!plain.contains("**"));
        assert!(!plain.contains('\x1b'));
        let styled = render_pretty(&header, &rows, true);
        assert!(styled.contains("\x1b[1m│ 2026-09-01"));
        assert!(!styled.contains("\x1b[1m│ 2026-09-02"));
        assert!(render_pretty(&header, &[], false).ends_with("┘\n"));
    }
}
