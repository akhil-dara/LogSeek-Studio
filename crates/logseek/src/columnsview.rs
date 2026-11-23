//! The Columns view: every column as a row of a table, for files with tens or
//! hundreds of columns. Its type, how full it is (exact), what it is (its role,
//! judged from its own values), how many different values and the most common
//! ones in a sample spread over the file (said as such), a tick to show it in
//! the grid and a click to go to it. Read down a list instead of across a grid.

use logseek_core::query::roles::{ColumnProfile, Role};
use std::collections::BTreeSet;

/// Which columns the view lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Shown,
    Role(Role),
}

/// The columns listed for `filter` and `find` (every word of `find` in the
/// name, any case), in the file's order.
pub fn listed(
    profiles: &[ColumnProfile],
    names: &[String],
    hidden: &BTreeSet<String>,
    filter: Filter,
    find: &str,
) -> Vec<usize> {
    let words: Vec<String> = find.split_whitespace().map(str::to_lowercase).collect();
    (0..names.len())
        .filter(|&c| {
            let n = &names[c];
            let ok = match filter {
                Filter::All => true,
                Filter::Shown => !hidden.contains(n),
                Filter::Role(r) => profiles.get(c).is_some_and(|p| p.role == r),
            };
            ok && words.iter().all(|w| n.to_lowercase().contains(w.as_str()))
        })
        .collect()
}

/// A role in words.
pub fn role_words(r: Role) -> &'static str {
    match r {
        Role::Anchor => "time",
        Role::Empty => "empty",
        Role::Constant => "same on every row",
        Role::Sparse => "sparse (under 5%)",
        Role::Id => "id: different on most rows",
        Role::Informative => "informative",
    }
}

/// A column's type in words: the time column says time, whatever it is stored as.
pub fn type_words(p: Option<&ColumnProfile>, stored: &str) -> String {
    match p {
        Some(p) if p.role == Role::Anchor => "time".into(),
        _ => stored.to_string(),
    }
}

