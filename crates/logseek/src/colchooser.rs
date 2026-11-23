//! "Choose columns": every column in its order, with a box to show or hide
//! it, ↑ ↓ to move it, and a sample value beside it so you know what it holds.

use crate::bridge::Snapshot;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::collections::BTreeSet;

/// The first non-empty value of column `c` on the loaded rows.
pub fn sample_of(cells: &[Vec<Option<String>>], c: usize) -> Option<String> {
    cells
        .iter()
        .filter_map(|r| r.get(c).and_then(|v| v.as_ref()))
        .find(|v| !v.trim().is_empty())
        .map(|v| v.replace(['\n', '\r', '\t'], " "))
}

/// Columns with no value on any loaded row.
pub fn empty_columns(cells: &[Vec<Option<String>>], n: usize) -> Vec<usize> {
    (0..n).filter(|&c| sample_of(cells, c).is_none()).collect()
}

/// Put `col` at place `to` among the columns shown (`visible`, in order; a
/// search may be hiding others). Past the end = just after the last shown.
pub fn move_among(order: &mut Vec<usize>, col: usize, visible: &[usize], to: usize) {
    let rest: Vec<usize> = visible.iter().copied().filter(|&c| c != col).collect();
    if let Some(&before) = rest.get(to) {
        crate::grid::move_before(order, col, Some(before));
        return;
    }
    let Some(&last) = rest.last() else {
        return;
    };
    order.retain(|&c| c != col);
    let at = order
        .iter()
        .position(|&c| c == last)
        .map_or(order.len(), |p| p + 1);
    order.insert(at, col);
}

/// The columns shown, in their order.
pub fn shown_in_order(order: &[usize], hidden: &BTreeSet<String>, names: &[String]) -> Vec<usize> {
    order
        .iter()
        .copied()
        .filter(|&c| names.get(c).is_some_and(|n| !hidden.contains(n)))
        .collect()
}

/// Show column `col` as the last of those shown: picked one after another,
/// columns line up in the order they were picked.
pub fn add_to_end(
    order: &mut Vec<usize>,
    hidden: &mut BTreeSet<String>,
    names: &[String],
    col: usize,
) {
    let shown = shown_in_order(order, hidden, names);
    if let Some(n) = names.get(col) {
        hidden.remove(n);
    }
    move_among(order, col, &shown, usize::MAX);
}

/// Hide every column but those kept on the left: a start from nothing.
pub fn show_none(hidden: &mut BTreeSet<String>, names: &[String], kept: &BTreeSet<String>) {
    for n in names {
        if !kept.contains(n) {
            hidden.insert(n.clone());
        }
    }
}

/// Pin column `col` (just after the columns already pinned, kept on the left)
/// or, if it is pinned, let it scroll with the others where it stands.
pub fn toggle_pin(
    order: &mut Vec<usize>,
    hidden: &BTreeSet<String>,
    pinned: &mut BTreeSet<String>,
    names: &[String],
    col: usize,
) {
    let Some(n) = names.get(col).cloned() else {
        return;
    };
    if pinned.remove(&n) {
        return;
    }
    let shown = shown_in_order(order, hidden, names);
    let before = shown
        .iter()
        .filter(|&&k| k != col && names.get(k).is_some_and(|m| pinned.contains(m)))
        .count();
    move_among(order, col, &shown, before);
    pinned.insert(n);
}

#[derive(Default)]
pub struct ChooserState {
    pub find: String,
    /// The find box has had its first focus.
    focused: bool,
    /// The column being dragged by its handle.
    drag: Option<usize>,
}

