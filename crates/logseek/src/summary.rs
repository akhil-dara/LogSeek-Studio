//! Summarise: group the current result by one or two columns (and optionally
//! a time bucket) and count, count distinct, or total a column. A result row
//! drills down into the filters that select exactly its rows.

use std::sync::atomic::AtomicBool;

/// Set from the UI to stop a long Summarise between pages.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

/// A bucket's time range, kept inside the search's own range when it has one.
pub fn bucket_range(t: i64, bucket: i64, search: Option<(i64, i64)>) -> (i64, i64) {
    match search {
        Some((a, b)) => (t.max(a), (t + bucket).min(b)),
        None => (t, t + bucket),
    }
}

/// The engine's order for a sheet order.
fn engine_sort(s: Sort) -> logseek_core::query::group::SummarySort {
    use logseek_core::query::group::SummarySort as E;
    match s {
        Sort::Most => E::MostRows,
        Sort::Rarest => E::FewestRows,
        Sort::Value => E::Value,
        Sort::Alpha => E::Key,
    }
}

/// The group of rows whose made-column value could not be worked out (a key
/// no value can hold), shown as [`ERROR_WORD`].
pub const ERROR_GROUP: &str = "\u{1}could not be worked out";

/// How those rows read, and the filter word that selects them (`iserror(col)`).
pub const ERROR_WORD: &str = "(could not be worked out)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    Rows,
    Distinct,
    Sum,
    Avg,
    Min,
    Max,
}

pub const MEASURES: [Measure; 6] = [
    Measure::Rows,
    Measure::Distinct,
    Measure::Sum,
    Measure::Avg,
    Measure::Min,
    Measure::Max,
];

impl Measure {
    pub fn label(self) -> &'static str {
        match self {
            Measure::Rows => "number of rows",
            Measure::Distinct => "number of different values of",
            Measure::Sum => "total of",
            Measure::Avg => "average of",
            Measure::Min => "smallest",
            Measure::Max => "largest",
        }
    }
    pub fn needs_column(self) -> bool {
        self != Measure::Rows
    }
    /// The result column's heading.
    pub fn heading(self, column: Option<&str>) -> String {
        let c = column.unwrap_or("");
        match self {
            Measure::Rows => "rows".into(),
            Measure::Distinct => format!("different {c}"),
            Measure::Sum => format!("total {c}"),
            Measure::Avg => format!("average {c}"),
            Measure::Min => format!("smallest {c}"),
            Measure::Max => format!("largest {c}"),
        }
    }
}

