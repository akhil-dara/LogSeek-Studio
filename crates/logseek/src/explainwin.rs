//! "What sets these rows apart?": the values, and pairs of values, far more
//! common in the rows shown than in the rows they are compared with, each
//! said in words with exact counts on both sides. One click shows those rows
//! or leaves them out. Nothing about the file is assumed: whatever stands out
//! is read from the file itself.

use crate::bridge::{Against, Bridge, Cmd, ContrastLine, ContrastOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum ExplainAction {
    None,
    /// Add this to the search (show only these rows, or leave them out).
    AddToSearch(String),
}

struct Run {
    id: u64,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    started: f64,
    /// The rows being explained, when asked.
    rows: u64,
}

pub struct ExplainWin {
    pub against: Against,
    /// In at least this share of the rows shown (percent).
    pub min_share: f64,
    /// At least this many times more common in them.
    pub min_ratio: f64,
    /// Ask as soon as the window shows.
    pub run_now: bool,
    next_id: u64,
    run: Option<Run>,
    find: String,
    /// The page of lines shown.
    page: usize,
    /// Compared with what came before: showing what is new (else what grew).
    show_new: bool,
    page_new: usize,
}

impl Default for ExplainWin {
    fn default() -> Self {
        ExplainWin {
            against: Against::RestOfFile,
            min_share: 5.0,
            min_ratio: 2.0,
            run_now: true,
            next_id: 0,
            run: None,
            find: String::new(),
            page: 0,
            show_new: true,
            page_new: 0,
        }
    }
}

/// A condition as a search term.
fn term(col: &str, v: &Option<String>) -> String {
    use logseek_core::formula::{column_ref, quote};
    match v {
        Some(v) => format!("{}={}", column_ref(col), quote(v)),
        None => format!("NOT {}:*", column_ref(col)),
    }
}

/// The search terms that keep the rows of a line, or leave them out.
pub fn keep_term(l: &ContrastLine) -> String {
    let t: Vec<String> = l.conds.iter().map(|(c, v)| term(c, v)).collect();
    t.join(" ")
}

pub fn leave_out_term(l: &ContrastLine) -> String {
    format!("NOT ({})", keep_term(l))
}

fn cond_words(c: &str, v: &Option<String>) -> String {
    match v {
        Some(v) => format!("{c} is {v}"),
        None => format!("{c} is empty"),
    }
}

/// A line in words: "App is v2  and  Note is empty".
pub fn words(l: &ContrastLine) -> String {
    l.conds
        .iter()
        .map(|(c, v)| cond_words(c, v))
        .collect::<Vec<_>>()
        .join("  and  ")
}

/// What holds in exactly the same rows as each condition, and what could
/// stand in for one of them with the very same counts, in words (one line
/// each), every one named.
pub fn same_words(l: &ContrastLine) -> Vec<String> {
    let instead = l.instead.as_ref().and_then(|(k, alts)| {
        let (c, v) = l.conds.get(*k)?;
        Some(format!(
            "The same counts with any one of these in place of \"{}\" ({} more): {}",
            cond_words(c, v),
            group(alts.len() as u64),
            alts.iter()
                .map(|(c, v)| cond_words(c, v))
                .collect::<Vec<_>>()
                .join(",  ")
        ))
    });
    l.conds
        .iter()
        .zip(&l.same)
        .filter(|(_, s)| !s.is_empty())
        .map(|((c, v), s)| {
            format!(
                "In exactly the same rows as \"{}\" ({}): {}",
                cond_words(c, v),
                if s.len() == 1 {
                    "1 more".to_string()
                } else {
                    format!("{} more", group(s.len() as u64))
                },
                s.iter()
                    .map(|(c, v)| cond_words(c, v))
                    .collect::<Vec<_>>()
                    .join(",  ")
            )
        })
        .chain(instead)
        .collect()
}

fn pct(a: u64, n: u64) -> String {
    let p = a as f64 * 100.0 / n.max(1) as f64;
    if p >= 10.0 || p == 0.0 {
        format!("{p:.0}%")
    } else if p >= 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.2}%")
    }
}

/// How much more common, in words, for what the rows are compared with: a value
/// never seen before a period is new in it.
pub fn times_vs(r: f64, against: Against) -> String {
    match against {
        Against::Before | Against::JustBefore if r.is_infinite() => {
            "new in this period: never before".into()
        }
        _ => times(r),
    }
}

/// How much more common, in words.
pub fn times(r: f64) -> String {
    if r.is_infinite() {
        "never in the other rows".into()
    } else if r >= 100.0 {
        format!("{} times more common here", group(r.round() as u64))
    } else if r >= 10.0 {
        format!("{r:.0} times more common here")
    } else {
        format!("{r:.1} times more common here")
    }
}

