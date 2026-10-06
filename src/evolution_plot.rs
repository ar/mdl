//! A line graph of sampled chart balances, using the same PNG/terminal path as statements.
use crate::account_chart::Evolution;
use crate::ledger::fmt_amount;
use crate::print::{day_number, png_from_typst, typst_str};

pub fn png(data: &Evolution) -> Result<Vec<u8>, String> {
    png_from_typst(&source(data))
}

fn source(data: &Evolution) -> String {
    let colors = [
        "#2b846d", "#4774a5", "#9971ae", "#be873f", "#478f9b", "#9e657a", "#818748", "#657583",
    ];
    let first = day_number(&data.dates[0]);
    let span = (day_number(data.dates.last().unwrap()) - first).max(1);
    let (min, max) = data
        .series
        .iter()
        .flat_map(|s| &s.values)
        .fold((0f64, 0f64), |(lo, hi), v| {
            (lo.min(*v as f64), hi.max(*v as f64))
        });
    let raw = (max - min).max(4.0) / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = mag
        * [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .find(|s| *s * mag >= raw)
            .unwrap();
    let lo = (min / step).floor() * step;
    let hi = ((max / step).ceil() * step).max(lo + step);
    let yticks = (0..=((hi - lo) / step).round() as usize)
        .map(|i| {
            let value = lo + i as f64 * step;
            let label = if step >= 100.0 {
                format!("{:.0}", value / 100.0)
            } else {
                format!("{:.2}", value / 100.0)
            };
            format!("({value}, {})", typst_str(&label))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ticks = data.dates.len().min(8);
    let xticks = (0..ticks)
        .map(|i| {
            let index = i * (data.dates.len() - 1) / ticks.saturating_sub(1).max(1);
            let date = &data.dates[index];
            format!(
                "({}, {})",
                day_number(date) - first,
                typst_str(if data.monthly { &date[..7] } else { date })
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut plots = Vec::new();
    let mut legend = Vec::new();
    for (i, series) in data.series.iter().enumerate() {
        let color = if i == 0 {
            "#c5373a"
        } else {
            colors[(i - 1) % colors.len()]
        };
        let width = if i == 0 {
            "2pt"
        } else if series.group {
            "1.2pt"
        } else {
            "0.8pt"
        };
        let dash = if i > 0 && series.group {
            "\"dashed\""
        } else {
            "\"solid\""
        };
        let points = data
            .dates
            .iter()
            .zip(&series.values)
            .map(|(date, value)| format!("({}, {value})", day_number(date) - first))
            .collect::<Vec<_>>()
            .join(", ");
        plots.push(format!(
            "(rgb({}), {width}, {dash}, ({points},))",
            typst_str(color)
        ));
        legend.push(format!("[#line(length: 14pt, stroke: (paint: rgb({}), thickness: {width}, dash: {dash}))], [#text(fill: rgb({}), {})], [#align(right, {})]",
            typst_str(color), typst_str(color), typst_str(&series.name), typst_str(&fmt_amount(*series.values.last().unwrap()))));
    }
    // Draw the total last so it stays visible where it coincides with a group.
    plots.rotate_left(1);
    let total = &data.series[0].values;
    let change = (*total.last().unwrap() as i128) - total[0] as i128;
    let change = format!(
        "{}{:.2}",
        if change >= 0 { "+" } else { "" },
        change as f64 / 100.0
    );
    format!(
        r##"
#set page(width: 24cm, height: auto, margin: 0.6cm, fill: rgb("#fafaf7"))
#set text(font: ("Helvetica Neue", "Libertinus Serif"), size: 10pt, fill: rgb("#2a313b"))
#text(size: 16pt, weight: "bold", {title})
#h(1fr)
#text(size: 9pt, {period})
#v(8pt)
#grid(columns: (1fr, 1fr),
  [#text(size: 25pt, fill: rgb("#c5373a"), {total})\ #text(size: 9pt, "Latest total")],
  [#text(size: 25pt, fill: rgb("#687078"), {change})\ #text(size: 9pt, "Change between first and last sample")])
#v(12pt)
#layout(size => {{
  let lw = 1.8cm
  let rw = 1.1cm
  let h = 6.5cm
  let w = size.width
  let x(d) = lw + (w - lw - rw) * d / {span}
  let y(v) = h - h * (v - {lo}) / ({hi} - {lo})
  let small = text.with(size: 8pt, fill: rgb("#687078"))
  box(width: w, height: h + 0.8cm, {{
    for (v, label) in ({yticks},) {{
      place(line(start: (lw, y(v)), end: (w - rw, y(v)), stroke: if v == 0 {{ 0.7pt + rgb("#a0a5aa") }} else {{ 0.4pt + rgb("#dcdfd9") }}))
      place(dy: y(v) - 5pt, box(width: lw - 6pt, align(right, small(label))))
    }}
    for (d, label) in ({xticks},) {{
      place(line(start: (x(d), 0pt), end: (x(d), h), stroke: 0.4pt + rgb("#e5e7e1")))
      place(dx: x(d) - 32pt, dy: h + 7pt, box(width: 64pt, align(center, small(label))))
    }}
    for (color, thickness, dash, points) in ({plots},) {{
      let (d0, v0) = points.first()
      let segments = points.slice(1).map(p => curve.line((x(p.at(0)), y(p.at(1)))))
      if segments.len() > 0 {{
        place(curve(stroke: (paint: color, thickness: thickness, dash: dash), curve.move((x(d0), y(v0))), ..segments))
      }}
      for (d, v) in points {{
        place(dx: x(d) - 1.7pt, dy: y(v) - 1.7pt, circle(radius: 1.7pt, fill: color))
      }}
    }}
  }})
}})
#v(8pt)
#text(size: 9pt, {sampling})
#v(5pt)
#table(columns: (20pt, 1fr, auto), stroke: none, inset: (x: 2pt, y: 3pt),
  {legend})
"##,
        title = typst_str(&data.title),
        period = typst_str(&format!(
            "{} – {}",
            data.dates[0],
            data.dates.last().unwrap()
        )),
        total = typst_str(&fmt_amount(*total.last().unwrap())),
        change = typst_str(&change),
        sampling = typst_str(if data.monthly {
            "Monthly balances (month-end or selected end date) · legend shows latest values"
        } else {
            "Daily closing balances · legend shows latest values"
        }),
        plots = plots.join(", "),
        legend = legend.join(",\n"),
    )
}
