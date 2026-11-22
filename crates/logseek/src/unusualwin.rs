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
        Some(v) => format!("{col} is {v}"),
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

/// The band's short line: the strongest reason, and how many more.
pub fn short(why: &[Reason], names: &[String]) -> String {
    let s = said(why, names);
    match s.len() {
        0 => String::new(),
        1 => s[0].0.clone(),
        n => format!(
            "{}  ·  {} more {} in Row view",
            s[0].0,
            n - 1,
            if n == 2 { "reason" } else { "reasons" }
        ),
    }
}

/// Every reason, said the way a person would, rarest first: the same fact
/// in several columns is said once with each column named ("NetworkListenIP6
/// in name, event.action: 8 of 89,361 rows"); values in 1% of rows or more
/// add little and are folded into one closing line, each still named.
pub fn full(why: &[Reason], names: &[String]) -> String {
    let col = |c: usize| names.get(c).map_or("", String::as_str).to_string();
    let rows_word = |n: u64| if n == 1 { "row" } else { "rows" };
    // (words, order key): rare facts, grouped across columns
    let mut lines: Vec<(String, f64)> = Vec::new();
    let mut rare: Vec<(Option<String>, u64, u64, bool, bool, Vec<String>, f64)> = Vec::new();
    let mut common: Vec<(String, u64, f64)> = Vec::new();
    let mut empties: Vec<(u64, u64, Vec<String>, f64)> = Vec::new();
    for r in why {
        if !r.together.is_empty() {
            lines.push((reason_words(r, names), r.bits));
        } else if r.value.is_none() && !r.largest && !r.smallest {
            match empties.iter_mut().find(|e| e.0 == r.rows && e.1 == r.of) {
                Some(e) => {
                    e.2.push(col(r.col));
                    e.3 = e.3.max(r.bits);
                }
                None => empties.push((r.rows, r.of, vec![col(r.col)], r.bits)),
            }
        } else if !r.largest && !r.smallest && r.of > 0 && r.rows * 100 >= r.of {
            let v = r.value.clone().unwrap_or_default();
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
        let v = v.unwrap_or_default();
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
    let mut out: Vec<String> = lines.into_iter().map(|(w, _)| w).collect();
    if !common.is_empty() {
        common.sort_by(|a, b| b.2.total_cmp(&a.2));
        let items: Vec<String> = common
            .iter()
            .map(|(w, rows, _)| format!("{w} ({} {})", group(*rows), rows_word(*rows)))
            .collect();
        out.push(format!(
            "Also less rare (each in 1% of rows or more): {}",
            items.join(", ")
        ));
    }
    out.join("\n")
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
                    RichText::new("Every row of those shown, scored by how rare its values are in their own columns: a value few rows share, a number far out in its column, an empty cell where others have one. Your file is the measure; a column naming every row, or counting up row by row, weighs nothing.")
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
                ui.label(format!(
                    "A usual row scores {:.1} (half the rows score less). {} {} weighed.",
                    out.usual,
                    group(out.weighed as u64),
                    if out.weighed == 1 { "column" } else { "columns" }
                ));
                if !out.left_out.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(
                                "Not weighed, their values nearly all different: {}",
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
                                "Weighing nothing (every value different, or counting up row by row): {}",
                                out.quiet.join(", ")
                            ))
                            .color(pal.text2),
                        )
                        .wrap(),
                    );
                }
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
                    ui.label("Rows scoring at least");
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
                            ": {} of {} rows  ·  read in {:.1} s",
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
            "3 columns empty that others fill (42 of 20,000 rows)  ·  1 more reason in Row view"
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
        assert_eq!(
            short(&why, &names),
            "User is mallory: 1 of 20,000 rows  ·  1 more reason in Row view"
        );
        assert_eq!(
            full(&why, &names),
            "User is mallory: 1 of 20,000 rows\nHost is empty: 1 of 20,000 rows"
        );
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
