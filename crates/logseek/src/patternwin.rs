//! "Patterns in this column": lines of free text folded into patterns with
//! the parts that vary marked, each with its exact rows; rare patterns first,
//! so a line unlike all the others stands out among millions.

use crate::bridge::{Bridge, Cmd, PatternsOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::patterns::pattern_words;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum PatternAction {
    None,
    /// Show exactly these rows, said as this.
    ShowRows(Arc<Vec<u64>>, String),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct PatternWin {
    pub col: String,
    /// How alike lines must be, as a share of their words (percent).
    alike: f64,
    rare_first: bool,
    next_id: u64,
    run: Option<Run>,
    want: bool,
    /// The pattern whose rows were asked for.
    asked: Option<usize>,
    find: String,
    /// The page of patterns shown.
    page: usize,
}

impl PatternWin {
    pub fn new(col: String) -> PatternWin {
        PatternWin {
            col,
            alike: 50.0,
            rare_first: true,
            next_id: 0,
            run: None,
            want: true,
            asked: None,
            find: String::new(),
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
        bridge.send(Cmd::Patterns {
            id: run.id,
            col: self.col.clone(),
            alike: self.alike / 100.0,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.run = Some(run);
        self.asked = None;
        self.page = 0;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, PatternAction) {
        let mut open = true;
        let mut act = PatternAction::None;
        let now = ctx.input(|i| i.time);
        if std::mem::take(&mut self.want) {
            self.start(bridge, now);
        }
        let out: Option<Arc<PatternsOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.patterns.clone().filter(|o| o.id == r.id));
        // the rows asked for have arrived
        if let (Some(r), Some(which), Some((id, w, ids))) =
            (&self.run, self.asked, &snap.pattern_rows)
        {
            if *id == r.id && *w == which {
                self.asked = None;
                let words = out
                    .as_ref()
                    .and_then(|o| o.patterns.get(which))
                    .map(pattern_words)
                    .unwrap_or_default();
                act = PatternAction::ShowRows(
                    ids.clone(),
                    format!("rows of the pattern \"{words}\" in {}", self.col),
                );
            }
        }
        let mut rerun = false;
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("patterns-in-column"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Patterns in").font(crate::fonts::strong(16.0)));
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
                    RichText::new("Lines folded into patterns: lines with the same number of words and the same first word fold together when enough of their words agree; the words that differ are marked [varies]. A word that starts with a digit, or is at least half digits, may vary.")
                        .color(pal.text2),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Lines fold together when at least");
                    let r = ui.add(
                        egui::DragValue::new(&mut self.alike)
                            .range(1.0..=100.0)
                            .speed(1.0)
                            .suffix("%"),
                    );
                    ui.label("of their words agree");
                    if r.drag_stopped() || r.lost_focus() {
                        rerun = true;
                    }
                    ui.separator();
                    ui.radio_value(&mut self.rare_first, true, "Fewest rows first");
                    ui.radio_value(&mut self.rare_first, false, "Most rows first");
                });
                ui.separator();
                let Some(run) = &self.run else { return };
                let Some(out) = out.clone() else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "{} rows read (two reads of {}) · {:.0} s",
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
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} in {} rows{}  ·  read in {:.1} s",
                            group(out.patterns.len() as u64),
                            if out.patterns.len() == 1 { "pattern" } else { "patterns" },
                            group(out.total),
                            if out.empty > 0 {
                                format!(", {} of them empty", group(out.empty))
                            } else {
                                String::new()
                            },
                            out.secs
                        ))
                        .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.find)
                                .hint_text("Find words in a pattern")
                                .desired_width(220.0),
                        );
                    });
                });
                let total = out.total.max(1);
                let find = self.find.to_lowercase();
                let n = out.patterns.len();
                let mut shown: Vec<usize> = (0..n)
                    .filter(|&k| {
                        find.is_empty()
                            || pattern_words(&out.patterns[k]).to_lowercase().contains(&find)
                    })
                    .collect();
                if self.rare_first {
                    shown.reverse();
                }
                let range = crate::pager::pager(ui, pal, &mut self.page, shown.len());
                egui::ScrollArea::vertical()
                    .id_salt("pattern-lines")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, &k) in shown[range].iter().enumerate() {
                            let p = &out.patterns[k];
                            let words = pattern_words(p);
                            egui::Frame::new()
                                .fill(if i % 2 == 0 { pal.surface2 } else { pal.surface })
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&words).font(theme::data_font()).strong(),
                                        )
                                        .wrap(),
                                    );
                                    if p.rows > 1 {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(RichText::new("for example").color(pal.text2));
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&p.example).font(theme::data_font()),
                                                )
                                                .wrap(),
                                            );
                                        });
                                    }
                                    ui.horizontal(|ui| {
                                        let full = ui.available_width().clamp(120.0, 300.0);
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(full, 8.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(rect, 3.0, pal.hairline);
                                        let mut bar = rect;
                                        bar.set_width(
                                            (rect.width() * p.rows as f32 / total as f32).max(2.0),
                                        );
                                        ui.painter().rect_filled(bar, 3.0, pal.accent);
                                        let pct = p.rows as f64 * 100.0 / total as f64;
                                        ui.label(
                                            RichText::new(format!(
                                                "{} {} ({})",
                                                group(p.rows),
                                                if p.rows == 1 { "row" } else { "rows" },
                                                if pct >= 1.0 {
                                                    format!("{pct:.1}%")
                                                } else {
                                                    format!("{pct:.4}%")
                                                }
                                            ))
                                            .color(pal.text2),
                                        );
                                        let busy = self.asked == Some(k);
                                        if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new(format!(
                                                    "{}  Show these rows",
                                                    ph::ROWS
                                                )),
                                            )
                                            .on_hover_text("Exactly the rows of this pattern, in the grid")
                                            .clicked()
                                        {
                                            self.asked = Some(k);
                                            bridge.send(Cmd::PatternRows { id: run.id, which: k });
                                        }
                                        if busy {
                                            ui.spinner();
                                        }
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    });
            });
        if rerun {
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
