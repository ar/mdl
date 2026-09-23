//! The statement typeset with Typst: the table, the chart, the PDF.
use std::io::Write;
use std::path::Path;
use std::process;

use crate::ledger::{Doc, Entry, fmt_amount, month_add, period_label};
use crate::render::{grid, totals};

/// A Typst string literal.
fn typst_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Typst source for the printed statement: the title as a heading, the prose around
/// the table as plain paragraphs (Markdown markup shows literally), and the table with
/// the totals row, then the chart of `series` (`balance`, `debit`, `credit`; none for no
/// chart). The file's name goes in the
/// page footer, the period (or today) in the header.
fn render_typst(doc: &Doc, file: &str, p: &Option<(String, String)>, series: &[String]) -> String {
    let name = Path::new(file).file_name().map_or(file.to_string(), |n| n.to_string_lossy().into_owned());
    let title = doc.title().map_or(name.as_str(), |t| t[2..].trim());
    let when = match p {
        Some((a, b)) => typst_str(&period_label(a, b)),
        None => "datetime.today().display(\"[year]-[month]-[day]\")".to_string(),
    };
    let mut out = format!(
        r#"#set page(paper: "a4", margin: (x: 2.2cm, y: 2.4cm),
  header: [
    #set text(9pt, fill: luma(40%))
    #{title} #h(1fr) #{when}
    #v(-6pt) #line(length: 100%, stroke: 0.5pt + luma(60%))
  ],
  footer: context [
    #set text(9pt, fill: luma(40%))
    #line(length: 100%, stroke: 0.5pt + luma(60%)) #v(-6pt)
    #{file} #h(1fr) #counter(page).display("1 / 1", both: true)
  ])
#set text(font: ("Helvetica Neue", "Libertinus Serif"), 10pt, number-type: "lining")
#show heading: it => [#set text(16pt, weight: "bold"); #it #v(2pt)]

= #{title}

"#,
        title = typst_str(title),
        file = typst_str(&name),
        when = when,
    );
    // prose: every line outside the table but the title, paragraphs split on blank lines
    let prose = |lines: &[String]| -> String {
        let mut paras = vec![];
        let mut cur = vec![];
        for l in lines.iter().chain([&String::new()]) {
            let l = l.trim();
            if l.is_empty() {
                if !cur.is_empty() {
                    paras.push(format!("#{}\n\n", typst_str(&cur.join(" "))));
                    cur.clear();
                }
            } else if !l.starts_with("# ") {
                cur.push(l);
            }
        }
        paras.concat()
    };
    out += &prose(&doc.lines[..doc.start]);
    let cells = |r: &[String], bold: bool| -> String {
        let c: Vec<String> = r.iter().map(|c| if bold { format!("strong({})", typst_str(c)) } else { typst_str(c) }).collect();
        format!("  {},\n", c.join(", "))
    };
    out += r#"#v(6pt)
