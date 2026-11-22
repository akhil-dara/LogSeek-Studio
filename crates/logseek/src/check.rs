//! "Check before opening": shown before a file is indexed for the first
//! time. Says how the file will be read, shows the first rows exactly as the
//! import reads them, and lets the user choose the field the timeline uses.

use crate::theme::{self, Palette};
use eframe::egui::{self, RichText, Stroke};
use egui_phosphor::regular as ph;
use logseek_core::ingest::pipeline::TimeChoice;
use logseek_core::ingest::preview::{Preview, TimeCandidate};
use logseek_core::store::SourceSpec;
use logseek_core::types::{fmt_time, ColType};
use std::path::PathBuf;

pub struct CheckState {
    pub path: PathBuf,
    pub preview: Preview,
    /// Index into `preview.time`; `None` = no timeline.
    pub time: Option<usize>,
    /// Where the index will be written.
    pub index_dir: PathBuf,
    /// First frame: the preview starts at its top-left.
    fresh: bool,
    /// A workbook sheet's title above its column names, said once.
    title: Option<String>,
    /// Indexes go to a folder the user chose (not the default one).
    pub chosen_home: bool,
    /// What became of the last folder chosen for indexes, in words.
    pub home_note: Option<String>,
    /// Names typed for a file with no row of names, by place (blank: keep).
    pub names: Vec<String>,
}

impl CheckState {
    pub fn new(path: PathBuf, preview: Preview, index_dir: PathBuf) -> Self {
        let time = preview.guess;
        let title = crate::book::title_sentence(&path);
        CheckState {
            path,
            preview,
            time,
            index_dir,
            fresh: true,
            title,
            chosen_home: false,
            home_note: None,
            names: Vec::new(),
        }
    }

    /// The file has no row of names: its columns are numbered and can be named here.
    pub fn nameable(&self) -> bool {
        match &self.preview.spec {
            SourceSpec::Csv { opts, .. } => !opts.has_header,
            SourceSpec::Text { .. } => true,
            SourceSpec::Json { .. } => false,
        }
    }

    pub fn choice(&self) -> TimeChoice {
        match self.time.and_then(|i| self.preview.time.get(i)) {
            // the timeline follows a time column the user renamed
            Some(t) => TimeChoice::Column(
                self.names
                    .get(t.col)
                    .map(|n| n.trim())
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| t.name.clone()),
            ),
            None => TimeChoice::Off,
        }
    }
}

pub enum CheckAction {
    None,
    Open,
    Cancel,
    /// Keep indexes in a folder the user picks (another drive with room).
    ChooseIndexHome,
    /// Keep indexes in the default folder again.
    DefaultIndexHome,
}

fn delimiter_name(d: u8) -> String {
    match d {
        b',' => "commas".into(),
        b'\t' => "tabs".into(),
        b';' => "semicolons".into(),
        b'|' => "pipes".into(),
        b' ' => "spaces".into(),
        o => format!("'{}'", o as char),
    }
}

/// One sentence: how the file will be read.
pub fn describe(p: &Preview) -> String {
    let n = p.columns.len();
    match &p.spec {
        SourceSpec::Csv { opts, .. } => format!(
            "A table: values separated by {}, {}, {n} columns.",
            delimiter_name(opts.delimiter),
            if opts.has_header {
                "the first row holds the column names"
            } else {
                "no row of names (columns are numbered)"
            }
        ),
        SourceSpec::Text { parts, header } => {
            let named = header.first().is_some_and(|h| !h.starts_with("column_"));
            format!(
                "Lines of text, each split into its {} parts (words, \"quoted\" and [bracketed] groups), as this file's own lines line up; {}. Whatever does not fit a line's columns is kept whole in {}.",
                parts.len(),
                if named {
                    "the file names its columns"
                } else {
                    "no row of names (columns are numbered)"
                },
                logseek_core::ingest::textlayout::REST
            )
        }
        SourceSpec::Json {
            record_path,
            array,
            stream,
            bulk,
        } => {
            let how = if *bulk {
                "Elasticsearch bulk export (index lines folded into each record)".to_string()
            } else if let Some(k) = record_path {
                format!("JSON records inside \"{k}\"")
            } else if *array {
                "a JSON array of records".to_string()
            } else if *stream {
                "JSON records spread over several lines".to_string()
            } else {
                "JSON, one record per line".to_string()
            };
            format!(
                "{} {n} fields (nested fields become a.b.c columns).",
                capital(&how) + ","
            )
        }
    }
}

