//! Choosing one column from a drop-down. A file can have hundreds of columns,
//! so every such list starts with a find box: type part of a name and the list
//! shrinks to the columns that have it. The whole list is always there to
//! scroll; nothing is left out.

use eframe::egui;
use egui::RichText;

/// The columns whose name has `typed` in it (letter case ignored), in the
/// file's order, the ones that start with it first. Everything when nothing
/// is typed.
pub fn matching<'a>(columns: &'a [String], typed: &str) -> Vec<&'a String> {
    let t = typed.trim().to_lowercase();
    if t.is_empty() {
        return columns.iter().collect();
    }
    let (mut first, mut rest) = (Vec::new(), Vec::new());
    for c in columns {
        let l = c.to_lowercase();
        if l.starts_with(&t) {
            first.push(c);
        } else if l.contains(&t) {
            rest.push(c);
        }
    }
    first.extend(rest);
    first
}

/// Draws `inner` so that nothing in it can scroll what lies around it. A
/// text box asks to be scrolled into view when it takes the cursor; in a
/// drop-down that request would reach the sheet behind and scroll that. Here
/// the request ends: this area scrolls in no direction and passes nothing on.
pub fn keeps_its_scrolling_to_itself<R>(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    inner: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::neither()
        .id_salt(("quiet", id))
        .auto_shrink([true, true])
        .show(ui, inner)
        .inner
}

/// The find box at the top of an open list; gives back what is typed in it.
/// What was typed is kept with the list (`id`) while the app runs.
pub fn find_box(ui: &mut egui::Ui, id: egui::Id, count: usize) -> String {
    find_box_saying(
        ui,
        id,
        format!(
            "Find a column ({} in this file)",
            crate::moment::group(count as u64)
        ),
    )
}

/// The find box of any open list, with its own words while empty.
pub fn find_box_saying(ui: &mut egui::Ui, id: egui::Id, hint: String) -> String {
    let key = id.with("find");
    let mut typed: String = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    let resp = keeps_its_scrolling_to_itself(ui, key, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut typed)
                .hint_text(hint)
                .desired_width(260.0),
        )
    });
    // the list opens ready to type in
    let fresh = ui.data(|d| d.get_temp::<bool>(key.with("seen"))).is_none();
    if fresh {
        resp.request_focus();
        ui.data_mut(|d| d.insert_temp(key.with("seen"), true));
    }
    ui.data_mut(|d| d.insert_temp(key, typed.clone()));
    typed
}

/// Called on frames where the list is closed, so it next opens with an empty
/// find box that takes the keyboard.
pub fn closed(ui: &egui::Ui, id: egui::Id) {
    let key = id.with("find");
    ui.data_mut(|d| {
        d.remove::<String>(key);
        d.remove::<bool>(key.with("seen"));
    });
}

/// A drop-down that sets `value` to one of `columns`. `empty` is shown while
/// nothing is chosen. True when the choice changed.
pub fn combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut String,
    columns: &[String],
    width: f32,
    empty: &str,
) -> bool {
    let id = egui::Id::new(("colpick", id));
    let before = value.clone();
    let shown = if value.is_empty() { empty } else { value.as_str() }.to_string();
    let inner = egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(width)
        .height(360.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            let typed = find_box(ui, id, columns.len());
            let names = matching(columns, &typed);
            if names.is_empty() {
                ui.label(RichText::new("No column has that in its name").weak());
            }
            for c in names {
                if ui.selectable_label(*value == *c, c.as_str()).clicked() {
                    *value = c.clone();
                    ui.close();
                }
            }
        });
    if inner.inner.is_none() {
        closed(ui, id);
    }
    *value != before
}

/// A drop-down that sets `value` to one of `columns`, or to none (`none` is
/// the word for that choice, listed first). True when the choice changed.
pub fn combo_or_none(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut Option<String>,
    columns: &[String],
    width: f32,
    none: &str,
) -> bool {
    let id = egui::Id::new(("colpick-opt", id));
    let before = value.clone();
    let inner = egui::ComboBox::from_id_salt(id)
        .selected_text(value.clone().unwrap_or_else(|| none.to_string()))
        .width(width)
        .height(360.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            let typed = find_box(ui, id, columns.len());
            // "none" is a choice, not a column: offered while nothing is typed
            if typed.trim().is_empty() && ui.selectable_label(value.is_none(), none).clicked() {
                *value = None;
                ui.close();
            }
            let names = matching(columns, &typed);
            if names.is_empty() {
                ui.label(RichText::new("No column has that in its name").weak());
            }
            for c in names {
                if ui.selectable_label(value.as_ref() == Some(c), c.as_str()).clicked() {
                    *value = Some(c.clone());
                    ui.close();
                }
            }
        });
    if inner.inner.is_none() {
        closed(ui, id);
    }
    *value != before
}

#[cfg(test)]
mod tests {
    use super::matching;