#table(
  columns: (auto, 1fr, auto, auto, auto),
  align: (left, left, right, right, right),
  stroke: none,
  inset: (x: 8pt, y: 5pt),
  fill: (_, y) => if y > 0 and calc.even(y) { luma(96%) } else { none },
  table.hline(stroke: 0.8pt),
  table.header(
"#;
    out += &cells(&doc.header, true);
    out += "  ),\n  table.hline(stroke: 0.5pt),\n";
    for (e, r) in doc.rows.iter().zip(grid(&doc.rows, false)) {
        out += &cells(&r, e.bold);
    }
    out += "  table.hline(stroke: 0.5pt),\n";
    out += &cells(&totals(&doc.rows), true);
    out += "  table.hline(stroke: 0.8pt),\n)\n\n#v(6pt)\n";
    if !series.is_empty() && !doc.rows.is_empty() {
        out += &render_chart(doc, p, series);
    }
    out += &prose(&doc.lines[doc.end..]);
    out
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (the inverse of `today`'s arithmetic).
fn day_number(date: &str) -> i64 {
    let n = |r: std::ops::Range<usize>| date.get(r).and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// The step between the y ticks of a chart spanning `range` cents: 1, 2 or 5 times a
/// power of ten, for four ticks or so.
fn tick_step(range: i64) -> i64 {
    let raw = range.max(1) as f64 / 4.0;
    let mag = 10f64.powi(raw.log10().floor() as i32);
    let m = raw / mag;
    (mag * if m <= 1.0 { 1.0 } else if m <= 2.0 { 2.0 } else if m <= 5.0 { 5.0 } else { 10.0 }).round() as i64
}

/// Typst for the chart: each of `series` (`balance`: the balance after every row;
/// `debit` | `credit`: the amount of every row that has one) stepping from row to row
/// across the period's months (or, without one, the months from the first row to the
/// last), the months ticked as densely as the page fits, the zero line drawn, the last
/// value of each series labelled in its colour. The geometry is computed here in day
/// offsets and cents; the Typst only scales and draws.
fn render_chart(doc: &Doc, p: &Option<(String, String)>, series: &[String]) -> String {
    /// One charted series: its header column, its colour, and the value of each row it plots.
    type Series<'a> = (usize, &'static str, Vec<(&'a Entry, i64)>);
    let (first, last) = (&doc.rows[0], &doc.rows[doc.rows.len() - 1]);
    // (header column, colour, the rows' values) per series, skipping empty ones
    let pick = |col: usize, color: &'static str, f: fn(&Entry) -> i64| -> Series {
        (col, color, doc.rows.iter().map(|e| (e, f(e))).filter(|(_, v)| col == 4 || *v != 0).collect())
    };
    let series: Vec<Series> = series
        .iter()
        .filter_map(|s| match s.as_str() {
            "balance" => Some(pick(4, "#2a6fdb", |e| e.balance)),
            "debit" => Some(pick(2, "#2a9d5c", |e| e.debit)),
            "credit" => Some(pick(3, "#d1495b", |e| e.credit)),
            _ => None,
        })
        .filter(|(_, _, v)| !v.is_empty())
        .collect();
    if series.is_empty() {
        return String::new();
    }
    let (from, to) = match p {
        Some((from, to)) => (from.clone(), to.clone()),
        None => (first.date[..7].to_string(), last.date[..7].to_string()),
    };
    let idx = |ym: &str| ym[..4].parse::<i64>().unwrap_or(0) * 12 + ym[5..7].parse::<i64>().unwrap_or(1) - 1; // months since year 0
    let n_months = idx(&to) - idx(&from) + 1;
    let (x0, x_end, month0) = (day_number(&format!("{from}-01")), day_number(&format!("{}-01", month_add(&to, 1))), from);
    let span = (x_end - x0).max(1);
    let (lo, hi) = series.iter().flat_map(|(_, _, v)| v).fold((0, 0), |(lo, hi), (_, v)| (lo.min(*v), hi.max(*v)));
    let step = tick_step(hi - lo);
    let (lo, hi) = (lo.div_euclid(step) * step, (-(-hi).div_euclid(step)) * step);
    let hi = if hi == lo { lo + step } else { hi };
    let label = |c: i64| {
        let s = fmt_amount(c);
        if step % 100 == 0 { s.trim_end_matches(".00").to_string() } else { s }
    };
    let yticks: Vec<String> = (lo..=hi).step_by(step as usize).map(|v| format!("({v}, {})", typst_str(&label(v)))).collect();
    // a tick every month, quarter or half year (labelled YYYY-MM), or every 1, 2, 5,
    // 10... years (labelled YYYY): the finest whose labels fit across the page
    let units = [1, 3, 6, 12, 24, 60, 120, 240, 600];
    let unit = units.iter().copied().find(|u| (n_months + u - 1) / u <= if *u < 12 { 10 } else { 18 }).unwrap_or(600);
    let mut months = vec![];
    let mut m = month0;
    loop {
        let d = day_number(&format!("{m}-01")) - x0;
        if d >= span {
            break;
        }
        let (y, mo) = (idx(&m) / 12, idx(&m) % 12); // mo: 0 for January
        let tick = if unit < 12 { mo % unit == 0 } else { mo == 0 && y % (unit / 12) == 0 };
        if d >= 0 && tick {
            months.push(format!("({d}, {})", typst_str(if unit < 12 { &m } else { &m[..4] })));
        }
        m = month_add(&m, 1);
    }
    let plots: Vec<String> = series
        .iter()
        .map(|(_, color, v)| {
            let pts: Vec<String> = v.iter().map(|(e, v)| format!("({}, {v})", day_number(&e.date) - x0)).collect();
            format!("(({},), {}, rgb({}))", pts.join(", "), typst_str(&fmt_amount(v[v.len() - 1].1)), typst_str(color))
        })
        .collect();
    let caption: Vec<String> = series.iter().map(|(c, color, _)| format!("text(fill: rgb({}), {})", typst_str(color), typst_str(&doc.header[*c]))).collect();
    format!(
        r##"#let ledger-chart(span, lo, hi, yticks, months, series, h: 5.5cm) = layout(size => {{
  let w = size.width
  let lw = 1.6cm
  let x(d) = lw + (w - lw) * d / span
  let y(v) = h - h * (v - lo) / (hi - lo)
  let small = text.with(7.5pt, fill: luma(40%))
  box(width: w, height: h + 1cm, {{
    for (v, label) in yticks {{
      place(line(start: (lw, y(v)), end: (w, y(v)), stroke: if v == 0 {{ 0.6pt + luma(45%) }} else {{ 0.4pt + luma(85%) }}))
      place(dx: 0pt, dy: y(v) - 5pt, box(width: lw - 6pt, align(right, small(label))))
    }}
    for (d, label) in months {{
      place(line(start: (x(d), 0pt), end: (x(d), h), stroke: 0.4pt + luma(85%)))
      if x(d) + 36pt <= w {{ place(dx: x(d) + 3pt, dy: h + 4pt, small(label)) }}
    }}
    for (pts, close, color) in series {{
      let (d0, v0) = pts.first()
      let prev = v0
      let segs = ()
      for (d, v) in pts.slice(1) {{
        segs.push(curve.line((x(d), y(prev))))
        segs.push(curve.line((x(d), y(v))))
        prev = v
      }}
      segs.push(curve.line((x(span), y(prev))))
      place(curve(fill: color.transparentize(88%), stroke: none,
        curve.move((x(d0), y(0))), curve.line((x(d0), y(v0))), ..segs, curve.line((x(span), y(0))), curve.close()))
      place(curve(stroke: 1.3pt + color, curve.move((x(d0), y(v0))), ..segs))
      place(dx: x(span) - 2pt, dy: y(prev) - 2pt, circle(radius: 2pt, fill: color))
      // the label above the dot, or below it when the last step came down onto it
      let fell = pts.len() > 1 and pts.at(-2).at(1) > prev
      let below = (fell or y(prev) < 20pt) and y(prev) < h - 16pt
      let dy = if below {{ y(prev) + 5pt }} else {{ y(prev) - 15pt }}
      place(dx: x(span) - 80pt, dy: dy, box(width: 80pt, align(right,
        box(fill: white.transparentize(15%), inset: (x: 2pt, y: 1pt), radius: 2pt, text(8pt, weight: "bold", fill: color, close)))))
    }}
  }})
}})
#v(6pt)
#block(breakable: false, [
  #text(9pt, fill: luma(40%), {caption})
  #v(4pt)
  #ledger-chart({span}, {lo}, {hi}, ({yticks},), ({months},), ({plots},))
])

