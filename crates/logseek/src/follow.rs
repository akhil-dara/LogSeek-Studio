//! The Follow sheet: from tagged rows, the selected row or the current
//! result, find the rows that link to them — one column equals another,
//! optionally near in time, optionally repeated into a tree.

use crate::bridge::{FollowStart, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::follow::{FollowSpec, Window};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    Tag(usize),
    Selected,
    Result,
}

pub struct FollowState {
    pub start: StartKind,
    pub from: String,
    pub to: String,
    /// A second column that may equal it instead (either is enough).
    pub also: Option<String>,
    /// 0 any time, 1 after, 2 before, 3 around.
    pub when: u8,
    pub amount: String,
    /// 0 seconds, 1 minutes, 2 hours.
    pub unit: u8,
    pub repeat: bool,
    /// The run whose answer is wanted.
    pub run: u64,
    /// Which tag "Tag all found" gives.
    pub tag_to: usize,
    /// The link the values show, when the file has one.
    pub suggested: Option<Suggested>,
    /// The name typed for saving this Follow.
    pub save_name: String,
}

/// A Follow saved by name: the link and the time limit, not where it starts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Recipe {
    pub name: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub also: Option<String>,
    pub when: u8,
    pub amount: String,
    pub unit: u8,
    pub repeat: bool,
}

impl Recipe {
    pub fn from_state(name: &str, st: &FollowState) -> Recipe {
        Recipe {
            name: name.trim().to_string(),
            from: st.from.clone(),
            to: st.to.clone(),
            also: st.also.clone(),
            when: st.when,
            amount: st.amount.clone(),
            unit: st.unit,
            repeat: st.repeat,
        }
    }

    /// Usable on a file that has both columns.
    pub fn fits(&self, columns: &[String]) -> bool {
        columns.contains(&self.from)
            && columns.contains(&self.to)
            && self.also.as_ref().is_none_or(|a| columns.contains(a))
    }

    pub fn apply(&self, st: &mut FollowState) {
        st.from = self.from.clone();
        st.to = self.to.clone();
        st.also = self.also.clone();
        st.when = self.when;
        st.amount = self.amount.clone();
        st.unit = self.unit;
        st.repeat = self.repeat;
    }
}

/// Keep `r` in `list` (replacing one of the same name, any case); an empty
/// name saves nothing.
pub fn save_recipe(list: &mut Vec<Recipe>, r: Recipe) {
    if r.name.is_empty() {
        return;
    }
    list.retain(|x| x.name.to_lowercase() != r.name.to_lowercase());
    list.push(r);
    list.sort_by_key(|x| x.name.to_lowercase());
}

pub enum FollowAction {
    None,
    Run(FollowStart, FollowSpec),
    /// Show the starting rows and everything found as the grid's rows.
    ShowRows(Arc<Vec<u64>>),
    ShowAll,
    TagAll(Vec<u64>, usize),
    Close,
}

/// "1 starting row leads to 26 linked rows".
pub fn lead_line(start: u64, found: u64) -> String {
    format!(
        "{} starting {} to {} linked {}",
        group(start),
        if start == 1 { "row leads" } else { "rows lead" },
        group(found),
        if found == 1 { "row" } else { "rows" }
    )
}

pub use logseek_core::query::links::Link;

/// A link the file's values show: follow from a row's `from` value to the
/// rows whose `to` holds it (and, if another column points there too, `also`).
#[derive(Clone, Debug, PartialEq)]
pub struct Suggested {
    pub from: String,
    pub to: String,
    pub also: Option<String>,
    /// The share of `to`'s different values that are `from` values.
    pub share: f64,
}

/// The strongest link between the columns, from their values (never their
/// names): `links` as the engine found them, most telling first.
pub fn suggest(links: &[Link], columns: &[String]) -> Option<Suggested> {
    let best = links
        .iter()
        .find(|l| l.from < columns.len() && l.to < columns.len())?;
    let also = links
        .iter()
        .find(|l| l.to == best.to && l.from != best.from && l.from < columns.len())
        .map(|l| columns[l.from].clone());
    Some(Suggested {
        from: columns[best.to].clone(),
        to: columns[best.from].clone(),
        also,
        share: best.share,
    })
}

