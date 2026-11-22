//! "Strange text in this column": the values written unlike the rest of the
//! column, strangest first. The column is its own measure (which characters
//! usually follow which, learned from its values); nothing is known in
//! advance. Every number is said in words against the column's own usual.

use crate::bridge::{Bridge, Cmd, Snapshot, StrangeOut};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::strange::{share_below, BUCKETS};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum StrangeAction {
    None,
    /// Add this to the search.
    AddToSearch(String),
    /// Show exactly these rows.
    ShowRows(Arc<Vec<u64>>),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct StrangeWin {
    pub col: String,
    /// The line asked for (bits per pair); `None`: the strangest thousandth.
    line: Option<f64>,
    /// The line being edited.
    edit: f64,
    next_id: u64,
    run: Option<Run>,
    want: bool,
    /// The page of values shown.
    page: usize,
}

/// "stranger than 99.98% of the column's rows"
pub fn rank_words(spread: &[u64], score: f64) -> String {
    let p = share_below(spread, score) * 100.0;
    let s = if p >= 99.995 {
        // not every row: never rounded up to 100%
        "over 99.99".to_string()
    } else if p >= 99.0 {
        format!("{p:.2}")
    } else {
        format!("{p:.0}")
    };
    format!("stranger than {s}% of the column's rows")
}

impl StrangeWin {
    pub fn new(col: String) -> StrangeWin {
        StrangeWin {
            col,
            line: None,
            edit: 0.0,
            next_id: 0,
            run: None,
            want: true,
            page: 0,
        }
    }

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
        bridge.send(Cmd::Strange {
            id: run.id,
            col: self.col.clone(),
            line: self.line,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.run = Some(run);
        self.page = 0;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, StrangeAction) {
        let mut open = true;
        let mut act = StrangeAction::None;
        let now = ctx.input(|i| i.time);
        if std::mem::take(&mut self.want) {
            self.start(bridge, now);
        }
        let out: Option<Arc<StrangeOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.strange.clone().filter(|o| o.id == r.id));
        if let (Some(o), None) = (&out, self.line) {
            self.edit = o.line;
        }
        let mut rerun = false;
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("strange-text"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Strange text in").font(crate::fonts::strong(16.0)));
                    ui.label(
                        RichText::new(&self.col)
                            .font(crate::fonts::strong(16.0))
                            .color(pal.accent_ink),
                    );
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
                    RichText::new("Values written unlike the rest of this column: encoded or packed text, garbled characters, broken lines, disguised commands. The column is its own measure: which characters usually follow which is learned from its own values.")
                        .color(pal.text2),
                );
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
                            format!("{}  The column could not be read: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                // the spread of the column's text, the line marked
                let scored: u64 = out.spread.iter().sum();
                ui.label(format!(
                    "Each value gets a score for how unexpected its letters are, judged from this column's own text: the higher, the stranger. Usual text here scores {:.1} (half the rows score less). {} rows scored{}.",
                    out.usual,
                    group(scored),
                    if out.unscored > 0 {
                        format!(", {} too short to score (fewer than two characters) or empty", group(out.unscored))
                    } else {
                        String::new()
                    }
                ));
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 70.0),
                    egui::Sense::click(),
                );
                let max = out.spread.iter().copied().max().unwrap_or(1).max(1) as f32;
                let bw = rect.width() / BUCKETS as f32;
                for (i, &n) in out.spread.iter().enumerate() {
                    if n == 0 {
                        continue;
                    }
                    // a height that shows even one row
                    let hgt = ((n as f32).ln_1p() / max.ln_1p() * (rect.height() - 4.0)).max(2.0);
                    let x = rect.left() + i as f32 * bw;
                    let above = i as f64 / 10.0 >= out.line;
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x, rect.bottom() - hgt),
                            egui::pos2(x + bw.max(1.0) - 0.5, rect.bottom()),
                        ),
                        0.0,
                        if above { pal.accent } else { pal.text2 },
                    );
                }
                let lx = rect.left() + (out.line * 10.0) as f32 * bw;
                ui.painter().line_segment(
                    [egui::pos2(lx, rect.top()), egui::pos2(lx, rect.bottom())],
                    egui::Stroke::new(1.5, pal.accent_ink),
                );
                if let Some(p) = resp.hover_pos() {
                    let b = (((p.x - rect.left()) / bw) as usize).min(BUCKETS - 1);
                    resp.clone().on_hover_text(format!(
                        "Score {:.1}: {} rows. Click to put the line here.",
                        b as f64 / 10.0,
                        group(out.spread[b])
                    ));
                    if resp.clicked() {
                        self.edit = b as f64 / 10.0;
                        rerun = true;
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Show values scoring at or above");
                    let r = ui.add(
                        egui::DragValue::new(&mut self.edit)
                            .range(0.0..=16.0)
                            .speed(0.05)
                            .fixed_decimals(1)
                            .suffix(""),
                    );
                    if r.drag_stopped() || r.lost_focus() {
                        rerun = (self.edit - out.line).abs() > 1e-9;
                    }
                    ui.label(
                        RichText::new(format!("({})", rank_words(&out.spread, out.line)))
                            .color(pal.text2),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} in {} rows  ·  read in {:.1} s",
                            group(out.values.len() as u64),
                            if out.values.len() == 1 { "value" } else { "values" },
                            group(out.rows.len() as u64),
                            out.secs
                        ))
                        .strong(),
                    );
                    if !out.rows.is_empty()
                        && ui
                            .button(format!("{}  Show these rows", ph::ROWS))
                            .on_hover_text("Exactly the rows holding these values, in the grid")
                            .clicked()
                    {
                        act = StrangeAction::ShowRows(out.rows.clone());
                    }
                });
                let col = self.col.clone();
                let range = crate::pager::pager(ui, pal, &mut self.page, out.values.len());
                egui::ScrollArea::vertical()
                    .id_salt("strange-lines")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, v) in out.values[range].iter().enumerate() {
                            egui::Frame::new()
                                .fill(if i % 2 == 0 { pal.surface2 } else { pal.surface })
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&v.text).font(theme::data_font()),
                                        )
                                        .wrap(),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "score {:.1} (usual {:.1})  ·  {}  ·  {} {}",
                                                v.score,
                                                out.usual,
                                                rank_words(&out.spread, v.score),
                                                group(v.rows),
                                                if v.rows == 1 { "row" } else { "rows" }
                                            ))
                                            .color(pal.text2),
                                        );
                                    });
                                    let term = format!(
                                        "{}={}",
                                        logseek_core::formula::column_ref(&col),
                                        logseek_core::formula::quote(&v.text)
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        if ui
                                            .button(format!("{}  Show only this value", ph::FUNNEL))
                                            .on_hover_text(term.clone())
                                            .clicked()
                                        {
                                            act = StrangeAction::AddToSearch(term.clone());
                                        }
                                        if ui
                                            .button(format!("{}  Leave it out", ph::FUNNEL_X))
                                            .on_hover_text(format!("NOT ({term})"))
                                            .clicked()
                                        {
                                            act = StrangeAction::AddToSearch(format!("NOT ({term})"));
                                        }
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    });
            });
        if rerun {
            self.line = Some(self.edit);
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

    #[test]
    fn a_score_is_said_against_the_column() {
        let mut spread = vec![0u64; BUCKETS];
        spread[20] = 9_998;
        spread[64] = 2;
        assert_eq!(
            rank_words(&spread, 6.4),
            "stranger than 99.98% of the column's rows"
        );
        assert_eq!(
            rank_words(&spread, 2.5),
            "stranger than 99.98% of the column's rows"
        );
        assert_eq!(
            rank_words(&spread, 1.0),
            "stranger than 0% of the column's rows"
        );
    }
}
