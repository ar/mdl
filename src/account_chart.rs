//! Optional Markdown account hierarchy and balance snapshots.
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::ledger::{balance, fmt_amount, load, resolve};
use crate::render::{csv_str, json_str};

pub struct Node {
    name: String,
    file: Option<String>,
    children: Vec<Node>,
    amount: i64,
}

pub struct Chart {
    title: Option<String>,
    before: String,
    after: String,
    nodes: Vec<Node>,
    total: i64,
}

pub fn validate_date(date: &str) -> Result<(), String> {
    let bad = || format!("invalid date `{date}`: expected YYYY-MM-DD");
    let b = date.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return Err(bad());
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: u32 = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(bad()),
    };
    if year == 0 || day == 0 || day > days {
        return Err(bad());
    }
    Ok(())
}

/// Same closing balance as the existing command; snapshots require valid, ordered dates.
pub fn account_balance(file: &str, as_of: Option<&str>) -> Result<i64, String> {
    let doc = load(file)?;
    if let Some(date) = as_of {
        let mut previous = "";
        for row in &doc.rows {
            validate_date(&row.date).map_err(|e| format!("{file}: {e}"))?;
            if row.date.as_str() < previous {
                return Err(format!("{file}: dates are not in order"));
            }
            previous = &row.date;
        }
        Ok(doc
            .rows
            .iter()
            .rev()
            .find(|e| e.date.as_str() <= date)
            .map_or(0, |e| e.balance))
    } else {
        Ok(balance(&doc.rows))
    }
}

fn sum(mut values: impl Iterator<Item = i64>) -> Result<i64, String> {
    values.try_fold(0i64, |a, b| {
        a.checked_add(b)
            .ok_or_else(|| "chart balance out of range".into())
    })
}

struct Item {
    depth: usize,
    name: String,
    file: Option<String>,
    line: usize,
}

fn nodes(
    items: &[Item],
    index: &mut usize,
    depth: usize,
    base: &Path,
    seen: &mut HashSet<std::path::PathBuf>,
    as_of: Option<&str>,
) -> Result<Vec<Node>, String> {
    let mut result = Vec::new();
    while *index < items.len() && items[*index].depth == depth {
        let item = &items[*index];
        *index += 1;
        let children = if *index < items.len() && items[*index].depth > depth {
            if item.file.is_some() || items[*index].depth != depth + 1 {
                return Err(format!(
                    "line {}: children require a group and one additional indentation level",
                    items[*index].line
                ));
            }
            nodes(items, index, depth + 1, base, seen, as_of)?
        } else {
            Vec::new()
        };
        let amount = if let Some(file) = &item.file {
            let path = resolve(&base.join(file).to_string_lossy());
            let canonical = fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
            if !seen.insert(canonical) {
                return Err(format!("line {}: duplicate account `{file}`", item.line));
            }
            account_balance(&path, as_of)?
        } else {
            sum(children.iter().map(|n| n.amount))?
        };
        result.push(Node {
            name: item.name.clone(),
            file: item.file.clone(),
            children,
            amount,
        });
    }
    Ok(result)
}

pub fn load_chart(file: &str, as_of: Option<&str>) -> Result<Chart, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let parse = || -> Result<Chart, String> {
        let lines: Vec<_> = text.lines().collect();
        // The first unfenced dash list is the hierarchy; the surrounding Markdown
        // is report text, including any optional currency description.
        let mut fence: Option<(char, usize)> = None;
        let start = lines
            .iter()
            .position(|line| {
                let trimmed = line.trim_start();
                let marker = trimmed.chars().next().unwrap_or(' ');
                let count = trimmed.chars().take_while(|c| *c == marker).count();
                if let Some((open, length)) = fence {
                    if marker == open && count >= length && trimmed[count..].trim().is_empty() {
                        fence = None;
                    }
                    return false;
                }
                if (marker == '`' || marker == '~') && count >= 3 {
                    fence = Some((marker, count));
                    return false;
                }
                trimmed.starts_with("- ")
            })
            .ok_or("chart needs a list starting at the top level")?;
        let end = start
            + lines[start..]
                .iter()
                .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("- "))
                .count();
        let before = lines[..start].join("\n");
        let after = lines[end..].join("\n");
        let title = lines[..start]
            .iter()
            .find_map(|line| line.strip_prefix("# "))
            .map(|title| title.trim().to_string());
        let mut items = Vec::new();
        for (i, line) in lines.iter().enumerate().take(end).skip(start) {
            let line_no = i + 1;
            if line.trim().is_empty() {
                continue;
            }
            let spaces = line.bytes().take_while(|c| *c == b' ').count();
            let body = line[spaces..]
                .strip_prefix("- ")
                .ok_or_else(|| format!("line {line_no}: expected a '- ' list item"))?
                .trim();
            if spaces % 2 != 0 || body.is_empty() || body.chars().any(char::is_control) {
                return Err(format!(
                    "line {line_no}: use two spaces per level and a nonempty label"
                ));
            }
            let (name, account) = if body.starts_with('[') {
                let (name, target) = body[1..]
                    .split_once("](")
                    .ok_or_else(|| format!("line {line_no}: expected [name](path)"))?;
                let target = target
                    .strip_suffix(')')
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("line {line_no}: expected [name](path)"))?;
                if name.is_empty() {
                    return Err(format!("line {line_no}: empty account label"));
                }
                (name.to_string(), Some(target.to_string()))
            } else {
                (body.to_string(), None)
            };
            items.push(Item {
                depth: spaces / 2,
                name,
                file: account,
                line: line_no,
            });
        }
        if items.is_empty() || items[0].depth != 0 {
            return Err("chart needs a list starting at the top level".into());
        }
        let mut index = 0;
        let nodes = nodes(
            &items,
            &mut index,
            0,
            Path::new(file).parent().unwrap_or(Path::new(".")),
            &mut HashSet::new(),
            as_of,
        )?;
        let total = sum(nodes.iter().map(|n| n.amount))?;
        Ok(Chart {
            title,
            before,
            after,
            nodes,
            total,
        })
    };
    parse().map_err(|e| format!("{file}: {e}"))
}