/// A column's most common sampled values with their share, on one line; the
/// time column's values read as UTC times, like the grid shows them.
pub fn top_words(p: &ColumnProfile) -> String {
    p.top
        .iter()
        .map(|(v, n)| {
            let share = 100.0 * *n as f64 / p.sampled.max(1) as f64;
            let v = if p.role == Role::Anchor {
                crate::grid::utc_cell(v).unwrap_or_else(|| v.clone())
            } else {
                v.clone()
            };
            format!("{}  {:.0}%", v.replace(['\n', '\r', '\t'], " "), share)
        })
        .collect::<Vec<_>>()
        .join("   ·   ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(col: usize, role: Role) -> ColumnProfile {
        ColumnProfile {
            col,
            role,
            filled: 0,
            rows: 0,
            distinct_sampled: 0,
            sampled: 0,
            median_len: 0,
            constant: None,
            score: 0.0,
            same_as: None,
            top: Vec::new(),
        }
    }

    #[test]
    fn the_view_lists_by_role_or_shown_narrowed_by_words() {
        let names: Vec<String> = ["timestamp", "UserName", "Version", "user.id", "Rare"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let prof = vec![
            p(0, Role::Anchor),
            p(1, Role::Informative),
            p(2, Role::Constant),
            p(3, Role::Id),
            p(4, Role::Sparse),
        ];
        let hidden: BTreeSet<String> = ["Version".to_string(), "Rare".to_string()].into();
        assert_eq!(
            listed(&prof, &names, &hidden, Filter::All, ""),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(
            listed(&prof, &names, &hidden, Filter::Shown, ""),
            vec![0, 1, 3]
        );
        assert_eq!(
            listed(&prof, &names, &hidden, Filter::Role(Role::Constant), ""),
            vec![2]
        );
        // words, any case, any order
        assert_eq!(
            listed(&prof, &names, &hidden, Filter::All, "USER"),
            vec![1, 3]
        );
        assert_eq!(
            listed(&prof, &names, &hidden, Filter::All, "id user"),
            vec![3]
        );
        // a made column has no profile: listed under All and Shown only
        let mut names2 = names.clone();
        names2.push("made".into());
        assert_eq!(
            listed(&prof, &names2, &hidden, Filter::Role(Role::Informative), ""),
            vec![1]
        );
        assert!(listed(&prof, &names2, &hidden, Filter::All, "made") == vec![5]);
    }

    #[test]
    fn the_time_column_reads_as_times_and_its_type_says_time() {
        let mut t = p(0, Role::Anchor);
        t.sampled = 4;
        t.top = vec![("1794564007000".into(), 1)];
        assert_eq!(top_words(&t), "2026-11-13 10:00:07.000 UTC  25%");
        assert_eq!(type_words(Some(&t), "int"), "time");
        // other columns keep their values and type as stored
        let mut n = p(1, Role::Informative);
        n.sampled = 2;
        n.top = vec![("1794564007000".into(), 1), ("a\tb".into(), 1)];
        assert_eq!(top_words(&n), "1794564007000  50%   ·   a b  50%");
        assert_eq!(type_words(Some(&n), "int"), "int");
        assert_eq!(type_words(None, "text"), "text");
    }
}

use crate::bridge::Snapshot;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;

#[derive(Default)]
pub struct ColumnsViewState {
    pub filter: Filter,
    pub find: String,
}

/// What the user asked of the view.
pub enum Act {
    None,
    Back,
    GoTo(usize),
}

/// A cell of a fixed width, its content aligned (names left, counts right).
fn cell(ui: &mut egui::Ui, w: f32, h: f32, right: bool, label: egui::Label) -> egui::Response {
    let layout = if right {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(w, h), layout, |ui| {
        ui.set_width(w);
        ui.add(label)
    })
    .inner
}

/// The view, in the space the grid otherwise takes.
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    snap: &Snapshot,
    hidden: &mut BTreeSet<String>,
    st: &mut ColumnsViewState,
) -> Act {
    let mut act = Act::None;
    let group = crate::moment::group;
    let empty_prof: Vec<ColumnProfile> = Vec::new();
    let prof: &[ColumnProfile] = snap
        .col_profile
        .as_deref()
        .map_or(&empty_prof, |v| v.as_slice());
    let names = &snap.columns;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{}  Back to rows", ph::ARROW_LEFT))
                    .on_hover_text("The rows again  (Ctrl+1)")
                    .clicked()
                {
                    act = Act::Back;
                }
                ui.label(
                    RichText::new(format!("Columns  {}", group(names.len() as u64)))
                        .font(crate::fonts::strong(15.0)),
                );
                let shown = names.iter().filter(|n| !hidden.contains(*n)).count();
                ui.label(
                    RichText::new(format!("{} shown in the grid", group(shown as u64)))
                        .color(pal.text2),
                );
            });
            ui.add_space(6.0);
            // what to list: every column, the ones shown, or one kind, each counted
            ui.horizontal_wrapped(|ui| {
                let count = |f: Filter| listed(prof, names, hidden, f, "").len();
                let mut pills: Vec<(Filter, String)> = vec![
                    (Filter::All, format!("All  {}", group(names.len() as u64))),
                    (
                        Filter::Shown,
                        format!("Shown  {}", group(count(Filter::Shown) as u64)),
                    ),
                ];
                for r in [
                    Role::Informative,
                    Role::Anchor,
                    Role::Constant,
                    Role::Id,
                    Role::Sparse,
                    Role::Empty,
                ] {
                    let n = count(Filter::Role(r));
                    if n > 0 {
                        pills.push((
                            Filter::Role(r),
                            format!("{}  {}", role_words(r), group(n as u64)),
                        ));
                    }
                }
                for (f, label) in pills {
                    ui.selectable_value(&mut st.filter, f, label);
                }
                ui.add_space(12.0);
                ui.add(
                    egui::TextEdit::singleline(&mut st.find)
                        .hint_text(format!("{}  Find a column", ph::MAGNIFYING_GLASS))
                        .desired_width(220.0),
                );
            });
            ui.add_space(4.0);
            if prof.is_empty() {
                ui.label(
                    RichText::new(
                        "What each column holds is worked out once the file is fully read.",
                    )
                    .color(pal.text2),
                );
            }
            let rows = listed(prof, names, hidden, st.filter, &st.find);
            let sample = prof.iter().map(|p| p.sampled).max().unwrap_or(0);
            // widths that fit every name and label: nothing cut, the table scrolls sideways
            let font = theme::ui_font();
            let measure = |ui: &egui::Ui, t: &str| {
                ui.painter()
                    .layout_no_wrap(t.to_string(), font.clone(), pal.text)
                    .size()
                    .x
            };
            let name_w = rows
                .iter()
                .map(|&c| measure(ui, &names[c]))
                .fold(measure(ui, "Column"), f32::max)
                + 16.0;
            let role_w = measure(ui, role_words(Role::Id)).max(measure(ui, "made by you")) + 16.0;
            let row_h = 30.0;
            let cols = [28.0, name_w, 70.0, 170.0, role_w, 130.0];
            let header = |ui: &mut egui::Ui| {
                ui.horizontal(|ui| {
                    for (i, (w, t)) in cols
                        .iter()
                        .zip([
                            "",
                            "Column",
                            "Type",
                            "Filled (exact)",
                            "What it is",
                            "Different",
                        ])
                        .enumerate()
                    {
                        // the count column's heading sits over its right-aligned numbers
                        let (w, right) = if i == 5 {
                            (*w - 16.0, true)
                        } else {
                            (*w, false)
                        };
                        cell(
                            ui,
                            w,
                            22.0,
                            right,
                            egui::Label::new(
                                RichText::new(t).font(theme::small_font()).color(pal.text2),
                            ),
                        );
                    }
                    ui.add_space(24.0);
                    ui.label(
                        RichText::new(format!(
                            "Most common, in a sample of {} rows spread over the file",
                            group(sample)
                        ))
                        .font(theme::small_font())
                        .color(pal.text2),
                    );
                });
            };
            egui::ScrollArea::horizontal()
                .id_salt("cols-h")
                .show(ui, |ui| {
                    header(ui);
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("cols-v")
                        .auto_shrink([false, false])
                        .show_rows(ui, row_h, rows.len(), |ui, range| {
                            for &c in &rows[range] {
                                let name = &names[c];
                                let p = prof.get(c);
                                let r = ui.horizontal(|ui| {
                                    ui.set_height(row_h - 4.0);
                                    let mut shown = !hidden.contains(name);
                                    ui.allocate_ui(egui::vec2(cols[0], row_h), |ui| {
                                        if ui
                                            .checkbox(&mut shown, "")
                                            .on_hover_text("Show it in the grid")
                                            .changed()
                                        {
                                            if shown {
                                                hidden.remove(name);
                                            } else {
                                                hidden.insert(name.clone());
                                            }
                                        }
                                    });
                                    let n = cell(
                                        ui,
                                        cols[1],
                                        row_h - 4.0,
                                        false,
                                        egui::Label::new(
                                            RichText::new(name).font(font.clone()).color(pal.text),
                                        )
                                        .sense(egui::Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text("Go to it in the rows");
                                    if n.clicked() {
                                        act = Act::GoTo(c);
                                    }
                                    let stored = snap
                                        .col_types
                                        .get(c)
                                        .map(|t| format!("{t:?}").to_lowercase())
                                        .unwrap_or_default();
                                    let ty = type_words(p, &stored);
                                    cell(
                                        ui,
                                        cols[2],
                                        row_h - 4.0,
                                        false,
                                        egui::Label::new(
                                            RichText::new(ty)
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                        ),
                                    );
                                    // fill: a bar and the share, exact
                                    ui.allocate_ui(egui::vec2(cols[3], row_h - 4.0), |ui| {
                                        ui.horizontal_centered(|ui| {
                                            let (frac, words) = match p {
                                                Some(p) if p.rows > 0 => {
                                                    let f = p.filled as f32 / p.rows as f32;
                                                    (f, format!("{:.1}%", f * 100.0))
                                                }
                                                _ => (0.0, String::new()),
                                            };
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(64.0, 8.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                egui::CornerRadius::same(2),
                                                pal.hairline,
                                            );
                                            let mut bar = rect;
                                            bar.set_width(rect.width() * frac.clamp(0.0, 1.0));
                                            ui.painter().rect_filled(
                                                bar,
                                                egui::CornerRadius::same(2),
                                                pal.info,
                                            );
                                            ui.label(
                                                RichText::new(words)
                                                    .font(theme::small_font())
                                                    .color(pal.text),
                                            );
                                        });
                                    });
                                    let role = match p {
                                        Some(p) => role_words(p.role),
                                        None => "made by you",
                                    };
                                    cell(
                                        ui,
                                        cols[4],
                                        row_h - 4.0,
                                        false,
                                        egui::Label::new(
                                            RichText::new(role)
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                        ),
                                    );
                                    let diff =
                                        p.map(|p| group(p.distinct_sampled)).unwrap_or_default();
                                    cell(
                                        ui,
                                        cols[5] - 16.0,
                                        row_h - 4.0,
                                        true,
                                        egui::Label::new(
                                            RichText::new(diff)
                                                .font(theme::data_font())
                                                .color(pal.text),
                                        ),
                                    );
                                    if let Some(p) = p {
                                        ui.add_space(24.0);
                                        ui.label(
                                            RichText::new(top_words(p))
                                                .font(theme::data_font())
                                                .color(pal.text),
                                        );
                                    }
                                });
                                let rr = r.response.rect;
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(rr.left(), rr.bottom() + 2.0),
                                        egui::pos2(rr.right() + 2000.0, rr.bottom() + 2.0),
                                    ],
                                    egui::Stroke::new(1.0, pal.hairline),
                                );
                            }
                        });
                });
        });
    act
}
