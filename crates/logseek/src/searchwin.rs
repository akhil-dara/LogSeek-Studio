//! The Search window: type a keyword and see every cell that has it — how
//! many, in which columns, each match in its cell with the keyword marked.
//! Click a match (or step with the arrow keys) to go to that cell in the
//! grid. Nothing is filtered out, and nothing is cut off.

use crate::bridge::{Bridge, Cmd, SearchRow, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::query::search::{cell_spans, SearchOpts};

type Sent = (String, bool, bool, Option<String>);

#[derive(Default)]
pub struct SearchWin {
    pub text: String,
    pub case: bool,
    pub word: bool,
    /// Only in this column.
    pub col: Option<String>,
    id: u64,
    sent: Option<Sent>,
    /// When the text or an option last changed (UI time), for a short pause before searching.
    edited: Option<f64>,
    rows: Vec<SearchRow>,
    asked: u64,
    seen_page: usize,
    /// Height of each listed result once drawn, so results off screen are skipped.
    heights: Vec<f32>,
    /// A column name was clicked: go to that column.
    name_jump: Option<usize>,
    /// The result stepped to with the arrow keys or clicked.
    sel: Option<usize>,
    locate: u64,
    jump: Option<(u64, usize)>,
    scroll_to_sel: bool,
    pub focus: bool,
    /// The words searched for in the box at the top: each is offered here.
    pub terms: Vec<String>,
    /// Opened from the search at the top ("Where it is"): it shows that
    /// search's own conditions and asks for nothing again.
    pub from_search: bool,
    /// The conditions with a text to find in cells: (how it reads, the text, its column).
    pub marks: Vec<(String, String, Option<String>)>,
    /// The conditions with no text to mark, in words.
    pub others: Vec<String>,
    /// The search these conditions were read from (to read them again when it changes).
    pub for_query: String,
    /// The result the matches were found in: found again when the rows change.
    seen_gen: u64,
}

/// Go to this place in the grid, and this column.
pub struct Jump {
    /// The row's place in the grid (`None`: only go to the column).
    pub pos: Option<u64>,
    pub col: usize,
}

/// "31 cells in 23 rows" and friends.
pub fn summary_line(cells: u64, rows: u64) -> String {
    match (cells, rows) {
        (0, _) => "No cell has it".to_string(),
        (1, _) => "1 cell has it".to_string(),
        (c, 1) => format!("{} cells have it, all in 1 row", group(c)),
        (c, r) => format!("{} cells have it, in {} rows", group(c), group(r)),
    }
}

/// `v` with the keyword's places marked.
fn marked(
    v: &str,
    o: &SearchOpts,
    pal: &Palette,
    font: egui::FontId,
    wrap: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    // line breaks and tabs become spaces (one byte each, so places stay true)
    let flat: String = v
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    let plain = TextFormat::simple(font.clone(), pal.text);
    let hit = TextFormat {
        background: pal.selection,
        ..TextFormat::simple(font, pal.accent_ink)
    };
    let mut at = 0;
    for (a, b) in cell_spans(&flat, o) {
        if a > at {
            job.append(&flat[at..a], 0.0, plain.clone());
        }
        job.append(&flat[a..b], 0.0, hit.clone());
        at = b;
    }
    if at < flat.len() {
        job.append(&flat[at..], 0.0, plain);
    }
    job
}

impl SearchWin {
    pub fn with_text(text: &str) -> SearchWin {
        SearchWin {
            text: text.to_string(),
            focus: true,
            edited: Some(f64::MIN),
            ..Default::default()
        }
    }

    fn now(&self) -> Sent {
        (
            self.text.trim().to_string(),
            self.case,
            self.word,
            self.col.clone(),
        )
    }

    /// Draw the window; returns `(still open, a place to go to)`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, Option<Jump>) {
        let mut open = true;
        let time = ctx.input(|i| i.time);
        let mut go: Option<usize> = None;
        let mut enter = false;
        let screen = ctx.content_rect();
        egui::Window::new("Search")
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, (screen.height() - 140.0).clamp(320.0, 620.0)])
            .default_pos(egui::pos2(
                (screen.right() - 664.0).max(screen.left() + 8.0),
                // right under the top bar: the strip below it is covered whole, never half
                screen.top() + 40.0,
            ))
            .show(ctx, |ui| {
                ui.set_min_size(egui::vec2(560.0, 300.0));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if self.from_search { "Where it is" } else { "Search" })
                            .font(crate::fonts::strong(14.0)),
                    );
                    ui.label(
                        RichText::new(if self.from_search {
                            "each part of your search, in the rows it keeps"
                        } else {
                            "every cell of the rows on show; no row is filtered out"
                        })
                            .font(theme::small_font())
                            .color(pal.text2),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::app::icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                            open = false;
                        }
                    });
                });
                let before = self.now();
                if self.from_search {
                    // the search as it was made: each part that has a text to find
                    // is one click; nothing is typed or ticked a second time
                    if self.marks.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new("This search has no word or value to mark in a cell.")
                                    .color(pal.text2),
                            )
                            .wrap(),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (label, text, col) in self.marks.clone() {
                            let on = self.text.trim() == text && self.col == col;
                            if ui
                                .selectable_label(on, label.as_str())
                                .on_hover_text("Show the cells that hold this part of the search")
                                .clicked()
                                && !on
                            {
                                self.text = text;
                                self.col = col;
                                enter = true;
                            }
                        }
                    });
                    if !self.others.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Also in your search, with nothing to mark in a cell: {}",
                                    self.others.join("; ")
                                ))
                                .font(theme::small_font())
                                .color(pal.text2),
                            )
                            .wrap(),
                        );
                    }
                    // the down and up arrows step through the matches
                    if !self.rows.is_empty() && !ui.ctx().egui_wants_keyboard_input() {
                        let (down, up) = ui.input(|i| {
                            (
                                i.key_pressed(egui::Key::ArrowDown),
                                i.key_pressed(egui::Key::ArrowUp),
                            )
                        });
                        let n = self.rows.len();
                        if down {
                            go = Some(self.sel.map_or(0, |s| (s + 1).min(n - 1)));
                        } else if up {
                            go = Some(self.sel.map_or(0, |s| s.saturating_sub(1)));
                        }
                    }
                } else {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.text)
                            .hint_text(format!("{}  Keyword to look for in every cell", ph::MAGNIFYING_GLASS))
                            .desired_width(300.0),
                    );
                    if std::mem::take(&mut self.focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter = true;
                        self.focus = true;
                    }
                    // step through the results without leaving the box
                    if r.has_focus() && !self.rows.is_empty() {
                        let (down, up) = ui.input(|i| {
                            (
                                i.key_pressed(egui::Key::ArrowDown),
                                i.key_pressed(egui::Key::ArrowUp),
                            )
                        });
                        let n = self.rows.len();
                        if down {
                            go = Some(self.sel.map_or(0, |s| (s + 1).min(n - 1)));
                        } else if up {
                            go = Some(self.sel.map_or(0, |s| s.saturating_sub(1)));
                        }
                    }
                    ui.checkbox(&mut self.case, "Capitals matter")
                        .on_hover_text("Root and root are different");
                    ui.checkbox(&mut self.word, "Whole words only")
                        .on_hover_text("root, but not rootkit");
                });
                if !self.terms.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("From your search").color(pal.text2));
                        for t in self.terms.clone() {
                            if ui
                                .selectable_label(self.text.trim() == t, t.as_str())
                                .on_hover_text("Where this word is: how many cells, in which columns")
                                .clicked()
                            {
                                self.text = t;
                                self.col = None;
                                enter = true;
                            }
                        }
                    });
                }
                }
                if self.now() != before {
                    self.edited = Some(time);
                }
                // search after a short pause in typing, or at once on Enter
                let want = self.now();
                // the rows on show changed (another search, a filter): found again in them
                if self.seen_gen != snap.generation {
                    self.seen_gen = snap.generation;
                    if self.sent.is_some() {
                        self.sent = None;
                        enter = true;
                    }
                }
                let due = self.edited.is_some_and(|t| time - t >= 0.35) || enter;
                if !want.0.is_empty() && self.sent.as_ref() != Some(&want) && due {
                    self.id += 1;
                    self.sent = Some(want.clone());
                    self.edited = None;
                    self.rows.clear();
                    self.heights.clear();
                    self.asked = 0;
                    self.sel = None;
                    bridge.send(Cmd::Search {
                        id: self.id,
                        text: want.0.clone(),
                        case: want.1,
                        word: want.2,
                        col: want.3.clone(),
                    });
                } else if self.edited.is_some() {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
                }
                if want.0.is_empty() {
                    ui.label(
                        RichText::new("Every cell that has the keyword is listed here, with the keyword marked. Nothing is filtered out of the grid.")
                            .color(pal.text2),
                    );
                    return;
                }
                // column names that have it: click to go to that column
                {
                    let needle = want.0.to_lowercase();
                    let cols: Vec<usize> = (0..snap.columns.len())
                        .filter(|&c| snap.columns[c].to_lowercase().contains(&needle))
                        .collect();
                    if !cols.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(if cols.len() == 1 {
                                    "1 column name has it:".to_string()
                                } else {
                                    format!("{} column names have it:", cols.len())
                                })
                                .color(pal.text2),
                            );
                            for c in cols {
                                if ui
                                    .link(&snap.columns[c])
                                    .on_hover_text("Go to this column")
                                    .clicked()
                                {
                                    self.name_jump = Some(c);
                                }
                            }
                        });
                    }
                }
                // pages of results as they arrive
                if let Some(p) = snap.search_page.as_ref().filter(|p| p.id == self.id) {
                    let stamp = std::sync::Arc::as_ptr(p) as usize;
                    if stamp != self.seen_page && p.start == self.rows.len() as u64 {
                        self.seen_page = stamp;
                        self.rows.extend(p.rows.iter().cloned());
                    }
                }
                let Some(out) = snap.search.as_ref().filter(|o| o.id == self.id) else {
                    ui.label(RichText::new("Searching").color(pal.text2));
                    return;
                };
                if let Some(e) = &out.error {
                    ui.label(RichText::new(format!("Cannot search for that: {e}")).color(pal.danger));
                    return;
                }
                ui.label(RichText::new(summary_line(out.cells, out.rows)).font(crate::fonts::strong(13.0)));
                // where it is: one chip per column, click to look only there
                if !out.per_col.is_empty() || self.col.is_some() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("In").color(pal.text2));
                        if ui
                            .selectable_label(self.col.is_none(), "all columns")
                            .clicked()
                        {
                            self.col = None;
                            self.edited = Some(f64::MIN);
                        }
                        for (name, n) in &out.per_col {
                            let on = self.col.as_deref() == Some(name.as_str());
                            if ui
                                .selectable_label(on, format!("{name}  {}", group(*n)))
                                .on_hover_text("Look only in this column")
                                .clicked()
                            {
                                self.col = if on { None } else { Some(name.clone()) };
                                self.edited = Some(f64::MIN);
                            }
                        }
                    });
                }
                ui.label(
                    RichText::new(if self.from_search {
                        "One line per match: the row's number in the file, the column, the cell. Click one to go to it; the down and up arrows step through them."
                    } else {
                        "One line per match: the row's number in the file, the column, the cell. Click one to go to it; with the box active, the down and up arrows step through them."
                    })
                        .font(theme::small_font())
                        .color(pal.text2),
                );
                ui.separator();
                let opts = SearchOpts {
                    text: want.0.clone(),
                    case: want.1,
                    word: want.2,
                    col: None,
                };
                let total = out.rows;
                let line_h = 20.0;
                ui.spacing_mut().item_spacing.y = 1.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let full_w = ui.available_width();
                        let wrap = (full_w - 250.0).max(160.0);
                        if self.heights.len() < self.rows.len() {
                            self.heights.resize(self.rows.len(), 0.0);
                        }
                        for i in 0..self.rows.len() {
                            let selected = self.sel == Some(i);
                            let known = self.heights[i];
                            let est = if known > 0.0 {
                                known
                            } else {
                                (line_h + 1.0) * self.rows[i].cells.len().max(1) as f32
                            };
                            let at = ui.cursor().min;
                            let probe = egui::Rect::from_min_size(at, egui::vec2(full_w, est));
                            // off screen: keep its space, draw nothing (200,000 results stay quick)
                            if !ui.is_rect_visible(probe) && !(selected && self.scroll_to_sel) {
                                ui.allocate_space(egui::vec2(full_w, est));
                                continue;
                            }
                            if selected {
                                ui.painter().rect_filled(probe, egui::CornerRadius::same(2), pal.selection);
                            }
                            let r = self.rows[i].clone();
                            let when = r
                                .time
                                .as_ref()
                                .map(|t| crate::grid::utc_cell(t).unwrap_or_else(|| t.clone()));
                            for (k, (c, v)) in r.cells.iter().enumerate() {
                                ui.horizontal_top(|ui| {
                                    ui.add_sized(
                                        [64.0, line_h],
                                        egui::Label::new(
                                            RichText::new(if k == 0 {
                                                group(r.row + 1)
                                            } else {
                                                String::new()
                                            })
                                            .strong()
                                            .color(if selected { pal.accent_ink } else { pal.text }),
                                        )
                                        .halign(egui::Align::RIGHT),
                                    );
                                    let name = snap.columns.get(*c).cloned().unwrap_or_default();
                                    ui.add_sized(
                                        [150.0, line_h],
                                        egui::Label::new(
                                            RichText::new(name).font(theme::small_font()).color(pal.text2),
                                        )
                                        .wrap()
                                        .halign(egui::Align::LEFT),
                                    );
                                    let cell = ui
                                        .add(
                                            egui::Label::new(marked(v, &opts, pal, theme::data_font(), wrap))
                                                .sense(egui::Sense::click()),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .on_hover_text(match &when {
                                            Some(t) => format!("Row {} · {t}\nClick to go to this cell", group(r.row + 1)),
                                            None => format!("Row {}\nClick to go to this cell", group(r.row + 1)),
                                        });
                                    if cell.clicked() {
                                        go = Some(i);
                                        self.jump = Some((0, *c));
                                    }
                                });
                            }
                            let h = (ui.cursor().min.y - at.y).max(line_h);
                            self.heights[i] = h;
                            if selected && std::mem::take(&mut self.scroll_to_sel) {
                                ui.scroll_to_rect(
                                    egui::Rect::from_min_size(at, egui::vec2(full_w, h)),
                                    Some(egui::Align::Center),
                                );
                            }
                        }
                        // more to come: ask when the end of what is loaded comes into view
                        if (self.rows.len() as u64) < total {
                            let more = ui.label(
                                RichText::new(format!(
                                    "Reading more: {} of {} rows listed",
                                    group(self.rows.len() as u64),
                                    group(total)
                                ))
                                .color(pal.text2),
                            );
                            if ui.is_rect_visible(more.rect) && self.asked <= self.rows.len() as u64 {
                                self.asked = self.rows.len() as u64 + 1;
                                bridge.send(Cmd::SearchPage {
                                    id: self.id,
                                    start: self.rows.len() as u64,
                                    len: 500,
                                });
                            }
                        }
                    });
            });
        // go to a result: ask the engine where its row sits in the grid now
        if let Some(i) = go {
            if let Some(r) = self.rows.get(i) {
                let col = match self.jump.take() {
                    Some((_, c)) => c,
                    None => r.cells.first().map_or(0, |c| c.0),
                };
                self.sel = Some(i);
                self.scroll_to_sel = true;
                self.locate += 1;
                self.jump = Some((self.locate, col));
                bridge.send(Cmd::Locate {
                    id: self.locate,
                    row: r.row,
                });
            }
        }
        let mut jump = None;
        if let (Some((id, col)), Some((lid, pos))) = (self.jump, snap.located) {
            if id == lid && id > 0 {
                self.jump = None;
                if let Some(pos) = pos {
                    jump = Some(Jump {
                        pos: Some(pos),
                        col,
                    });
                }
            }
        }
        if let Some(col) = self.name_jump.take() {
            jump = Some(Jump { pos: None, col });
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            open = false;
        }
        (open, jump)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_cells_and_rows_in_words() {
        assert_eq!(summary_line(0, 0), "No cell has it");
        assert_eq!(summary_line(1, 1), "1 cell has it");
        assert_eq!(summary_line(3, 1), "3 cells have it, all in 1 row");
        assert_eq!(summary_line(1204, 987), "1,204 cells have it, in 987 rows");
    }

    #[test]
    fn the_keyword_is_marked_in_place_and_the_whole_value_kept() {
        let o = SearchOpts {
            text: "root".into(),
            ..Default::default()
        };
        let job = marked(
            "su\troot -c id",
            &o,
            &crate::theme::DARK,
            theme::data_font(),
            400.0,
        );
        assert_eq!(job.text, "su root -c id");
        // before, the keyword, after
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[1].format.color, crate::theme::DARK.accent_ink);
        assert_eq!(job.sections[0].format.color, crate::theme::DARK.text);
    }
}
