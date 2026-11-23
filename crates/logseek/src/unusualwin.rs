//! "Unusual rows": every row scored by how rare its values are in their own
//! columns, the file being its own measure. The window shows the spread of
//! the rows with a line to move and shows the rows at or above it; beside
//! the row numbers a "Why" band says, for each row on screen, which of its
//! values make it unusual, with exact counts.

use crate::bridge::{Bridge, Cmd, Snapshot, UnusualOut};
use crate::moment::group;
use crate::theme::Palette;
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::unusual::{Model, Reason, BUCKETS};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum UnusualAction {
    None,
    /// Show exactly these rows, with the reasons beside them.
    ShowRows(Arc<Vec<u64>>, Arc<Model>),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct UnusualWin {
    /// Score rows for their numbers together only.
    pub together: bool,
    line: Option<f64>,
    edit: f64,
    next_id: u64,
    run: Option<Run>,
    want: bool,
    /// Show the rows as soon as they are counted (dev switch).
    pub show_now: bool,
}

impl Default for UnusualWin {
    fn default() -> Self {
        UnusualWin {
            together: false,
            line: None,
            edit: 0.0,
            next_id: 0,
            run: None,
            want: true,
            show_now: false,
        }
    }
}

/// The numbers of a "together" reason with the column names put in
/// (`{k}` is the k-th of the model's columns, which are every column in order).
fn fill(text: &str, names: &[String]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(a) = rest.find('{') {
        out.push_str(&rest[..a]);
        let Some(b) = rest[a..].find('}') else {
            out.push_str(&rest[a..]);
            return out;
        };
        let k: Option<usize> = rest[a + 1..a + b].parse().ok();
        out.push_str(k.and_then(|k| names.get(k)).map_or("", String::as_str));
        rest = &rest[a + b + 1..];
    }
    out.push_str(rest);
    out
}

/// How long a value may run inside a reason before the rest is counted, not
/// shown: a reason is a line to read, and the whole value is in its own cell.
const VALUE_SHOWN: usize = 80;

/// A value as it is said inside a reason: on one line (a value written over
/// several lines has them joined), and when it is long, its beginning with
/// how many characters follow, as a cut cell says it.
pub fn value_words(v: &str) -> String {
    let one = crate::grid::cell_text(v);
    let total = one.chars().count();
    if total <= VALUE_SHOWN {
        return one;
    }
    let shown: String = one.chars().take(VALUE_SHOWN).collect();
    format!("{shown} +{} chars", group((total - VALUE_SHOWN) as u64))
}

/// One reason in words: "User is mallory: 1 of 20,000 rows",
/// "Bytes is 94500000: the largest in its column (1 of 20,000 rows at or above it)".
pub fn reason_words(r: &Reason, names: &[String]) -> String {
    if !r.together.is_empty() {
        return format!(
            "These numbers are unusual together: {}",
            fill(r.value.as_deref().unwrap_or(""), names)
        );
    }
    let col = names.get(r.col).map_or("", String::as_str);
    let what = match &r.value {
        Some(v) => format!("{col} is {}", value_words(v)),
        None => format!("{col} is empty"),
    };
    let rows = |n: u64| if n == 1 { "row" } else { "rows" };
    if r.largest {
        format!(
            "{what}: the largest in its column ({} of {} {} at or above it)",
            group(r.rows),
            group(r.of),
            rows(r.of)
        )
    } else if r.smallest {
        format!(
            "{what}: the smallest in its column ({} of {} {} at or below it)",
            group(r.rows),
            group(r.of),
            rows(r.of)
        )
    } else {
        format!(
            "{what}: {} of {} {}",
            group(r.rows),
            group(r.of),
            rows(r.of)
        )
    }
}

/// Reasons as they are said: columns empty in the same few rows (the same
/// count) are one reason, every column named; the strongest first. Each item
/// is (the words, the columns it names, what it adds).
fn said(why: &[Reason], names: &[String]) -> Vec<(String, Vec<String>, f64)> {
    let mut out: Vec<(String, Vec<String>, f64)> = Vec::new();
    let mut empties: Vec<(u64, u64, Vec<String>, f64)> = Vec::new(); // (rows, of, columns, bits)
    for r in why {
        if r.value.is_none() && !r.largest && !r.smallest && r.together.is_empty() {
            let name = names.get(r.col).cloned().unwrap_or_default();
            match empties.iter_mut().find(|e| e.0 == r.rows && e.1 == r.of) {
                Some(e) => {
                    e.2.push(name);
                    e.3 += r.bits;
                }
                None => empties.push((r.rows, r.of, vec![name], r.bits)),
            }
        } else {
            out.push((reason_words(r, names), Vec::new(), r.bits));
        }
    }
    for (rows, of, cols, bits) in empties {
        if cols.len() == 1 {
            out.push((
                format!(
                    "{} is empty: {} of {} rows",
                    cols[0],
                    group(rows),
                    group(of)
                ),
                Vec::new(),
                bits,
            ));
        } else {
            out.push((
                format!(
                    "{} columns empty that others fill ({} of {} rows)",
                    group(cols.len() as u64),
                    group(rows),
                    group(of)
                ),
                cols,
                bits,
            ));
        }
    }
    out.sort_by(|a, b| b.2.total_cmp(&a.2));
    out
}

/// How much of what a reason says is shown in the line beside a row: the
/// line sits in a column that stays in view, so it has to leave the rows room.
const LINE_SHOWN: usize = 36;

/// A reason made short for the line beside a row: what is so (its beginning,
/// with what follows counted), then how many rows share it. "User is mallory
/// · 1 row", "SHA256 is f36112d3ffa5f653a5d36015 +44 chars · 4 rows".
fn line_words(reason: &str) -> String {
    // several columns empty in the same few rows: how many columns, how many rows
    if let Some((cols, rest)) = reason.split_once(" columns empty that others fill (") {
        if let Some((n, _)) = rest.split_once(" of ") {
            return format!(
                "{cols} columns empty  ·  {n} {}",
                if n == "1" { "row" } else { "rows" }
            );
        }
    }
    let (what, count) = halves(reason);
    // only a reason that ends in a count of rows is taken apart; any other
    // (several numbers together, columns empty in the same rows) is said as it is
    let rows = count
        .split_once(" of ")
        .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == ','))
        .map(|(n, _)| format!("{n} {}", if n == "1" { "row" } else { "rows" }));
    let edge = ["the largest", "the smallest"]
        .into_iter()
        .find(|e| count.starts_with(e));
    let tail = match (rows, edge) {
        (Some(r), _) => r,
        (None, Some(e)) => e.to_string(),
        (None, None) => return reason.to_string(),
    };
    let total = what.chars().count();
    let what = if total > LINE_SHOWN {
        let shown: String = what.chars().take(LINE_SHOWN).collect();
        format!("{shown} +{} chars", group((total - LINE_SHOWN) as u64))
    } else {
        what.to_string()
    };
    format!("{what}  ·  {tail}")
}

