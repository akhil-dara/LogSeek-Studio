//! "Shapes in this column": every shape the column's values have (runs of
//! digits, letters and spaces, the marks between kept as they are), with how
//! many rows have each, read from the values themselves. The odd ones stand
//! out at once: an address where a postal code belongs, a broken line, a
//! value cut short. One click shows those rows or leaves them out.

use crate::bridge::{Bridge, Cmd, ShapesOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::shape::{shape_term, shape_words, Shape};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum ShapeAction {
    None,
    /// Add this to the search.
    AddToSearch(String),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct ShapeWin {
    pub col: String,
    pub how: Shape,
    /// Fewest rows first (the odd ones on top).
    pub rare_first: bool,
    next_id: u64,
    run: Option<Run>,
    want: bool,
    /// The page of lines shown.
    page: usize,
}

impl ShapeWin {
    pub fn new(col: String) -> ShapeWin {
        ShapeWin {
            col,
            how: Shape::AnyLength,
            rare_first: true,
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
        bridge.send(Cmd::Shapes {
            id: run.id,
            col: self.col.clone(),
            how: self.how,
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
    ) -> (bool, ShapeAction) {
        let mut open = true;
        let mut act = ShapeAction::None;
        let now = ctx.input(|i| i.time);
        if std::mem::take(&mut self.want) {
            self.start(bridge, now);
        }
        let out: Option<Arc<ShapesOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.shapes.clone().filter(|o| o.id == r.id));
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("shapes-in-column"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Shapes in").font(crate::fonts::strong(16.0)));
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
                    RichText::new("Each value read as its runs of digits, letters and spaces, with the marks between kept as they are. Values of an unusual shape stand out at once.")
                        .color(pal.text2),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Lengths:");
                    let mut changed = false;
                    for s in Shape::ALL {
                        changed |= ui
                            .radio_value(&mut self.how, s, s.label())
                            .on_hover_text(s.hint())
                            .changed();
                    }
                    if changed {
                        self.start(bridge, now);
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
                            "{} of {} rows read · {:.0} s",
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
                            format!("{}  The shapes could not be read: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                ui.label(
                    RichText::new(format!(
                        "{} {} in {} rows{}  ·  read in {:.1} s",
                        group(out.shapes.len() as u64),
                        if out.shapes.len() == 1 { "shape" } else { "shapes" },
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
                let col = self.col.clone();
                let total = out.total.max(1);
                let mut line = |ui: &mut egui::Ui, i: usize, words: String, example: Option<&str>, rows: u64, keep: String| {
                    egui::Frame::new()
                        .fill(if i % 2 == 0 { pal.surface2 } else { pal.surface })
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(words).strong());
                            });
                            if let Some(ex) = example {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new("for example").color(pal.text2));
                                    ui.label(RichText::new(ex).font(theme::data_font()));
                                });
                            }
                            // the share, the count and the actions on one line
                            ui.horizontal_wrapped(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(140.0, 8.0), egui::Sense::hover());
                                ui.painter().rect_filled(rect, 3.0, pal.hairline);
                                let f = rows as f32 / total as f32;
                                let mut bar = rect;
                                bar.set_width((rect.width() * f).max(2.0));
                                ui.painter().rect_filled(bar, 3.0, pal.accent);
                                let pct = rows as f64 * 100.0 / total as f64;
                                ui.label(
                                    RichText::new(format!(
                                        "{} {} ({})",
                                        group(rows),
                                        if rows == 1 { "row" } else { "rows" },
                                        if pct >= 1.0 {
                                            format!("{pct:.1}%")
                                        } else {
                                            format!("{pct:.4}%")
                                        }
                                    ))
                                    .color(pal.text2),
                                );
                                if ui
                                    .small_button(format!("{}  Show only these rows", ph::FUNNEL))
                                    .on_hover_text(keep.clone())
                                    .clicked()
                                {
                                    act = ShapeAction::AddToSearch(keep.clone());
                                }
                                if ui
                                    .small_button(format!("{}  Leave these out", ph::FUNNEL_X))
                                    .on_hover_text(format!("NOT ({keep})"))
                                    .clicked()
                                {
                                    act = ShapeAction::AddToSearch(format!("NOT ({keep})"));
                                }
                            });
                        });
                    ui.add_space(2.0);
                };
                // every line in order (None: the empty cells), a page at a time
                let n = out.shapes.len();
                let mut items: Vec<Option<usize>> = (0..n).map(Some).collect();
                if self.rare_first {
                    items.reverse();
                }
                if out.empty > 0 {
                    if self.rare_first {
                        items.push(None);
                    } else {
                        items.insert(0, None);
                    }
                }
                let range = crate::pager::pager(ui, pal, &mut self.page, items.len());
                egui::ScrollArea::vertical()
                    .id_salt("shape-lines")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, item) in items[range].iter().enumerate() {
                            match item {
                                None => line(
                                    ui,
                                    i,
                                    "Empty".to_string(),
                                    None,
                                    out.empty,
                                    format!("NOT {}:*", logseek_core::formula::column_ref(&col)),
                                ),
                                Some(k) => {
                                    let s = &out.shapes[*k];
                                    let words = shape_words(&s.pattern)
                                        .unwrap_or_else(|| format!("exactly \"{}\"", s.example));
                                    line(ui, i, words, Some(&s.example), s.rows, shape_term(&col, &s.pattern));
                                }
                            }
                        }
                    });
            });
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