/// What to summarise.
#[derive(Clone, Debug, PartialEq)]
pub struct SummarySpec {
    pub by: Vec<String>,
    pub measure: Measure,
    pub column: Option<String>,
    /// Group by time too, in buckets of this many µs.
    pub bucket: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SummaryRow {
    /// The bucket start (when bucketed) then one value per group-by column.
    pub key: Vec<String>,
    pub value: f64,
    pub rows: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SummaryResult {
    pub spec: Option<SummarySpec>,
    /// The result generation it was computed for.
    pub generation: u64,
    /// The first groups (most rows first); the rest are read by page.
    pub rows: Vec<SummaryRow>,
    pub total_rows: u64,
    /// Every group there is (no limit) and the longest value per key column.
    pub groups: u64,
    pub widths: Vec<usize>,
    /// The engine's table these groups come from (for pages and Copy).
    pub table: u64,
    /// Distinct counts stopped growing at the memory limit: they are lower bounds.
    pub distinct_capped: bool,
    pub secs: f64,
    pub error: Option<String>,
}

/// How a group key reads in the table.
pub fn label_of(k: &str) -> &str {
    if k.is_empty() {
        "(no value)"
    } else if k == ERROR_GROUP {
        ERROR_WORD
    } else {
        k
    }
}

/// The column filters that select exactly one group's rows.
pub fn drill_down(by: &[String], key: &[String]) -> Vec<(String, String)> {
    by.iter()
        .zip(key)
        .map(|(c, v)| {
            // quoted and escaped: spaces, quotes and backslashes stay part of the value
            let t = if v.is_empty() {
                "EMPTY".to_string()
            } else if v == ERROR_GROUP {
                ERROR_WORD.to_string()
            } else {
                format!("={}", logseek_core::formula::quote(v))
            };
            (c.clone(), t)
        })
        .collect()
}

/// The table as tab-separated text, with a heading row.
pub fn to_tsv(
    by: &[String],
    m: Measure,
    column: Option<&str>,
    rows: &[SummaryRow],
    is_time: bool,
) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut head: Vec<String> = by.iter().map(|b| clean(b)).collect();
    head.push(m.heading(column));
    if m != Measure::Rows {
        head.push("rows".into());
    }
    let mut out = head.join("\t");
    for r in rows {
        out.push('\n');
        // empty and error groups copy as empty cells (an error is written as empty, as in exports)
        let mut cells: Vec<String> = r
            .key
            .iter()
            .map(|k| {
                if k == ERROR_GROUP {
                    String::new()
                } else {
                    clean(k)
                }
            })
            .collect();
        cells.push(if is_time && matches!(m, Measure::Min | Measure::Max) {
            fmt_measure(r.value, m, true)
        } else {
            fmt_value(r.value)
        });
        if m != Measure::Rows {
            cells.push(r.rows.to_string());
        }
        out.push_str(&cells.join("\t"));
    }
    out.push('\n');
    out
}

/// A measure value as text: whole numbers plainly, others with 2 decimals.
pub fn fmt_value(v: f64) -> String {
    if v.is_nan() {
        String::new()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

// ───────────────────────── the sheet ─────────────────────────

use crate::bridge::Snapshot;
use crate::theme::{self, Palette};
use eframe::egui::{self, Align2, CornerRadius, RichText, Sense, Stroke};
use egui_phosphor::regular as ph;

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Most,
    Rarest,
    Value,
    Alpha,
}

/// The sheet's choices and its cached ordering.
pub struct SummaryState {
    /// The columns to group by, in order (at least one; as many as wanted).
    by: Vec<String>,
    bucket: Option<i64>,
    measure: Measure,
    column: String,
    sort: Sort,
    sent: Option<(SummarySpec, u64)>,
    /// Groups loaded from the engine, by place in the current order.
    cache: std::collections::HashMap<usize, SummaryRow>,
    /// (table, order) the cache belongs to.
    cache_for: (u64, u8),
    /// Pages asked for (their first place).
    asked: std::collections::HashSet<usize>,
    /// Copy pressed: waiting for the engine's whole table.
    copy_wanted: Option<u64>,
    /// Save pressed: the whole table goes to this file when the engine has it.
    save_wanted: Option<(u64, std::path::PathBuf)>,
    /// What the last save did, said under the buttons.
    save_note: Option<String>,
    widths: Vec<f32>,
    /// For screenshots: "click" this row (in the shown order) once the result is in.
    auto_drill: Option<usize>,
}

pub enum SumAction {
    None,
    Run(SummarySpec),
    /// Column filters, and a time range (µs, end exclusive) for a bucketed row.
    Drill(Vec<(String, String)>, Option<(i64, i64)>),
    Close,
}

impl SummaryState {
    pub fn new(by: Option<String>, columns: &[String]) -> SummaryState {
        let first = by
            .or_else(|| columns.iter().find(|c| !c.starts_with('_')).cloned())
            .or_else(|| columns.first().cloned())
            .unwrap_or_default();
        SummaryState {
            by: vec![first.clone()],
            bucket: None,
            measure: Measure::Rows,
            column: first,
            sort: Sort::Most,
            sent: None,
            cache: Default::default(),
            cache_for: (0, 0),
            asked: Default::default(),
            copy_wanted: None,
            save_wanted: None,
            save_note: None,
            widths: Vec::new(),
            auto_drill: None,
        }
    }

    /// For screenshots: "by1|by2|measure#|column|bucket seconds"; by2 may
    /// hold several columns separated by commas.
    pub fn from_spec(spec: &str, columns: &[String]) -> SummaryState {
        let f: Vec<&str> = spec.split('|').collect();
        let mut st = SummaryState::new(f.first().map(|s| s.to_string()), columns);
        if let Some(more) = f.get(1).filter(|s| !s.is_empty()) {
            st.by.extend(more.split(',').map(|s| s.trim().to_string()));
        }
        st.measure = f
            .get(2)
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|i| MEASURES.get(i).copied())
            .unwrap_or(Measure::Rows);
        if let Some(c) = f.get(3).filter(|s| !s.is_empty()) {
            st.column = c.to_string();
        }
        st.bucket = f
            .get(4)
            .and_then(|s| s.parse::<i64>().ok())
            .map(|s| s * 1_000_000);
        st.auto_drill = f.get(5).and_then(|s| s.parse().ok());
        st
    }

    fn spec(&self) -> SummarySpec {
        SummarySpec {
            by: self.by.clone(),
            measure: self.measure,
            column: self.measure.needs_column().then(|| self.column.clone()),
            bucket: self.bucket,
        }
    }
}

/// A tab-separated table as CSV: a cell with a comma, a quote or a line
/// break is quoted, and a quote inside it doubled. Cells that a spreadsheet
/// would read as a formula get a leading apostrophe, as file export does.
pub fn table_to_csv(tsv: &str) -> String {
    let mut out = String::with_capacity(tsv.len() + tsv.len() / 8);
    for line in tsv.lines() {
        for (i, cell) in line.split('\t').enumerate() {
            if i > 0 {
                out.push(',');
            }
            let risky = cell.starts_with(['=', '+', '-', '@']) && cell.parse::<f64>().is_err();
            let cell = if risky { format!("'{cell}") } else { cell.to_string() };
            if cell.contains([',', '"', '\n', '\r']) {
                out.push('"');
                out.push_str(&cell.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(&cell);
            }
        }
        out.push_str("\r\n");
    }
    out
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, columns: &[String]) {
    crate::colpick::combo(ui, id, value, columns, 170.0, "choose a column");
}

/// Draws the sheet; the caller places it.
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    st: &mut SummaryState,
    snap: &Snapshot,
    bridge: &crate::bridge::Bridge,
) -> SumAction {
    let mut action = SumAction::None;
    let columns = &snap.columns;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Summarise").font(crate::fonts::strong(14.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::app::icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                action = SumAction::Close;
            }
        });
    });
    ui.label(
        RichText::new(
            "Groups the rows your search and filters keep. Click a group to see its rows.",
        )
        .font(theme::small_font())
        .color(pal.text2),
    );
    ui.add_space(4.0);
    // as many columns as wanted, in order; the line wraps
    ui.horizontal_wrapped(|ui| {
        let mut drop: Option<usize> = None;
        for i in 0..st.by.len() {
            ui.label(RichText::new(if i == 0 { "Group by" } else { "then by" }).color(pal.text2));
            combo(ui, &format!("sum-by{i}"), &mut st.by[i], columns);
            if i > 0
                && crate::app::icon_button(ui, pal, ph::X, "Stop grouping by this column").clicked()
            {
                drop = Some(i);
            }
        }
        if let Some(i) = drop {
            st.by.remove(i);
        }
        if ui
            .button(format!("{}  then by", ph::PLUS))
            .on_hover_text("Group by one more column")
            .clicked()
        {
            // the first column not yet used
            let next = columns
                .iter()
                .find(|c| !c.starts_with('_') && !st.by.contains(c))
                .or_else(|| columns.iter().find(|c| !st.by.contains(c)))
                .cloned();
            if let Some(c) = next {
                st.by.push(c);
            }
        }
    });
    if snap.time_col.is_some() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Per").color(pal.text2));
            for (b, n) in [
                (None, "no time split"),
                (Some(60_000_000), "minute"),
                (Some(3_600_000_000), "hour"),
                (Some(86_400_000_000), "day"),
            ] {
                ui.selectable_value(&mut st.bucket, b, n);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Show").color(pal.text2));
        egui::ComboBox::from_id_salt("sum-measure")
            .selected_text(st.measure.label())
            .width(210.0)
            .show_ui(ui, |ui| {
                for m in MEASURES {
                    ui.selectable_value(&mut st.measure, m, m.label());
                }
            });
        if st.measure.needs_column() {
            combo(ui, "sum-col", &mut st.column, columns);
        }
    });
    // run whenever the choice or the result changes
    let spec = st.spec();
    let want = (spec.clone(), snap.generation);
    if st.sent.as_ref() != Some(&want) && !spec.by[0].is_empty() {
        st.sent = Some(want);
        action = SumAction::Run(spec.clone());
    }
    ui.separator();
    let res = snap
        .summary
        .as_ref()
        .filter(|r| r.spec.as_ref() == Some(&spec) && r.generation == snap.generation)
        .cloned();
    let Some(res) = res else {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Counting").color(pal.muted));
            if ui
                .small_button("Cancel")
                .on_hover_text("Stop counting; change a choice to count again")
                .clicked()
            {
                CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        return action;
    };
    if let Some(e) = &res.error {
        ui.label(RichText::new(format!("This cannot be summarised: {e}")).color(pal.danger));
        return action;
    }
    // header line: what came out, and the order
    ui.horizontal(|ui| {
        let groups = res.groups;
        ui.label(
            RichText::new(format!(
                "{} groups · {} rows · {:.2} s",
                crate::moment::group(groups),
                crate::moment::group(res.total_rows),
                res.secs
            ))
            .color(pal.text2),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{}  Copy as table", ph::COPY))
                .on_hover_text("Tab-separated, ready to paste into a spreadsheet")
                .clicked()
            {
                let mut heads: Vec<String> = Vec::new();
                if spec.bucket.is_some() {
                    heads.push("time (UTC)".into());
                }
                heads.extend(spec.by.iter().cloned());
                // every group, in the order shown, written out by the engine
                bridge.send(crate::bridge::Cmd::SummaryCopy {
                    table: res.table,
                    sort: engine_sort(st.sort),
                    heads,
                    is_time: measure_is_time(snap, spec.column.as_deref()),
                });
                st.copy_wanted = Some(res.table);
            }
            if ui
                .button(format!("{}  Save as a file", ph::EXPORT))
                .on_hover_text("Every group, in the order shown, as a CSV or tab-separated file")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Save the summary")
                    .set_file_name("summary.csv")
                    .add_filter("CSV (comma-separated)", &["csv"])
                    .add_filter("Tab-separated", &["tsv", "txt"])
                    .save_file()
                {
                    let mut heads: Vec<String> = Vec::new();
                    if spec.bucket.is_some() {
                        heads.push("time (UTC)".into());
                    }
                    heads.extend(spec.by.iter().cloned());
                    bridge.send(crate::bridge::Cmd::SummaryCopy {
                        table: res.table,
                        sort: engine_sort(st.sort),
                        heads,
                        is_time: measure_is_time(snap, spec.column.as_deref()),
                    });
                    st.save_wanted = Some((res.table, path));
                    st.save_note = None;
                }
            }
            let mut sorts = vec![
                (Sort::Alpha, "A to Z"),
                (Sort::Rarest, "Rarest"),
                (Sort::Most, "Most"),
            ];
            if spec.measure != Measure::Rows {
                sorts.insert(0, (Sort::Value, "By value"));
            }
            for (s, n) in sorts {
                ui.selectable_value(&mut st.sort, s, n);
            }
        });
    });
    if res.distinct_capped {
        ui.label(
            RichText::new("The memory limit was reached while counting different values: the counts shown are at least these.")
                .font(theme::small_font())
                .color(pal.warning),
        );
    }
    // every group lives in the engine; the sheet keeps what it has shown
    if let Some(c) = snap
        .summary_copy
        .as_ref()
        .filter(|c| Some(c.0) == st.copy_wanted)
    {
        crate::clip::copy(ui.ctx(), c.1.clone(), "the summary");
        st.copy_wanted = None;
    }
    if let Some(c) = snap
        .summary_copy
        .as_ref()
        .filter(|c| st.save_wanted.as_ref().is_some_and(|w| w.0 == c.0))
    {
        if let Some((_, path)) = st.save_wanted.take() {
            let tabs = matches!(
                path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
                Some("tsv" | "txt")
            );
            let text = if tabs { c.1.clone() } else { table_to_csv(&c.1) };
            st.save_note = Some(match std::fs::write(&path, text) {
                Ok(()) => format!("Saved every group to {}", path.display()),
                Err(e) => format!("Could not save to {}: {e}", path.display()),
            });
        }
    }
    if let Some(n) = &st.save_note {
        ui.add(egui::Label::new(RichText::new(n).font(theme::small_font()).color(pal.text2)).wrap());
    }
    let want = engine_sort(st.sort);
    if st.cache_for != (res.table, st.sort as u8) {
        st.cache.clear();
        st.asked.clear();
        st.cache_for = (res.table, st.sort as u8);
        if st.sort == Sort::Most {
            for (k, r) in res.rows.iter().enumerate() {
                st.cache.insert(k, r.clone());
            }
        }
        // every key shown whole: column widths fit the longest value (monospace)
        let cw = ui
            .painter()
            .layout_no_wrap("0".into(), theme::data_font(), pal.text)
            .size()
            .x;
        st.widths = res
            .widths
            .iter()
            .map(|&n| (n.max(label_of("").chars().count()) as f32 * cw + 24.0).max(90.0))
            .collect();
    }
    if let Some(pg) = snap
        .summary_page
        .as_ref()
        .filter(|p| p.table == res.table && p.sort == want)
    {
        for (k, r) in pg.rows.iter().enumerate() {
            st.cache
                .entry(pg.start as usize + k)
                .or_insert_with(|| r.clone());
        }
    }
    let shown = res.groups as usize;
    if let Some(i) = st.auto_drill.take() {
        if let Some(row) = st.cache.get(&i) {
            let off = usize::from(spec.bucket.is_some());
            return SumAction::Drill(drill_down(&spec.by, &row.key[off..]), None);
        }
    }
    let total = res.total_rows.max(1) as f32;
    let value_w = 110.0;
    let rows_w = if spec.measure != Measure::Rows {
        90.0
    } else {
        0.0
    };
    let bar_w = 120.0;
    let table_w = st.widths.iter().sum::<f32>() + value_w + rows_w + bar_w + 16.0;
    let row_h = 22.0;
    let font = theme::data_font();
    let max_h = (ui.ctx().content_rect().height() - 360.0).max(160.0);
    egui::ScrollArea::both()
        .max_height(max_h)
        .min_scrolled_height(
            ((shown + 1) as f32 * (row_h + ui.spacing().item_spacing.y) + 4.0).min(max_h),
        )
        .auto_shrink([false, true])
        .show_rows(ui, row_h, shown + 1, |ui, range| {
            ui.set_min_width(table_w);
            for line in range {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(table_w, row_h),
                    if line == 0 {
                        Sense::hover()
                    } else {
                        Sense::click()
                    },
                );
                let p = ui.painter();
                let mut x = rect.left() + 6.0;
                let cy = rect.center().y;
                if line == 0 {
                    // headings
                    let mut heads: Vec<String> = Vec::new();
                    if spec.bucket.is_some() {
                        heads.push("time (UTC)".into());
                    }
                    heads.extend(spec.by.iter().cloned());
                    for (k, h) in heads.iter().enumerate() {
                        p.text(
                            egui::pos2(x, cy),
                            Align2::LEFT_CENTER,
                            h,
                            crate::fonts::strong(12.0),
                            pal.text2,
                        );
                        x += st.widths.get(k).copied().unwrap_or(120.0);
                    }
                    p.text(
                        egui::pos2(x + value_w - 8.0, cy),
                        Align2::RIGHT_CENTER,
                        spec.measure.heading(spec.column.as_deref()),
                        crate::fonts::strong(12.0),
                        pal.text2,
                    );
                    if rows_w > 0.0 {
                        p.text(
                            egui::pos2(x + value_w + rows_w - 8.0, cy),
                            Align2::RIGHT_CENTER,
                            "rows",
                            crate::fonts::strong(12.0),
                            pal.text2,
                        );
                    }
                    p.line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0, pal.hairline),
                    );
                    continue;
                }
                let Some(row) = st.cache.get(&(line - 1)).cloned() else {
                    // not loaded yet: ask for its page, say so meanwhile
                    let start = (line - 1) / 200 * 200;
                    if st.asked.insert(start) {
                        bridge.send(crate::bridge::Cmd::SummaryPage {
                            table: res.table,
                            sort: want,
                            start: start as u64,
                            len: 400,
                        });
                    }
                    p.text(
                        egui::pos2(x, cy),
                        Align2::LEFT_CENTER,
                        "reading",
                        theme::small_font(),
                        pal.text2,
                    );
                    continue;
                };
                let row = &row;
                if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::same(3), pal.surface2);
                }
                for (k, v) in row.key.iter().enumerate() {
                    let col = if v.is_empty() { pal.muted } else { pal.text };
                    p.text(
                        egui::pos2(x, cy),
                        Align2::LEFT_CENTER,
                        label_of(v),
                        font.clone(),
                        col,
                    );
                    x += st.widths.get(k).copied().unwrap_or(120.0);
                }
                p.text(
                    egui::pos2(x + value_w - 8.0, cy),
                    Align2::RIGHT_CENTER,
                    fmt_measure(
                        row.value,
                        spec.measure,
                        measure_is_time(snap, spec.column.as_deref()),
                    ),
                    font.clone(),
                    pal.text,
                );
                x += value_w;
                if rows_w > 0.0 {
                    p.text(
                        egui::pos2(x + rows_w - 8.0, cy),
                        Align2::RIGHT_CENTER,
                        crate::moment::group(row.rows),
                        font.clone(),
                        pal.text2,
                    );
                    x += rows_w;
                }
                // share of the result, with the percentage printed (never colour alone)
                let share = row.rows as f32 / total;
                let track = egui::Rect::from_min_size(
                    egui::pos2(x + 4.0, cy - 3.0),
                    egui::vec2(bar_w - 50.0, 6.0),
                );
                p.rect_filled(track, CornerRadius::same(2), pal.hairline);
                let mut bar = track;
                bar.set_width(track.width() * share.clamp(0.0, 1.0));
                p.rect_filled(bar, CornerRadius::same(2), pal.info.gamma_multiply(0.8));
                let pct = if share > 0.0 && share < 0.01 {
                    "<1%".to_string()
                } else {
                    format!("{:.0}%", share * 100.0)
                };
                p.text(
                    egui::pos2(track.right() + 6.0, cy),
                    Align2::LEFT_CENTER,
                    pct,
                    theme::small_font(),
                    pal.text2,
                );
                if resp.on_hover_text("Show only these rows").clicked() {
                    let off = usize::from(spec.bucket.is_some());
                    let filters = drill_down(&spec.by, &row.key[off..]);
                    let range = spec.bucket.and_then(|b| {
                        let t = logseek_core::time::parse_time(row.key.first()?)?;
                        Some((t, t + b))
                    });
                    action = SumAction::Drill(filters, range);
                }
            }
        });
    action
}