#v(6pt)
"##,
        caption = caption.join(" + \" / \" + "),
        yticks = yticks.join(", "),
        months = months.join(", "),
        plots = plots.join(", "),
    )
}

/// `print`: the statement typeset by `typst` (the source on its stdin) as `<file>.pdf`,
/// or `<file>-<from>[-<to>].pdf` for a period; with `series`, those charted after the
/// table. `out` overrides the PDF's path.
pub fn print_pdf(doc: &Doc, file: &str, p: &Option<(String, String)>, series: &[String], out: Option<&str>) -> Result<String, String> {
    let suffix = match p {
        Some((a, b)) if a == b => format!("-{a}"),
        Some((a, b)) => format!("-{a}-{b}"),
        None => String::new(),
    };
    let stem = Path::new(file).with_extension("");
    let pdf = match out {
        Some(o) => Path::new(o).to_path_buf(),
        None => Path::new(&format!("{}{suffix}", stem.display())).with_extension("pdf"),
    };
    let mut child = process::Command::new("typst")
        .args(["compile", "-", &pdf.to_string_lossy()])
        .stdin(process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("typst: {e} (install it: brew install typst)"))?;
    child.stdin.take().unwrap().write_all(render_typst(doc, file, p, series).as_bytes()).map_err(|e| format!("typst: {e}"))?;
    let st = child.wait().map_err(|e| format!("typst: {e}"))?;
    if !st.success() {
        return Err("typst failed".into());
    }
    Ok(format!("wrote {}", pdf.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{load, scoped};

    #[test]
    fn print_typesets_the_statement() {
        let mut doc = load("cash.md").unwrap();
        doc.rows[1].desc = "Sale \"over\" the counter \\ rest".into();
        doc.rows[1].bold = true;
        let t = render_typst(&doc, "cash.md", &None, &[]);
        assert!(t.contains("#h(1fr) #datetime.today()"));
        let p = Some(("2026-08".to_string(), "2026-09".to_string()));
        assert!(render_typst(&doc, "cash.md", &p, &[]).contains("#h(1fr) #\"2026-08 – 2026-09\"\n"));
        assert!(t.contains("= #\"Cash\"\n"));
        assert!(t.contains("#\"Ledger of the Cash account. Balance = Σ debit − Σ credit.\"\n\n"));
        assert!(t.contains("#\"Free notes below the table are left untouched.\"\n\n"));
        assert!(t.contains("strong(\"Sale \\\"over\\\" the counter \\\\ rest\")"));
        assert!(t.contains("  \"2026-09-05\", \"Supplier payment\", \"\", \"81.00\", \"277.00\",\n"));
        assert!(t.contains("strong(\"\"), strong(\"\"), strong(\"1510.50\"), strong(\"1178.85\"), strong(\"\"),\n"));
        assert!(!t.contains("balance-chart"));
        typst_compiles(&t);
    }

    /// With typst installed, the source compiles to a PDF.
    fn typst_compiles(t: &str) {
        if let Ok(mut c) = process::Command::new("typst").args(["compile", "-", "-"]).stdin(process::Stdio::piped()).stdout(process::Stdio::piped()).spawn() {
            c.stdin.take().unwrap().write_all(t.as_bytes()).unwrap();
            let out = c.wait_with_output().unwrap();
            assert!(out.status.success());
            assert!(out.stdout.starts_with(b"%PDF"));
        }
    }

    #[test]
    fn graph_charts_the_balance() {
        assert_eq!(day_number("1970-01-01"), 0);
        assert_eq!(day_number("2000-03-01"), 11017);
        assert_eq!(day_number("2026-09-18") - day_number("2026-09-01"), 17);
        assert_eq!(day_number("2026-03-01") - day_number("2026-02-01"), 28);
        assert_eq!((tick_step(15000), tick_step(150), tick_step(1), tick_step(99999)), (5000, 50, 1, 50000));

        // the whole ledger: day offsets from the first row, the y ticks in whole units
        let doc = load("cash.md").unwrap();
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains("#block(breakable: false, [\n  #text(9pt, fill: luma(40%), text(fill: rgb(\"#2a6fdb\"), \"Balance\"))\n"));
        assert!(t.contains("#ledger-chart(30, 0, 150000, ((0, \"0\"), (50000, \"500\"), (100000, \"1000\"), (150000, \"1500\"),), ((0, \"2026-09\"),), ((((0, 0), (1, 15000), (2, 13800), (3, 35800), (4, 27700), (7, 30250), (8, 26760), (9, 36260), (10, 34460), (11, 29960), (14, 79960), (15, 73730), (16, 91730), (17, 90755), (18, 90755), (21, 111755), (22, 104255), (23, 103605), (24, 116605), (25, 36605), (28, 34365), (29, 33165),), \"331.65\", rgb(\"#2a6fdb\")),))\n"));
        assert!(t.find("ledger-chart").unwrap() > t.find("table.hline(stroke: 0.8pt),\n)").unwrap());
        assert!(t.find("#ledger-chart").unwrap() < t.find("Free notes").unwrap());
        typst_compiles(&t);
        // a period: the axis spans its months, the opening row at day 0
        let p = Some(("2026-08".to_string(), "2026-09".to_string()));
        let t = render_typst(&scoped(load("cash.md").unwrap(), &p), "cash.md", &p, &["balance".to_string()]);
        assert!(t.contains("#ledger-chart(61, 0, 150000, ((0, \"0\"), (50000, \"500\"), (100000, \"1000\"), (150000, \"1500\"),), ((0, \"2026-08\"), (31, \"2026-09\"),), ((((0, 0), (31, 0), (32, 15000), (33, 13800),"));
        typst_compiles(&t);
        // negative balances put the zero line inside; small ones keep their cents
        let mut doc = load("cash.md").unwrap();
        for e in &mut doc.rows {
            e.balance = -e.balance / 3;
        }
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains("#ledger-chart(30, -40000, 0, ((-40000, \"-400\"), (-30000, \"-300\"), (-20000, \"-200\"), (-10000, \"-100\"), (0, \"0\"),),"));
        for e in &mut doc.rows {
            e.balance = -e.balance / 100;
        }
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains("#ledger-chart(30, 0, 400, ((0, \"0\"), (100, \"1\"), (200, \"2\"), (300, \"3\"), (400, \"4\"),),"));
        assert!(t.contains("(29, 110),), \"1.10\", rgb(\"#2a6fdb\")),))"));
        typst_compiles(&t);
        // long spans: quarters, then years, as many as the page fits
        let mut doc = load("cash.md").unwrap();
        doc.rows[0].date = "2025-01-05".into();
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains(", ((0, \"2025-01\"), (90, \"2025-04\"), (181, \"2025-07\"), (273, \"2025-10\"), (365, \"2026-01\"), (455, \"2026-04\"), (546, \"2026-07\"),), ((((4, 0),"));
        doc.rows[0].date = "2010-01-05".into();
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains(", ((0, \"2010\"), (365, \"2011\"), (730, \"2012\"), (1096, \"2013\"),") && t.contains("(5844, \"2026\"),), ((((4, 0),"));
        doc.rows[0].date = "1990-01-05".into();
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains(", ((0, \"1990\"), (1826, \"1995\"),") && !t.contains("\"1991\""));
        typst_compiles(&t);
        // one row: its month, the y range at least a step
        doc.rows.truncate(1);
        doc.rows[0].date = "2026-09-01".into();
        let t = render_typst(&doc, "cash.md", &None, &["balance".to_string()]);
        assert!(t.contains("#ledger-chart(30, 0, 1, ((0, \"0.00\"), (1, \"0.01\"),), ((0, \"2026-09\"),), ((((0, 0),), \"0.00\", rgb(\"#2a6fdb\")),))"));
        typst_compiles(&t);
        // debit and credit: each entry's amount, rows without one skipped, both in one
        // chart under a two-colour caption; an unknown or empty series draws nothing
        let doc = load("cash.md").unwrap();
        let s = |w: &[&str]| -> Vec<String> { w.iter().map(|x| x.to_string()).collect() };
        let t = render_typst(&doc, "cash.md", &None, &s(&["debit", "credit"]));
        assert!(t.contains("#block(breakable: false, [\n  #text(9pt, fill: luma(40%), text(fill: rgb(\"#2a9d5c\"), \"Debit\") + \" / \" + text(fill: rgb(\"#d1495b\"), \"Credit\"))\n"));
        let debits = "(((1, 15000), (3, 22000), (7, 2550), (9, 9500), (14, 50000), (16, 18000), (21, 21000), (24, 13000),), \"130.00\", rgb(\"#2a9d5c\"))";
        let credits = "(((2, 1200), (4, 8100), (8, 3490), (10, 1800), (11, 4500), (15, 6230), (17, 975), (22, 7500), (23, 650), (25, 80000), (28, 2240), (29, 1200),), \"12.00\", rgb(\"#d1495b\"))";
        assert!(t.contains(&format!("#ledger-chart(30, 0, 80000, ((0, \"0\"), (20000, \"200\"), (40000, \"400\"), (60000, \"600\"), (80000, \"800\"),), ((0, \"2026-09\"),), ({debits}, {credits},))\n")));
        typst_compiles(&t);
        let t = render_typst(&doc, "cash.md", &None, &s(&["credit"]));
        assert!(t.contains("text(fill: rgb(\"#d1495b\"), \"Credit\"))\n"));
        assert!(t.contains(&format!("#ledger-chart(30, 0, 80000, ((0, \"0\"), (20000, \"200\"), (40000, \"400\"), (60000, \"600\"), (80000, \"800\"),), ((0, \"2026-09\"),), ({credits},))\n")));
        let mut doc = load("cash.md").unwrap();
        doc.rows.truncate(1);
        assert!(!render_typst(&doc, "cash.md", &None, &s(&["debit", "bogus"])).contains("ledger-chart"));
        assert!(!render_typst(&doc, "cash.md", &None, &[]).contains("ledger-chart"));
    }
}
