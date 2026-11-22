//! The "Add a column" sheet: pick a recipe card, fill its small form, see the
//! result on the rows on screen, name it, add it. Each recipe becomes formula
//! text, so the new column is virtual and the file is never changed.

use crate::bridge::Snapshot;
use crate::colors::{self, RuleOp};
use crate::recipes::{Case, DatePartKind, Decode, MathOp, Measure, Operand, Piece, Recipe};
use crate::theme::{self, Palette};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::types::ColType;

/// The cards: icon, title, what it is for.
/// A conversion offered straight from a column's menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Convert {
    /// Numbers in this unit ("s", "ms", "us", "ns", "filetime", "webkit", "days").
    Time(&'static str),
    Decode(Decode),
    /// One field out of a column of JSON text.
    Json,
}

pub const CARDS: [(&str, &str, &str); 11] = [
    (
        ph::SCISSORS,
        "Split",
        "One piece of a value: the file from a path, the user from DOMAIN\\user",
    ),
    (
        ph::LINK_SIMPLE,
        "Combine",
        "Join columns into one, like host / user",
    ),
    (
        ph::BROOM,
        "Clean up",
        "Trim spaces, change case, replace text",
    ),
    (
        ph::TAG,
        "Classify",
        "Label rows by rules: when a column holds a value, a word of yours",
    ),
    (
        ph::CALENDAR_DOTS,
        "Date and time part",
        "The hour, weekday or date of a time column",
    ),
    (
        ph::KEY,
        "Decode",
        "Base64, hex, the host of a URL, the file or folder of a path",
    ),
    (
        ph::CALCULATOR,
        "Maths",
        "Add, subtract, multiply or divide, then round",
    ),
    (
        ph::RULER,
        "Length and randomness",
        "Long or random-looking values often hide encoded data",
    ),
    (
        ph::CLOCK_COUNTDOWN,
        "Read as time",
        "Turn epoch numbers or Windows ticks into times",
    ),
    (
        ph::STACK,
        "First with a value",
        "The first non-empty of several columns",
    ),
    (
        ph::BRACKETS_CURLY,
        "Take from JSON",
        "One field out of a column whose values are JSON text",
    ),
];

/// The fields found in a column's JSON values, each with a value it holds:
/// every path of every value given, in the order the file has them. A field inside
/// a list is offered for one item (`[0].name`) and, before that, for every
/// item at once (`[*].name`).
pub fn json_paths(values: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for v in values {
        for (path, sample) in crate::inspect::json_fields(v) {
            if path.is_empty() || out.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let every = every_item(&path);
            if every != path && !out.iter().any(|(p, _)| *p == every) {
                let all = logseek_core::jsonpath::extract(v, &every).unwrap_or_default();
                out.push((every, all));
            }
            out.push((path, sample));
        }
    }
    out
}

/// What to say when most of the times made from a column of numbers fall
/// before 1990 or after 2100: the unit is probably another one.
pub fn odd_times(values: &[String]) -> Option<String> {
    let years: Vec<i32> = values
        .iter()
        .filter_map(|v| v.get(..4)?.parse::<i32>().ok())
        .collect();
    if years.is_empty() {
        return None;
    }
    let early = years.iter().filter(|y| **y < 1990).count();
    let late = years.iter().filter(|y| **y > 2100).count();
    if early * 2 > years.len() {
        Some("Most of these times fall before 1990, so these numbers are probably not times counted this way. Try another choice above, or another column.".into())
    } else if late * 2 > years.len() {
        Some("Most of these times fall after 2100, so these numbers are probably not times counted this way. Try another choice above, or another column.".into())
    } else {
        None
    }
}

/// A path with every list position turned into "every item": `a[0].b[12]`
/// becomes `a[*].b[*]`.
fn every_item(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..=open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) if after[..close].chars().all(|c| c.is_ascii_digit()) && close > 0 => {
                out.push('*');
                rest = &after[close..];
            }
            _ => rest = after,
        }
    }
    out.push_str(rest);
    out
}

