//! Every action of the app by name, in one list: the labelled menus of the top
//! bar draw from it, and Ctrl+K finds any of them, or any column, by typing a
//! few letters of it. Nothing to remember about where a button lives.

use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};

/// What the app can be asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenFile,
    Summarise,
    ChooseColumns,
    ColumnsView,
    SmartColumns,
    EveryColumn,
    AddColumn,
    RowView,
    Fields,
    FoldTimeRail,
    Unusual,
    Explain,
    WhatsNew,
    TimelineSignals,
    Calendar,
    Follow,
    SearchList,
    SearchCells,
    QueryWorkspace,
    ColourRules,
    SaveSearch,
    Theme,
    Density,
    GridLines,
    Export,
}

/// Each command, its words, and where else it is (a shortcut, a menu).
pub const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::OpenFile, "Open a file", "Ctrl+O"),
    (
        Command::Summarise,
        "Summarise: group the rows and count",
        "Summarise",
    ),
    (
        Command::ChooseColumns,
        "Choose columns: pick, hide and order them",
        "Columns menu",
    ),
    (
        Command::ColumnsView,
        "Columns view: every column at a glance, what it holds",
        "Ctrl+2",
    ),
    (
        Command::SmartColumns,
        "Columns: the smart view (the most telling ones)",
        "Columns menu",
    ),
    (
        Command::EveryColumn,
        "Columns: every column, in file order",
        "Columns menu",
    ),
    (
        Command::AddColumn,
        "Add a column: split, combine, clean, convert, decode",
        "Columns menu",
    ),
    (
        Command::RowView,
        "Row view: every field of the selected row",
        "Space",
    ),
    (Command::Fields, "Fields and tags panel", "Ctrl+B"),
    (
        Command::FoldTimeRail,
        "Time rail: fold it away, or bring it back",
        "Ctrl+Shift+B",
    ),
    (
        Command::Unusual,
        "Unusual rows: the rarest rows, with the reasons",
        "Analyse menu",
    ),
    (
        Command::Explain,
        "What sets these rows apart?",
        "Analyse menu",
    ),
    (
        Command::WhatsNew,
        "What's new in this period?",
        "Analyse menu",
    ),
    (
        Command::TimelineSignals,
        "Timeline signals: silences, rate changes, repeats",
        "Analyse menu",
    ),
    (
        Command::Calendar,
        "Calendar: pick days or hours",
        "Analyse menu",
    ),
    (Command::Follow, "Follow related rows", "Analyse menu"),
    (
        Command::SearchList,
        "Search a whole list of keywords",
        "Analyse menu",
    ),
    (
        Command::SearchCells,
        "Search every cell for a keyword",
        "Ctrl+Shift+F",
    ),
    (
        Command::QueryWorkspace,
        "Query workspace: write, check and re-run queries",
        "Ctrl+E",
    ),
    (
        Command::ColourRules,
        "Colour rules: tint rows or cells that match",
        "View menu",
    ),
    (
        Command::SaveSearch,
        "Save the current search and filters",
        "View menu",
    ),
    (Command::Theme, "Light or dark theme", "View menu"),
    (Command::Density, "Compact or comfortable rows", "View menu"),
    (Command::GridLines, "Grid lines on or off", "View menu"),
    (Command::Export, "Export the rows shown", "status bar"),
];

/// One line the palette offers.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Run(Command),
    /// Bring this column into view (shown first if it was hidden).
    Column(usize),
}

/// The lines matching what was typed: every word typed is in the line (any
/// case, any order); commands first, then columns. Nothing typed: every command.
pub fn find(typed: &str, columns: &[String]) -> Vec<(Item, String)> {
    let words: Vec<String> = typed.split_whitespace().map(str::to_lowercase).collect();
    let fits = |text: &str| {
        let t = text.to_lowercase();
        words.iter().all(|w| t.contains(w.as_str()))
    };
    let mut out: Vec<(Item, String)> = COMMANDS
        .iter()
        .filter(|(_, label, _)| fits(label))
        .map(|(c, label, _)| (Item::Run(*c), label.to_string()))
        .collect();
    if !words.is_empty() {
        out.extend(
            columns
                .iter()
                .enumerate()
                .filter(|(_, n)| fits(n))
                .map(|(i, n)| (Item::Column(i), format!("Go to column {n}"))),
        );
    }
    out
}

