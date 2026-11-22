//! "Timeline signals": silent gaps far longer than the file's usual pauses,
//! lasting steps in the rate of rows, and values of a column whose rows come
//! like clockwork. Read from the rows shown; every time and length in words.

use crate::bridge::{Bridge, Cmd, RegularOut, SignalsOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum RhythmAction {
    None,
    /// Replace the time range of the search with this one (µs).
    Period(i64, i64),
    /// Add this to the search.
    AddToSearch(String),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct RhythmWin {
    /// 0 gaps, 1 steps, 2 regular repeats.
    pub tab: u8,
    pub col: Option<String>,
    next_id: u64,
    signals: Option<Run>,
    regular: Option<Run>,
    page: usize,
}

impl Default for RhythmWin {
    fn default() -> Self {
        RhythmWin {
            tab: 0,
            col: None,
            next_id: 0,
            signals: None,
            regular: None,
            page: 0,
        }
    }
}

/// A length of time in words: "4 h 5 m 53 s", "15 m 1 s", "0.4 s".
pub fn dur(us: i64) -> String {
    let us = us.max(0);
    if us < 1_000_000 {
        return format!("{:.1} s", us as f64 / 1e6);
    }
    let s = us / 1_000_000;
    let (d, h, m, s) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d} d"));
    }
    if h > 0 {
        parts.push(format!("{h} h"));
    }
    // "1 h 0 m 55 s", never "1 h 55 s" (which reads as minutes)
    if m > 0 || (!parts.is_empty() && s > 0) {
        parts.push(format!("{m} m"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s} s"));
    }
    parts.join(" ")
}

/// A rate in words, per second or per minute as reads best.
pub fn rate(per_sec: f64) -> String {
    if per_sec >= 1.0 {
        format!("{per_sec:.1} rows a second")
    } else {
        format!("{:.1} rows a minute", per_sec * 60.0)
    }
}

/// A time as the search writes it.
pub fn stamp(us: i64) -> String {
    logseek_core::types::fmt_time(us).replace(' ', "T")
}

fn when(us: i64) -> String {
    logseek_core::types::fmt_time(us)
}

impl RhythmWin {
    fn start_signals(&mut self, bridge: &Bridge, now: f64) {
        if let Some(r) = &self.signals {
            r.stop.store(true, Ordering::SeqCst);
        }
        self.next_id += 1;
        let run = Run {
            id: self.next_id,
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            started: now,
        };
        bridge.send(Cmd::Signals {
            id: run.id,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.signals = Some(run);
        self.page = 0;
    }

    fn start_regular(&mut self, bridge: &Bridge, now: f64) {
        let Some(col) = self.col.clone() else { return };
        if let Some(r) = &self.regular {
            r.stop.store(true, Ordering::SeqCst);
        }
        self.next_id += 1;
        let run = Run {
            id: self.next_id,
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            started: now,
        };
        bridge.send(Cmd::Regular {
            id: run.id,
            col,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.regular = Some(run);
        self.page = 0;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, RhythmAction) {
        let mut open = true;
        let mut act = RhythmAction::None;
        let now = ctx.input(|i| i.time);
        if self.signals.is_none() {
            self.start_signals(bridge, now);
        }
        if self.regular.is_none() && self.col.is_some() {
            self.start_regular(bridge, now);
        }
        let sig: Option<Arc<SignalsOut>> = self
            .signals
            .as_ref()
            .and_then(|r| snap.signals.clone().filter(|o| o.id == r.id));
        let reg: Option<Arc<RegularOut>> = self
            .regular
            .as_ref()
            .and_then(|r| snap.regular.clone().filter(|o| o.id == r.id));
        let mut picked: Option<String> = None;
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("timeline-signals"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Timeline signals").font(crate::fonts::strong(16.0)));
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
                    RichText::new("Read from the times of the rows shown, on their own terms: silences far longer than the usual pause, lasting changes in the rate of rows, and values whose rows come like clockwork.")
                        .color(pal.text2),
                );
                ui.horizontal(|ui| {
                    let before = self.tab;
                    ui.selectable_value(&mut self.tab, 0, "Silent gaps");
                    ui.selectable_value(&mut self.tab, 1, "Steps in the rate");
                    ui.selectable_value(&mut self.tab, 2, "Regular repeats");
                    if self.tab != before {
                        self.page = 0;
                    }
                });
                ui.separator();
                if self.tab == 2 {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Each value of");
                        let mut col = self.col.clone().unwrap_or_default();
                        if crate::colpick::combo(ui, "rhythm-col", &mut col, &snap.columns, 220.0, "choose a column") {
                            picked = Some(col);
                        }
                        ui.label("in time order: the more alike the gaps between its rows, the higher it comes.");
                    });
                    let Some(run) = &self.regular else { return };
                    let Some(out) = reg.clone() else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!(
                                "{} rows read · {:.0} s",
                                group(run.done.load(Ordering::Relaxed)),
                                now - run.started
                            ));
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(150));
                        return;
                    };
                    if let Some(e) = &out.error {
                        ui.label(RichText::new(format!("{}  {e}", ph::WARNING)).color(pal.danger));
                        return;
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} values with enough rows to judge (10 gaps or more); {} with fewer  ·  read in {:.1} s",
                            group(out.values.len() as u64),
                            group(out.too_few as u64),
                            out.secs
                        ))
                        .strong(),
                    );
                    let range = crate::pager::pager(ui, pal, &mut self.page, out.values.len());
                    egui::ScrollArea::vertical()
                        .id_salt("rhythm-lines")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, k) in range.enumerate() {
                                let v = &out.values[k];
                                let name = v.value.clone().unwrap_or_else(|| "(empty)".into());
                                let keep = match &v.value {
                                    Some(x) => format!(
                                        "{}={}",
                                        logseek_core::formula::column_ref(&out.col),
                                        logseek_core::formula::quote(x)
                                    ),
                                    None => format!("NOT {}:*", logseek_core::formula::column_ref(&out.col)),
                                };
                                line(ui, pal, i, |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(name).font(theme::data_font()).strong())
                                            .wrap(),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "every {}, give or take {}  ·  {} rows  ·  {} to {}",
                                                dur(v.every),
                                                dur(v.give_or_take),
                                                group(v.rows),
                                                when(v.first),
                                                when(v.last)
                                            ))
                                            .color(pal.text2),
                                        );
                                        if ui
                                            .small_button(format!("{}  Show only these rows", ph::FUNNEL))
                                            .on_hover_text(keep.clone())
                                            .clicked()
                                        {
                                            act = RhythmAction::AddToSearch(keep.clone());
                                        }
                                    });
                                });
                            }
                        });
                    return;
                }
                let Some(run) = &self.signals else { return };
                let Some(out) = sig.clone() else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "{} rows read · {:.0} s",
                            group(run.done.load(Ordering::Relaxed)),
                            now - run.started
                        ));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    return;
                };
                if let Some(e) = &out.error {
                    ui.label(RichText::new(format!("{}  {e}", ph::WARNING)).color(pal.danger));
                    return;
                }
                if self.tab == 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} silences of {} or more (the usual pause between rows is {})  ·  longest first  ·  read in {:.1} s",
                            group(out.gaps.len() as u64),
                            dur(out.gap_line),
                            dur(out.usual_pause),
                            out.secs
                        ))
                        .strong(),
                    );
                    let range = crate::pager::pager(ui, pal, &mut self.page, out.gaps.len());
                    egui::ScrollArea::vertical()
                        .id_salt("gap-lines")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, k) in range.enumerate() {
                                let g = &out.gaps[k];
                                line(ui, pal, i, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format!("No rows for {}", dur(g.to - g.from)))
                                                .strong(),
                                        );
                                        ui.label(
                                            RichText::new(format!(
                                                "from {} to {}",
                                                when(g.from),
                                                when(g.to)
                                            ))
                                            .font(theme::data_font())
                                            .color(pal.text2),
                                        );
                                        if ui
                                            .small_button(format!("{}  Show the minutes either side", ph::CLOCK))
                                            .clicked()
                                        {
                                            act = RhythmAction::Period(
                                                g.from - 5 * 60_000_000,
                                                g.to + 5 * 60_000_000,
                                            );
                                        }
                                    });
                                });
                            }
                        });
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{} lasting {} in the rate of rows (at least twice or half the rate before, read in steps of {})  ·  read in {:.1} s",
                            group(out.steps.len() as u64),
                            if out.steps.len() == 1 { "change" } else { "changes" },
                            dur(out.step_width),
                            out.secs
                        ))
                        .strong(),
                    );
                    if out.steps.is_empty() {
                        ui.label("The rate of rows holds steady: no lasting change of twice or half.");
                    }
                    let range = crate::pager::pager(ui, pal, &mut self.page, out.steps.len());
                    egui::ScrollArea::vertical()
                        .id_salt("step-lines")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, k) in range.enumerate() {
                                let s = &out.steps[k];
                                let up = s.after > s.before;
                                let times = if up { s.after / s.before } else { s.before / s.after };
                                line(ui, pal, i, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "The rate went {} {:.1} times at {}",
                                                if up { "up" } else { "down" },
                                                times,
                                                when(s.at)
                                            ))
                                            .strong(),
                                        );
                                        ui.label(
                                            RichText::new(format!("from {} to {}", rate(s.before), rate(s.after)))
                                                .color(pal.text2),
                                        );
                                        if ui
                                            .small_button(format!("{}  Show the hour either side", ph::CLOCK))
                                            .clicked()
                                        {
                                            act = RhythmAction::Period(s.at - 3_600_000_000, s.at + 3_600_000_000);
                                        }
                                    });
                                });
                            }
                        });
                }
            });
        if let Some(c) = picked {
            self.col = Some(c);
            self.start_regular(bridge, now);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            open = false;
        }
        if !open {
            for r in [&self.signals, &self.regular].into_iter().flatten() {
                r.stop.store(true, Ordering::SeqCst);
            }
        }
        (open, act)
    }
}

fn line(ui: &mut egui::Ui, pal: &Palette, i: usize, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(if i % 2 == 0 {
            pal.surface2
        } else {
            pal.surface
        })
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            body(ui);
        });
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_and_rates_read_in_words() {
        assert_eq!(dur(14_753 * 1_000_000), "4 h 5 m 53 s");
        assert_eq!(dur(901 * 1_000_000), "15 m 1 s");
        assert_eq!(dur(400_000), "0.4 s");
        assert_eq!(dur(2 * 86_400 * 1_000_000), "2 d");
        assert_eq!(dur(3_655 * 1_000_000), "1 h 0 m 55 s");
        assert_eq!(rate(6.54), "6.5 rows a second");
        assert_eq!(rate(0.25), "15.0 rows a minute");
        // a time as written in a search reads back as the same time
        let us = 1_794_441_600_000_000 + 1_234_567;
        let q = format!("@{}..{}", stamp(us), stamp(us + 60_000_000));
        let p = logseek_core::query::parse(&q).unwrap();
        assert_eq!(p.time.map(|t| t.0 / 1000), Some(us / 1000));
    }
}