/// Everything the forms hold between frames.
pub struct AddColState {
    pub card: usize,
    col: String,
    col2: String,
    cols: Vec<String>,
    sep: String,
    /// The column the separator was last chosen for from its values.
    sep_for: Option<String>,
    piece: usize,
    nth: u32,
    trim: bool,
    case: Case,
    find: String,
    with: String,
    rules: Vec<(String, RuleOp, String, String)>,
    otherwise: String,
    part: DatePartKind,
    decode: Decode,
    math: MathOp,
    use_col2: bool,
    number: String,
    round_on: bool,
    digits: u8,
    measure: Measure,
    unit: String,
    /// Take from JSON: the path of the field, and what is typed to find one.
    json_path: String,
    json_find: String,
    name: String,
    name_edited: bool,
}

pub enum AddAction {
    None,
    Add { name: String, formula: String },
    Close,
}

impl AddColState {
    /// The sheet opened on a conversion of `col` (from the column menu).
    pub fn convert(col: Option<String>, columns: &[String], how: Convert) -> AddColState {
        let mut st = AddColState::new(col, columns);
        match how {
            Convert::Time(unit) => {
                st.card = 8;
                st.unit = unit.to_string();
            }
            Convert::Decode(d) => {
                st.card = 5;
                st.decode = d;
            }
            Convert::Json => st.card = 10,
        }
        st
    }

    pub fn new(col: Option<String>, columns: &[String]) -> AddColState {
        let first = col.or_else(|| columns.first().cloned()).unwrap_or_default();
        AddColState {
            card: 0,
            col: first.clone(),
            col2: columns.get(1).cloned().unwrap_or_else(|| first.clone()),
            cols: vec![first.clone(), columns.get(1).cloned().unwrap_or_default()],
            sep: "\\".into(),
            sep_for: None,
            piece: 1,
            nth: 2,
            trim: true,
            case: Case::Keep,
            find: String::new(),
            with: String::new(),
            rules: vec![(first.clone(), RuleOp::Is, String::new(), String::new())],
            otherwise: "Other".into(),
            part: DatePartKind::Hour,
            decode: Decode::Base64,
            math: MathOp::Div,
            use_col2: false,
            number: "1024".into(),
            round_on: true,
            digits: 1,
            measure: Measure::Entropy,
            unit: "s".into(),
            json_path: String::new(),
            json_find: String::new(),
            name: String::new(),
            name_edited: false,
        }
    }

    /// The recipe the form describes now, when it is complete.
    pub fn recipe(&self) -> Option<Recipe> {
        let col = self.col.clone();
        Some(match self.card {
            0 => Recipe::Split {
                col,
                sep: (!self.sep.is_empty()).then(|| self.sep.clone())?,
                piece: match self.piece {
                    0 => Piece::First,
                    1 => Piece::Last,
                    _ => Piece::Nth(self.nth.max(1)),
                },
            },
            1 => Recipe::Combine {
                cols: self
                    .cols
                    .iter()
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .collect(),
                sep: self.sep.clone(),
            },
            2 => Recipe::Clean {
                col,
                trim: self.trim,
                case: self.case,
                replace: (!self.find.is_empty()).then(|| (self.find.clone(), self.with.clone())),
            },
            3 => Recipe::Classify {
                rules: self
                    .rules
                    .iter()
                    .filter(|r| !r.2.trim().is_empty() && !r.3.trim().is_empty())
                    .cloned()
                    .collect(),
                otherwise: self.otherwise.clone(),
            },
            4 => Recipe::DatePart {
                col,
                part: self.part,
            },
            5 => Recipe::Decode {
                col,
                how: self.decode,
            },
            6 => Recipe::Maths {
                col,
                op: self.math,
                other: if self.use_col2 {
                    Operand::Column(self.col2.clone())
                } else {
                    Operand::Number(self.number.trim().parse().ok()?)
                },
                round: self.round_on.then_some(self.digits),
            },
            7 => Recipe::Measure {
                col,
                what: self.measure,
            },
            8 => Recipe::ToDate {
                col,
                unit: self.unit.clone(),
            },
            10 => Recipe::Json {
                col,
                path: (!self.json_path.trim().is_empty()).then(|| self.json_path.trim().to_string())?,
            },
            _ => Recipe::FirstNonEmpty {
                cols: self
                    .cols
                    .iter()
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .collect(),
            },
        })
    }
}