/// The band's short line: the strongest reason, made short, and how many
/// more rare reasons there are (all of them are in Row view).
pub fn short(why: &[Reason], names: &[String]) -> String {
    let s = said(why, names);
    // what the row shares with many others is not a reason to count here
    let rare = parts(why, names).rare.len();
    let more = if rare > 0 { rare - 1 } else { s.len().saturating_sub(1) };
    // the line leads with a value the row has, when one is among its rare
    // reasons: that tells rows apart. Cells empty in the same rows are the
    // same words on every row of a kind, so they lead only when nothing else does.
    let names_a_value =
        |w: &str| !w.contains(" columns empty that others fill (") && !w.contains(" is empty: ");
    let lead = s
        .iter()
        .take(rare.max(1))
        .find(|x| names_a_value(&x.0))
        .or(s.first());
    match (lead, more) {
        (None, _) => String::new(),
        (Some(l), 0) => line_words(&l.0),
        (Some(l), n) => format!("{}  ·  +{} more", line_words(&l.0), group(n as u64)),
    }
}

/// The reasons a row stands out, in the three parts they are read in.
#[derive(Debug, Default, PartialEq)]
pub struct Parts {
    /// What is rare about the row (a value, a number at the edge of its
    /// column, an empty cell, in fewer than 1 row in 100), strongest first.
    pub rare: Vec<String>,
    /// Values the row shares with 1 row in 100 or more: they add little.
    pub common: Vec<String>,
    /// Columns empty here as they are in 1 row in 100 or more.
    pub empty: Vec<String>,
}