/// The counts of a line in words.
pub fn counts(l: &ContrastLine, here: u64, rest: u64, against: Against) -> String {
    let other = match against {
        Against::RestOfFile => "other rows",
        Against::OtherTimes => "rows at other times",
        Against::Before => "rows before this period",
        Against::JustBefore => "rows just before",
    };
    format!(
        "{} of these {} rows ({})  ·  {} of the {} {} ({})",
        group(l.here),
        group(here),
        pct(l.here, here),
        group(l.rest),
        group(rest),
        other,
        pct(l.rest, rest)
    )
}

/// A column's new values in words: how many, in how many rows, and how
/// varied the column was before (what makes a new value stand out).
pub fn new_col_words(c: &crate::bridge::NewCol) -> String {
    let n = c.values.len() as u64;
    let before = if c.before_filled == 0 {
        "never filled".to_string()
    } else {
        format!(
            "{} different {} in {} rows",
            group(c.before_values),
            if c.before_values == 1 {
                "value"
            } else {
                "values"
            },
            group(c.before_filled)
        )
    };
    format!(
        "{} {} never seen before, in {} of {} rows here \u{b7} before: {before}",
        group(n),
        if n == 1 { "value" } else { "values" },
        group(c.new_rows),
        group(c.filled)
    )
}

/// Every line as a table (tab-separated), for the clipboard.
pub fn table_text(out: &ContrastOut) -> String {
    let mut s = String::from("What\tRows here\tOf\tRows compared\tOf\tTimes more common\n");
    for l in &out.lines {
        let r = l.ratio(out.here_total, out.rest_total);
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            words(l).replace("  and  ", " and "),
            l.here,
            out.here_total,
            l.rest,
            out.rest_total,
            if r.is_infinite() {
                "never elsewhere".to_string()
            } else {
                format!("{r:.2}")
            }
        ));
    }
    s
}

