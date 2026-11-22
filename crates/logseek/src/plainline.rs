//! Plain words under the search box. While the search is typed in ordinary
//! words, a slim line says how this file reads them: "order_status is
//! cancelled · billing_city is Lisbon · grand_total is more than 15,000 ·
//! 412 rows", each part a chip (a word that fits two columns offers the other
//! reading), with "Use (Alt+Enter)". The search box itself is never changed
//! until the reading is used; a search already written in the search syntax
//! is left alone.

use crate::bridge::{Bridge, Cmd, Snapshot};
use crate::moment::group;
use crate::theme::Palette;
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::plain::{recast, Recast, Summary};

pub enum PlainAction {
    None,
    /// Put this search in the box (and open this summary, when asked for).
    Use(String, Option<Summary>),
}

#[derive(Default)]
pub struct PlainLine {
    text: String,
    recast: Option<Recast>,
    /// The reading chosen for each part (0: the likelier).
    picks: Vec<usize>,
    asked: Option<String>,
    /// Hidden for this text.
    dismissed: Option<String>,
}

/// Is the search already written in the search syntax (then it is not read
/// as plain words)?
pub fn written_as_syntax(q: &str) -> bool {
    q.contains(['=', '~', '@', '<', '>', '`', '(', '|'])
        || q.split_whitespace()
            .any(|w| w.contains(':') || w == "AND" || w == "OR" || w == "NOT")
}

/// The search the chosen readings make.
pub fn assembled(r: &Recast, picks: &[usize]) -> String {
    let term = |i: usize| -> &str {
        let p = &r.parts[i];
        match picks.get(i).copied().unwrap_or(0) {
            0 => p.term.as_str(),
            k => p
                .others
                .get(k - 1)
                .map_or(p.term.as_str(), |c| c.term.as_str()),
        }
    };
    let mut t: Vec<&str> = (0..r.parts.len())
        .filter(|&i| r.parts[i].time)
        .map(term)
        .collect();
    t.extend((0..r.parts.len()).filter(|&i| !r.parts[i].time).map(term));
    t.join(" ")
}

/// A summary in words.
pub fn summary_words(s: &Summary) -> String {
    let what = match (s.measure.as_str(), &s.column) {
        ("rows", _) | (_, None) => "rows".to_string(),
        ("total", Some(c)) => format!("total {c}"),
        ("average", Some(c)) => format!("average {c}"),
        ("different", Some(c)) => format!("different {c}"),
        ("smallest", Some(c)) => format!("smallest {c}"),
        ("largest", Some(c)) => format!("largest {c}"),
        (_, Some(c)) => c.clone(),
    };
    let mut w = format!("Summarise: {what}");
    if !s.by.is_empty() {
        w.push_str(&format!(" by {}", s.by.join(" and ")));
    }
    if let Some(p) = s.per {
        w.push_str(match p {
            60_000_000 => " per minute",
            3_600_000_000 => " per hour",
            86_400_000_000 => " per day",
            _ => " per step of time",
        });
    }
    w
}