/// A measure value for people: the smallest or largest of a time column is
/// a time (UTC), never raw microseconds; numbers get grouped digits.
pub fn fmt_measure(v: f64, m: Measure, is_time: bool) -> String {
    if is_time && matches!(m, Measure::Min | Measure::Max) && !v.is_nan() {
        logseek_core::types::fmt_time(v as i64)
    } else {
        fmt_value_grouped(v)
    }
}

/// The measured column is a time column.
fn measure_is_time(snap: &Snapshot, column: Option<&str>) -> bool {
    column
        .and_then(|c| snap.columns.iter().position(|x| x == c))
        .and_then(|i| snap.col_types.get(i))
        .is_some_and(|t| *t == logseek_core::types::ColType::Time)
}

/// A measure value for the table: grouped digits for whole numbers.
fn fmt_value_grouped(v: f64) -> String {
    if v.is_nan() {
        String::new()
    } else if v.fract() == 0.0 && v.abs() < 1e15 && v >= 0.0 {
        crate::moment::group(v as u64)
    } else {
        fmt_value(v)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn earliest_and_latest_times_read_as_times() {
        use super::{fmt_measure, Measure};
        // 2026-11-13 07:14:46.929 UTC in µs
        let t = 1_763_018_086_929_000.0;
        let s = fmt_measure(t, Measure::Min, true);
        assert!(
            s.starts_with("2025-11-13") || s.starts_with("2026-") || s.contains(':'),
            "{s}"
        );
        assert!(!s.contains("1763018086929000"), "{s}");
        assert_eq!(fmt_measure(1234.0, Measure::Max, false), "1,234");
        assert_eq!(fmt_measure(1234.0, Measure::Rows, true), "1,234");
    }
    #[test]
    fn drill_down_selects_exactly_the_value() {
        for v in [
            "trail ",
            r#""C:\x.exe" "a b""#,
            r"C:\Windows\cmd.exe",
            "  both  ",
        ] {
            let f = drill_down(&["c".to_string()], &[v.to_string()]);
            let clause = crate::filters::text_clause("c", &f[0].1).unwrap().unwrap();
            assert!(
                clause.contains(&format!("c={}", logseek_core::formula::quote(v))),
                "{v:?} → {clause}"
            );
        }
    }

    #[test]
    fn drill_range_stays_inside_the_search() {
        let day = 86_400_000_000i64;
        let h = 3_600_000_000i64;
        assert_eq!(bucket_range(0, day, None), (0, day));
        assert_eq!(
            bucket_range(0, day, Some((h, h + h / 2))),
            (h, h + h / 2),
            "a 30-minute search stays 30 minutes"
        );
        assert_eq!(bucket_range(day, day, Some((0, day + h))), (day, day + h));
    }

    use super::*;

    #[test]
    fn drill_down_clauses() {
        let by = vec!["host".to_string(), "user".to_string()];
        let key = vec!["srv-b02".to_string(), String::new()];
        let f = drill_down(&by, &key);
        assert_eq!(
            f,
            vec![
                ("host".to_string(), "=\"srv-b02\"".to_string()),
                ("user".to_string(), "EMPTY".to_string())
            ]
        );
        for (c, t) in &f {
            assert!(
                matches!(crate::filters::text_clause(c, t), Ok(Some(_))),
                "{t}"
            );
        }
        // quotes inside a value stay literal
        let q = drill_down(&["cmd".to_string()], &["say \"hi\"".to_string()]);
        assert!(
            matches!(crate::filters::text_clause("cmd", &q[0].1), Ok(Some(_))),
            "{}",
            q[0].1
        );
    }

    #[test]
    fn copy_as_tsv_has_a_header_and_every_row() {
        let rows = vec![
            SummaryRow {
                key: vec!["a\tb".into()],
                value: 1.0,
                rows: 1,
            },
            SummaryRow {
                key: vec!["c".into()],
                value: 1.0,
                rows: 1,
            },
        ];
        let t = to_tsv(&["host".to_string()], Measure::Rows, None, &rows, false);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "host\trows");
        assert_eq!(lines.len(), 3);
        assert!(
            lines.iter().any(|l| l.starts_with("a b\t")),
            "a tab inside a value cannot break the columns"
        );
    }
}

#[cfg(test)]
mod csv_tests {
    use super::table_to_csv;

    #[test]
    fn a_summary_table_saves_as_csv_with_every_cell_whole() {
        let tsv = "user\tnote\trows\nann\tsaid \"hi\", left\t3\n=cmd\t-5\t1\n";
        assert_eq!(
            table_to_csv(tsv),
            "user,note,rows\r\nann,\"said \"\"hi\"\", left\",3\r\n'=cmd,-5,1\r\n"
        );
    }
}