/// Where else a command lives (its shortcut or menu), for the palette's right side.
pub fn hint(c: Command) -> &'static str {
    COMMANDS
        .iter()
        .find(|(k, _, _)| *k == c)
        .map_or("", |(_, _, h)| h)
}

#[derive(Default)]
pub struct PaletteState {
    pub typed: String,
    pub at: usize,
    focused: bool,
}

/// The palette, near the top of the window; returns the line chosen, if any,
/// and false when it should close.
pub fn show(
    ctx: &egui::Context,
    pal: &Palette,
    st: &mut PaletteState,
    columns: &[String],
) -> (Option<Item>, bool) {
    let mut chosen = None;
    let mut open = true;
    let found = find(&st.typed, columns);
    if st.at >= found.len() {
        st.at = found.len().saturating_sub(1);
    }
    let screen = ctx.content_rect();
    let w = 620.0_f32.min(screen.width() - 32.0);
    egui::Window::new("Find a command or a column")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        // as tall as what it lists, up to most of the window (then it scrolls)
        .fixed_size([w, 0.0])
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
        .show(ctx, |ui| {
            ui.set_width(w);
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.typed)
                    .hint_text("Type what you want to do, or a column's name")
                    .desired_width(f32::INFINITY),
            );
            if !st.focused {
                r.request_focus();
                st.focused = true;
            }
            if r.changed() {
                st.at = 0;
            }
            let (up, down, enter, esc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if up {
                st.at = st.at.saturating_sub(1);
            }
            if down && st.at + 1 < found.len() {
                st.at += 1;
            }
            if enter {
                chosen = found.get(st.at).map(|f| f.0.clone());
            }
            if esc {
                open = false;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height((screen.height() * 0.6).max(260.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (k, (item, label)) in found.iter().enumerate() {
                        let on = k == st.at;
                        // the highlight goes behind the words, never over them
                        let behind = ui.painter().add(egui::Shape::Noop);
                        let resp = ui
                            .horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(label).color(pal.text)).wrap(),
                                );
                                if let Item::Run(c) = item {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(hint(*c))
                                                    .font(theme::small_font())
                                                    .color(pal.text2),
                                            );
                                        },
                                    );
                                }
                            })
                            .response
                            .interact(egui::Sense::click());
                        if on {
                            ui.painter().set(
                                behind,
                                egui::Shape::rect_filled(
                                    resp.rect.expand2(egui::vec2(4.0, 1.0)),
                                    egui::CornerRadius::same(3),
                                    pal.selection,
                                ),
                            );
                            if down || up {
                                resp.scroll_to_me(None);
                            }
                        }
                        if resp.clicked() {
                            chosen = Some(item.clone());
                        }
                    }
                    if found.is_empty() {
                        ui.label(RichText::new("Nothing has those words.").color(pal.muted));
                    }
                });
        });
    if chosen.is_some() {
        open = false;
    }
    (chosen, open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_words_find_commands_then_columns_in_any_order_and_case() {
        let cols: Vec<String> = ["timestamp", "UserName", "user.name", "Host"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // nothing typed: every command, no columns
        let all = find("", &cols);
        assert_eq!(all.len(), COMMANDS.len());
        assert!(all.iter().all(|(i, _)| matches!(i, Item::Run(_))));
        // words in any order and case
        let f = find("APART sets", &cols);
        assert_eq!(
            f.first().map(|x| x.0.clone()),
            Some(Item::Run(Command::Explain))
        );
        // a column by part of its name, after any command that also fits
        let f = find("user", &cols);
        let cols_found: Vec<&Item> = f
            .iter()
            .map(|x| &x.0)
            .filter(|i| matches!(i, Item::Column(_)))
            .collect();
        assert_eq!(cols_found, vec![&Item::Column(1), &Item::Column(2)]);
        assert!(
            f.iter().position(|x| matches!(x.0, Item::Column(_)))
                > f.iter().position(|x| matches!(x.0, Item::Run(_)))
                || !f.iter().any(|x| matches!(x.0, Item::Run(_)))
        );
        // nothing fits
        assert!(find("zzqq", &cols).is_empty());
    }

    #[test]
    fn every_command_has_words_and_a_place() {
        for (c, label, hint) in COMMANDS {
            assert!(!label.is_empty() && !hint.is_empty(), "{c:?}");
        }
    }
}