/// What happened to repeated or empty column names, in words (every name
/// listed, never cut); `None` when the header was clean.
pub fn renamed_line(renamed: &[(String, String)]) -> Option<String> {
    if renamed.is_empty() {
        return None;
    }
    let empty = renamed.iter().filter(|(raw, _)| raw.is_empty()).count();
    let repeated = renamed.len() - empty;
    let mut what = Vec::new();
    if repeated > 0 {
        what.push(format!(
            "{repeated} column {} repeated in the file",
            if repeated == 1 {
                "name is"
            } else {
                "names are"
            }
        ));
    }
    if empty > 0 {
        what.push(format!(
            "{empty} {} no name",
            if empty == 1 {
                "column has"
            } else {
                "columns have"
            }
        ));
    }
    let given: Vec<&str> = renamed.iter().map(|(_, g)| g.as_str()).collect();
    Some(format!(
        "{}. Each keeps its own column, named: {}.",
        what.join(" and "),
        given.join(", ")
    ))
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// How a timeline candidate reads, e.g. "on 100% of rows · 2024-03-01 10:00:00
/// to 2024-03-01 10:00:09 UTC · a number read as epoch seconds".
#[cfg(test)]
pub fn time_line(t: &TimeCandidate) -> String {
    let pct = (t.filled * 100.0).round() as u32;
    let mut s = format!("on {pct}% of the first rows");
    if let (Some(a), Some(b)) = (t.first, t.last) {
        s += &format!(" · {} to {} UTC", fmt_time(a), fmt_time(b));
    }
    if let Some(u) = t.epoch_unit_us {
        let unit = match u {
            1_000_000 => "seconds",
            1_000 => "milliseconds",
            _ => "microseconds",
        };
        s += &format!(" · a number read as epoch {unit}");
    }
    s
}

/// The same facts in columns, for the list of timeline choices: share of the
/// first rows with a time, earliest, latest (UTC), and a note for a number.
pub fn time_parts(t: &TimeCandidate) -> (String, String, String, Option<String>) {
    let pct = format!("{}%", (t.filled * 100.0).round() as u32);
    let (a, b) = match (t.first, t.last) {
        (Some(a), Some(b)) => (fmt_time(a), fmt_time(b)),
        _ => (String::new(), String::new()),
    };
    let note = t.epoch_unit_us.map(|u| {
        let unit = match u {
            1_000_000 => "seconds",
            1_000 => "milliseconds",
            _ => "microseconds",
        };
        format!("a number read as epoch {unit}")
    });
    (pct, a, b, note)
}

/// Height the list of timeline choices may take: every choice when they fit
/// in `room`, else `room` (the rest scroll; none is left out).
pub fn time_list_height(choices: usize, row_h: f32, room: f32) -> f32 {
    (choices as f32 * row_h).min(room.max(row_h * 3.0))
}

fn type_name(t: ColType) -> &'static str {
    match t {
        ColType::Text => "text",
        ColType::Int => "whole number",
        ColType::Float => "number",
        ColType::Bool => "yes/no",
        ColType::Time => "time",
    }
}