/// Draw the chooser; returns false when it was closed.
pub fn show(
    ctx: &egui::Context,
    pal: &Palette,
    st: &mut ChooserState,
    snap: &Snapshot,
    grid: &mut crate::grid::GridState,
) -> bool {
    let mut open = true;
    let default_order = grid.default_order.clone();
    let pinned = grid.pinned.clone();
    let mut goto: Option<usize> = None;
    let mut pin_toggle: Option<usize> = None;
    let (order, hidden): (&mut Vec<usize>, &mut BTreeSet<String>) =
        (&mut grid.order, &mut grid.hidden);
    let cells: &[Vec<Option<String>>] = snap.page.as_ref().map_or(&[], |p| p.cells.as_slice());
    let names = &snap.columns;
    let screen = ctx.content_rect();
    let w = 520.0_f32.min(screen.width() - 32.0);
    let h = (screen.height() - 140.0).max(320.0);
    egui::Window::new("Choose columns")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .fixed_size([w, h])
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 90.0))
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_width(w);
            let shown = shown_in_order(order, hidden, names);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Choose columns").font(crate::fonts::strong(14.0)));
                ui.label(
                    RichText::new(format!(
                        "showing {} of {}",
                        crate::moment::group(shown.len() as u64),
                        crate::moment::group(names.len() as u64)
                    ))
                    .color(pal.text2),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(ph::X).frame(false))
                        .on_hover_text("Close (Esc)")
                        .clicked()
                    {
                        open = false;
                    }
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Pick the columns you want in the order you want them: each one you add goes at the end. Drag or use the arrows to reorder.",
                    )
                    .color(pal.text2),
                )
                .wrap(),
            );
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Show none")
                    .on_hover_text("Start from no columns (the one kept on the left stays), then add the ones you want")
                    .clicked()
                {
                    show_none(hidden, names, &pinned);
                }
                if ui.button("Show all").clicked() {
                    hidden.clear();
                }
                if ui
                    .button("Hide empty")
                    .on_hover_text("Hide columns with no value on the rows loaded")
                    .clicked()
                {
                    for c in empty_columns(cells, names.len()) {
                        if let Some(n) = names.get(c) {
                            hidden.insert(n.clone());
                        }
                    }
                }
                if ui.button("Reset order").clicked() {
                    *order = default_order.clone();
                }
            });
            ui.separator();
            let sample = |c: usize| -> Option<String> {
                sample_of(cells, c).map(|v| {
                    if crate::grid::reads_as_time(&snap.col_types, snap.time_col, c) {
                        crate::grid::utc_cell(&v).unwrap_or(v)
                    } else {
                        v
                    }
                })
            };
            // ── shown, in this order ──
            ui.label(
                RichText::new(format!("Shown, in this order  ({})", crate::moment::group(shown.len() as u64)))
                    .font(crate::fonts::strong(12.5)),
            );
            let mut moved: Option<(usize, usize)> = None;
            let mut hide: Option<usize> = None;
            let top_h = (ui.available_height() * 0.5).max(140.0);
            egui::ScrollArea::both()
                .id_salt("shown-cols")
                .max_height(top_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut rows: Vec<(usize, egui::Rect)> = Vec::with_capacity(shown.len());
                    let icon = |g: &str| egui::Button::new(g).frame(false).min_size(egui::vec2(24.0, 24.0));
                    for (pos, &c) in shown.iter().enumerate() {
                        let Some(name) = names.get(c) else { continue };
                        let dragging = st.drag == Some(c);
                        let row = ui.horizontal(|ui| {
                            let hd = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(ph::DOTS_SIX_VERTICAL)
                                            .size(16.0)
                                            .color(if dragging { pal.accent_ink } else { pal.text2 }),
                                    )
                                    .sense(egui::Sense::drag()),
                                )
                                .on_hover_cursor(egui::CursorIcon::Grab)
                                .on_hover_text("Drag to move this column");
                            if hd.drag_started() {
                                st.drag = Some(c);
                            }
                            if ui.add_enabled(pos > 0, icon(ph::ARROW_UP)).on_hover_text("Move up (left in the grid)").clicked() {
                                moved = Some((c, pos - 1));
                            }
                            if ui
                                .add_enabled(pos + 1 < shown.len(), icon(ph::ARROW_DOWN))
                                .on_hover_text("Move down (right in the grid)")
                                .clicked()
                            {
                                moved = Some((c, pos + 1));
                            }
                            if ui.add(icon(ph::X)).on_hover_text("Hide this column").clicked() {
                                hide = Some(c);
                            }
                            let is_pinned = pinned.contains(name);
                            let pin = egui::Button::new(
                                RichText::new(ph::PUSH_PIN).color(if is_pinned { pal.accent_ink } else { pal.text2 }),
                            )
                            .frame(false)
                            .min_size(egui::vec2(24.0, 24.0));
                            let tip = if is_pinned {
                                "Kept on the left: click to let it scroll with the others"
                            } else {
                                "Keep it on the left while scrolling sideways (and first in this list)"
                            };
                            if ui.add(pin).on_hover_text(tip).clicked() {
                                pin_toggle = Some(c);
                            }
                            ui.vertical(|ui| {
                                let r = ui
                                    .add(egui::Label::new(RichText::new(name).color(pal.text)).wrap().sense(egui::Sense::click()))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text("Go to this column");
                                if r.clicked() {
                                    goto = Some(c);
                                }
                                if pinned.contains(name) {
                                    ui.label(
                                        RichText::new(format!("{} kept on the left", ph::PUSH_PIN))
                                            .font(theme::small_font())
                                            .color(pal.accent_ink),
                                    );
                                }
                                if let Some(v) = sample(c) {
                                    ui.add(egui::Label::new(RichText::new(v).font(theme::small_font()).color(pal.text2)).extend());
                                }
                            });
                        });
                        if dragging {
                            ui.painter().rect_filled(row.response.rect, egui::CornerRadius::same(3), pal.selection.gamma_multiply(0.5));
                        }
                        rows.push((c, row.response.rect));
                    }
                    if shown.is_empty() {
                        ui.label(RichText::new("No columns shown yet: add them below.").color(pal.muted));
                    }
                    // a drag in progress: a line where the row will land
                    if let Some(c) = st.drag {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        if let Some(y) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y) {
                            let rest: Vec<&(usize, egui::Rect)> = rows.iter().filter(|(k, _)| *k != c).collect();
                            let to = rest.iter().filter(|(_, r)| r.center().y < y).count();
                            let line_y = match (rest.get(to), rest.last()) {
                                (Some((_, r)), _) => r.top() - 1.0,
                                (None, Some((_, r))) => r.bottom() + 1.0,
                                (None, None) => y,
                            };
                            let clip = ui.clip_rect();
                            ui.painter().line_segment(
                                [egui::pos2(clip.left() + 4.0, line_y), egui::pos2(clip.right() - 12.0, line_y)],
                                egui::Stroke::new(3.0, pal.accent_ink),
                            );
                            if y < clip.top() + 28.0 {
                                ui.scroll_with_delta(egui::vec2(0.0, 12.0));
                            } else if y > clip.bottom() - 28.0 {
                                ui.scroll_with_delta(egui::vec2(0.0, -12.0));
                            }
                            ui.ctx().request_repaint();
                            if ui.input(|i| i.pointer.any_released()) {
                                moved = Some((c, to));
                            }
                        }
                        if !ui.input(|i| i.pointer.any_down()) {
                            st.drag = None;
                        }
                    }
                });
            if let Some((c, to)) = moved {
                move_among(order, c, &shown, to);
            }
            if let Some(c) = hide {
                if let Some(n) = names.get(c) {
                    hidden.insert(n.clone());
                }
            }
            ui.separator();
            // ── add a column ──
            let not_shown: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&c| names.get(c).is_some_and(|n| hidden.contains(n)))
                .collect();
            ui.label(
                RichText::new(format!("Add a column  ({} not shown)", crate::moment::group(not_shown.len() as u64)))
                    .font(crate::fonts::strong(12.5)),
            );
            let needle = st.find.to_lowercase();
            let found: Vec<usize> = not_shown
                .iter()
                .copied()
                .filter(|&c| needle.is_empty() || names[c].to_lowercase().contains(&needle))
                .collect();
            let mut add: Option<usize> = None;
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.find)
                    .hint_text(format!("{}  Find a column; Enter adds the first", ph::MAGNIFYING_GLASS))
                    .desired_width(f32::INFINITY),
            );
            if !st.focused {
                r.request_focus();
                st.focused = true;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                add = found.first().copied();
                st.find.clear();
                r.request_focus();
            }
            egui::ScrollArea::both()
                .id_salt("add-cols")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for &c in &found {
                        let name = &names[c];
                        // the hover shade goes behind the words, never over them
                        let behind = ui.painter().add(egui::Shape::Noop);
                        let resp = ui
                            .horizontal(|ui| {
                                ui.label(RichText::new(ph::PLUS).color(pal.accent_ink));
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(RichText::new(name).color(pal.text)).wrap());
                                    if let Some(v) = sample(c) {
                                        ui.add(egui::Label::new(RichText::new(v).font(theme::small_font()).color(pal.text2)).extend());
                                    }
                                });
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Add it after the last column shown");
                        if resp.hovered() {
                            ui.painter().set(
                                behind,
                                egui::Shape::rect_filled(resp.rect, egui::CornerRadius::same(3), pal.surface2),
                            );
                        }
                        if resp.clicked() {
                            add = Some(c);
                        }
                    }
                    if found.is_empty() {
                        let msg = if not_shown.is_empty() { "Every column is shown." } else { "No hidden column has that in its name." };
                        ui.label(RichText::new(msg).color(pal.muted));
                    }
                });
            if let Some(c) = add {
                add_to_end(order, hidden, names, c);
            }
        });
    // a pinned column goes just after those already pinned, and stays on the left
    if let Some(c) = pin_toggle {
        toggle_pin(
            &mut grid.order,
            &grid.hidden,
            &mut grid.pinned,
            &snap.columns,
            c,
        );
    }
    if let Some(c) = goto {
        if let Some(n) = snap.columns.get(c) {
            grid.hidden.remove(n);
        }
        grid.goto = Some(c);
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_pinned_column_goes_after_those_already_pinned_and_pinning_again_frees_it() {
        let names: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let hidden = BTreeSet::new();
        let mut pinned = BTreeSet::new();
        let mut order = vec![0, 1, 2, 3];
        toggle_pin(&mut order, &hidden, &mut pinned, &names, 2); // c first
        assert_eq!(order, vec![2, 0, 1, 3]);
        toggle_pin(&mut order, &hidden, &mut pinned, &names, 3); // d after c
        assert_eq!(order, vec![2, 3, 0, 1]);
        assert!(pinned.contains("c") && pinned.contains("d"));
        // pinning c again frees it where it stands
        toggle_pin(&mut order, &hidden, &mut pinned, &names, 2);
        assert!(!pinned.contains("c"));
        assert_eq!(order, vec![2, 3, 0, 1]);
    }

    #[test]
    fn picking_from_nothing_lines_columns_up_in_the_order_picked() {
        let names: Vec<String> = ["time", "a", "b", "c", "d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let kept: BTreeSet<String> = ["time".to_string()].into();
        let mut order = vec![0, 1, 2, 3, 4];
        let mut hidden = BTreeSet::new();
        show_none(&mut hidden, &names, &kept);
        // only the column kept on the left stays
        assert_eq!(shown_in_order(&order, &hidden, &names), vec![0]);
        // pick d, then a, then c: that is their order
        for c in [4, 1, 3] {
            add_to_end(&mut order, &mut hidden, &names, c);
        }
        assert_eq!(shown_in_order(&order, &hidden, &names), vec![0, 4, 1, 3]);
        // b stays hidden, and every column is still in the order list once
        assert!(hidden.contains("b"));
        let mut all = order.clone();
        all.sort();
        assert_eq!(all, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn moving_among_the_columns_a_search_shows() {
        // all columns: 0 1 2 3 4; the search shows 1 and 3
        let vis = [1usize, 3];
        // drop 3 at the top of what is shown: it lands just before 1
        let mut o = vec![0, 1, 2, 3, 4];
        move_among(&mut o, 3, &vis, 0);
        assert_eq!(o, vec![0, 3, 1, 2, 4]);
        // drop 1 at the bottom of what is shown: just after 3
        let mut o = vec![0, 1, 2, 3, 4];
        move_among(&mut o, 1, &vis, 1);
        assert_eq!(o, vec![0, 2, 3, 1, 4]);
        // with nothing hidden it is a plain move
        let all = [0usize, 1, 2, 3, 4];
        let mut o = vec![0, 1, 2, 3, 4];
        move_among(&mut o, 4, &all, 1);
        assert_eq!(o, vec![0, 4, 1, 2, 3]);
        // past the end means last of what is shown
        let mut o = vec![0, 1, 2, 3, 4];
        move_among(&mut o, 0, &all, 99);
        assert_eq!(o, vec![1, 2, 3, 4, 0]);
    }

    #[test]
    fn samples_skip_empty_values_and_flatten_lines() {
        let cells = vec![
            vec![None, Some("".into())],
            vec![Some("a\nb".into()), Some("  ".into())],
            vec![Some("c".into()), None],
        ];
        assert_eq!(sample_of(&cells, 0), Some("a b".into()));
        assert_eq!(sample_of(&cells, 1), None);
        assert_eq!(empty_columns(&cells, 2), vec![1]);
    }
}