/// Every reason, said the way a person would: the same fact in several
/// columns is said once with each column named ("NetworkListenIP6 in name,
/// event.action: 8 of 89,361 rows"). What is rare comes first, strongest
/// first; what the row shares with many others (a value or an empty cell in
/// 1 row in 100 or more) is kept apart, since it says little about this row.
pub fn parts(why: &[Reason], names: &[String]) -> Parts {
    let col = |c: usize| names.get(c).map_or("", String::as_str).to_string();
    let rows_word = |n: u64| if n == 1 { "row" } else { "rows" };
    let many = |r: &Reason| r.of > 0 && r.rows * 100 >= r.of;
    // (words, order key): rare facts, grouped across columns
    let mut lines: Vec<(String, f64)> = Vec::new();
    let mut rare: Vec<(Option<String>, u64, u64, bool, bool, Vec<String>, f64)> = Vec::new();
    let mut common: Vec<(String, u64, f64)> = Vec::new();
    let mut empties: Vec<(u64, u64, Vec<String>, f64)> = Vec::new();
    let mut empty_common: Vec<(String, f64)> = Vec::new();
    for r in why {
        if !r.together.is_empty() {
            lines.push((reason_words(r, names), r.bits));
        } else if r.value.is_none() && !r.largest && !r.smallest {
            if many(r) {
                empty_common.push((col(r.col), r.bits));
                continue;
            }
            match empties.iter_mut().find(|e| e.0 == r.rows && e.1 == r.of) {
                Some(e) => {
                    e.2.push(col(r.col));
                    e.3 = e.3.max(r.bits);
                }
                None => empties.push((r.rows, r.of, vec![col(r.col)], r.bits)),
            }
        } else if !r.largest && !r.smallest && many(r) {
            let v = value_words(r.value.as_deref().unwrap_or_default());
            common.push((format!("{} is {v}", col(r.col)), r.rows, r.bits));
        } else {
            match rare.iter_mut().find(|x| {
                x.0 == r.value
                    && x.1 == r.rows
                    && x.2 == r.of
                    && x.3 == r.largest
                    && x.4 == r.smallest
            }) {
                Some(x) => {
                    x.5.push(col(r.col));
                    x.6 = x.6.max(r.bits);
                }
                None => rare.push((
                    r.value.clone(),
                    r.rows,
                    r.of,
                    r.largest,
                    r.smallest,
                    vec![col(r.col)],
                    r.bits,
                )),
            }
        }
    }
    for (v, rows, of, largest, smallest, cols, bits) in rare {
        let v = value_words(&v.unwrap_or_default());
        let what = if cols.len() == 1 {
            format!("{} is {v}", cols[0])
        } else {
            format!("{v} in {}", crate::keep::and_list(&cols))
        };
        let how = if largest {
            format!(
                "the largest ({} of {} {} at or above it)",
                group(rows),
                group(of),
                rows_word(of)
            )
        } else if smallest {
            format!(
                "the smallest ({} of {} {} at or below it)",
                group(rows),
                group(of),
                rows_word(of)
            )
        } else {
            format!("{} of {} {}", group(rows), group(of), rows_word(of))
        };
        lines.push((format!("{what}: {how}"), bits));
    }
    for (rows, of, cols, bits) in empties {
        let w = if cols.len() == 1 {
            format!(
                "{} is empty: {} of {} {}",
                cols[0],
                group(rows),
                group(of),
                rows_word(of)
            )
        } else {
            format!(
                "{} empty where others are filled: {} of {} {}",
                crate::keep::and_list(&cols),
                group(rows),
                group(of),
                rows_word(of)
            )
        };
        lines.push((w, bits));
    }
    lines.sort_by(|a, b| b.1.total_cmp(&a.1));
    common.sort_by(|a, b| b.2.total_cmp(&a.2));
    empty_common.sort_by(|a, b| b.1.total_cmp(&a.1));
    Parts {
        rare: lines.into_iter().map(|(w, _)| w).collect(),
        common: common
            .iter()
            .map(|(w, rows, _)| format!("{w} ({} {})", group(*rows), rows_word(*rows)))
            .collect(),
        empty: empty_common.into_iter().map(|(c, _)| c).collect(),
    }
}