fn col_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut String,
    columns: &[String],
) {
    crate::colpick::combo(ui, format!("{id:?}"), value, columns, 180.0, "choose a column");
}

fn label(ui: &mut egui::Ui, pal: &Palette, t: &str) {
    ui.label(RichText::new(t).color(pal.text2));
}

/// The list of columns used in a multi-column form, with add / remove.
fn col_list(
    ui: &mut egui::Ui,
    pal: &Palette,
    cols: &mut Vec<String>,
    columns: &[String],
    id: &str,
) {
    let mut remove = None;
    let n = cols.len();
    for (i, c) in cols.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            label(ui, pal, &format!("{}.", i + 1));
            col_combo(ui, (id, i), c, columns);
            if n > 1 && crate::app::icon_button(ui, pal, ph::X, "Remove this column").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        cols.remove(i);
    }
    if ui.button(format!("{}  Add a column", ph::PLUS)).clicked() {
        cols.push(columns.first().cloned().unwrap_or_default());
    }
}

/// The formula the engine gets: columns that are formulas themselves are
/// written in place (the engine compiles against the file's own columns).
pub fn engine_formula(r: &Recipe, snap: &Snapshot) -> String {
    r.formula_with(
        &|c: &str| match snap.virtual_cols.iter().find(|(n, _)| n == c) {
            Some((_, f)) => format!("({f})"),
            None => logseek_core::formula::column_ref(c),
        },
    )
}

/// Draws the sheet; the caller places it.
/// The separator a value is most usefully split at: the first of the offered
/// ones that the value holds. A space comes last, since most text has spaces.
/// `None` when it holds none of them.
pub fn separator_in(value: &str) -> Option<&'static str> {
    ["\\", "/", ",", "=", "@", " "]
        .into_iter()
        .find(|s| value.contains(s))
}

