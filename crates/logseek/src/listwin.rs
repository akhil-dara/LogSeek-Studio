//! "Search a list": paste any number of keywords (indicators, names,
//! addresses) and see, from ONE pass over the rows on screen, which of them
//! are in the file at all, in how many rows and cells, and in which columns.
//! The ones that found nothing are listed too: knowing a value is absent is
//! an answer.

use crate::bridge::{Bridge, Cmd, ListOut, Snapshot};
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum ListAction {
    None,
    /// Replace the search with this.
    Search(String),
    /// Open the Search window on this keyword: every cell that holds it,
    /// one line per cell, so a row with it in several fields shows each one.
    Cells(String),
    /// Show these rows (the ones with any keyword) in the grid, with the
    /// keywords that were found and how they were searched (capitals, whole words).
    Show {
        ids: Arc<Vec<u64>>,
        found: Vec<String>,
        case: bool,
        word: bool,
    },
}

struct Run {
    id: u64,
    terms: Arc<Vec<String>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    /// How this run was searched.
    case: bool,
    word: bool,
    started: f64,
    /// Seconds it took, once answered.
    took: Option<f64>,
}

#[derive(Default)]
pub struct ListWin {
    pub text: String,
    pub case: bool,
    pub word: bool,
    /// Run as soon as the window shows (dev switch).
    pub run_now: bool,
    next_id: u64,
    run: Option<Run>,
    /// 0 found, 1 not found, 2 all.
    tab: u8,
    find: String,
    picked: Option<usize>,
    want_rows: bool,
    /// Close the window once the rows are showing (dev switch).
    close_on_show: bool,
    /// A table file being loaded: which of its columns holds the keywords?
    load: Option<Loaded>,
    /// Why a file could not be loaded.
    load_error: Option<String>,
}

/// A file of keywords read into rows of cells.
pub struct Loaded {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    /// The first row names the columns.
    pub header: bool,
    pub col: usize,
}