impl ExplainWin {
    /// What is new in the period, column by column (the columns with the
    /// fewest different values before first), every value one click from its rows.
    fn new_view(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        out: &ContrastOut,
        act: &mut ExplainAction,
    ) {
        if out.new.is_empty() {
            ui.label(format!(
                "Nothing in these {} rows is new: every value of every column was seen in the {} rows before.",
                group(out.here_total),
                group(out.rest_total)
            ));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("The columns that had the fewest different values before come first: a new value stands out there.")
                    .color(pal.text2),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.find)
                        .hint_text("Find a column or value")
                        .desired_width(200.0),
                );
            });
        });
        // one list: each column, then its values
        let find = self.find.to_lowercase();
        let mut items: Vec<(usize, Option<usize>)> = Vec::new();
        for (ci, c) in out.new.iter().enumerate() {
            let name_hit = find.is_empty() || c.name.to_lowercase().contains(&find);
            let vals: Vec<usize> = (0..c.values.len())
                .filter(|&k| name_hit || c.values[k].0.to_lowercase().contains(&find))
                .collect();
            if vals.is_empty() {
                continue;
            }
            items.push((ci, None));
            items.extend(vals.into_iter().map(|k| (ci, Some(k))));
        }
        let range = crate::pager::pager(ui, pal, &mut self.page_new, items.len());
        egui::ScrollArea::vertical()
            .id_salt("explain-new")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for &(ci, k) in &items[range] {
                    let c = &out.new[ci];
                    match k {
                        None => {
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(&c.name).strong());
                                ui.label(RichText::new(new_col_words(c)).color(pal.text2));
                            });
                        }
                        Some(k) => {
                            let (v, rows) = &c.values[k];
                            egui::Frame::new()
                                .fill(pal.surface2)
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(10, 3))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(v).font(theme::data_font()));
                                        ui.label(
                                            RichText::new(format!(
                                                "{} {}",
                                                group(*rows),
                                                if *rows == 1 { "row" } else { "rows" }
                                            ))
                                            .color(pal.text2),
                                        );
                                        let t = term(&c.name, &Some(v.clone()));
                                        if ui
                                            .small_button(format!(
                                                "{}  Show only these rows",
                                                ph::FUNNEL
                                            ))
                                            .on_hover_text(t.clone())
                                            .clicked()
                                        {
                                            *act = ExplainAction::AddToSearch(t);
                                        }
                                    });
                                });
                        }
                    }
                }
            });
    }

    fn start(&mut self, bridge: &Bridge, now: f64, rows: u64) {
        if let Some(r) = &self.run {
            r.stop.store(true, Ordering::SeqCst);
        }
        self.next_id += 1;
        let run = Run {
            id: self.next_id,
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            started: now,
            rows,
        };
        bridge.send(Cmd::Contrast {
            id: run.id,
            against: self.against,
            min_share: self.min_share / 100.0,
            min_ratio: self.min_ratio,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.run = Some(run);
        self.page = 0;
        self.page_new = 0;
    }

    /// `period`: the search keeps a period of time, so "the same search at
    /// other times" can be offered.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
        period: bool,
    ) -> (bool, ExplainAction) {
        let mut open = true;
        let mut act = ExplainAction::None;
        let now = ctx.input(|i| i.time);
        if !period && self.against != Against::RestOfFile {
            self.against = Against::RestOfFile;
        }
        if std::mem::take(&mut self.run_now) && snap.matched > 0 {
            self.start(bridge, now, snap.matched);
        }
        let out: Option<Arc<ContrastOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.contrast.clone().filter(|o| o.id == r.id));
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("what-sets-apart"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("What sets these rows apart?")
                            .font(crate::fonts::strong(16.0)),
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
                    RichText::new(if matches!(self.against, Against::Before | Against::JustBefore) {
                        "What is new in the rows shown, never seen before them, and what is far more common than before. Read from your file; counted exactly."
                    } else {
                        "The values, and pairs of values, that are far more common in the rows shown than in the rows they are compared with. Read from your file; counted exactly."
                    })
                    .color(pal.text2),
                );
                ui.add_space(4.0);
                let running = self.run.is_some() && out.is_none();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("The {} rows shown, compared with", group(snap.matched)));
                    let label = |a: Against| match a {
                        Against::RestOfFile => "every other row of the file",
                        Against::OtherTimes => "the same search at other times",
                        Against::Before => "the same search before this period",
                        Against::JustBefore => "the same search just before (same length)",
                    };
                    egui::ComboBox::from_id_salt("explain-against")
                        .selected_text(label(self.against))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.against,
                                Against::RestOfFile,
                                label(Against::RestOfFile),
                            );
                            ui.add_enabled_ui(period, |ui| {
                                for a in [Against::OtherTimes, Against::Before, Against::JustBefore] {
                                    ui.selectable_value(&mut self.against, a, label(a))
                                        .on_disabled_hover_text(
                                            "Keep a period on the timeline first (drag across the bars)",
                                        );
                                }
                            });
                        });
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("In at least");
                    ui.add(
                        egui::DragValue::new(&mut self.min_share)
                            .range(0.01..=100.0)
                            .speed(0.5)
                            .suffix("%"),
                    );
                    ui.label("of these rows, and at least");
                    ui.add(
                        egui::DragValue::new(&mut self.min_ratio)
                            .range(1.01..=1_000_000.0)
                            .speed(0.1)
                            .suffix(" times"),
                    );
                    ui.label("more common in them");
                    if running {
                        if ui.button("Stop").clicked() {
                            if let Some(r) = &self.run {
                                r.stop.store(true, Ordering::SeqCst);
                            }
                        }
                    } else if ui
                        .add_enabled(
                            snap.matched > 0,
                            egui::Button::new(
                                RichText::new(format!("{}  Compare", ph::SCALES))
                                    .color(pal.on_accent),
                            )
                            .fill(pal.accent),
                        )
                        .clicked()
                    {
                        self.start(bridge, now, snap.matched);
                    }
                });
                ui.separator();
                let Some(run) = &self.run else {
                    ui.label(
                        RichText::new("Show some rows (a search, a period, tagged rows) and press Compare.")
                            .color(pal.text2),
                    );
                    return;
                };
                let Some(out) = out.clone() else {
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
                    ui.label(
                        RichText::new(if e == "stopped" {
                            "Stopped before the end: nothing to show.".to_string()
                        } else {
                            format!("{}  The rows could not be compared: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                if run.rows != snap.matched {
                    ui.label(
                        RichText::new(format!(
                            "{}  The rows shown have changed since: press Compare again.",
                            ph::INFO
                        ))
                        .color(pal.text2),
                    );
                }
                if out.rest_total == 0 {
                    ui.label("There are no other rows to compare with.");
                    return;
                }
                // compared with what came before: what is new, or what grew
                if out.new_asked {
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut self.show_new,
                            true,
                            format!(
                                "New in this period ({} {})",
                                group(out.new.len() as u64),
                                if out.new.len() == 1 { "column" } else { "columns" }
                            ),
                        )
                        .on_hover_text("Values seen in these rows and never before them");
                        ui.selectable_value(
                            &mut self.show_new,
                            false,
                            format!("More common than before ({})", group(out.lines.len() as u64)),
                        );
                    });
                    ui.add_space(2.0);
                    if self.show_new {
                        self.new_view(ui, pal, &out, &mut act);
                        return;
                    }
                }
                if out.lines.is_empty() {
                    ui.label(format!(
                        "Nothing sets these {} rows apart: no value, or pair of values, is in at least {} of them and at least {} times more common than in the {} other rows.",
                        group(out.here_total),
                        format!("{}%", self.min_share),
                        self.min_ratio,
                        group(out.rest_total)
                    ));
                    ui.label(
                        RichText::new(format!(
                            "{} columns looked at in {:.1} s.",
                            group(out.columns as u64),
                            out.secs
                        ))
                        .color(pal.text2),
                    );
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} these rows apart",
                            group(out.lines.len() as u64),
                            if out.lines.len() == 1 {
                                "thing sets"
                            } else {
                                "things set"
                            }
                        ))
                        .strong(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "·  strongest first  ·  {} columns, counted exactly in {:.1} s",
                            group(out.columns as u64),
                            out.secs
                        ))
                        .color(pal.text2),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(format!("{}  Copy as table", ph::COPY))
                            .on_hover_text("Every line, tab-separated, for a spreadsheet or a report")
                            .clicked()
                        {
                            crate::clip::copy(ui.ctx(), table_text(&out), "the table");
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.find)
                                .hint_text("Find a column or value")
                                .desired_width(200.0),
                        );
                    });
                });
                let find = self.find.to_lowercase();
                let shown: Vec<usize> = (0..out.lines.len())
                    .filter(|&i| {
                        let l = &out.lines[i];
                        find.is_empty()
                            || words(l).to_lowercase().contains(&find)
                            || same_words(l).join(" ").to_lowercase().contains(&find)
                    })
                    .collect();
                let range = crate::pager::pager(ui, pal, &mut self.page, shown.len());
                egui::ScrollArea::vertical()
                    .id_salt("explain-lines")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, &k) in shown[range].iter().enumerate() {
                            let l = &out.lines[k];
                            let w = words(l);
                            let r = l.ratio(out.here_total, out.rest_total);
                            egui::Frame::new()
                                .fill(if i % 2 == 0 { pal.surface2 } else { pal.surface })
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(10, 4))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(&w)
                                                .font(theme::data_font())
                                                .strong(),
                                        );
                                    });
                                    for s in same_words(l) {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(s).font(theme::data_font()).color(pal.text2),
                                            )
                                            .wrap(),
                                        );
                                    }
                                    // the two shares, the counts and the strength on one
                                    // line (numbers written out); the actions beside them
                                    ui.horizontal_wrapped(|ui| {
                                        for (label, a, n, fill) in [
                                            ("here", l.here, out.here_total, pal.accent),
                                            ("other rows", l.rest, out.rest_total, pal.text2),
                                        ] {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(96.0, 8.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(rect, 3.0, pal.hairline);
                                            let f = (a as f32 / n.max(1) as f32).clamp(0.0, 1.0);
                                            let mut bar = rect;
                                            bar.set_width(
                                                (rect.width() * f).max(if a > 0 { 2.0 } else { 0.0 }),
                                            );
                                            ui.painter().rect_filled(bar, 3.0, fill);
                                            ui.label(
                                                RichText::new(format!("{} {label}", pct(a, n)))
                                                    .color(pal.text2),
                                            );
                                        }
                                        ui.label(RichText::new(times_vs(r, out.against)).color(pal.accent_ink).strong());
                                    });
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(counts(l, out.here_total, out.rest_total, out.against))
                                                .color(pal.text2),
                                        );
                                        if ui
                                            .small_button(format!("{}  Show only these rows", ph::FUNNEL))
                                            .on_hover_text(keep_term(l))
                                            .clicked()
                                        {
                                            act = ExplainAction::AddToSearch(keep_term(l));
                                        }
                                        if ui
                                            .small_button(format!("{}  Leave these out", ph::FUNNEL_X))
                                            .on_hover_text(leave_out_term(l))
                                            .clicked()
                                        {
                                            act = ExplainAction::AddToSearch(leave_out_term(l));
                                        }
                                    });
                                });
                            ui.add_space(2.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(conds: &[(&str, Option<&str>)], here: u64, rest: u64) -> ContrastLine {
        ContrastLine {
            conds: conds
                .iter()
                .map(|(c, v)| (c.to_string(), v.map(|v| v.to_string())))
                .collect(),
            same: vec![Vec::new(); conds.len()],
            instead: None,
            here,
            rest,
        }
    }

    #[test]
    fn a_line_reads_in_words_and_becomes_a_search_that_runs() {
        let l = line(&[("App", Some("v2")), ("Dst Port", None)], 2700, 460);
        assert_eq!(words(&l), "App is v2  and  Dst Port is empty");
        assert_eq!(keep_term(&l), "App=\"v2\" NOT `Dst Port`:*");
        assert_eq!(leave_out_term(&l), "NOT (App=\"v2\" NOT `Dst Port`:*)");
        for q in [keep_term(&l), leave_out_term(&l)] {
            assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        }
        // a value with quotes and spaces stays whole
        let l = line(&[("Cmd", Some("cmd /c \"x y\""))], 1, 0);
        let q = logseek_core::query::parse(&keep_term(&l)).unwrap();
        assert!(format!("{q:?}").contains("cmd /c \\\"x y\\\""), "{q:?}");
    }

    #[test]
    fn what_holds_in_the_same_rows_is_named_in_full() {
        let mut l = line(
            &[("IntegrityLevel", Some("16384")), ("source.ip", None)],
            9,
            0,
        );
        assert!(same_words(&l).is_empty());
        l.same[1] = vec![
            ("network.type".to_string(), None),
            ("agent.id".to_string(), None),
        ];
        assert_eq!(
            same_words(&l),
            vec!["In exactly the same rows as \"source.ip is empty\" (2 more): network.type is empty,  agent.id is empty".to_string()]
        );
        l.instead = Some((1, vec![("ContextTimeStamp".to_string(), None)]));
        assert_eq!(
            same_words(&l)[1],
            "The same counts with any one of these in place of \"source.ip is empty\" (1 more): ContextTimeStamp is empty"
        );
    }

    #[test]
    fn a_period_is_compared_with_what_came_before_it() {
        use crate::bridge::baseline_range;
        let h = 3_600_000_000i64;
        // everything before the period, or the same length just before it
        assert_eq!(
            baseline_range(Against::Before, (10 * h, 11 * h)),
            Some((i64::MIN, 10 * h))
        );
        assert_eq!(
            baseline_range(Against::JustBefore, (10 * h, 11 * h)),
            Some((9 * h, 10 * h))
        );
        assert_eq!(
            baseline_range(Against::JustBefore, (10 * h, 13 * h)),
            Some((7 * h, 10 * h))
        );
        // the other choices are not a time range
        assert_eq!(baseline_range(Against::RestOfFile, (10 * h, 11 * h)), None);
        // a value only in the period reads as new, in words
        let l = line(&[("App", Some("tor.exe"))], 12, 0);
        assert_eq!(
            times_vs(l.ratio(40, 900), Against::Before),
            "new in this period: never before"
        );
        assert_eq!(
            times_vs(l.ratio(40, 900), Against::RestOfFile),
            "never in the other rows"
        );
        assert!(counts(&l, 40, 900, Against::JustBefore)
            .ends_with("0 of the 900 rows just before (0%)"));
        assert!(counts(&l, 40, 900, Against::Before)
            .ends_with("0 of the 900 rows before this period (0%)"));
    }
    #[test]
    fn a_column_with_new_values_is_said_in_full() {
        let c = crate::bridge::NewCol {
            name: "Tactic".into(),
            filled: 72_421,
            new_rows: 24,
            before_values: 6,
            before_filled: 300_120,
            values: vec![("Malware".into(), 21), ("Discovery".into(), 3)],
        };
        assert_eq!(
            new_col_words(&c),
            "2 values never seen before, in 24 of 72,421 rows here \u{b7} before: 6 different values in 300,120 rows"
        );
        let once = crate::bridge::NewCol {
            name: "CommandName".into(),
            filled: 73,
            new_rows: 73,
            before_values: 0,
            before_filled: 0,
            values: vec![("ls".into(), 73)],
        };
        assert_eq!(
            new_col_words(&once),
            "1 value never seen before, in 73 of 73 rows here \u{b7} before: never filled"
        );
    }
    #[test]
    fn counts_and_strength_are_said_in_full() {
        let l = line(&[("App", Some("v2"))], 2700, 460);
        assert_eq!(
            counts(&l, 3000, 23_000, Against::RestOfFile),
            "2,700 of these 3,000 rows (90%)  ·  460 of the 23,000 other rows (2.0%)"
        );
        assert_eq!(times(l.ratio(3000, 23_000)), "45 times more common here");
        assert_eq!(times(f64::INFINITY), "never in the other rows");
        assert_eq!(times(2.345), "2.3 times more common here");
        assert_eq!(times(1234.4), "1,234 times more common here");
    }
}