pub fn show(ui: &mut egui::Ui, pal: &Palette, st: &mut AddColState, snap: &Snapshot) -> AddAction {
    let mut action = AddAction::None;
    let columns = &snap.columns;
    // Split starts on a separator the column's values really hold, so the
    // result on screen shows something being split. Chosen once per column;
    // a separator picked by hand after that is kept.
    if st.card == 0 && st.sep_for.as_deref() != Some(st.col.as_str()) {
        let c = columns.iter().position(|n| *n == st.col);
        let sample = c.and_then(|c| {
            snap.page.as_ref()?.cells.iter().find_map(|r| {
                r.get(c)
                    .and_then(|v| v.as_deref())
                    .filter(|v| !v.trim().is_empty())
            })
        });
        if let Some(sep) = sample.and_then(separator_in) {
            st.sep = sep.to_string();
        }
        if sample.is_some() {
            st.sep_for = Some(st.col.clone());
        }
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Add a column").font(crate::fonts::strong(14.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::app::icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                action = AddAction::Close;
            }
        });
    });
    ui.label(
        RichText::new("Made from the columns you have. The file itself is never changed.")
            .font(theme::small_font())
            .color(pal.text2),
    );
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        // the cards
        ui.vertical(|ui| {
            ui.set_width(190.0);
            for (i, (icon, title, line)) in CARDS.iter().enumerate() {
                let on = st.card == i;
                let b = egui::Button::new(RichText::new(format!("{icon}  {title}")).color(if on { pal.text } else { pal.text2 }))
                    .fill(if on { pal.selection } else { pal.surface })
                    .stroke(if on {
                        egui::Stroke::new(1.0, pal.accent_ink)
                    } else {
                        egui::Stroke::new(1.0, pal.hairline)
                    })
                    .min_size(egui::vec2(186.0, 28.0));
                if ui.add(b).on_hover_text(*line).clicked() && !on {
                    st.card = i;
                    st.name_edited = false;
                }
            }
        });
        ui.separator();
        // the form
        ui.vertical(|ui| {
            ui.set_width(390.0);
            let (_, title, line) = CARDS[st.card];
            ui.label(RichText::new(title).font(crate::fonts::strong(13.0)));
            ui.label(RichText::new(line).font(theme::small_font()).color(pal.text2));
            ui.add_space(4.0);
            match st.card {
                0 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal(|ui| {
                        label(ui, pal, "Split at");
                        for (s, name) in [("\\", "\\"), ("/", "/"), (",", "comma"), (" ", "space"), ("=", "="), ("@", "@")] {
                            ui.selectable_value(&mut st.sep, s.to_string(), name);
                        }
                        ui.add(egui::TextEdit::singleline(&mut st.sep).desired_width(50.0).hint_text("other"));
                    });
                    ui.horizontal(|ui| {
                        label(ui, pal, "Keep");
                        ui.selectable_value(&mut st.piece, 0, "the first piece");
                        ui.selectable_value(&mut st.piece, 1, "the last piece");
                        ui.selectable_value(&mut st.piece, 2, "piece number");
                        if st.piece == 2 {
                            ui.add(egui::DragValue::new(&mut st.nth).range(1..=u32::MAX));
                        }
                    });
                }
                1 | 9 => {
                    col_list(ui, pal, &mut st.cols, columns, "ac-cols");
                    if st.card == 1 {
                        ui.horizontal(|ui| {
                            label(ui, pal, "Put between them");
                            ui.add(egui::TextEdit::singleline(&mut st.sep).desired_width(80.0));
                        });
                    }
                }
                2 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.checkbox(&mut st.trim, "Remove spaces at both ends");
                    ui.horizontal(|ui| {
                        label(ui, pal, "Letters");
                        ui.selectable_value(&mut st.case, Case::Keep, "as they are");
                        ui.selectable_value(&mut st.case, Case::Upper, "UPPER CASE");
                        ui.selectable_value(&mut st.case, Case::Lower, "lower case");
                    });
                    ui.horizontal(|ui| {
                        label(ui, pal, "Replace");
                        ui.add(egui::TextEdit::singleline(&mut st.find).desired_width(110.0).hint_text("text"));
                        label(ui, pal, "with");
                        ui.add(egui::TextEdit::singleline(&mut st.with).desired_width(110.0).hint_text("nothing"));
                    });
                }
                3 => {
                    label(ui, pal, "The first rule that matches gives the label:");
                    let mut remove = None;
                    let n = st.rules.len();
                    for (i, r) in st.rules.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            col_combo(ui, ("ac-rule-col", i), &mut r.0, columns);
                            egui::ComboBox::from_id_salt(("ac-rule-op", i))
                                .selected_text(colors::op_label(r.1))
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for op in colors::OPS {
                                        ui.selectable_value(&mut r.1, op, colors::op_label(op));
                                    }
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut r.2).desired_width(150.0).hint_text("value"));
                            label(ui, pal, "gives");
                            ui.add(egui::TextEdit::singleline(&mut r.3).desired_width(120.0).hint_text("label"));
                            if n > 1 && crate::app::icon_button(ui, pal, ph::X, "Remove this rule").clicked() {
                                remove = Some(i);
                            }
                        });
                        ui.add_space(2.0);
                    }
                    if let Some(i) = remove {
                        st.rules.remove(i);
                    }
                    if ui.button(format!("{}  Add a rule", ph::PLUS)).clicked() {
                        let c = st.rules.last().map(|r| r.0.clone()).unwrap_or_default();
                        st.rules.push((c, RuleOp::Is, String::new(), String::new()));
                    }
                    ui.horizontal(|ui| {
                        label(ui, pal, "Otherwise");
                        ui.add(egui::TextEdit::singleline(&mut st.otherwise).desired_width(140.0));
                    });
                }
                4 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Time column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal_wrapped(|ui| {
                        label(ui, pal, "Part");
                        for (p, n) in [
                            (DatePartKind::Date, "date"),
                            (DatePartKind::Year, "year"),
                            (DatePartKind::Month, "month"),
                            (DatePartKind::Day, "day"),
                            (DatePartKind::Weekday, "weekday"),
                            (DatePartKind::Hour, "hour"),
                            (DatePartKind::Minute, "minute"),
                        ] {
                            ui.selectable_value(&mut st.part, p, n);
                        }
                    });
                }
                5 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal_wrapped(|ui| {
                        for (d, n) in [
                            (Decode::Base64, "Base64"),
                            (Decode::Base64Utf16, "Base64 of UTF-16 text (two bytes a letter)"),
                            (Decode::Hex, "hex"),
                            (Decode::UrlHost, "host of a URL"),
                            (Decode::FileName, "file name of a path"),
                            (Decode::Folder, "folder of a path"),
                            (Decode::Extension, "extension"),
                        ] {
                            ui.selectable_value(&mut st.decode, d, n);
                        }
                    });
                }
                6 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal(|ui| {
                        for (m, n) in [(MathOp::Add, "+ add"), (MathOp::Sub, "− subtract"), (MathOp::Mul, "× multiply"), (MathOp::Div, "÷ divide by")] {
                            ui.selectable_value(&mut st.math, m, n);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut st.use_col2, false, "a number");
                        ui.selectable_value(&mut st.use_col2, true, "another column");
                        if st.use_col2 {
                            col_combo(ui, "ac-col2", &mut st.col2, columns);
                        } else {
                            ui.add(egui::TextEdit::singleline(&mut st.number).desired_width(80.0));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut st.round_on, "Round to");
                        ui.add_enabled(st.round_on, egui::DragValue::new(&mut st.digits).range(0..=15));
                        label(ui, pal, "decimal places");
                    });
                }
                7 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut st.measure, Measure::Length, "length (characters)");
                        ui.selectable_value(&mut st.measure, Measure::Entropy, "randomness (bits per character)");
                    });
                    if st.measure == Measure::Entropy {
                        ui.label(
                            RichText::new("Plain words score about 3 to 4; Base64 and random names score above 4.5.")
                                .font(theme::small_font())
                                .color(pal.text2),
                        );
                    }
                }
                8 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    ui.horizontal_wrapped(|ui| {
                        label(ui, pal, "The numbers count");
                        for (u, n) in [
                            ("s", "seconds"),
                            ("ms", "milliseconds"),
                            ("us", "microseconds"),
                            ("ns", "nanoseconds"),
                            ("filetime", "Windows ticks (since 1601)"),
                            ("webkit", "microseconds since 1601"),
                            ("cocoa", "seconds since 2001"),
                            ("days", "spreadsheet day number"),
                        ] {
                            ui.selectable_value(&mut st.unit, u.to_string(), n);
                        }
                    });
                }
                10 => {
                    ui.horizontal(|ui| {
                        label(ui, pal, "Column");
                        col_combo(ui, "ac-col", &mut st.col, columns);
                    });
                    // the fields this column's values on screen really hold
                    let c = columns.iter().position(|n| *n == st.col);
                    let held: Vec<&str> = c
                        .and_then(|c| {
                            snap.page.as_ref().map(|p| {
                                p.cells
                                    .iter()
                                    .filter_map(|r| r.get(c).and_then(|v| v.as_deref()))
                                    .collect()
                            })
                        })
                        .unwrap_or_default();
                    let found = json_paths(&held);
                    if found.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new("None of this column's values on screen is JSON. Choose a column whose values start with { or [, or type the field's path below.")
                                    .color(pal.text2),
                            )
                            .wrap(),
                        );
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.json_find)
                                .hint_text(format!(
                                    "Find a field ({} in the rows on screen)",
                                    crate::moment::group(found.len() as u64)
                                ))
                                .desired_width(380.0),
                        );
                        let typed = st.json_find.trim().to_lowercase();
                        egui::ScrollArea::vertical()
                            .id_salt("ac-json-fields")
                            .max_height(230.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let mut any = false;
                                for (path, sample) in &found {
                                    if !typed.is_empty() && !path.to_lowercase().contains(&typed) {
                                        continue;
                                    }
                                    any = true;
                                    let words = if path.contains("[*]") {
                                        format!("{path}   (every item)")
                                    } else {
                                        path.clone()
                                    };
                                    if ui
                                        .add(
                                            egui::Button::selectable(
                                                st.json_path == *path,
                                                RichText::new(words).font(theme::data_font()),
                                            )
                                            .wrap_mode(egui::TextWrapMode::Wrap),
                                        )
                                        .clicked()
                                    {
                                        st.json_path = path.clone();
                                        st.name_edited = false;
                                    }
                                    ui.indent(("ac-json-sample", path), |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(sample)
                                                    .font(theme::small_font())
                                                    .color(pal.text2),
                                            )
                                            .wrap(),
                                        );
                                    });
                                }
                                if !any {
                                    ui.label(RichText::new("No field has that in its name").color(pal.muted));
                                }
                            });
                    }
                    ui.horizontal(|ui| {
                        label(ui, pal, "Field path");
                        ui.add(
                            egui::TextEdit::singleline(&mut st.json_path)
                                .hint_text("pick one above, or type: a.b  [0].name  [*].name")
                                .desired_width(290.0),
                        );
                    });
                }
                _ => {}
            }
            ui.add_space(6.0);
            ui.separator();
            // live preview on the rows on screen
            let recipe = st.recipe();
            if let Some(r) = &recipe {
                if !st.name_edited {
                    st.name = r.default_name();
                }
            }
            let types: Vec<(String, ColType)> = columns
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), snap.col_types.get(i).copied().unwrap_or(ColType::Text)))
                .collect();
            let rows: Vec<Vec<Option<String>>> = snap
                .page
                .as_ref()
                .filter(|p| p.generation == snap.generation)
                .map(|p| p.cells.iter().take(8).cloned().collect())
                .unwrap_or_default();
            let formula = recipe.as_ref().map(Recipe::formula);
            let result = formula.as_ref().map(|f| crate::recipes::preview(f, &types, &rows));
            let input_col = columns.iter().position(|c| *c == st.col);
            match &result {
                None => {
                    ui.label(RichText::new("Fill in the form to see the result").color(pal.text2));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("This cannot be made: {e}")).color(pal.danger));
                }
                Some(Ok(vals)) => {
                    label(ui, pal, "On the rows on screen:");
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        // two fixed lanes: the value it starts from, and the result (both wrap, never cut)
                        for (k, v) in vals.iter().enumerate() {
                            let src = input_col
                                .and_then(|c| rows.get(k).and_then(|r| r.get(c)).cloned().flatten())
                                .unwrap_or_default();
                            ui.horizontal_top(|ui| {
                                ui.allocate_ui(egui::vec2(200.0, 0.0), |ui| {
                                    ui.set_width(200.0);
                                    ui.add(egui::Label::new(RichText::new(src).font(theme::data_font()).color(pal.text2)).wrap());
                                });
                                ui.label(RichText::new(ph::ARROW_RIGHT).color(pal.muted));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(if v.is_empty() { "(no value)".to_string() } else { v.clone() })
                                            .font(theme::data_font())
                                            .color(if v.is_empty() { pal.muted } else { pal.text }),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.separator();
                        }
                    });
                }
            }
            if let Some(f) = &formula {
                ui.label(RichText::new(format!("Formula: {f}")).font(theme::small_font()).color(pal.muted));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                label(ui, pal, "Name");
                if ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(200.0)).changed() {
                    st.name_edited = true;
                }
            });
            let name = st.name.trim().to_string();
            let clash = columns.iter().any(|c| c.eq_ignore_ascii_case(&name));
            if clash {
                ui.label(RichText::new(format!("A column named {name} already exists")).color(pal.danger));
            }
            let ok = matches!(result, Some(Ok(_))) && !name.is_empty() && !clash;
            // numbers read in the wrong unit give times that cannot be right: say so
            if st.card == 8 {
                if let Some(Ok(vals)) = &result {
                    if let Some(why) = odd_times(vals) {
                        ui.add(egui::Label::new(RichText::new(why).color(pal.warning)).wrap());
                    }
                }
            }
            if !ok && !clash {
                let why = if recipe.is_none() {
                    if st.card == 10 {
                        "Pick a field above to make the column."
                    } else {
                        "Fill in the form above to make the column."
                    }
                } else if name.is_empty() {
                    "Give the column a name."
                } else {
                    ""
                };
                if !why.is_empty() {
                    ui.label(RichText::new(why).font(theme::small_font()).color(pal.text2));
                }
            }
            ui.horizontal(|ui| {
                if ui.add_enabled_ui(ok, |ui| crate::app::primary_button(ui, pal, "Add column")).inner.clicked() {
                    if let Some(r) = &recipe {
                        action = AddAction::Add { name: name.clone(), formula: engine_formula(r, snap) };
                    }
                }
                if ui.button("Cancel").clicked() {
                    action = AddAction::Close;
                }
            });
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::{json_paths, odd_times, separator_in};

    #[test]
    fn times_that_cannot_be_right_are_said() {
        let t = |v: &[&str]| odd_times(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(t(&["1970-01-01 00:16:40", "1970-01-01 00:16:41", "2026-01-01 00:00:00"])
            .unwrap()
            .contains("before 1990"));
        assert!(t(&["58341-02-03 00:00:00", "5834-02-03 00:00:00"]).unwrap().contains("after 2100"));
        assert_eq!(t(&["2026-03-02 08:00:00", "2025-11-30 10:00:00"]), None);
        assert_eq!(t(&["", "not a time"]), None);
    }

    #[test]
    fn the_fields_of_json_values_are_listed_with_a_value_each() {
        let a = r#"[{"resource": "projects/1", "granted": true}, {"resource": "projects/2", "granted": false}]"#;
        let b = r#"[{"resource": "projects/9", "permissionType": "DATA_READ"}]"#;
        let got = json_paths(&[a, b]);
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "[*].resource",
                "[0].resource",
                "[*].granted",
                "[0].granted",
                "[1].resource",
                "[1].granted",
                "[*].permissionType",
                "[0].permissionType",
            ]
        );
        // every item at once: all of them, as the engine gives them
        assert_eq!(got[0].1, r#"["projects/1","projects/2"]"#);
        assert_eq!(got[1].1, "projects/1");
        // each path is one the engine reads
        for (p, sample) in &got {
            assert_eq!(logseek_core::jsonpath::extract(if p.contains("permissionType") { b } else { a }, p).as_deref(), Some(sample.as_str()), "{p}");
        }
    }

    #[test]
    fn an_object_gives_its_names_and_text_that_is_not_json_gives_nothing() {
        let got = json_paths(&[r#"{"user": {"name": "ana", "ids": [7, 8]}}"#, "plain text", ""]);
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["user.name", "user.ids[*]", "user.ids[0]", "user.ids[1]"]);
        assert!(json_paths(&["not json", "{broken"]).is_empty());
    }

    #[test]
    fn split_starts_on_a_separator_the_value_holds() {
        assert_eq!(separator_in(r"C:\Windows\System32\cmd.exe"), Some("\\"));
        assert_eq!(separator_in("/catalog/item/5430"), Some("/"));
        assert_eq!(separator_in("a,b c"), Some(","));
        assert_eq!(separator_in("user@host"), Some("@"));
        // plain words: only a space to split at
        assert_eq!(separator_in("payment declined"), Some(" "));
        assert_eq!(separator_in("oneword"), None);
    }
}
