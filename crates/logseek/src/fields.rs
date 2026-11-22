//! The Fields panel: every field of the source at a glance, its share of rows,
//! and — one click away — its top values in the current result. Clicking a
//! value narrows the search to it; Alt+click excludes it.

use crate::bridge::{Bridge, Cmd, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{
    self, Align2, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke, StrokeKind,
};
use egui_phosphor::regular as ph;
use logseek_core::types::ColType;

#[derive(Default)]
pub struct FieldsState {
    pub filter: String,
    pub open: Option<usize>,
    /// Stacking: show the rarest values instead of the most frequent.
    pub rare: bool,
    requested: Option<(usize, u64, bool)>,
    /// Row height per field name at a panel width, so rows off screen are
    /// skipped (not laid out or drawn) without the list jumping.
    heights: std::collections::HashMap<String, f32>,
    heights_width: f32,
}

/// A filter the user picked from the panel, to merge into the query.
pub enum Pick {
    Only(String, String),
    Exclude(String, String),
    /// Only the rows where a made column could not be worked out, or none of them.
    Errors(String),
    NoErrors(String),
    /// Not a filter: open the column's every value, paged (most or rarest first).
    EveryValue {
        col: usize,
        rare: bool,
    },
}

/// A part of the Fields list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// The columns in the grid, in the grid's order.
    Shown,
    /// Not in the grid, most telling first.
    Informative,
    Id,
    Constant,
    /// Fullest first.
    Sparse,
    Empty,
    /// Made columns not in the grid.
    Made,
    /// Before the columns are profiled: every field, time first, fullest next.
    All,
}

impl Section {
    pub fn words(self) -> &'static str {
        match self {
            Section::Shown => "In the grid",
            Section::Informative => "Informative",
            Section::Id => "Ids: different on most rows",
            Section::Constant => "Same on every row",
            Section::Sparse => "Sparse: under 5% of rows",
            Section::Empty => "Empty",
            Section::Made => "Made by you",
            Section::All => "",
        }
    }
}

/// The Fields list in sections: what the grid shows (in its order), then the
/// rest by what each column tells, judged from its own values. Every field is
/// in exactly one section; empty sections are left out.
pub fn sections(
    names: &[String],
    order: &[usize],
    hidden: &std::collections::BTreeSet<String>,
    profiles: Option<&[logseek_core::query::roles::ColumnProfile]>,
    counts: &[u64],
    time_col: Option<usize>,
) -> Vec<(Section, Vec<usize>)> {
    use logseek_core::query::roles::Role;
    let n = names.len();
    let count = |c: usize| counts.get(c).copied().unwrap_or(0);
    let Some(prof) = profiles else {
        let mut all: Vec<usize> = (0..n).collect();
        all.sort_by(|&a, &b| {
            (time_col == Some(b))
                .cmp(&(time_col == Some(a)))
                .then(count(b).cmp(&count(a)))
                .then(a.cmp(&b))
        });
        return vec![(Section::All, all)];
    };
    let mut placed = vec![false; n];
    let mut shown = Vec::new();
    for c in order.iter().copied().chain(0..n) {
        if c < n && !placed[c] && !hidden.contains(&names[c]) {
            placed[c] = true;
            shown.push(c);
        }
    }
    let mut out = vec![(Section::Shown, shown)];
    let rest: Vec<usize> = (0..n).filter(|&c| !placed[c]).collect();
    let of = |s: Section| -> Vec<usize> {
        rest.iter()
            .copied()
            .filter(|&c| {
                let r = prof.get(c).map(|p| p.role);
                match s {
                    Section::Informative => matches!(r, Some(Role::Informative | Role::Anchor)),
                    Section::Id => r == Some(Role::Id),
                    Section::Constant => r == Some(Role::Constant),
                    Section::Sparse => r == Some(Role::Sparse),
                    Section::Empty => r == Some(Role::Empty),
                    Section::Made => r.is_none(),
                    _ => false,
                }
            })
            .collect()
    };
    let mut info = of(Section::Informative);
    info.sort_by(|&a, &b| {
        let s = |c: usize| prof.get(c).map_or(0.0, |p| p.score);
        s(b).total_cmp(&s(a)).then(a.cmp(&b))
    });
    let mut sparse = of(Section::Sparse);
    sparse.sort_by(|&a, &b| count(b).cmp(&count(a)).then(a.cmp(&b)));
    out.push((Section::Informative, info));
    out.push((Section::Id, of(Section::Id)));
    out.push((Section::Constant, of(Section::Constant)));
    out.push((Section::Sparse, sparse));
    out.push((Section::Empty, of(Section::Empty)));
    out.push((Section::Made, of(Section::Made)));
    out.retain(|s| !s.1.is_empty());
    out
}