fn flatten<'a>(
    nodes: &'a [Node],
    depth: usize,
    parent: &[String],
    rows: &mut Vec<(&'a Node, usize, Vec<String>)>,
) {
    for node in nodes {
        let mut path = parent.to_vec();
        path.push(node.name.clone());
        rows.push((node, depth, path.clone()));
        flatten(&node.children, depth + 1, &path, rows);
    }
}

fn json_node(node: &Node) -> String {
    let source = match &node.file {
        Some(file) => format!("\"account\": {}", json_str(file)),
        None => format!(
            "\"children\": [{}]",
            node.children
                .iter()
                .map(json_node)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!(
        "{{\"name\": {}, \"balance\": {}, {source}}}",
        json_str(&node.name),
        fmt_amount(node.amount)
    )
}

pub fn render(
    chart: &Chart,
    as_of: Option<&str>,
    format: &str,
    only_total: bool,
    styled: bool,
) -> String {
    let total = fmt_amount(chart.total);
    let mut rows = Vec::new();
    if !only_total {
        flatten(&chart.nodes, 0, &[], &mut rows);
    }
    if format == "json" {
        let nodes = if only_total {
            String::new()
        } else {
            chart
                .nodes
                .iter()
                .map(json_node)
                .collect::<Vec<_>>()
                .join(", ")
        };
        return format!("{{\"title\": {}, \"before\": {}, \"after\": {}, \"as_of\": {}, \"nodes\": [{nodes}], \"total\": {total}}}\n",
            chart.title.as_deref().map(json_str).unwrap_or_else(|| "null".into()),
            json_str(&chart.before), json_str(&chart.after), as_of.map(json_str).unwrap_or_else(|| "null".into()));
    }
    if format == "csv" {
        let mut out = String::from("Path,Type,Balance,AsOf\n");
        for (node, _, path) in &rows {
            out += &format!(
                "{},{},{},{}\n",
                csv_str(&path.join(" / ")),
                if node.file.is_some() {
                    "account"
                } else {
                    "group"
                },
                fmt_amount(node.amount),
                as_of.unwrap_or("")
            );
        }
        return out + &format!("Total,total,{total},{}\n", as_of.unwrap_or(""));
    }
    if format == "quiet" {
        return rows
            .iter()
            .map(|(n, _, _)| fmt_amount(n.amount) + "\n")
            .collect::<String>()
            + &total
            + "\n";
    }
    let aw = rows
        .iter()
        .map(|(n, d, _)| n.name.chars().count() + 2 * d)
        .chain([7])
        .max()
        .unwrap();
    let bw = rows
        .iter()
        .map(|(n, d, _)| fmt_amount(n.amount).len() + 2 * d)
        .chain([total.len(), 7])
        .max()
        .unwrap();
    let mut out = document_text(&chart.before, format, styled);
    if !out.is_empty() {
        out += "\n\n";
    }
    if let Some(date) = as_of {
        out += &format!("As of {date}\n\n");
    }
    // A fenced block preserves both indentation directions in Markdown renderers.
    if format == "markdown" {
        out += "```text\n";
    }
    let line = |name: &str, amount: &str, depth: usize, bold: bool| {
        let width = bw - 2 * depth;
        let text = format!("{name:<aw$}  {amount:>width$}\n");
        if styled && format == "pretty" && bold {
            format!("\x1b[1m{}\x1b[0m\n", text.trim_end())
        } else {
            text
        }
    };
    out += &line("Account", "Balance", 0, true);
    for (node, depth, _) in rows {
        out += &line(
            &format!("{}{}", "  ".repeat(depth), node.name),
            &fmt_amount(node.amount),
            depth,
            node.file.is_none(),
        );
    }
    out += "\n";
    out += &line("Total", &total, 0, true);
    if format == "markdown" {
        out += "```\n";
    }
    let after = document_text(&chart.after, format, styled);
    if !after.is_empty() {
        out += &format!("\n{after}\n");
    }
    out
}

/// Keep document prose and Markdown intact; simplify headings for terminal output.
fn document_text(text: &str, format: &str, styled: bool) -> String {
    if format == "markdown" {
        return text.trim_end().to_string();
    }
    text.trim_end()
        .lines()
        .map(|line| {
            let heading = line.bytes().take_while(|c| *c == b'#').count();
            if (1..=6).contains(&heading) && line.as_bytes().get(heading) == Some(&b' ') {
                let title = &line[heading + 1..];
                if styled {
                    format!("\x1b[1m{title}\x1b[0m")
                } else {
                    title.to_string()
                }
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
