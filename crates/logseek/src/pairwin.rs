//! "Rare pairs": for one column (A) and another (B), a value of B rarely
//! seen with a value of A that is otherwise common, and values of B seen with
//! the fewest values of A, however many rows they have. Exact counts, every
//! pair kept, a page at a time.

use crate::bridge::{Bridge, Cmd, PairsOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum PairAction {
    None,
    AddToSearch(String),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
}

pub struct PairWin {
    pub a: String,
    pub b: Option<String>,
    /// 0: rarely seen with a common value; 1: seen with the fewest.
    tab: u8,
    next_id: u64,
    run: Option<Run>,
    page: usize,
}

fn value_words(col: &str, v: &Option<String>) -> String {
    match v {
        Some(v) => format!("{col} is {v}"),
        None => format!("{col} is empty"),
    }
}

fn term(col: &str, v: &Option<String>) -> String {
    use logseek_core::formula::{column_ref, quote};
    match v {
        Some(v) => format!("{}={}", column_ref(col), quote(v)),
        None => format!("NOT {}:*", column_ref(col)),
    }
}

/// "1 row · 1 in 5,000 of the rows where Vendor is Acme"
pub fn unusual_words(a: &str, l: &logseek_core::query::pairs::PairLine) -> String {
    let one_in = l.a_rows as f64 / l.rows.max(1) as f64;
    let of = if l.a_rows == 1 {
        "the only row".to_string()
    } else if l.rows == l.a_rows {
        format!("every one of the {} rows", group(l.a_rows))
    } else if one_in >= 10.0 {
        // few among many: the plain count says it best
        format!("out of {} rows", group(l.a_rows))
    } else {
        format!("{:.0}% of the {} rows", 100.0 / one_in, group(l.a_rows))
    };
    format!(
        "{} {}  ·  {} where {}, which has {} different {}",
        group(l.rows),
        if l.rows == 1 { "row" } else { "rows" },
        of,
        value_words(a, &l.a),
        group(l.partners),
        if l.partners == 1 {
            "partner"
        } else {
            "partners"
        }
    )
}

/// "seen with 1 of 501 values of ClientIp  ·  2,000 rows"
pub fn peer_words(a: &str, l: &logseek_core::query::pairs::PeerLine, a_values: u64) -> String {
    format!(
        "seen with {} of {} {} of {}  ·  {} {}",
        group(l.peers),
        group(a_values),
        if a_values == 1 { "value" } else { "values" },
        a,
        group(l.rows),
        if l.rows == 1 { "row" } else { "rows" }
    )
}

impl PairWin {
    pub fn new(a: String) -> PairWin {
        PairWin {
            a,
            b: None,
            tab: 0,
            next_id: 0,
            run: None,
            page: 0,
        }
    }

    pub fn with(a: String, b: String) -> PairWin {
        let mut w = PairWin::new(a);
        w.b = Some(b);
        w
    }

    fn start(&mut self, bridge: &Bridge, now: f64) {
        let Some(b) = self.b.clone() else { return };
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
        bridge.send(Cmd::Pairs {
            id: run.id,
            a: self.a.clone(),
            b,
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
    ) -> (bool, PairAction) {
        let mut open = true;
        let mut act = PairAction::None;
        let now = ctx.input(|i| i.time);
        if self.run.is_none() && self.b.is_some() {
            self.start(bridge, now);
        }
        let out: Option<Arc<PairsOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.pairs.clone().filter(|o| o.id == r.id));
        let mut picked: Option<String> = None;
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("rare-pairs"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Rare pairs: for each").font(crate::fonts::strong(16.0)));
                    ui.label(
                        RichText::new(&self.a)
                            .font(crate::fonts::strong(16.0))
                            .color(pal.accent_ink),
                    );
                    ui.label(RichText::new("and").font(crate::fonts::strong(16.0)));
                    // every column but the first one, in the same list as everywhere else
                    let others: Vec<String> = snap
                        .columns
                        .iter()
                        .filter(|c| **c != self.a)
                        .cloned()
                        .collect();
                    let mut b = self.b.clone().unwrap_or_default();
                    if crate::colpick::combo(ui, "pair-b", &mut b, &others, 220.0, "choose a column") {
                        picked = Some(b);
                    }
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
                    RichText::new("Two columns read together, every pair counted exactly. Rare pairs break a habit: a value seen thousands of times with one partner and once with another, or a value only one or two of the other column's values ever have.")
                        .color(pal.text2),
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, 0, "Rarely seen with a common value");
                    ui.selectable_value(&mut self.tab, 1, "Seen with the fewest");
                });
                ui.separator();
                let Some(run) = &self.run else {
                    ui.label(
                        RichText::new("Choose the second column to read the two together.")
                            .color(pal.text2),
                    );
                    return;
                };
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
                            format!("{}  The columns could not be read: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                ui.label(
                    RichText::new(format!(
                        "{} different pairs, {} values of {}, {} rows  ·  read in {:.1} s",
                        group(out.unusual.len() as u64),
                        group(out.a_values),
                        out.a,
                        group(out.total),
                        out.secs
                    ))
                    .strong(),
                );
                let n = if self.tab == 0 {
                    out.unusual.len()
                } else {
                    out.peers.len()
                };
                let range = crate::pager::pager(ui, pal, &mut self.page, n);
                egui::ScrollArea::vertical()
                    .id_salt("pair-lines")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, k) in range.enumerate() {
                            let (title, detail, keep) = if self.tab == 0 {
                                let l = &out.unusual[k];
                                (
                                    format!(
                                        "{}  with  {}",
                                        value_words(&out.a, &l.a),
                                        value_words(&out.b, &l.b)
                                    ),
                                    unusual_words(&out.a, l),
                                    format!("{} {}", term(&out.a, &l.a), term(&out.b, &l.b)),
                                )
                            } else {
                                let l = &out.peers[k];
                                (
                                    value_words(&out.b, &l.b),
                                    peer_words(&out.a, l, out.a_values),
                                    term(&out.b, &l.b),
                                )
                            };
                            egui::Frame::new()
                                .fill(if i % 2 == 0 { pal.surface2 } else { pal.surface })
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(10, 4))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(title).font(theme::data_font()).strong(),
                                        )
                                        .wrap(),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(detail).color(pal.text2));
                                        if ui
                                            .small_button(format!("{}  Show only these rows", ph::FUNNEL))
                                            .on_hover_text(keep.clone())
                                            .clicked()
                                        {
                                            act = PairAction::AddToSearch(keep.clone());
                                        }
                                        if ui
                                            .small_button(format!("{}  Leave these out", ph::FUNNEL_X))
                                            .on_hover_text(format!("NOT ({keep})"))
                                            .clicked()
                                        {
                                            act = PairAction::AddToSearch(format!("NOT ({keep})"));
                                        }
                                    });
                                });
                            ui.add_space(2.0);
                        }
                    });
            });
        if let Some(b) = picked {
            self.b = Some(b);
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
    use logseek_core::query::pairs::{PairLine, PeerLine};

    #[test]
    fn pairs_are_said_with_their_counts() {
        let l = PairLine {
            a: Some("Acme".into()),
            b: Some("ACC-9".into()),
            rows: 1,
            a_rows: 5_000,
            partners: 2,
            surprise: 11.3,
        };
        assert_eq!(
            unusual_words("Vendor", &l),
            "1 row  ·  out of 5,000 rows where Vendor is Acme, which has 2 different partners"
        );
        let l = PairLine {
            a: None,
            b: Some("x".into()),
            rows: 30,
            a_rows: 60,
            partners: 2,
            surprise: 0.0,
        };
        assert_eq!(
            unusual_words("Vendor", &l),
            "30 rows  ·  50% of the 60 rows where Vendor is empty, which has 2 different partners"
        );
        let p = PeerLine {
            b: Some("/admin/export.sql".into()),
            peers: 1,
            rows: 2_000,
        };
        assert_eq!(
            peer_words("ClientIp", &p, 501),
            "seen with 1 of 501 values of ClientIp  ·  2,000 rows"
        );
        assert_eq!(term("Client Ip", &None), "NOT `Client Ip`:*");
    }
}