/// Text of a file whatever it was saved as: UTF-8 (with or without a mark
/// in front) or UTF-16 with its mark (what some Windows tools write).
pub fn file_text(bytes: &[u8]) -> String {
    let u16s = |le: bool| -> String {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => u16s(true),
        [0xFE, 0xFF, ..] => u16s(false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// A delimited file as rows of cells: quoted cells may hold the delimiter,
/// line breaks and doubled quotes. Empty lines are left out.
pub fn table_rows(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut it = text.chars().peekable();
    let mut end_row = |row: &mut Vec<String>, cell: &mut String| {
        row.push(std::mem::take(cell));
        if row.iter().any(|c| !c.trim().is_empty()) {
            rows.push(std::mem::take(row));
        } else {
            row.clear();
        }
    };
    while let Some(c) = it.next() {
        if quoted {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    cell.push('"');
                    it.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' {
            end_row(&mut row, &mut cell);
        } else if c != '\r' {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        end_row(&mut row, &mut cell);
    }
    rows
}

/// The delimiter a table file most likely uses, from its first line: the
/// one that splits it into the most cells; `None` = one keyword per line.
pub fn guess_delim(text: &str) -> Option<char> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    [',', '\t', ';', '|']
        .into_iter()
        .map(|d| (d, table_rows(first, d).first().map_or(1, Vec::len)))
        .filter(|(_, n)| *n > 1)
        .max_by_key(|(_, n)| *n)
        .map(|(d, _)| d)
}

/// Whether the first row reads as column names: none of its cells looks
/// like a value that the rows below also hold (an address, a hash, a number).
pub fn looks_like_header(rows: &[Vec<String>]) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    rows.len() > 1
        && first.iter().all(|c| {
            let c = c.trim();
            !c.is_empty()
                && c.len() <= 40
                && !c.contains(['.', ':', '/', '\\', '@'])
                && c.parse::<f64>().is_err()
                && !(c.len() >= 32 && c.chars().all(|x| x.is_ascii_hexdigit()))
        })
}

impl Loaded {
    /// The keywords of the chosen column, one per line.
    pub fn column_text(&self) -> String {
        self.rows
            .iter()
            .skip(self.header as usize)
            .filter_map(|r| r.get(self.col))
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Column names: the first row's, or "Column 1", "Column 2".
    pub fn names(&self) -> Vec<String> {
        let n = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        (0..n)
            .map(
                |i| match (self.header, self.rows.first().and_then(|r| r.get(i))) {
                    (true, Some(h)) if !h.trim().is_empty() => h.trim().to_string(),
                    _ => format!("Column {}", i + 1),
                },
            )
            .collect()
    }
}

/// The keywords of a pasted list: one per line, trimmed, no empties, no
/// repeats (a keyword that differs only in capitals is a repeat unless
/// capitals matter), in the order given.
pub fn keywords(text: &str, case: bool) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for l in text.lines() {
        // a byte-order mark (from a file saved by some editors) is not part of a keyword
        let t = l.trim().trim_matches('\u{feff}').trim();
        if t.is_empty() {
            continue;
        }
        let key = if case {
            t.to_string()
        } else {
            t.to_lowercase()
        };
        if seen.insert(key) {
            out.push(t.to_string());
        }
    }
    out
}

/// "132 of 2,000 keywords were found, in 48,210 of 603,813 rows."
pub fn summary(found: usize, total: usize, rows: u64, of: u64) -> String {
    format!(
        "{} of {} {} {} found, in {} of {} {}.",
        group(found as u64),
        group(total as u64),
        if total == 1 { "keyword" } else { "keywords" },
        if found == 1 { "was" } else { "were" },
        group(rows),
        group(of),
        if of == 1 { "row" } else { "rows" }
    )
}

/// How much of the data had to be read: "Read 14% of the data: the index
/// showed that the rest cannot hold any of these keywords." `None` when
/// everything was read.
pub fn read_line(columns_read: u64, columns_skipped: u64) -> Option<String> {
    let all = columns_read + columns_skipped;
    if all == 0 || columns_skipped == 0 {
        return None;
    }
    let pct = columns_read as f64 * 100.0 / all as f64;
    let share = if columns_read == 0 {
        "none".to_string()
    } else if pct < 1.0 {
        "under 1 %".to_string()
    } else {
        format!("{pct:.0}%")
    };
    Some(format!(
        "Read {share} of the data: the index showed that the rest cannot hold any of these keywords."
    ))
}

/// The table as text for a spreadsheet: keyword, rows, cells, columns.
pub fn table_text(terms: &[String], out: &ListOut) -> String {
    let mut s = String::from("keyword\trows\tcells\tcolumns\n");
    for (t, h) in terms.iter().zip(&out.terms) {
        let cols: Vec<String> = h.cols.iter().map(|(c, n)| format!("{c} {n}")).collect();
        s.push_str(&format!(
            "{t}\t{}\t{}\t{}\n",
            h.rows,
            h.cells,
            cols.join("; ")
        ));
    }
    s
}

impl ListWin {
    /// Keywords join what is already in the box (a list can come from several files).
    fn add_text(&mut self, more: &str) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(more);
    }

    /// Read a file of keywords: one per line, or a table whose column the user picks.
    pub fn load_file(&mut self, path: &std::path::Path) {
        self.load_error = None;
        self.load = None;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.load_error = Some(format!("{name} could not be read: {e}"));
                return;
            }
        };
        let text = file_text(&bytes);
        match guess_delim(&text) {
            // one keyword per line
            None => self.add_text(&text),
            Some(d) => {
                let rows = table_rows(&text, d);
                let header = looks_like_header(&rows);
                self.load = Some(Loaded {
                    name,
                    rows,
                    header,
                    col: 0,
                });
            }
        }
    }

    fn start(&mut self, bridge: &Bridge, now: f64) {
        let terms = Arc::new(keywords(&self.text, self.case));
        if terms.is_empty() {
            return;
        }
        if let Some(r) = &self.run {
            r.stop.store(true, Ordering::SeqCst);
        }
        self.next_id += 1;
        let run = Run {
            id: self.next_id,
            terms: terms.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            case: self.case,
            word: self.word,
            started: now,
            took: None,
        };
        bridge.send(Cmd::ListSearch {
            id: run.id,
            terms,
            case: self.case,
            word: self.word,
            stop: run.stop.clone(),
            done: run.done.clone(),
        });
        self.run = Some(run);
        self.picked = None;
        self.tab = 0;
        self.want_rows = false;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        bridge: &Bridge,
    ) -> (bool, ListAction) {
        let mut open = true;
        let mut act = ListAction::None;
        let now = ctx.input(|i| i.time);
        if std::mem::take(&mut self.run_now) {
            self.start(bridge, now);
        }
        // the answer to the run in hand
        let out: Option<Arc<ListOut>> = self
            .run
            .as_ref()
            .and_then(|r| snap.list.clone().filter(|o| o.id == r.id));
        if let (Some(r), Some(_)) = (self.run.as_mut(), &out) {
            r.took.get_or_insert(now - r.started);
        }
        // dev switch: LOGSEEK_SHOT_LIST_SHOW=1 presses "Show these rows" once the answer is in
        if let (Some(r), Some(o)) = (&self.run, &out) {
            if o.error.is_none() && crate::dev::var_os("LOGSEEK_SHOT_LIST_SHOW").is_some() {
                std::env::remove_var("LOGSEEK_SHOT_LIST_SHOW");
                self.want_rows = true;
                self.close_on_show = true;
                bridge.send(Cmd::ListRows(r.id));
            }
        }
        // the rows asked for have arrived: hand them to the grid
        if self.want_rows {
            if let (Some(r), Some((id, ids))) = (&self.run, &snap.list_rows) {
                if *id == r.id {
                    self.want_rows = false;
                    let found: Vec<String> = snap
                        .list
                        .as_ref()
                        .filter(|o| o.id == r.id)
                        .map(|o| {
                            r.terms
                                .iter()
                                .zip(&o.terms)
                                .filter(|(_, h)| h.rows > 0)
                                .map(|(t, _)| t.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    act = ListAction::Show {
                        ids: ids.clone(),
                        found,
                        case: r.case,
                        word: r.word,
                    };
                }
            }
        }
        // every sheet opens in the same place at the same size and stays
        // there; what it holds scrolls inside it
        crate::sheet::placed(egui::Window::new("search-a-list"), ctx, 980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Search a list").font(crate::fonts::strong(16.0)));
                    ui.label(
                        RichText::new("many keywords at once, in every column of the rows shown")
                            .color(pal.text2),
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
                egui::ScrollArea::vertical()
                    .id_salt("list-input")
                    .max_height(110.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.text)
                                .font(theme::data_font())
                                .hint_text("Paste the keywords here, one per line: addresses, hashes, names, anything")
                                .desired_rows(4)
                                .desired_width(f32::INFINITY),
                        );
                    });
                let n = keywords(&self.text, self.case).len();
                let running = self.run.as_ref().is_some_and(|_| out.is_none());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            group(n as u64),
                            if n == 1 { "keyword" } else { "keywords" }
                        ))
                        .color(pal.text2),
                    );
                    if ui
                        .button(format!("{}  Load from a file", ph::FOLDER_OPEN))
                        .on_hover_text("A text file with one keyword per line, or a CSV / TSV: you choose which column holds the keywords")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("A file of keywords")
                            .add_filter("Lists and tables", &["txt", "csv", "tsv", "lst", "ioc", "list"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                        {
                            self.load_file(&path);
                        }
                    }
                    ui.checkbox(&mut self.case, "Capitals matter");
                    ui.checkbox(&mut self.word, "Whole words only")
                        .on_hover_text("\"net\" is found in \"net use\", not in \"netsvcs\"");
                    if running {
                        if ui.button("Stop").clicked() {
                            if let Some(r) = &self.run {
                                r.stop.store(true, Ordering::SeqCst);
                            }
                        }
                    } else if ui
                        .add_enabled(
                            n > 0,
                            egui::Button::new(
                                RichText::new(format!(
                                    "{}  Search the {} rows shown",
                                    ph::MAGNIFYING_GLASS,
                                    group(snap.matched)
                                ))
                                .color(pal.on_accent),
                            )
                            .fill(pal.accent),
                        )
                        .clicked()
                    {
                        self.start(bridge, now);
                    }
                });
                if let Some(e) = &self.load_error {
                    ui.label(RichText::new(format!("{}  {e}", ph::WARNING)).color(pal.danger));
                }
                // a table was loaded: which column holds the keywords?
                let mut take: Option<bool> = None;
                if let Some(l) = self.load.as_mut() {
                    egui::Frame::new()
                        .fill(pal.surface2)
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            let names = l.names();
                            ui.label(
                                RichText::new(format!(
                                    "{} has {} columns. Which one holds the keywords?",
                                    l.name,
                                    group(names.len() as u64)
                                ))
                                .strong(),
                            );
                            ui.checkbox(&mut l.header, "The first row is column names");
                            egui::ScrollArea::vertical()
                                .id_salt("list-load-cols")
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    for (i, n) in names.iter().enumerate() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.radio_value(&mut l.col, i, RichText::new(n).strong());
                                            let sample: Vec<&str> = l
                                                .rows
                                                .iter()
                                                .skip(l.header as usize)
                                                .filter_map(|r| r.get(i))
                                                .map(|c| c.trim())
                                                .filter(|c| !c.is_empty())
                                                .take(3)
                                                .collect();
                                            ui.label(
                                                RichText::new(sample.join("   ·   "))
                                                    .font(theme::data_font())
                                                    .color(pal.text2),
                                            );
                                        });
                                    }
                                });
                            ui.horizontal(|ui| {
                                let n = keywords(&l.column_text(), self.case).len();
                                if ui
                                    .button(format!(
                                        "Use this column ({} {})",
                                        group(n as u64),
                                        if n == 1 { "keyword" } else { "keywords" }
                                    ))
                                    .clicked()
                                {
                                    take = Some(true);
                                }
                                if ui.button("Cancel").clicked() {
                                    take = Some(false);
                                }
                            });
                        });
                }
                match take {
                    Some(true) => {
                        if let Some(l) = self.load.take() {
                            self.add_text(&l.column_text());
                        }
                    }
                    Some(false) => self.load = None,
                    None => {}
                }
                ui.separator();
                let Some(run) = &self.run else {
                    ui.label(
                        RichText::new("Each keyword is looked for in every column, in one read of the rows. You get which keywords are there, in how many rows and cells, and in which columns, and which found nothing.")
                            .color(pal.text2),
                    );
                    return;
                };
                let Some(out) = &out else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "{} of {} rows read · {:.0} s",
                            group(run.done.load(Ordering::Relaxed)),
                            group(snap.matched),
                            now - run.started
                        ));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    return;
                };
                if let Some(e) = &out.error {
                    ui.label(
                        RichText::new(if e == "stopped" {
                            "Stopped before the end: no totals to show.".to_string()
                        } else {
                            format!("{}  The list could not be searched: {e}", ph::WARNING)
                        })
                        .color(pal.danger),
                    );
                    return;
                }
                let found = out.terms.iter().filter(|t| t.rows > 0).count();
                let total = out.terms.len();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(summary(found, total, out.rows_any, out.of)).strong());
                    if let Some(t) = run.took {
                        ui.label(RichText::new(format!("{t:.1} s")).color(pal.text2));
                    }
                });
                if let Some(line) = read_line(out.columns_read, out.columns_skipped) {
                    ui.label(RichText::new(line).color(pal.text2));
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            out.rows_any > 0,
                            egui::Button::new(format!(
                                "{}  Show these {} rows in the grid",
                                ph::ROWS,
                                group(out.rows_any)
                            )),
                        )
                        .on_hover_text("The rows that hold at least one keyword")
                        .clicked()
                    {
                        self.want_rows = true;
                        bridge.send(Cmd::ListRows(run.id));
                    }
                    if ui
                        .button(format!("{}  Copy the table", ph::COPY))
                        .on_hover_text("Every keyword with its rows, cells and columns, ready for a spreadsheet")
                        .clicked()
                    {
                        crate::clip::copy(ui.ctx(), table_text(&run.terms, out), "the table");
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for (k, label) in [
                        (0u8, format!("Found  {}", group(found as u64))),
                        (1, format!("Not found  {}", group((total - found) as u64))),
                        (2, format!("All  {}", group(total as u64))),
                    ] {
                        if ui.selectable_label(self.tab == k, label).clicked() {
                            self.tab = k;
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.find)
                            .hint_text(format!("{}  Find a keyword", ph::MAGNIFYING_GLASS))
                            .desired_width(220.0),
                    );
                });
                // which keywords are listed: by tab and by the find box, most rows first
                let needle = self.find.to_lowercase();
                let mut list: Vec<usize> = (0..total)
                    .filter(|&i| match self.tab {
                        0 => out.terms[i].rows > 0,
                        1 => out.terms[i].rows == 0,
                        _ => true,
                    })
                    .filter(|&i| needle.is_empty() || run.terms[i].to_lowercase().contains(&needle))
                    .collect();
                list.sort_by(|&a, &b| out.terms[b].rows.cmp(&out.terms[a].rows).then(a.cmp(&b)));
                // the picked keyword's detail sits under the list
                let detail_h = if self.picked.is_some() { 96.0 } else { 0.0 };
                let row_h = ui.text_style_height(&egui::TextStyle::Body).max(18.0) + 6.0;
                let list_h = (ui.available_height() - detail_h - 8.0).max(120.0);
                let widths = [0.46f32, 0.12, 0.12, 0.30];
                let full = ui.available_width();
                // text lines up on the left, numbers on the right
                let slot = |ui: &mut egui::Ui, wf: f32, right: bool, text: RichText| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(full * wf - 12.0, row_h),
                        if right {
                            egui::Layout::right_to_left(egui::Align::Center)
                        } else {
                            egui::Layout::left_to_right(egui::Align::Center)
                        },
                        |ui| {
                            // the whole slot, so the columns line up whatever the text
                            ui.set_min_width(full * wf - 12.0);
                            ui.add(egui::Label::new(text).sense(egui::Sense::click()))
                        },
                    )
                    .inner
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for ((label, wf), right) in ["Keyword", "Rows", "Cells", "Columns"]
                        .iter()
                        .zip(widths)
                        .zip([false, true, true, false])
                    {
                        slot(ui, wf, right, RichText::new(*label).color(pal.text2).strong());
                    }
                });
                if list.is_empty() {
                    ui.label(
                        RichText::new(match (self.tab, self.find.is_empty()) {
                            (_, false) => "No keyword here matches what you typed.",
                            (0, _) => "None of the keywords is in these rows. See \"Not found\".",
                            (1, _) => "Every keyword was found.",
                            _ => "No keywords.",
                        })
                        .color(pal.text2),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("list-rows")
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, list.len(), |ui, range| {
                        for k in range {
                            let i = list[k];
                            let t = &out.terms[i];
                            let sel = self.picked == Some(i);
                            let cols = match t.cols.len() {
                                0 => String::new(),
                                1 => t.cols[0].0.clone(),
                                n => format!("{} columns, most in {}", group(n as u64), t.cols[0].0),
                            };
                            let r = ui
                                .horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 12.0;
                                    let ink = if t.rows == 0 { pal.text2 } else { pal.text };
                                    let a = slot(
                                        ui,
                                        widths[0],
                                        false,
                                        RichText::new(&run.terms[i]).font(theme::data_font()).color(ink),
                                    );
                                    let b = slot(ui, widths[1], true, RichText::new(group(t.rows)).color(ink));
                                    let c = slot(ui, widths[2], true, RichText::new(group(t.cells)).color(ink));
                                    let d = slot(ui, widths[3], false, RichText::new(cols).color(pal.text2));
                                    a.clicked() || b.clicked() || c.clicked() || d.clicked()
                                });
                            if sel {
                                ui.painter().rect_stroke(
                                    r.response.rect,
                                    egui::CornerRadius::same(2),
                                    egui::Stroke::new(1.0, pal.accent_ink),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if r.inner {
                                self.picked = if sel { None } else { Some(i) };
                            }
                        }
                    });
                if let Some(i) = self.picked.filter(|&i| i < total) {
                    let t = &out.terms[i];
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&run.terms[i]).font(theme::data_font()).strong());
                        if t.rows == 0 {
                            ui.label(RichText::new("is not in these rows.").color(pal.text2));
                        } else {
                            ui.label(format!(
                                "is in {} {} and {} {}:",
                                group(t.rows),
                                if t.rows == 1 { "row" } else { "rows" },
                                group(t.cells),
                                if t.cells == 1 { "cell" } else { "cells" }
                            ));
                            for (c, n) in &t.cols {
                                ui.label(
                                    RichText::new(format!("{c} {}", group(*n))).color(pal.accent_ink),
                                );
                            }
                            if ui
                                .button(format!("{}  Search for it", ph::MAGNIFYING_GLASS))
                                .on_hover_text("Replace the search with this keyword: the rows that hold it")
                                .clicked()
                            {
                                act = ListAction::Search(logseek_core::formula::quote(&run.terms[i]));
                            }
                            if ui
                                .button(format!("{}  See every cell", ph::LIST_MAGNIFYING_GLASS))
                                .on_hover_text("Every cell that holds it, one line per cell with its row and column: a row with it in several fields shows each of them")
                                .clicked()
                            {
                                act = ListAction::Cells(run.terms[i].clone());
                            }
                        }
                    });
                }
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            open = false;
        }
        if self.close_on_show && matches!(act, ListAction::Show { .. }) {
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
    use crate::bridge::ListTerm;

    #[test]
    fn a_pasted_list_becomes_keywords_once_each_in_order() {
        let text = "  10.9.8.7 \n\nBob\nbob\r\nevil.example.com\n10.9.8.7\n";
        assert_eq!(
            keywords(text, false),
            ["10.9.8.7", "Bob", "evil.example.com"]
        );
        // capitals matter: "Bob" and "bob" are two keywords
        assert_eq!(
            keywords(text, true),
            ["10.9.8.7", "Bob", "bob", "evil.example.com"]
        );
        assert!(keywords(" \n\t\n", false).is_empty());
        // a byte-order mark in front of the first line is not part of the keyword
        assert_eq!(keywords("\u{feff}bash\nroot", false), ["bash", "root"]);
    }

    #[test]
    fn a_table_file_gives_its_rows_and_the_chosen_column_becomes_the_keywords() {
        let text = "indicator,type,note\r\n10.9.8.7,ip,\"seen, twice\"\r\n\r\nevil.example.com,domain,\"line one\nline two\"\n\"a \"\"quoted\"\" one\",text,\n";
        assert_eq!(guess_delim(text), Some(','));
        let rows = table_rows(text, ',');
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], ["10.9.8.7", "ip", "seen, twice"]);
        assert_eq!(rows[2][2], "line one\nline two");
        assert_eq!(rows[3][0], "a \"quoted\" one");
        assert!(looks_like_header(&rows));
        let mut l = Loaded {
            name: "ioc.csv".into(),
            rows,
            header: true,
            col: 0,
        };
        assert_eq!(l.names(), ["indicator", "type", "note"]);
        assert_eq!(
            l.column_text(),
            "10.9.8.7\nevil.example.com\na \"quoted\" one"
        );
        l.col = 1;
        assert_eq!(l.column_text(), "ip\ndomain\ntext");
        // no names in the first row: it is data, and columns are numbered
        let plain = table_rows("10.9.8.7\tip\n10.9.8.8\tip\n", '\t');
        assert!(!looks_like_header(&plain));
        let l = Loaded {
            name: "x.tsv".into(),
            rows: plain,
            header: false,
            col: 0,
        };
        assert_eq!(l.names(), ["Column 1", "Column 2"]);
        assert_eq!(l.column_text(), "10.9.8.7\n10.9.8.8");
        // a plain list has no delimiter: one keyword per line
        assert_eq!(guess_delim("10.9.8.7\nevil.example.com\n"), None);
    }

    #[test]
    fn a_file_reads_whatever_it_was_saved_as() {
        assert_eq!(file_text(b"abc"), "abc");
        assert_eq!(file_text(&[0xEF, 0xBB, 0xBF, b'a']), "a");
        // UTF-16 with its mark, as some Windows tools write
        assert_eq!(file_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(file_text(&[0xFE, 0xFF, 0, b'h', 0, b'i']), "hi");
    }

    #[test]
    fn the_answer_reads_as_a_sentence_and_copies_as_a_table() {
        assert_eq!(
            summary(132, 2000, 48_210, 603_813),
            "132 of 2,000 keywords were found, in 48,210 of 603,813 rows."
        );
        assert_eq!(
            summary(1, 1, 1, 1),
            "1 of 1 keyword was found, in 1 of 1 row."
        );
        // how much had to be read
        assert_eq!(read_line(100, 0), None);
        assert_eq!(
            read_line(14, 86).unwrap(),
            "Read 14% of the data: the index showed that the rest cannot hold any of these keywords."
        );
        assert!(read_line(0, 50)
            .unwrap()
            .starts_with("Read none of the data"));
        assert!(read_line(1, 999)
            .unwrap()
            .starts_with("Read under 1 % of the data"));
        let out = ListOut {
            id: 1,
            terms: vec![
                ListTerm {
                    rows: 3,
                    cells: 4,
                    cols: vec![("User".into(), 3), ("Cmd".into(), 1)],
                },
                ListTerm {
                    rows: 0,
                    cells: 0,
                    cols: vec![],
                },
            ],
            rows_any: 3,
            of: 10,
            columns_read: 2,
            columns_skipped: 0,
            error: None,
        };
        let terms = vec!["bob".to_string(), "nope".to_string()];
        assert_eq!(
            table_text(&terms, &out),
            "keyword\trows\tcells\tcolumns\nbob\t3\t4\tUser 3; Cmd 1\nnope\t0\t0\t\n"
        );
    }
}