fn glyph(t: Option<&ColType>) -> &'static str {
    match t {
        Some(ColType::Int) | Some(ColType::Float) => ph::HASH,
        Some(ColType::Time) => ph::CLOCK,
        Some(ColType::Bool) => ph::TOGGLE_LEFT,
        _ => ph::TEXT_AA,
    }
}

pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    snap: &Snapshot,
    st: &mut FieldsState,
    bridge: &Bridge,
    grid_order: &[usize],
    hidden: &mut std::collections::BTreeSet<String>,
) -> Option<Pick> {
    let mut pick = None;
    let n = snap.columns.len();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Fields")
                .font(crate::fonts::strong(13.5))
                .color(pal.text),
        );
        ui.label(RichText::new(group(n as u64)).color(pal.muted));
    });
    ui.add_space(4.0);
    let fid = ui.id().with("field-filter");
    let edit = egui::TextEdit::singleline(&mut st.filter)
        .id(fid)
        .hint_text(
            RichText::new(format!("{}  Find a field", ph::MAGNIFYING_GLASS)).color(pal.muted),
        )
        .desired_width(f32::INFINITY);
    ui.add(edit);
    ui.add_space(6.0);

    // what the grid shows first (in its order), then the rest by what each tells
    let total = snap.total_rows.max(1);
    let needle = st.filter.to_lowercase();
    let fits = |c: usize| needle.is_empty() || snap.columns[c].to_lowercase().contains(&needle);
    let mut entries: Vec<(Option<(Section, usize)>, usize)> = Vec::new();
    for (sec, cols) in sections(
        &snap.columns,
        grid_order,
        hidden,
        snap.col_profile.as_deref().map(|v| v.as_slice()),
        &snap.col_counts,
        snap.time_col,
    ) {
        let kept: Vec<usize> = cols.into_iter().filter(|&c| fits(c)).collect();
        if kept.is_empty() {
            continue;
        }
        if sec != Section::All {
            entries.push((Some((sec, kept.len())), 0));
        }
        entries.extend(kept.into_iter().map(|c| (None, c)));
    }
    if entries.is_empty() && !needle.is_empty() {
        ui.label(RichText::new("No field has those letters.").color(pal.muted));
    }
    let summary = snap
        .field
        .as_ref()
        .filter(|f| f.generation == snap.generation && f.rare == st.rare);
    // keep the open field's numbers in step with the current result
    if let Some(c) = st.open {
        if summary.is_none_or(|f| f.col != c) && st.requested != Some((c, snap.generation, st.rare))
        {
            st.requested = Some((c, snap.generation, st.rare));
            bridge.send(Cmd::FieldSummary {
                col: c,
                rare: st.rare,
            });
        }
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for &(head, c) in &entries {
                if let Some((sec, k)) = head {
                    // a section's heading, and how many fields it holds
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(sec.words())
                                .font(theme::small_font())
                                .color(pal.text2),
                        );
                        ui.label(
                            RichText::new(group(k as u64))
                                .font(theme::small_font())
                                .color(pal.muted),
                        );
                    });
                    ui.add_space(2.0);
                    continue;
                }
                let name = &snap.columns[c];
                let share = snap.col_counts.get(c).copied().unwrap_or(0) as f64 / total as f64;
                let is_open = st.open == Some(c);
                let w = ui.available_width();
                if (st.heights_width - w).abs() > 0.5 {
                    st.heights.clear();
                    st.heights_width = w;
                }
                // a closed row that is off screen: keep its space, skip the work
                if !is_open {
                    // not yet measured: one line until it scrolls into view
                    let h = st.heights.get(name).copied().unwrap_or(26.0);
                    let probe = Rect::from_min_size(ui.cursor().min, egui::vec2(w, h));
                    if !ui.is_rect_visible(probe) {
                        ui.allocate_space(egui::vec2(w, h));
                        continue;
                    }
                }
                let name_col = if hidden.contains(name) { pal.muted } else { pal.text };
                let name_g = ui.painter().layout(name.to_string(), theme::ui_font(), name_col, (w - 42.0 - 70.0).max(40.0));
                let row_h = (name_g.size().y + 10.0).max(26.0);
                st.heights.insert(name.clone(), row_h);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, row_h), Sense::click());
                let p = ui.painter();
                if is_open {
                    p.rect_filled(rect, CornerRadius::same(4), pal.selection);
                } else if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::same(4), pal.surface2);
                }
                if resp.has_focus() {
                    p.rect_stroke(
                        rect,
                        CornerRadius::same(4),
                        Stroke::new(2.0, pal.focus),
                        StrokeKind::Inside,
                    );
                }
                let caret = if is_open {
                    ph::CARET_DOWN
                } else {
                    ph::CARET_RIGHT
                };
                p.text(
                    egui::pos2(rect.left() + 8.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    caret,
                    FontId::new(12.0, FontFamily::Proportional),
                    pal.muted,
                );
                p.text(
                    egui::pos2(rect.left() + 24.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    if c >= snap.columns.len().saturating_sub(snap.virtual_cols.len()) { ph::FUNCTION } else { glyph(snap.col_types.get(c)) },
                    FontId::new(13.0, FontFamily::Proportional),
                    pal.text2,
                );
                let is_virtual = c >= snap.columns.len().saturating_sub(snap.virtual_cols.len());
                // a made column: said so (what it is made from is on the header's hover)
                let pct = if is_virtual { "made".to_string() } else { pct_label(share) };
                let pct_w = 64.0;
                let is_hidden = hidden.contains(name);
                let name_rect = Rect::from_min_max(
                    egui::pos2(rect.left() + 42.0, rect.top()),
                    egui::pos2(rect.right() - pct_w - 6.0, rect.bottom()),
                );
                let _ = is_hidden;
                p.galley(egui::pos2(name_rect.left(), rect.center().y - name_g.size().y * 0.5), name_g, name_col);
                // show / hide in the grid (the column chooser lives here)
                let eye = Rect::from_center_size(egui::pos2(rect.right() - pct_w + 4.0, rect.center().y), egui::vec2(24.0, 24.0));
                let er = ui.interact(eye, ui.id().with(("eye", c)), Sense::click());
                if er.has_focus() {
                    ui.painter().rect_stroke(eye, CornerRadius::same(3), Stroke::new(2.0, pal.focus), StrokeKind::Inside);
                }
                if is_hidden || resp.hovered() || er.hovered() || er.has_focus() {
                    let g = if is_hidden { ph::EYE_SLASH } else { ph::EYE };
                    ui.painter().text(eye.center(), Align2::CENTER_CENTER, g, FontId::new(13.0, FontFamily::Proportional), if is_hidden { pal.accent_ink } else { pal.text2 });
                }
                if er.on_hover_text(if is_hidden { "Show this column in the grid" } else { "Hide this column from the grid (still searchable)" }).clicked() {
                    if is_hidden {
                        hidden.remove(name);
                    } else {
                        hidden.insert(name.clone());
                    }
                }
                p.text(
                    egui::pos2(rect.right() - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    pct,
                    theme::small_font(),
                    pal.muted,
                );
                let resp = resp.on_hover_text(format!(
                    "{name}\n{} of {} rows have a value",
                    group(snap.col_counts.get(c).copied().unwrap_or(0)),
                    group(snap.total_rows)
                ));
                if resp.clicked() {
                    st.open = if is_open { None } else { Some(c) };
                }
                if is_open {
                    ui.add_space(2.0);
                    match summary.filter(|f| f.col == c) {
                        None => {
                            ui.label(RichText::new("   Counting values").color(pal.muted));
                        }
                        Some(f) => {
                            let cover = if f.matched > 0 {
                                f.with_value as f64 / f.matched as f64 * 100.0
                            } else {
                                0.0
                            };
                            ui.horizontal(|ui| {
                                ui.add_space(14.0);
                                for (rare, label, tip) in [(false, "Most", "Most frequent values first"), (true, "Rarest", "Rarest values first: one-off values are often the interesting ones")] {
                                    let on = st.rare == rare;
                                    let b = egui::Button::new(RichText::new(label).font(theme::small_font()).color(if on { pal.text } else { pal.text2 }))
                                        .fill(if on { pal.selection } else { pal.surface })
                                        .stroke(if on { Stroke::new(1.0, pal.accent_ink) } else { Stroke::new(1.0, pal.hairline) })
                                        .min_size(egui::vec2(0.0, 22.0));
                                    if ui.add(b).on_hover_text(tip).clicked() {
                                        st.rare = rare;
                                    }
                                }
                            });
                            ui.label(
                                RichText::new(format!(
                                    "   {} distinct · in {:.0}% of {} rows",
                                    group(f.distinct),
                                    cover,
                                    group(f.matched)
                                ))
                                .font(theme::small_font())
                                .color(pal.text2),
                            );
                            ui.add_space(2.0);
                            for (val, cnt) in &f.top {
                                let frac = *cnt as f32 / f.matched.max(1) as f32;
                                let col = if val.is_empty() { pal.muted } else { pal.text };
                                let shown = if val.is_empty() { "(no value)".to_string() } else { val.clone() };
                                // the value on one line beside its count when it fits; else the
                                // panel's whole width (never broken mid-word to make room for
                                // the count), the count on a line of its own under it
                                let count_words = format!("{}  {}", group(*cnt), pct_label(frac as f64));
                                let cw = ui.painter().layout_no_wrap(count_words.clone(), theme::small_font(), pal.text2).size().x;
                                let one = ui.painter().layout_no_wrap(shown.clone(), theme::data_font(), col);
                                let beside = one.size().x <= w - 24.0 - cw - 16.0;
                                let vg = if beside { one } else { ui.painter().layout(shown, theme::data_font(), col, (w - 32.0).max(40.0)) };
                                let row_h = if beside { (vg.size().y + 10.0).max(24.0) } else { vg.size().y + 26.0 };
                                let (r, vr) = ui.allocate_exact_size(
                                    egui::vec2(w, row_h),
                                    if val.is_empty() {
                                        Sense::hover()
                                    } else {
                                        Sense::click()
                                    },
                                );
                                let p = ui.painter();
                                if vr.hovered() && !val.is_empty() {
                                    p.rect_filled(r, CornerRadius::same(3), pal.surface2);
                                }
                                // share bar under the value, never colour alone: the % is printed too
                                let track = Rect::from_min_size(
                                    egui::pos2(r.left() + 24.0, r.bottom() - 3.0),
                                    egui::vec2(r.width() - 32.0, 2.0),
                                );
                                p.rect_filled(track, CornerRadius::same(1), pal.hairline);
                                let mut bar = track;
                                bar.set_width(track.width() * frac.clamp(0.0, 1.0));
                                p.rect_filled(
                                    bar,
                                    CornerRadius::same(1),
                                    pal.info.gamma_multiply(0.75),
                                );
                                p.galley(egui::pos2(r.left() + 24.0, r.top() + 4.0), vg, col);
                                p.text(
                                    egui::pos2(r.right() - 8.0, if beside { r.top() + 12.0 } else { r.bottom() - 12.0 }),
                                    Align2::RIGHT_CENTER,
                                    count_words,
                                    theme::small_font(),
                                    pal.text2,
                                );
                                if !val.is_empty() {
                                    let vr = vr.on_hover_text(format!(
                                        "{val}\nClick: show only this · Alt+click: exclude it"
                                    ));
                                    if vr.clicked() {
                                        let alt = ui.input(|i| i.modifiers.alt);
                                        pick = Some(if alt {
                                            Pick::Exclude(name.clone(), val.clone())
                                        } else {
                                            Pick::Only(name.clone(), val.clone())
                                        });
                                    }
                                }
                            }
                            if f.errors > 0 {
                                // neither a value nor empty: its own line, not a filter
                                let frac = f.errors as f64 / f.matched.max(1) as f64;
                                let (r, vr) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
                                let p = ui.painter();
                                if vr.hovered() {
                                    p.rect_filled(r, CornerRadius::same(3), pal.surface2);
                                }
                                p.text(
                                    egui::pos2(r.left() + 24.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    format!("{}  (could not be worked out)", egui_phosphor::regular::WARNING),
                                    theme::data_font(),
                                    pal.warning,
                                );
                                p.text(
                                    egui::pos2(r.right() - 8.0, r.center().y),
                                    Align2::RIGHT_CENTER,
                                    format!("{}  {}", group(f.errors), pct_label(frac)),
                                    theme::small_font(),
                                    pal.text2,
                                );
                                let vr = vr.on_hover_text("Rows where this made column could not be worked out; the grid says why on each cell. Not counted as values or as empty.\nClick: show only these \u{b7} Alt+click: exclude them");
                                if vr.clicked() {
                                    let alt = ui.input(|i| i.modifiers.alt);
                                    pick = Some(if alt {
                                        Pick::NoErrors(name.clone())
                                    } else {
                                        Pick::Errors(name.clone())
                                    });
                                }
                            }
                            let shown = f.top.iter().filter(|(v, _)| !v.is_empty()).count();
                            if let Some(words) = every_value_words(shown, f.distinct) {
                                ui.horizontal(|ui| {
                                    ui.add_space(18.0);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(format!("{}  {words}", ph::LIST_BULLETS))
                                                    .font(theme::small_font())
                                                    .color(pal.accent_ink),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text(format!(
                                            "Search, tick and page through every value of {name}, counted exactly ({})",
                                            if f.rare { "rarest first" } else { "most rows first" }
                                        ))
                                        .clicked()
                                    {
                                        pick = Some(Pick::EveryValue { col: c, rare: f.rare });
                                    }
                                });
                            }
                            ui.add_space(6.0);
                        }
                    }
                }
            }
        });
    pick
}