/// Draw the sheet.
pub fn show(ctx: &egui::Context, pal: &Palette, st: &mut CheckState) -> CheckAction {
    let mut act = CheckAction::None;
    let screen = ctx.content_rect();
    // dim what is behind: this decision comes first
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("check-scrim"),
    ))
    .rect_filled(
        screen,
        0.0,
        egui::Color32::from_black_alpha(if pal.dark { 120 } else { 70 }),
    );
    let w = (screen.width() - 32.0).min(980.0);
    let h = (screen.height() - 48.0).min(760.0);
    egui::Window::new("check-before-opening")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .fixed_size([w, h])
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            // the buttons stay on screen whatever the file holds: the foot is
            // laid out first and the rest shares what is left
            egui::Panel::bottom("check-foot")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(
                                "{}  Your file is only read, never changed. The index is saved in {}",
                                ph::SHIELD_CHECK,
                                st.index_dir.display()
                            ))
                            .color(pal.text2),
                        )
                        .wrap(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(format!("{}  Keep indexes in another folder", ph::FOLDER_OPEN))
                            .on_hover_text("For a drive with more room. Files indexed before keep their index where it is.")
                            .clicked()
                        {
                            act = CheckAction::ChooseIndexHome;
                        }
                        if st.chosen_home
                            && ui
                                .button("Use the usual folder again")
                                .on_hover_text("New indexes go back to the app's own folder")
                                .clicked()
                        {
                            act = CheckAction::DefaultIndexHome;
                        }
                    });
                    if let Some(n) = &st.home_note {
                        ui.add(egui::Label::new(RichText::new(n).color(pal.text2)).wrap());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if crate::app::primary_button(ui, pal, &format!("{}  Open", ph::ARROW_RIGHT)).clicked() {
                            act = CheckAction::Open;
                        }
                        if ui.button("Cancel").clicked() {
                            act = CheckAction::Cancel;
                        }
                    });
                });
            let name = st
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Check before opening").font(crate::fonts::strong(16.0)));
                ui.label(RichText::new(&name).color(pal.text2));
            });
            ui.add_space(6.0);
            ui.label(RichText::new(describe(&st.preview)).color(pal.text));
            if let Some(line) = renamed_line(&st.preview.renamed) {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{}  {line}", ph::INFO)).color(pal.text2),
                    )
                    .wrap(),
                );
            }
            if let Some(t) = &st.title {
                ui.add(
                    egui::Label::new(RichText::new(format!("{}  {t}", ph::INFO)).color(pal.text2))
                        .wrap(),
                );
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Timeline").font(crate::fonts::strong(14.0)));
            ui.label(
                RichText::new(
                    "The time line beside the rows and the time filters use this field. You can change it later.",
                )
                .color(pal.text2),
            );
            ui.add_space(4.0);
            let guess = st.preview.guess;
            let fresh = st.fresh;
            if !st.preview.time.is_empty() {
                // one line per choice, in columns, so many time fields still read
                // at a glance; when they do not all fit, the list scrolls
                let row_h = ui.spacing().interact_size.y + 4.0;
                let room = (ui.available_height() - 330.0).max(row_h * 3.0);
                let list_h = time_list_height(st.preview.time.len(), row_h, room);
                if st.preview.time.len() > 1 {
                    ui.label(
                        RichText::new(format!(
                            "{} fields read as a time. Each shows its share of the first rows, then its earliest and latest time (UTC).",
                            st.preview.time.len()
                        ))
                        .color(pal.text2),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt(("check-times", &st.path))
                    .max_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("check-times-grid")
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                for (i, t) in st.preview.time.iter().enumerate() {
                                    let (pct, a, b, note) = time_parts(t);
                                    let r = ui.radio_value(
                                        &mut st.time,
                                        Some(i),
                                        RichText::new(&t.name).strong(),
                                    );
                                    if fresh && st.time == Some(i) {
                                        r.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    let data = |ui: &mut egui::Ui, v: String| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(v)
                                                    .font(theme::data_font())
                                                    .color(pal.text2),
                                            )
                                            .extend(),
                                        );
                                    };
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| data(ui, pct),
                                    );
                                    data(ui, a);
                                    data(ui, b);
                                    let mut words: Vec<&str> = Vec::new();
                                    if guess == Some(i) {
                                        words.push("suggested");
                                    }
                                    if let Some(n) = note.as_deref() {
                                        words.push(n);
                                    }
                                    ui.add(
                                        egui::Label::new(RichText::new(words.join(" · ")).color(
                                            if guess == Some(i) { pal.accent_ink } else { pal.text2 },
                                        ))
                                        .extend(),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            }
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut st.time, None, RichText::new("No timeline").strong());
                if st.preview.time.is_empty() {
                    ui.label(
                        RichText::new("no field in the first rows reads as a time")
                            .color(pal.text2),
                    );
                }
            });
            ui.add_space(10.0);
            let shown = st.preview.rows.len();
            ui.label(RichText::new("First rows, as they will be read").font(crate::fonts::strong(14.0)));
            ui.label(
                RichText::new(format!(
                    "{} of the {} rows read from the start of the file; the type under each name is what was found.",
                    crate::moment::group(shown as u64),
                    crate::moment::group(st.preview.sampled_rows as u64)
                ))
                .color(pal.text2),
            );
            if st.nameable() {
                ui.label(
                    RichText::new(format!(
                        "{}  This file has no row of names: type a name over any column (blank keeps its number).",
                        ph::PENCIL_SIMPLE
                    ))
                    .color(pal.accent_ink),
                );
            }
            ui.add_space(4.0);
            // the foot is already laid out: the rows take what is left
            let table_h = (ui.available_height() - 20.0).max(120.0);
            let time_col = st.time.and_then(|i| st.preview.time.get(i)).map(|t| t.col);
            egui::Frame::new()
                .stroke(Stroke::new(1.0, pal.control_border))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    let mut area = egui::ScrollArea::both()
                        .id_salt(("check-rows", &st.path))
                        .max_height(table_h);
                    if std::mem::take(&mut st.fresh) {
                        area = area.scroll_offset(egui::Vec2::ZERO);
                    }
                    area
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            egui::Grid::new("check-grid")
                                .striped(true)
                                .spacing([18.0, 4.0])
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new("row").font(theme::small_font()).color(pal.text2))
                                            .extend(),
                                    );
                                    let nameable = st.nameable();
                                    if nameable {
                                        st.names.resize(st.preview.columns.len(), String::new());
                                    }
                                    for (c, n) in st.preview.columns.iter().enumerate() {
                                        let col = if Some(c) == time_col { pal.accent_ink } else { pal.text };
                                        ui.vertical(|ui| {
                                            if nameable && n.as_str() != logseek_core::ingest::textlayout::REST {
                                                // no row of names: name the column here (blank keeps the number)
                                                // as wide as the longer of its name and what is typed, never cut
                                                let font = egui::TextStyle::Body.resolve(ui.style());
                                                let wide = |t: &str| {
                                                    ui.painter()
                                                        .layout_no_wrap(t.to_string(), font.clone(), pal.text)
                                                        .size()
                                                        .x
                                                };
                                                let w = wide(n).max(wide(&st.names[c])).max(90.0) + 24.0;
                                                ui.add_sized(
                                                    [w, 24.0],
                                                    egui::TextEdit::singleline(&mut st.names[c])
                                                        .hint_text(n.as_str()),
                                                )
                                                .on_hover_text(format!("A name for {n} (leave blank to keep it)"));
                                            } else {
                                                ui.add(egui::Label::new(RichText::new(n).strong().color(col)).extend());
                                            }
                                            let ty = st.preview.types.get(c).copied().unwrap_or(ColType::Text);
                                            ui.add(egui::Label::new(
                                                RichText::new(if Some(c) == time_col {
                                                    "timeline".to_string()
                                                } else {
                                                    type_name(ty).to_string()
                                                })
                                                .font(theme::small_font())
                                                .color(if Some(c) == time_col { pal.accent_ink } else { pal.text2 }),
                                            ).extend());
                                        });
                                    }
                                    ui.end_row();
                                    let mono: Vec<bool> = (0..st.preview.columns.len())
                                        .map(|c| {
                                            let ty = st.preview.types.get(c).copied().unwrap_or(ColType::Text);
                                            let vals: Vec<&str> = st
                                                .preview
                                                .rows
                                                .iter()
                                                .filter_map(|r| r.get(c).and_then(|v| v.as_deref()))
                                                .collect();
                                            theme::is_machine_value(ty, &vals)
                                        })
                                        .collect();
                                    for (i, r) in st.preview.rows.iter().enumerate() {
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new((i + 1).to_string()).font(theme::data_font()).color(pal.text2),
                                                )
                                                .extend(),
                                            );
                                        });
                                        for (c, v) in r.iter().enumerate() {
                                            let t = v.as_deref().unwrap_or("");
                                            let rt = RichText::new(t.replace(['\n', '\r'], " "));
                                            ui.add(egui::Label::new(if mono.get(c).copied().unwrap_or(false) {
                                                rt.font(theme::data_font())
                                            } else {
                                                rt
                                            }).extend());
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                });
        });
    // Enter opens, unless a button has focus (Enter then presses that button)
    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && ctx.memory(|m| m.focused().is_none()) {
        act = CheckAction::Open;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        act = CheckAction::Cancel;
    }
    act
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseek_core::ingest::csv::CsvOptions;

    fn pv(spec: SourceSpec, n: usize) -> Preview {
        Preview {
            spec,
            file_len: 0,
            columns: (0..n).map(|i| format!("c{i}")).collect(),
            types: vec![ColType::Text; n],
            rows: vec![],
            sampled_rows: 0,
            time: vec![],
            guess: None,
            renamed: vec![],
        }
    }

    #[test]
    fn a_file_with_no_row_of_names_can_be_named_and_the_timeline_follows() {
        let csv = |has_header| SourceSpec::Csv {
            opts: CsvOptions {
                has_header,
                ..Default::default()
            },
            header: vec![],
        };
        let mut p = pv(csv(false), 3);
        p.time = vec![TimeCandidate {
            col: 1,
            name: "c1".into(),
            filled: 1.0,
            first: None,
            last: None,
            epoch_unit_us: None,
            in_order: true,
        }];
        let mut st = CheckState::new(PathBuf::from("x.log"), p, PathBuf::from("i"));
        st.time = Some(0);
        assert!(st.nameable());
        assert_eq!(st.choice(), TimeChoice::Column("c1".into()));
        st.names = vec![String::new(), " when ".into(), String::new()];
        assert_eq!(st.choice(), TimeChoice::Column("when".into()));
        // a file that names its own columns keeps them
        let named = CheckState::new(PathBuf::from("x.csv"), pv(csv(true), 3), PathBuf::from("i"));
        assert!(!named.nameable());
    }

    #[test]
    fn describe_says_how_the_file_is_read_in_words() {
        let csv = SourceSpec::Csv {
            opts: CsvOptions {
                delimiter: b'\t',
                ..Default::default()
            },
            header: vec![],
        };
        assert_eq!(
            describe(&pv(csv, 3)),
            "A table: values separated by tabs, the first row holds the column names, 3 columns."
        );
        let js = SourceSpec::Json {
            record_path: Some("Records".into()),
            array: false,
            stream: false,
            bulk: false,
        };
        assert_eq!(
            describe(&pv(js, 9)),
            "JSON records inside \"Records\", 9 fields (nested fields become a.b.c columns)."
        );
    }

    #[test]
    fn repeated_and_empty_names_are_explained_with_every_new_name() {
        let r = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(renamed_line(&[]), None);
        assert_eq!(
            renamed_line(&[r("Time", "Time_2")]).as_deref(),
            Some(
                "1 column name is repeated in the file. Each keeps its own column, named: Time_2."
            )
        );
        assert_eq!(
            renamed_line(&[r("Time", "Time_2"), r("", "column_4"), r("Host", "Host_2")]).as_deref(),
            Some("2 column names are repeated in the file and 1 column has no name. Each keeps its own column, named: Time_2, column_4, Host_2.")
        );
    }

    #[test]
    fn time_parts_give_each_candidate_in_columns() {
        let t = TimeCandidate {
            col: 0,
            name: "ts".into(),
            filled: 0.6,
            first: Some(1_709_287_200_000_000),
            last: Some(1_709_287_209_000_000),
            epoch_unit_us: Some(1_000),
            in_order: true,
        };
        let p = time_parts(&t);
        assert_eq!(p.0, "60%");
        assert_eq!(p.1, "2024-03-01 10:00:00");
        assert_eq!(p.2, "2024-03-01 10:00:09");
        assert_eq!(p.3.as_deref(), Some("a number read as epoch milliseconds"));
        // no time read yet: the range stays blank, nothing is made up
        let none = TimeCandidate { first: None, last: None, epoch_unit_us: None, ..t };
        assert_eq!(time_parts(&none), ("60%".into(), String::new(), String::new(), None));
    }

    #[test]
    fn time_line_gives_fill_range_and_epoch_unit() {
        let t = TimeCandidate {
            col: 1,
            name: "timestamp".into(),
            filled: 1.0,
            first: Some(1_709_287_200_000_000),
            last: Some(1_709_287_209_000_000),
            epoch_unit_us: Some(1_000_000),
            in_order: true,
        };
        assert_eq!(
            time_line(&t),
            "on 100% of the first rows · 2024-03-01 10:00:00 to 2024-03-01 10:00:09 UTC · a number read as epoch seconds"
        );
    }
}