impl PlainLine {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
        query: &str,
    ) -> PlainAction {
        let q = query.trim();
        let Some(lex) = snap.lexicon.as_ref() else {
            return PlainAction::None;
        };
        if q.is_empty() || written_as_syntax(q) || self.dismissed.as_deref() == Some(q) {
            return PlainAction::None;
        }
        if self.text != q {
            self.text = q.to_string();
            let r = recast(q, lex);
            self.picks = vec![0; r.parts.len()];
            self.recast = Some(r);
            self.asked = None;
        }
        let Some(r) = self.recast.clone() else {
            return PlainAction::None;
        };
        if !r.understood() {
            return PlainAction::None;
        }
        let search = assembled(&r, &self.picks);
        if search == q && r.summary.is_none() {
            return PlainAction::None;
        }
        // how many rows the reading gives
        if self.asked.as_deref() != Some(&search) {
            bridge.send(Cmd::Count(search.clone()));
            self.asked = Some(search.clone());
        }
        let count = snap
            .preview
            .as_ref()
            .filter(|(s, _)| *s == search)
            .map(|(_, n)| *n);
        let mut act = PlainAction::None;
        egui::Frame::new()
            .fill(pal.surface2)
            .inner_margin(egui::Margin::symmetric(12, 4))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(ph::CHAT_TEXT).color(pal.accent_ink))
                        .on_hover_text(
                            "Your words, read against this file's own columns and values",
                        );
                    ui.label(RichText::new("In this file's words:").color(pal.text2));
                    for (i, p) in r.parts.iter().enumerate() {
                        let pick = self.picks.get(i).copied().unwrap_or(0);
                        let shown = match pick {
                            0 => p.words.clone(),
                            k => p
                                .others
                                .get(k - 1)
                                .map_or(p.words.clone(), |c| c.words.clone()),
                        };
                        let text =
                            RichText::new(shown).color(if p.free { pal.text2 } else { pal.text });
                        if p.others.is_empty() {
                            ui.add(
                                egui::Button::new(text)
                                    .small()
                                    .frame(true)
                                    .sense(egui::Sense::hover()),
                            )
                            .on_hover_text(if p.free {
                                "Not a column or value of this file: looked for as text"
                            } else {
                                "Read from this file"
                            });
                        } else {
                            ui.menu_button(
                                RichText::new(format!(
                                    "{}  {}",
                                    match pick {
                                        0 => p.words.clone(),
                                        k => p.others[k - 1].words.clone(),
                                    },
                                    ph::CARET_DOWN
                                )),
                                |ui| {
                                    ui.label(
                                        RichText::new("This word fits more than one column:")
                                            .color(pal.text2),
                                    );
                                    let all: Vec<String> = std::iter::once(p.words.clone())
                                        .chain(p.others.iter().map(|c| c.words.clone()))
                                        .collect();
                                    for (k, w) in all.iter().enumerate() {
                                        if ui.selectable_label(pick == k, w).clicked() {
                                            if let Some(x) = self.picks.get_mut(i) {
                                                *x = k;
                                            }
                                            ui.close();
                                        }
                                    }
                                },
                            );
                        }
                    }
                    if let Some(s) = &r.summary {
                        ui.add(
                            egui::Button::new(RichText::new(summary_words(s)))
                                .small()
                                .sense(egui::Sense::hover()),
                        );
                    }
                    ui.label(
                        RichText::new(match count {
                            Some(n) => format!("{} rows", group(n)),
                            None => "counting".to_string(),
                        })
                        .color(pal.text2),
                    );
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Use  (Alt+Enter)").color(pal.on_accent),
                            )
                            .fill(pal.accent)
                            .small(),
                        )
                        .on_hover_text(search.clone())
                        .clicked()
                    {
                        act = PlainAction::Use(search.clone(), r.summary.clone());
                    }
                    if ui
                        .add(egui::Button::new(ph::X).frame(false).small())
                        .on_hover_text("Keep my words as they are")
                        .clicked()
                    {
                        self.dismissed = Some(q.to_string());
                    }
                });
            });
        if ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::Enter)) {
            act = PlainAction::Use(search, r.summary.clone());
        }
        act
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_is_left_alone_and_summaries_are_said() {
        assert!(written_as_syntax("EventID=4625"));
        assert!(written_as_syntax("host:web*"));
        assert!(written_as_syntax("@2026-11-01..2026-11-02 x"));
        assert!(written_as_syntax("a OR b"));
        assert!(!written_as_syntax("cancelled orders from hyderabad"));
        let s = Summary {
            by: vec!["billing_city".into()],
            measure: "average".into(),
            column: Some("grand_total".into()),
            per: Some(3_600_000_000),
        };
        assert_eq!(
            summary_words(&s),
            "Summarise: average grand_total by billing_city per hour"
        );
    }
}