/// 1 → "100%", 0.004 → "<1%", 0 → "—".
fn pct_label(f: f64) -> String {
    if f >= 0.995 {
        "100%".into()
    } else if f >= 0.01 {
        format!("{:.0}%", f * 100.0)
    } else if f > 0.0 {
        "<1%".into()
    } else {
        "—".into()
    }
}

/// `field="value"` / `field!="value"` in the query language, with safe quoting.
/// A pick from the Fields panel or the inspector becomes the column's filter
/// (a sentence chip), never text in the search box: Only keeps that value,
/// Exclude adds it to the values left out, Errors keeps the rows a made column
/// could not work out.
pub fn apply(p: &Pick, filters: &mut crate::filters::Filters) {
    use crate::filters::ColFilter;
    use std::collections::BTreeSet;
    let fresh = |f: &mut ColFilter| {
        f.enabled = true;
        f.invert = false;
    };
    match p {
        Pick::Only(col, v) => {
            let f = filters.entry(col.clone()).or_default();
            fresh(f);
            f.values = Some(BTreeSet::from([v.clone()]));
            f.values_except = false;
        }
        Pick::Exclude(col, v) => {
            let f = filters.entry(col.clone()).or_default();
            fresh(f);
            match (&mut f.values, f.values_except) {
                // already leaving values out: one more
                (Some(set), true) => {
                    set.insert(v.clone());
                }
                // a list of values kept: this one is no longer among them
                (Some(set), false) => {
                    set.remove(v);
                }
                (None, _) => {
                    f.values = Some(BTreeSet::from([v.clone()]));
                    f.values_except = true;
                }
            }
        }
        Pick::Errors(col) | Pick::NoErrors(col) => {
            let f = filters.entry(col.clone()).or_default();
            fresh(f);
            f.text = if matches!(p, Pick::Errors(_)) {
                crate::summary::ERROR_WORD.to_string()
            } else {
                format!("NOT {}", crate::summary::ERROR_WORD)
            };
        }
        // opens a list; filters nothing
        Pick::EveryValue { .. } => {}
    }
}