    fn cols() -> Vec<String> {
        ["timestamp", "name", "ComputerName", "aid", "ContextTimeStamp", "CommandLine"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn nothing_typed_lists_every_column_in_the_files_order() {
        let c = cols();
        assert_eq!(matching(&c, ""), c.iter().collect::<Vec<_>>());
        assert_eq!(matching(&c, "   ").len(), 6);
    }

    #[test]
    fn typing_keeps_the_columns_with_it_whatever_the_letter_case_starts_first() {
        let c = cols();
        // "time": starts-with first, then the ones that only contain it
        assert_eq!(matching(&c, "time"), vec!["timestamp", "ContextTimeStamp"]);
        assert_eq!(matching(&c, "NAME"), vec!["name", "ComputerName"]);
        assert_eq!(matching(&c, "com"), vec!["ComputerName", "CommandLine"]);
    }

    #[test]
    fn a_name_no_column_has_gives_an_empty_list() {
        assert!(matching(&cols(), "zzz").is_empty());
    }
}

#[cfg(test)]
mod scroll_tests {
    use eframe::egui;

    /// Draws a short scrolling list whose content asks to be scrolled to a
    /// place far below it, the way a text box does when it takes the cursor.
    /// Gives back how far the list scrolled.
    fn offset_after(quiet: bool) -> f32 {
        let ctx = egui::Context::default();
        let mut offset = 0.0;
        for _ in 0..4 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                let out = egui::ScrollArea::vertical()
                    .id_salt("outer")
                    .max_height(100.0)
                    .show(ui, |ui| {
                        ui.allocate_space(egui::vec2(50.0, 600.0));
                        let ask = |ui: &mut egui::Ui| {
                            let far = egui::Rect::from_min_size(
                                egui::pos2(0.0, 500.0),
                                egui::vec2(10.0, 10.0),
                            );
                            ui.scroll_to_rect(far, None);
                        };
                        if quiet {
                            super::keeps_its_scrolling_to_itself(ui, "q", ask);
                        } else {
                            ask(ui);
                        }
                    });
                offset = out.state.offset.y;
            });
            // a headless frame: its textures are not used
            out.textures_delta.clear();
        }
        offset
    }

    #[test]
    fn a_find_box_does_not_scroll_the_list_it_sits_in() {
        // the fault: a request made inside moves the list around it
        assert!(offset_after(false) > 0.0);
        // inside the wrapper it moves nothing
        assert_eq!(offset_after(true), 0.0);
    }
}

#[cfg(test)]
mod rest_tests {
    use eframe::egui;

    /// Opens a column list, rests the pointer on its find box, and gives back
    /// where the find box was drawn on each frame after that.
    fn find_box_tops() -> Vec<f32> {
        let ctx = egui::Context::default();
        let columns: Vec<String> = (0..40).map(|i| format!("column_{i}")).collect();
        let mut value = String::new();
        let mut button = egui::Rect::NOTHING;
        let mut tops: Vec<f32> = Vec::new();
        let mut events: Vec<egui::Event> = Vec::new();
        for frame in 0..40 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 700.0),
                )),
                time: Some(frame as f64 / 60.0),
                events: std::mem::take(&mut events),
                ..Default::default()
            };
            let mut top = None;
            let mut out = ctx.run_ui(input, |ui| {
                ui.add_space(40.0);
                let id = egui::Id::new(("colpick", "t"));
                let r = egui::ComboBox::from_id_salt(id)
                    .selected_text("choose a column")
                    .width(160.0)
                    .height(360.0)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show_ui(ui, |ui| {
                        top = Some(ui.cursor().top());
                        let typed = super::find_box(ui, id, columns.len());
                        for c in super::matching(&columns, &typed) {
                            if ui.selectable_label(value == *c, c.as_str()).clicked() {
                                value = c.clone();
                            }
                        }
                    });
                button = r.response.rect;
            });
            out.textures_delta.clear();
            if let Some(t) = top {
                tops.push(t);
            }
            let at = button.center();
            match frame {
                1 => events.push(egui::Event::PointerMoved(at)),
                2 => events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                }),
                3 => events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }),
                // then the pointer rests on the find box, just under the button
                f if f >= 6 => events.push(egui::Event::PointerMoved(egui::pos2(
                    at.x,
                    button.bottom() + 20.0,
                ))),
                _ => {}
            }
        }
        tops
    }

    /// Opens a column list with `combo`, clicks in its find box, and says
    /// whether the list is still open some frames later.
    fn open_after_a_click_in_the_find_box() -> bool {
        let ctx = egui::Context::default();
        let columns: Vec<String> = (0..40).map(|i| format!("column_{i}")).collect();
        let mut value = String::new();
        let mut button = egui::Rect::NOTHING;
        let mut events: Vec<egui::Event> = Vec::new();
        let mut open = false;
        let click = |at: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for frame in 0..30 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 700.0),
                )),
                time: Some(frame as f64 / 60.0),
                events: std::mem::take(&mut events),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                ui.add_space(40.0);
                let r = ui
                    .horizontal(|ui| {
                        super::combo(ui, "t", &mut value, &columns, 160.0, "choose a column");
                    })
                    .response;
                button = r.rect;
            });
            out.textures_delta.clear();
            open = egui::Popup::is_any_open(&ctx);
            let at = egui::pos2(button.left() + 30.0, button.center().y);
            let find = egui::pos2(button.left() + 40.0, button.bottom() + 20.0);
            match frame {
                1 => events.push(egui::Event::PointerMoved(at)),
                2 => events.push(click(at, true)),
                3 => events.push(click(at, false)),
                8 => events.push(egui::Event::PointerMoved(find)),
                10 => events.push(click(find, true)),
                11 => events.push(click(find, false)),
                _ => {}
            }
            if frame == 7 {
                assert!(open, "the list did not open");
            }
        }
        open
    }

    #[test]
    fn a_click_in_the_find_box_leaves_the_list_open() {
        assert!(open_after_a_click_in_the_find_box());
    }

    #[test]
    fn a_list_stays_where_it_is_while_the_pointer_rests_on_its_find_box() {
        let tops = find_box_tops();
        assert!(tops.len() > 20, "the list did not open: {tops:?}");
        let settled = &tops[4..];
        assert!(
            settled.iter().all(|t| (*t - settled[0]).abs() < 0.5),
            "the find box moved: {tops:?}"
        );
    }
}