/// Every reason as text, the rare ones a line each, then one closing line
/// for the values many rows share and one for the cells empty in many rows.
pub fn full(why: &[Reason], names: &[String]) -> String {
    let p = parts(why, names);
    let mut out = p.rare;
    if !p.common.is_empty() {
        out.push(format!(
            "Also less rare (each in 1% of rows or more): {}",
            p.common.join(", ")
        ));
    }
    if !p.empty.is_empty() {
        out.push(format!(
            "Empty here, as in 1% of rows or more: {}",
            p.empty.join(", ")
        ));
    }
    out.join("\n")
}

/// What is said of the rest once the rare reasons are listed: "9 values it
/// shares with many rows and 14 columns empty as in many rows".
pub fn rest_words(p: &Parts) -> Option<String> {
    let v = match p.common.len() {
        0 => None,
        1 => Some("1 value it shares with many rows".to_string()),
        n => Some(format!("{} values it shares with many rows", group(n as u64))),
    };
    let e = match p.empty.len() {
        0 => None,
        1 => Some("1 column empty as in many rows".to_string()),
        n => Some(format!("{} columns empty as in many rows", group(n as u64))),
    };
    match (v, e) {
        (Some(v), Some(e)) => Some(format!("{v} and {e}")),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// How many reasons are shown before the rest is asked for: the strongest
/// few say why a row stands out; the whole list is one click further.
pub const LEAD: usize = 3;

/// The reasons for a pointer resting on a row: the strongest few, a line
/// each, and in one last line how much more there is to read in Row view.
pub fn brief(why: &[Reason], names: &[String]) -> String {
    let p = parts(why, names);
    let mut out: Vec<String> = p.rare.iter().take(LEAD).cloned().collect();
    let more = p.rare.len().saturating_sub(LEAD);
    let rest = rest_words(&p);
    let tail = match (more, rest) {
        (0, None) => None,
        (0, Some(r)) => Some(format!("Also {r}")),
        (n, None) => Some(format!(
            "And {} more rare {}",
            group(n as u64),
            if n == 1 { "reason" } else { "reasons" }
        )),
        (n, Some(r)) => Some(format!(
            "And {} more rare {}, {r}",
            group(n as u64),
            if n == 1 { "reason" } else { "reasons" }
        )),
    };
    if let Some(t) = tail {
        out.push(format!("{t}: all in Row view"));
    }
    out.join("\n")
}

/// A reason as its two halves: what is so, and how many rows have it
/// ("User is mallory", "1 of 20,000 rows").
pub fn halves(line: &str) -> (&str, &str) {
    line.rsplit_once(": ").unwrap_or((line, ""))
}

impl UnusualWin {
    fn start(&mut self, bridge: &Bridge, now: f64) {
        if let Some(r) = &self.run {
            r.stop.store(true, Ordering::SeqCst);
        }
        self.next_id += 1;
        let run = Run {
            id: self.next_id,
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            started: now,
        };
        bridge.send(Cmd::Unusual {
            id: run.id,
            together: self.together,
            line: self.line,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.run = Some(run);
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, UnusualAction) {
        let mut open = true;
        let mut act = UnusualAction::None;
        let now = ctx.input(|i| i.time);
        if std::mem::take(&mut self.want) {
            self.start(bridge, now);
        }
        let out: Option<Arc<UnusualOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.unusual.clone().filter(|o| o.id == r.id));
        if let (Some(o), None) = (&out, self.line) {
            self.edit = o.line;
        }
        if let Some(o) = out.as_ref().filter(|o| o.error.is_none()) {
            if std::mem::take(&mut self.show_now) {
                act = UnusualAction::ShowRows(o.rows.clone(), o.model.clone());
            }
        }
        let screen = ctx.content_rect();
        let w = (screen.width() - 32.0).min(900.0);
        let mut rerun = false;
        egui::Window::new("unusual-rows")
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_width(w)
            .max_size([screen.width() - 16.0, screen.height() - 16.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Unusual rows").font(crate::fonts::strong(16.0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(ph::X).frame(false))
                            .on_hover_text("Close  (Esc)")
                            .clicked()
                        {
                            open = false;
                        }
                    });
                });
                ui.label(
                    RichText::new("Finds the rows that are least like the others: a value few rows share, a number far out in its column, an empty cell where others have one. Your file is the only measure.")
                        .color(pal.text2),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Unusual for");
                    let a = ui.radio_value(&mut self.together, false, "any reason");
                    let b = ui
                        .radio_value(&mut self.together, true, "their numbers together")
                        .on_hover_text("Numbers each ordinary on their own but rare together (a hot bearing at idle load): rows at the far end of what every row follows do not count");
                    if a.changed() || b.changed() {
                        self.line = None;
                        rerun = true;
                    }
                });
                ui.separator();
                let Some(run) = &self.run else { return };
                let Some(out) = out.clone() else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "{} rows read (three reads of {}) · {:.0} s",
                            group(run.done.load(Ordering::Relaxed)),
                            group(snap.matched),
                            now - run.started
                        ));
                        if ui.button("Stop").clicked() {
                            run.stop.store(true, Ordering::SeqCst);
                        }
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    return;
                };
                if let Some(e) = &out.error {
                    ui.label(
                        RichText::new(if e == "stopped" {
                            "Stopped before the end: nothing to show.".to_string()
                        } else {
                            format!("{}  The rows could not be scored: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                ui.add(
                    egui::Label::new(format!(
                        "Each row has a score for how rare its values are; a usual row here scores {:.1}. The chart shows every row, usual on the left, unusual on the right. Click it to move the line: the rows to its right are the ones shown.",
                        out.usual
                    ))
                    .wrap(),
                );
                // which columns counted, and which did not: there to open, out of the way
                let not_counted = out.left_out.len() + out.quiet.len();
                egui::CollapsingHeader::new(
                    RichText::new(format!(
                        "{} {} counted{}",
                        group(out.weighed as u64),
                        if out.weighed == 1 { "column" } else { "columns" },
                        if not_counted > 0 {
                            format!(", {} not", group(not_counted as u64))
                        } else {
                            String::new()
                        }
                    ))
                    .color(pal.text2),
                )
                .id_salt("unusual-columns")
                .show(ui, |ui| {
                    if not_counted == 0 {
                        ui.label(RichText::new("Every column was counted.").color(pal.text2));
                    }
                    if !out.left_out.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Not counted, their values nearly all different: {}",
                                    out.left_out.join(", ")
                                ))
                                .color(pal.text2),
                            )
                            .wrap(),
                        );
                    }
                    if !out.quiet.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Counting for nothing (every value different, or counting up row by row): {}",
                                    out.quiet.join(", ")
                                ))
                                .color(pal.text2),
                            )
                            .wrap(),
                        );
                    }
                });
                // the spread, the line marked
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 70.0),
                    egui::Sense::click(),
                );
                let last = out
                    .spread
                    .iter()
                    .rposition(|&n| n > 0)
                    .unwrap_or(0)
                    .max(((out.line / 0.5) as usize).min(BUCKETS - 1))
                    + 2;
                let max = out.spread.iter().copied().max().unwrap_or(1).max(1) as f32;
                let bw = rect.width() / last as f32;
                for (i, &n) in out.spread.iter().take(last).enumerate() {
                    if n == 0 {
                        continue;
                    }
                    let hgt = ((n as f32).ln_1p() / max.ln_1p() * (rect.height() - 4.0)).max(2.0);
                    let x = rect.left() + i as f32 * bw;
                    let above = i as f64 * 0.5 >= out.line;
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x, rect.bottom() - hgt),
                            egui::pos2(x + bw.max(1.0) - 0.5, rect.bottom()),
                        ),
                        0.0,
                        if above { pal.accent } else { pal.text2 },
                    );
                }
                let lx = rect.left() + (out.line / 0.5) as f32 * bw;
                ui.painter().line_segment(
                    [egui::pos2(lx, rect.top()), egui::pos2(lx, rect.bottom())],
                    egui::Stroke::new(1.5, pal.accent_ink),
                );
                if let Some(p) = resp.hover_pos() {
                    let b = (((p.x - rect.left()) / bw) as usize).min(BUCKETS - 1);
                    resp.clone().on_hover_text(format!(
                        "Score {:.1}: {} rows. Click to put the line here.",
                        b as f64 * 0.5,
                        group(out.spread[b])
                    ));
                    if resp.clicked() {
                        self.edit = b as f64 * 0.5;
                        rerun = true;
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Show rows that score");
                    let r = ui.add(
                        egui::DragValue::new(&mut self.edit)
                            .range(0.0..=4000.0)
                            .speed(0.1)
                            .fixed_decimals(1),
                    );
                    if r.drag_stopped() || r.lost_focus() {
                        rerun = (self.edit - out.line).abs() > 1e-9;
                    }
                    ui.label(
                        RichText::new(format!(
                            "or more: the {} most unusual of {} rows  ·  read in {:.1} s",
                            group(out.rows.len() as u64),
                            group(out.total),
                            out.secs
                        ))
                        .strong(),
                    );
                    if !out.rows.is_empty()
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{}  Show these rows", ph::ROWS))
                                        .color(pal.on_accent),
                                )
                                .fill(pal.accent),
                            )
                            .on_hover_text("These rows in the grid, each with the reasons it is unusual beside its number")
                            .clicked()
                    {
                        act = UnusualAction::ShowRows(out.rows.clone(), out.model.clone());
                    }
                });
            });
        if rerun {
            // a new line (or a new kind of score: the line is worked out again)
            if self.line.is_some()
                || (self.edit - out.as_ref().map_or(0.0, |o| o.line)).abs() > 1e-9
            {
                self.line = Some(self.edit);
            }
            self.start(bridge, now);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            open = false;
        }
        if !open {
            if let Some(r) = &self.run {
                r.stop.store(true, Ordering::SeqCst);
            }
        }
        (open, act)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(col: usize, v: Option<&str>, rows: u64, largest: bool, bits: f64) -> Reason {
        Reason {
            col,
            value: v.map(str::to_string),
            rows,
            of: 20_000,
            largest,
            smallest: false,
            bits,
            together: Vec::new(),
        }
    }

    #[test]
    fn numbers_unusual_together_are_said_with_their_values() {
        let names: Vec<String> = ["Unit", "Load", "Temp"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let r = Reason {
            col: 1,
            value: Some("{1} is 500, {2} is 85".into()),
            rows: 0,
            of: 20_000,
            largest: false,
            smallest: false,
            bits: 7.4,
            together: vec![1, 2],
        };
        assert_eq!(
            reason_words(&r, &names),
            "These numbers are unusual together: Load is 500, Temp is 85"
        );
        assert_eq!(
            short(&[r], &names),
            "These numbers are unusual together: Load is 500, Temp is 85"
        );
    }

    #[test]
    fn many_columns_empty_in_the_same_few_rows_are_one_reason() {
        let names: Vec<String> = ["A", "B", "C", "User"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let why = vec![
            r(0, None, 42, false, 13.8),
            r(1, None, 42, false, 13.8),
            r(2, None, 42, false, 13.8),
            r(3, Some("mallory"), 1, false, 9.0),
        ];
        assert_eq!(
            short(&why, &names),
            "User is mallory  ·  1 row  ·  +1 more"
        );
        assert_eq!(
            full(&why, &names),
            "A, B and C empty where others are filled: 42 of 20,000 rows\nUser is mallory: 1 of 20,000 rows"
        );
    }

    #[test]
    fn reasons_are_said_in_full_with_their_counts() {
        let names: Vec<String> = ["User", "Bytes", "Host"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            reason_words(&r(0, Some("mallory"), 1, false, 14.3), &names),
            "User is mallory: 1 of 20,000 rows"
        );
        assert_eq!(
            reason_words(&r(1, Some("94500000"), 1, true, 13.3), &names),
            "Bytes is 94500000: the largest in its column (1 of 20,000 rows at or above it)"
        );
        assert_eq!(
            reason_words(&r(2, None, 1, false, 14.3), &names),
            "Host is empty: 1 of 20,000 rows"
        );
        let why = vec![
            r(0, Some("mallory"), 1, false, 14.3),
            r(2, None, 1, false, 4.0),
        ];
        assert_eq!(short(&why, &names), "User is mallory  ·  1 row  ·  +1 more");
        assert_eq!(
            full(&why, &names),
            "User is mallory: 1 of 20,000 rows\nHost is empty: 1 of 20,000 rows"
        );
    }

    #[test]
    fn a_long_value_over_several_lines_is_said_on_one_line_with_what_follows_counted() {
        let names: Vec<String> = ["ScriptContent"].iter().map(|s| s.to_string()).collect();
        let script = format!("try {{\n    $a = 1\n    {}\n}}", "x".repeat(200));
        let w = reason_words(&r(0, Some(&script), 1, false, 14.0), &names);
        assert!(!w.contains('\n'), "{w}");
        assert!(w.starts_with("ScriptContent is try { $a = 1 xxx"), "{w}");
        assert!(w.contains(" chars: 1 of 20,000 rows"), "{w}");
        // a short value is said whole
        assert_eq!(value_words("mallory"), "mallory");
        assert_eq!(value_words("a\n  b"), "a b");
    }

    #[test]
    fn what_a_row_shares_with_many_others_is_kept_apart_from_what_is_rare() {
        let names: Vec<String> = ["User", "Shard", "ParentId", "Region", "Host"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let why = vec![
            r(0, Some("mallory"), 1, false, 14.3),
            // empty in 23% and 35% of rows: not what makes this row stand out
            r(1, None, 4_600, false, 2.1),
            r(2, None, 7_000, false, 1.5),
            // a value in 30% of rows
            r(3, Some("eu"), 6_000, false, 1.7),
            // empty in 5 rows of 20,000: rare, so it is a reason
            r(4, None, 5, false, 12.0),
        ];
        let p = parts(&why, &names);
        assert_eq!(
            p.rare,
            vec![
                "User is mallory: 1 of 20,000 rows".to_string(),
                "Host is empty: 5 of 20,000 rows".to_string()
            ]
        );
        assert_eq!(p.common, vec!["Region is eu (6,000 rows)".to_string()]);
        assert_eq!(p.empty, vec!["Shard".to_string(), "ParentId".to_string()]);
        assert_eq!(
            rest_words(&p).unwrap(),
            "1 value it shares with many rows and 2 columns empty as in many rows"
        );
        // the line beside the row counts only what is rare
        assert_eq!(short(&why, &names), "User is mallory  ·  1 row  ·  +1 more");
        // resting on the row: the rare reasons, and one line for the rest
        assert_eq!(
            brief(&why, &names),
            "User is mallory: 1 of 20,000 rows\nHost is empty: 5 of 20,000 rows\nAlso 1 value it shares with many rows and 2 columns empty as in many rows: all in Row view"
        );
        // everything is still said in full
        assert!(full(&why, &names).ends_with("Empty here, as in 1% of rows or more: Shard, ParentId"));
    }

    #[test]
    fn the_line_beside_a_row_leads_with_a_value_and_says_empty_columns_briefly() {
        let names: Vec<String> = ["A", "B", "C", "User"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // empty columns are the stronger reason, yet the value is what tells rows apart
        let why = vec![
            r(0, None, 42, false, 13.8),
            r(1, None, 42, false, 13.8),
            r(2, None, 42, false, 13.8),
            r(3, Some("mallory"), 3, false, 9.0),
        ];
        assert_eq!(short(&why, &names), "User is mallory  ·  3 rows  ·  +1 more");
        // with no value to lead, the empty columns are said in a few words
        assert_eq!(short(&why[..3], &names), "3 columns empty  ·  42 rows");
    }

    #[test]
    fn the_line_beside_a_row_is_short_whatever_the_value() {
        let names: Vec<String> = ["SHA256HashData", "Bytes", "ScriptContent"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hash = "f36112d3ffa5f653a5d36015d27384e5a78116c98d185f1ba39dc94bab3ac53c";
        assert_eq!(
            short(&[r(0, Some(hash), 4, false, 12.0)], &names),
            "SHA256HashData is f36112d3ffa5f653a5 +46 chars  ·  4 rows"
        );
        // a number at the edge of its column says which edge
        assert_eq!(
            short(&[r(1, Some("94500000"), 1, true, 13.0)], &names),
            "Bytes is 94500000  ·  the largest"
        );
        // never more than one line, never a page of script
        let script = format!("try {{\n  {}\n}}", "x".repeat(400));
        let line = short(&[r(2, Some(&script), 1, false, 14.0)], &names);
        assert!(!line.contains('\n') && line.chars().count() < 70, "{line}");
    }

    #[test]
    fn resting_on_a_row_gives_the_strongest_few_reasons_and_counts_the_rest() {
        let names: Vec<String> = (0..8).map(|i| format!("c{i}")).collect();
        let why: Vec<Reason> = (0..6)
            .map(|i| r(i, Some("rare"), 1 + i as u64, false, 14.0 - i as f64))
            .collect();
        let b = brief(&why, &names);
        let lines: Vec<&str> = b.lines().collect();
        assert_eq!(lines.len(), LEAD + 1, "{b}");
        assert_eq!(lines[0], "c0 is rare: 1 of 20,000 rows");
        assert_eq!(lines[LEAD], "And 3 more rare reasons: all in Row view");
        assert_eq!(halves(lines[0]), ("c0 is rare", "1 of 20,000 rows"));
        assert_eq!(halves("no count here"), ("no count here", ""));
    }

    #[test]
    fn the_why_hover_says_each_fact_once_and_folds_the_common_ones() {
        let names: Vec<String> = [
            "name",
            "event.action",
            "TechniqueId",
            "event.type",
            "host.os",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let of = 89_361;
        let mk = |col: usize, v: &str, rows: u64, bits: f64| Reason {
            col,
            value: Some(v.into()),
            rows,
            of,
            largest: false,
            smallest: false,
            bits,
            together: Vec::new(),
        };
        let why = vec![
            mk(0, "NetworkListenIP6", 8, 13.4),
            mk(1, "NetworkListenIP6", 8, 13.3),
            mk(2, "T1095", 8, 13.4),
            mk(3, "info", 2_296, 5.3),
            mk(4, "Win", 9_615, 3.2),
        ];
        let t = full(&why, &names);
        // the same value in two columns: one line naming both; no bits anywhere
        assert!(
            t.starts_with("NetworkListenIP6 in name and event.action: 8 of 89,361 rows"),
            "{t}"
        );
        assert!(t.contains("TechniqueId is T1095: 8 of 89,361 rows"), "{t}");
        assert!(!t.contains("bits"), "{t}");
        // the common ones: one closing line, each still named
        let last = t.lines().last().unwrap();
        assert_eq!(
            last,
            "Also less rare (each in 1% of rows or more): event.type is info (2,296 rows), host.os is Win (9,615 rows)"
        );
        assert_eq!(t.lines().count(), 3);
    }
}