impl FollowState {
    pub fn new(columns: &[String], start: StartKind, links: &[Link]) -> FollowState {
        let suggested = suggest(links, columns);
        let first = columns.first().cloned().unwrap_or_default();
        let (from, to) = suggested
            .as_ref()
            .map(|s| (s.from.clone(), s.to.clone()))
            .unwrap_or((first.clone(), first));
        FollowState {
            start,
            from,
            to,
            // another column pointing at the same values: either is enough
            also: suggested.as_ref().and_then(|s| s.also.clone()),
            when: 0,
            amount: "5".into(),
            unit: 1,
            repeat: suggested.is_some(),
            run: 0,
            tag_to: 1,
            suggested,
            save_name: String::new(),
        }
    }

    pub fn spec(&self) -> Option<FollowSpec> {
        let n: i64 = self.amount.trim().parse().ok().filter(|n| *n > 0)?;
        let us = n * [1_000_000i64, 60_000_000, 3_600_000_000][self.unit as usize % 3];
        Some(FollowSpec {
            from_col: self.from.clone(),
            to_col: self.to.clone(),
            also_to: self.also.iter().cloned().collect(),
            window: match self.when {
                1 => Window::After(us),
                2 => Window::Before(us),
                3 => Window::Around(us),
                _ => Window::Any,
            },
            repeat: self.repeat,
        })
    }
}

/// One way to follow the value of a cell, offered in the cell's menu.
#[derive(Clone, Debug, PartialEq)]
pub struct CellChoice {
    pub label: String,
    /// The column whose rows hold the cell's value.
    pub to: String,
    /// Keep following from what is found (the whole tree).
    pub repeat: bool,
}

/// The ways to follow a cell of column `col`: one for each column its values
/// are found in (the links worked out from the file's own values, in either
/// direction, the strongest first), then the whole tree through the strongest.
/// Empty when nothing links to the column.
pub fn cell_choices(
    col: usize,
    columns: &[String],
    links: &[logseek_core::query::links::Link],
) -> Vec<CellChoice> {
    let mut others: Vec<(usize, f64)> = Vec::new();
    for l in links {
        let other = if l.from == col {
            l.to
        } else if l.to == col {
            l.from
        } else {
            continue;
        };
        match others.iter_mut().find(|(o, _)| *o == other) {
            Some(seen) => seen.1 = seen.1.max(l.share),
            None => others.push((other, l.share)),
        }
    }
    others.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out: Vec<CellChoice> = others
        .iter()
        .filter_map(|(o, _)| columns.get(*o))
        .map(|name| CellChoice {
            label: format!("Rows whose {name} is this value"),
            to: name.clone(),
            repeat: false,
        })
        .collect();
    if let Some(best) = out.first().map(|c| c.to.clone()) {
        out.push(CellChoice {
            label: format!("The whole tree from this row, through {best}"),
            to: best,
            repeat: true,
        });
    }
    out
}

/// What a follow from a cell found, in words, for the chip above the rows.
pub fn followed_words(from: &str, value: &str, to: &str, repeat: bool) -> String {
    if repeat {
        format!("the whole tree from {from} {value}, through {to}")
    } else {
        format!("rows whose {to} is {value} ({from} of the row you started from)")
    }
}

/// Where Follow starts when it opens: the row the user is on, if one is
/// selected; else the first tag that has rows; else every row shown.
pub fn first_start(selected: bool, tagged: Option<usize>) -> StartKind {
    match (selected, tagged) {
        (true, _) => StartKind::Selected,
        (false, Some(i)) => StartKind::Tag(i),
        (false, None) => StartKind::Result,
    }
}

/// The link said for the row it starts from, with that row's own value, so
/// the form reads as something concrete. `value` is the selected row's value
/// in `from` (`None`: it has none).
pub fn link_example(to: &str, also: Option<&str>, from: &str, value: Option<&str>) -> String {
    match value.filter(|v| !v.trim().is_empty()) {
        Some(v) => {
            let cols = match also {
                Some(a) => format!("{to} or {a}"),
                None => to.to_string(),
            };
            format!("From the selected row: rows whose {cols} is {v}.")
        }
        None => format!(
            "The selected row has no value in {from}, so nothing links to it this way. Choose another column, or another row."
        ),
    }
}