/// The way from the few values the panel shows to every one of them, with the
/// exact count; none when every value is already shown.
pub fn every_value_words(shown: usize, distinct: u64) -> Option<String> {
    (distinct > shown as u64).then(|| format!("Every value: {} in all", group(distinct)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseek_core::query::roles::{ColumnProfile, Role};
    use std::collections::BTreeSet;
    #[test]
    fn a_pick_becomes_the_column_filter_not_search_text() {
        use crate::filters::{compose, describe, Filters};
        let mut f = Filters::new();
        apply(&Pick::Only("host".into(), "host-a01".into()), &mut f);
        assert_eq!(describe("host", &f["host"]), "host is host-a01");
        // the engine still gets an exact query, and the search box is untouched
        let q = compose("", &f, None);
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        // Exclude on a fresh column: every value but this one
        apply(&Pick::Exclude("geo src".into(), "US".into()), &mut f);
        assert!(f["geo src"].values_except);
        apply(&Pick::Exclude("geo src".into(), "CN".into()), &mut f);
        assert_eq!(f["geo src"].values.as_ref().unwrap().len(), 2);
        // Exclude from a list of kept values takes it off that list
        apply(&Pick::Only("level".into(), "warn".into()), &mut f);
        f.get_mut("level")
            .unwrap()
            .values
            .as_mut()
            .unwrap()
            .insert("error".into());
        apply(&Pick::Exclude("level".into(), "warn".into()), &mut f);
        assert_eq!(
            f["level"]
                .values
                .as_ref()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            ["error"]
        );
        // a quote inside a value survives into the query
        apply(
            &Pick::Only("event.CommandLine".into(), "cmd /c \"x\"".into()),
            &mut f,
        );
        let q = compose("", &f, None);
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
    }

    fn prof(col: usize, role: Role, score: f64) -> ColumnProfile {
        ColumnProfile {
            col,
            role,
            filled: 0,
            rows: 0,
            distinct_sampled: 0,
            sampled: 0,
            median_len: 0,
            constant: None,
            score,
            same_as: None,
            top: Vec::new(),
        }
    }

    #[test]
    fn fields_come_in_sections_shown_first_then_by_what_they_tell() {
        let names: Vec<String> = [
            "time", "a", "b", "ver", "id", "rare1", "rare2", "none", "c", "made",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = vec![
            prof(0, Role::Anchor, 0.0),
            prof(1, Role::Informative, 0.2),
            prof(2, Role::Informative, 0.9),
            prof(3, Role::Constant, 0.0),
            prof(4, Role::Id, 0.0),
            prof(5, Role::Sparse, 0.0),
            prof(6, Role::Sparse, 0.0),
            prof(7, Role::Empty, 0.0),
            prof(8, Role::Informative, 0.5),
        ];
        let counts = [10, 9, 9, 10, 10, 1, 3, 0, 9, 5];
        // the grid shows time, c, a (in that order); "made" is hidden
        let order = vec![0, 8, 1, 2, 3, 4, 5, 6, 7, 9];
        let hidden: BTreeSet<String> = ["b", "ver", "id", "rare1", "rare2", "none", "made"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let s = sections(&names, &order, &hidden, Some(&p), &counts, Some(0));
        assert_eq!(
            s,
            vec![
                (Section::Shown, vec![0, 8, 1]),
                (Section::Informative, vec![2]),
                (Section::Id, vec![4]),
                (Section::Constant, vec![3]),
                (Section::Sparse, vec![6, 5]),
                (Section::Empty, vec![7]),
                (Section::Made, vec![9]),
            ]
        );
        // every field is in exactly one section
        let all: usize = s.iter().map(|x| x.1.len()).sum();
        assert_eq!(all, names.len());
        // before the profiles are worked out: one list, time first, then the fullest
        let s = sections(&names, &order, &hidden, None, &counts, Some(0));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, Section::All);
        assert_eq!(&s[0].1[..4], &[0, 3, 4, 1]);
        assert_eq!(s[0].1.len(), names.len());
    }

    #[test]
    fn asking_for_every_value_changes_no_filter_and_says_how_many() {
        let mut f = crate::filters::Filters::new();
        apply(&Pick::EveryValue { col: 2, rare: true }, &mut f);
        assert!(f.is_empty());
        // more values than the panel shows: the way to every one, with the exact count
        assert_eq!(
            every_value_words(10, 1_204),
            Some("Every value: 1,204 in all".to_string())
        );
        // all already shown: nothing to open
        assert_eq!(every_value_words(7, 7), None);
    }

    #[test]
    fn labels() {
        assert_eq!(pct_label(0.004), "<1%");
        assert_eq!(pct_label(0.996), "100%");
        assert_eq!(pct_label(0.0), "—");
    }
}