fn column_pick(ui: &mut egui::Ui, id: &str, cur: &mut String, columns: &[String]) {
    crate::colpick::combo(ui, id, cur, columns, 220.0, "choose a column");
}

/// Draw the sheet's contents. `tag_rows(i)` = rows carrying tag `i`.
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    st: &mut FollowState,
    snap: &Snapshot,
    tags: &crate::tags::TagMap,
    selected: Option<u64>,
    showing: bool,
    recipes: &mut Vec<Recipe>,
) -> FollowAction {
    let mut act = FollowAction::None;
    ui.set_width(600.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Follow").font(crate::fonts::strong(14.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::app::icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                act = FollowAction::Close;
            }
        });
    });
    ui.label(
        RichText::new("Find the rows that link to the rows you start from: children of a process, everything from the same address, the same session.")
            .color(pal.text2),
    );
    ui.add_space(6.0);
    let ns = snap.columns.len() - snap.virtual_cols.len();
    let columns = &snap.columns[..ns];
    let tag_rows = |i: usize| crate::tags::rows_with(tags, 1 << i);
    // saved Follows that fit this file: one click sets the sheet
    let fitting: Vec<usize> = (0..recipes.len())
        .filter(|&i| recipes[i].fits(columns))
        .collect();
    if !fitting.is_empty() {
        let mut remove: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Saved").color(pal.text2));
            for &i in &fitting {
                let r = &recipes[i];
                let on = Recipe::from_state(&r.name, st) == *r;
                if ui
                    .selectable_label(on, &r.name)
                    .on_hover_text(match &r.also {
                        Some(a) => format!("Rows where {} or {} equals their {}", r.to, a, r.from),
                        None => format!("Rows where {} equals their {}", r.to, r.from),
                    })
                    .clicked()
                {
                    r.apply(st);
                }
                if ui
                    .add(
                        egui::Button::new(ph::X)
                            .frame(false)
                            .min_size(egui::vec2(24.0, 24.0)),
                    )
                    .on_hover_text(format!("Forget \"{}\"", r.name))
                    .clicked()
                {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            recipes.remove(i);
        }
        ui.add_space(4.0);
    }
    // start from
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Start from").color(pal.text2));
        let label = |k: StartKind| match k {
            StartKind::Tag(i) => format!(
                "rows tagged {} ({})",
                crate::tags::name(i),
                group(tag_rows(i).len() as u64)
            ),
            StartKind::Selected => match selected {
                Some(_) => "the row you have selected".to_string(),
                None => "the selected row (none is selected)".to_string(),
            },
            StartKind::Result => format!("every row shown now ({})", group(snap.matched)),
        };
        egui::ComboBox::from_id_salt("follow-start")
            .selected_text(label(st.start))
            .width(300.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut st.start,
                    StartKind::Selected,
                    label(StartKind::Selected),
                );
                for i in 0..crate::tags::TAGS.len() {
                    if !tag_rows(i).is_empty() {
                        ui.selectable_value(
                            &mut st.start,
                            StartKind::Tag(i),
                            label(StartKind::Tag(i)),
                        );
                    }
                }
                ui.selectable_value(&mut st.start, StartKind::Result, label(StartKind::Result));
            });
    });
    // the link
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Find the rows whose").color(pal.text2));
        column_pick(ui, "follow-to", &mut st.to, columns);
        ui.label(RichText::new("or").color(pal.text2));
        ui.scope(|ui| {
            crate::colpick::combo_or_none(
                ui,
                "follow-also",
                &mut st.also,
                columns,
                200.0,
                "no other column",
            );
        })
        .response
        .on_hover_text("A second column that may hold the same value: for example children point back with a parent id, and everything else a process did points back with a context id");
        ui.label(RichText::new("is the same as the starting rows'").color(pal.text2));
        column_pick(ui, "follow-from", &mut st.from, columns);
        if ui
            .button(format!("{}  Swap", ph::ARROWS_LEFT_RIGHT))
            .on_hover_text("Walk the other way (for a process: up to the parents instead of down to the children)")
            .clicked()
        {
            std::mem::swap(&mut st.from, &mut st.to);
            st.also = None;
        }
    });
    if st.start == StartKind::Selected {
        // the selected row's own value in the column it is followed from
        let value = selected.and_then(|row| {
            let p = snap.page.as_ref()?;
            let k = p.rows.iter().position(|r| *r == row)?;
            let c = columns.iter().position(|n| *n == st.from)?;
            p.cells.get(k)?.get(c)?.clone()
        });
        if selected.is_some() {
            ui.add(
                egui::Label::new(
                    RichText::new(link_example(&st.to, st.also.as_deref(), &st.from, value.as_deref()))
                        .color(pal.text),
                )
                .wrap(),
            );
        }
    }
    if let Some(s) = st
        .suggested
        .as_ref()
        .filter(|s| s.from == st.from && s.to == st.to)
    {
        ui.label(
            RichText::new(format!(
                "Suggested from the values: {:.0}% of the different values of {} are values of {} on other rows. Change either column for any other link.",
                s.share * 100.0,
                s.to,
                s.from
            ))
            .font(theme::small_font())
            .color(pal.accent_ink),
        );
    }
    // the choices few follows need sit one click away, so the sheet opens
    // on the one sentence that matters
    egui::CollapsingHeader::new(RichText::new("More options: a time limit").color(pal.text2))
        .id_salt("follow-more")
        .default_open(st.when != 0)
        .show(ui, |ui| {
        // when
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("When").color(pal.text2));
            let whens = [
                "at any time",
                "within, after it",
                "within, before it",
                "within, either side",
            ];
            egui::ComboBox::from_id_salt("follow-when")
                .selected_text(whens[st.when as usize % 4])
                .show_ui(ui, |ui| {
                    for (i, w) in whens.iter().enumerate() {
                        ui.selectable_value(&mut st.when, i as u8, *w);
                    }
                });
            if st.when != 0 {
                ui.add(egui::TextEdit::singleline(&mut st.amount).desired_width(50.0));
                let units = ["seconds", "minutes", "hours"];
                egui::ComboBox::from_id_salt("follow-unit")
                    .selected_text(units[st.unit as usize % 3])
                    .show_ui(ui, |ui| {
                        for (i, u) in units.iter().enumerate() {
                            ui.selectable_value(&mut st.unit, i as u8, *u);
                        }
                    });
                if snap.time_col.is_none() {
                    ui.label(
                        RichText::new("This file has no timeline, so a time limit cannot be used.")
                            .color(pal.danger),
                    );
                }
            }
        });
        });
    ui.checkbox(
        &mut st.repeat,
        "Keep following from the rows found, until nothing new turns up (the whole tree)",
    );
    ui.add_space(6.0);
    let start_rows: Option<FollowStart> = match st.start {
        StartKind::Tag(i) => Some(FollowStart::Rows(Arc::new(tag_rows(i)))),
        StartKind::Selected => selected.map(|r| FollowStart::Rows(Arc::new(vec![r]))),
        StartKind::Result => Some(FollowStart::Result),
    };
    let spec = st
        .spec()
        .filter(|s| !(s.window != Window::Any && snap.time_col.is_none()));
    ui.horizontal(|ui| {
        let ready = start_rows.is_some() && spec.is_some();
        let b = ui
            .add_enabled_ui(ready, |ui| {
                crate::app::primary_button(ui, pal, &format!("{}  Follow", ph::TREE_STRUCTURE))
            })
            .inner
            .on_disabled_hover_text("Choose rows to start from (select a row or tag some), and a whole number for the time");
        if b.clicked() {
            if let (Some(s), Some(sp)) = (start_rows.clone(), spec.clone()) {
                st.run += 1;
                act = FollowAction::Run(s, sp);
            }
        }
        if showing && ui.button("Show all rows again").clicked() {
            act = FollowAction::ShowAll;
        }
        // keep this link for next time, on any file with these columns
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !st.save_name.trim().is_empty() && spec.is_some();
            if ui
                .add_enabled(ok, egui::Button::new("Save"))
                .on_hover_text("Keep this link and time limit by name, for any file that has these two columns")
                .on_disabled_hover_text("Type a name to save this Follow under")
                .clicked()
            {
                save_recipe(recipes, Recipe::from_state(&st.save_name, st));
                st.save_name.clear();
            }
            ui.add(
                egui::TextEdit::singleline(&mut st.save_name)
                    .hint_text("Name to save it as")
                    .desired_width(170.0),
            );
        });
    });
    // the answer
    let Some(out) = snap
        .follow
        .as_ref()
        .filter(|o| o.id == st.run && st.run > 0)
    else {
        if st.run > 0 {
            ui.label(RichText::new("Following").color(pal.text2));
        }
        return act;
    };
    ui.separator();
    if let Some(e) = &out.error {
        ui.label(RichText::new(format!("This cannot be followed: {e}")).color(pal.danger));
        return act;
    }
    let found: u64 = out.levels.iter().map(|l| l.len() as u64).sum();
    ui.label(
        RichText::new(lead_line(out.start_rows.len() as u64, found))
            .font(crate::fonts::strong(13.0)),
    );
    if found == 0 {
        ui.label(
            RichText::new("No row has that value. Try the other direction (Swap), another column, or a wider time.")
                .color(pal.text2),
        );
        return act;
    }
    for (i, l) in out.levels.iter().enumerate() {
        ui.label(
            RichText::new(format!("Step {}: {} rows", i + 1, group(l.len() as u64)))
                .color(pal.text2),
        );
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("{}  Show these rows", ph::ROWS))
            .on_hover_text("The starting rows and everything found, in the grid (your search and filters are cleared; Undo brings them back). Search, Summarise and export then work on these rows")
            .clicked()
        {
            let mut rows: Vec<u64> = out.start_rows.as_ref().clone();
            rows.extend(out.levels.iter().flatten().map(|(r, _)| *r));
            rows.sort_unstable();
            rows.dedup();
            act = FollowAction::ShowRows(Arc::new(rows));
        }
        ui.label(RichText::new("or tag what was found as").color(pal.text2));
        egui::ComboBox::from_id_salt("follow-tag")
            .selected_text(crate::tags::name(st.tag_to))
            .show_ui(ui, |ui| {
                for i in 0..crate::tags::TAGS.len() {
                    ui.selectable_value(&mut st.tag_to, i, crate::tags::name(i));
                }
            });
        if ui.button("Tag them").clicked() {
            act = FollowAction::TagAll(
                out.levels.iter().flatten().map(|(r, _)| *r).collect(),
                st.tag_to,
            );
        }
    });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    fn link(from: usize, to: usize, share: f64) -> Link {
        Link { from, to, share }
    }

    #[test]
    fn the_link_suggested_is_the_one_the_values_show_whatever_the_names() {
        let c = cols(&["time", "a", "b", "c"]);
        // "b" holds "a"'s values on other rows: follow from a row's "a" to the
        // rows whose "b" is it; "c" points at "a" too
        let links = [link(2, 1, 0.9), link(3, 1, 0.6)];
        let s = suggest(&links, &c).unwrap();
        assert_eq!(
            (s.from.as_str(), s.to.as_str(), s.also.as_deref()),
            ("a", "b", Some("c"))
        );
        assert!((s.share - 0.9).abs() < 1e-9);
        let st = FollowState::new(&c, StartKind::Result, &links);
        assert_eq!((st.from.as_str(), st.to.as_str()), ("a", "b"));
        assert_eq!(st.spec().unwrap().also_to, vec!["c".to_string()]);
        assert!(st.repeat, "a chain of links is followed on");
        // names alone suggest nothing
        let named = cols(&["TargetProcessId", "ParentProcessId"]);
        assert!(suggest(&[], &named).is_none());
        let st = FollowState::new(&named, StartKind::Result, &[]);
        assert_eq!(st.also, None);
        assert!(!st.repeat);
    }

    #[test]
    fn the_answer_line_reads_right_for_one_and_many() {
        assert_eq!(lead_line(1, 26), "1 starting row leads to 26 linked rows");
        assert_eq!(lead_line(12, 1), "12 starting rows lead to 1 linked row");
    }

    #[test]
    fn a_recipe_fits_files_with_both_columns_and_restores_the_sheet() {
        let mut st = FollowState::new(&cols(&["src_ip", "dst_ip", "user"]), StartKind::Result, &[]);
        st.from = "dst_ip".into();
        st.to = "src_ip".into();
        st.when = 3;
        st.amount = "10".into();
        st.unit = 1;
        st.repeat = false;
        let r = Recipe::from_state("Next hop, 10 minutes either side", &st);
        assert!(r.fits(&cols(&["time", "src_ip", "dst_ip"])));
        assert!(!r.fits(&cols(&["time", "src_ip"])));
        let mut other = FollowState::new(&cols(&["src_ip", "dst_ip"]), StartKind::Selected, &[]);
        r.apply(&mut other);
        assert_eq!(other.spec(), st.spec());
        // applying keeps where the user chose to start
        assert!(other.start == StartKind::Selected);
    }

    #[test]
    fn saving_under_a_name_again_replaces_it() {
        let st = FollowState::new(&cols(&["a", "b"]), StartKind::Result, &[]);
        let mut list = vec![Recipe::from_state("Mine", &st)];
        let mut st2 = FollowState::new(&cols(&["a", "b"]), StartKind::Result, &[]);
        st2.to = "b".into();
        save_recipe(&mut list, Recipe::from_state("mine ", &st2));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].to, "b");
        // an empty name saves nothing
        save_recipe(&mut list, Recipe::from_state("  ", &st2));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn the_time_limit_becomes_a_window() {
        let mut st = FollowState::new(&cols(&["a", "b"]), StartKind::Result, &[]);
        assert_eq!(st.spec().unwrap().window, Window::Any);
        st.when = 1;
        st.amount = "5".into();
        st.unit = 1;
        assert_eq!(st.spec().unwrap().window, Window::After(300_000_000));
        st.amount = "soon".into();
        assert!(st.spec().is_none());
    }

    #[test]
    fn follow_starts_from_the_row_you_are_on_before_anything_else() {
        // a selected row is what the user is looking at: it comes first
        assert!(first_start(true, Some(0)) == StartKind::Selected);
        assert!(first_start(true, None) == StartKind::Selected);
        // nothing selected: the tagged rows, then everything shown
        assert!(first_start(false, Some(2)) == StartKind::Tag(2));
        assert!(first_start(false, None) == StartKind::Result);
    }

    #[test]
    fn the_link_is_said_for_the_row_you_start_from() {
        assert_eq!(
            link_example("ContextProcessId", None, "TargetProcessId", Some("7179631355853")),
            "From the selected row: rows whose ContextProcessId is 7179631355853."
        );
        assert_eq!(
            link_example("ppid", Some("pid"), "pid", Some("4012")),
            "From the selected row: rows whose ppid or pid is 4012."
        );
        // no value to follow from: said, with what to do
        assert_eq!(
            link_example("ContextProcessId", None, "TargetProcessId", None),
            "The selected row has no value in TargetProcessId, so nothing links to it this way. Choose another column, or another row."
        );
    }

    #[test]
    fn a_cell_offers_the_columns_its_own_column_is_linked_to() {
        use logseek_core::query::links::Link;
        let columns = cols(&["time", "pid", "parent_pid", "session", "name"]);
        // pid's values are found in parent_pid (strongly) and in session (weakly)
        let links = [
            Link { from: 1, to: 3, share: 0.4 },
            Link { from: 1, to: 2, share: 0.9 },
            Link { from: 4, to: 0, share: 0.5 },
        ];
        let c = cell_choices(1, &columns, &links);
        // the strongest link first, each said in words; then the whole tree through it
        assert_eq!(
            c.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(),
            vec![
                "Rows whose parent_pid is this value",
                "Rows whose session is this value",
                "The whole tree from this row, through parent_pid",
            ]
        );
        assert_eq!((c[0].to.as_str(), c[0].repeat), ("parent_pid", false));
        assert_eq!((c[2].to.as_str(), c[2].repeat), ("parent_pid", true));
        // a link counts from either side: a cell in parent_pid is followed to pid
        let c = cell_choices(2, &columns, &links);
        assert_eq!(c[0].label, "Rows whose pid is this value");
        // a column nothing links to offers nothing of its own
        assert!(cell_choices(4, &columns, &[]).is_empty());
    }

    #[test]
    fn what_was_followed_is_said_with_the_value() {
        assert_eq!(
            followed_words("TargetProcessId", "7179631355853", "ContextProcessId", false),
            "rows whose ContextProcessId is 7179631355853 (TargetProcessId of the row you started from)"
        );
        assert_eq!(
            followed_words("pid", "4012", "parent_pid", true),
            "the whole tree from pid 4012, through parent_pid"
        );
    }
}
