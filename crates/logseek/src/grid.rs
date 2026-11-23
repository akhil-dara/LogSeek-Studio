//! The virtual grid. It scrolls by row index, not pixels, so 100 million rows
//! cost the same as 100: only the rows on screen are requested and drawn.

use crate::bridge::{Bridge, Cmd, Snapshot};
use crate::filters::{match_ranges, Filters};
use crate::theme::{self, Palette, HEADER_H, ROW_H};
use eframe::egui::{
    self, Align2, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind,
};
use egui_phosphor::regular as ph;

/// No filter row under the header: a column is filtered from its header (the
/// funnel: values and conditions) and its filter reads as a sentence chip.
const FILTER_H: f32 = 0.0;

/// Left of the row numbers: tag marks, colour-rule marker, the selection ring.
const MARKS: f32 = 30.0;
/// Grouped view: one step of the tree strip per grouping level.
const TREE_STEP: f32 = 18.0;
/// A Follow tree stops stepping in after this many levels.
const FTREE_MAX: u16 = 12;

/// The x of level `k`'s rail (the centre of its caret) in the tree strip.
fn rail_x(left: f32, band_w: f32, k: usize) -> f32 {
    left + band_w + 8.0 + k as f32 * TREE_STEP + 6.0
}

/// The words of a group's header, in order: its rows and share first (always
/// in view, however long the value), the column only when the rows are
/// grouped by more than one (the chip above names it otherwise), the value in
/// full, then when its rows begin and end.
pub fn header_parts(
    by: Option<&str>,
    value: Option<&str>,
    rows: u64,
    matched: u64,
    span: Option<(i64, i64)>,
) -> Vec<String> {
    let share = if matched > 0 {
        rows as f64 * 100.0 / matched as f64
    } else {
        0.0
    };
    let pct = if rows < matched && share >= 99.95 {
        // nearly every row, but not every row: never rounded up to 100%
        "over 99.9%".to_string()
    } else if rows < matched && share >= 99.5 {
        format!("{share:.1}%")
    } else if share >= 10.0 || share == 0.0 {
        format!("{share:.0}%")
    } else if share >= 0.1 {
        format!("{share:.1}%")
    } else {
        "under 0.1%".to_string()
    };
    let mut out = vec![
        if rows == 1 {
            "1 row".to_string()
        } else {
            format!("{} rows", crate::moment::group(rows))
        },
        pct,
    ];
    if let Some(b) = by {
        out.push(b.to_string());
    }
    out.push(match value {
        Some(v) => v.replace(['\n', '\r', '\t'], " "),
        None => "(no value)".to_string(),
    });
    if let Some((a, b)) = span {
        out.push(format!(
            "{}  to  {} UTC",
            logseek_core::types::fmt_time(a),
            logseek_core::types::fmt_time(b)
        ));
    }
    out
}

/// Where a group header's words start, for the group at `depth`.
fn header_x(left: f32, gutter: f32, depth: usize) -> f32 {
    left + gutter + 8.0 + depth as f32 * TREE_STEP + 18.0
}

/// A group header's words laid out to wrap within `wrap_w` (a long value
/// takes more lines, never cut).
fn header_galley(
    p: &egui::Painter,
    pal: &Palette,
    g: &crate::groups::Grouping,
    path: &[u64],
    matched: u64,
    wrap_w: f32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};
    let depth = path.len().saturating_sub(1);
    let by = (g.by.len() > 1).then(|| g.by.get(depth).cloned()).flatten();
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_w.max(80.0);
    match g.group(path) {
        Some(row) => {
            let parts = header_parts(
                by.as_deref(),
                row.value.as_deref(),
                row.rows,
                matched,
                row.first.zip(row.last),
            );
            for (k, part) in parts.iter().enumerate() {
                let is_value =
                    k == parts.len() - 1 - usize::from(row.first.zip(row.last).is_some());
                let is_by = by.is_some() && k == 2;
                let fmt = if k == 0 {
                    TextFormat::simple(theme::ui_font(), pal.text)
                } else if is_value {
                    TextFormat::simple(
                        crate::fonts::strong(13.0),
                        if row.value.is_some() {
                            pal.text
                        } else {
                            pal.text2
                        },
                    )
                } else if k > 1 && !is_by {
                    TextFormat::simple(theme::data_font(), pal.text2)
                } else {
                    TextFormat::simple(theme::small_font(), pal.text2)
                };
                job.append(part, if k == 0 { 0.0 } else { 12.0 }, fmt);
            }
        }
        None => {
            if let Some(b) = &by {
                job.append(b, 0.0, TextFormat::simple(theme::small_font(), pal.text2));
            }
            job.append(
                "reading this group",
                if by.is_some() { 10.0 } else { 0.0 },
                TextFormat::simple(theme::ui_font(), pal.text2),
            );
        }
    }
    p.layout_job(job)
}

/// A group's header line: caret, then its rows, share, value and time span.
#[allow(clippy::too_many_arguments)]
fn draw_group_header(
    p: &egui::Painter,
    pal: &Palette,
    g: &crate::groups::Grouping,
    path: &[u64],
    rect: Rect,
    gutter: f32,
    selected: bool,
    matched: u64,
) {
    p.rect_filled(
        rect,
        CornerRadius::ZERO,
        if selected { pal.selection } else { pal.surface },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, pal.hairline),
    );
    let band = Rect::from_min_size(rect.min, egui::vec2(gutter, rect.height()));
    p.rect_filled(
        band,
        CornerRadius::ZERO,
        if selected {
            pal.selection
        } else {
            pal.surface2
        },
    );
    p.line_segment(
        [band.right_top(), band.right_bottom()],
        Stroke::new(1.0, pal.hairline),
    );
    let depth = path.len().saturating_sub(1);
    let x = rect.left() + gutter + 8.0 + depth as f32 * TREE_STEP;
    for k in 0..depth {
        let rx = rail_x(rect.left(), gutter, k);
        p.line_segment(
            [egui::pos2(rx, rect.top()), egui::pos2(rx, rect.bottom())],
            Stroke::new(1.0, pal.control_border.gamma_multiply(0.6)),
        );
    }
    let caret = if g.tree.is_open(path) {
        ph::CARET_DOWN
    } else {
        ph::CARET_RIGHT
    };
    let gx = header_x(rect.left(), gutter, depth);
    let galley = header_galley(p, pal, g, path, matched, rect.right() - gx - 12.0);
    // the caret on the first line of the words
    let top = rect.center().y - galley.size().y * 0.5;
    let first_h = galley.rows.first().map_or(galley.size().y, |r| r.height());
    p.text(
        egui::pos2(x + 6.0, top + first_h * 0.5),
        Align2::CENTER_CENTER,
        caret,
        FontId::new(13.0, FontFamily::Proportional),
        pal.text2,
    );
    p.galley(egui::pos2(gx, top), galley, pal.text);
}

/// A row inside an open group: its position in the group, then the cells.
#[allow(clippy::too_many_arguments)]
fn draw_group_row(
    p: &egui::Painter,
    pal: &Palette,
    snap: &Snapshot,
    cells: Option<&Vec<Option<String>>>,
    row_id: Option<u64>,
    rect: Rect,
    gutter: f32,
    view: &[usize],
    widths: &[f32],
    col_x: &[f32],
    lo_of: &dyn Fn(usize) -> f32,
    selected: bool,
    lines: bool,
    utc: bool,
    shown_as: &std::collections::BTreeMap<String, String>,
    depth: usize,
) {
    if selected {
        p.rect_filled(rect, CornerRadius::ZERO, pal.selection);
    }
    for k in 0..depth {
        let rx = rail_x(rect.left(), gutter, k);
        p.line_segment(
            [egui::pos2(rx, rect.top()), egui::pos2(rx, rect.bottom())],
            Stroke::new(1.0, pal.control_border.gamma_multiply(0.6)),
        );
    }
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(
            1.0,
            if lines {
                pal.hairline
            } else {
                pal.hairline.gamma_multiply(0.55)
            },
        ),
    );
    let band = Rect::from_min_size(rect.min, egui::vec2(gutter, rect.height()));
    p.rect_filled(
        band,
        CornerRadius::ZERO,
        if selected {
            pal.selection
        } else {
            pal.surface2
        },
    );
    p.line_segment(
        [band.right_top(), band.right_bottom()],
        Stroke::new(1.0, pal.hairline),
    );
    p.text(
        egui::pos2(rect.left() + gutter - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        row_label(row_id),
        theme::small_font(),
        if selected { pal.accent_ink } else { pal.text2 },
    );
    let Some(cells) = cells else {
        return;
    };
    for (n, &c) in view.iter().enumerate() {
        let w = widths.get(c).copied().unwrap_or(120.0);
        let (x, lo) = (col_x[n], lo_of(n));
        if x + w < lo || x > rect.right() {
            continue;
        }
        let clip = p.with_clip_rect(
            Rect::from_min_max(
                egui::pos2(x.max(lo), rect.top()),
                egui::pos2(x + w - 4.0, rect.bottom()),
            )
            .intersect(p.clip_rect()),
        );
        if lines {
            p.line_segment(
                [
                    egui::pos2(x + w, rect.top()),
                    egui::pos2(x + w, rect.bottom()),
                ],
                Stroke::new(1.0, pal.hairline),
            );
        }
        if let Some(Some(v)) = cells.get(c) {
            let f = if col_mono(snap, c) {
                theme::data_font()
            } else {
                theme::ui_font()
            };
            // every time column reads as times, as in the plain list
            let is_time = reads_as_time(&snap.col_types, snap.time_col, c);
            let shown = if let Some(t) = shown_cell(snap, shown_as, utc, c, v) {
                t
            } else {
                v.clone()
            };
            clip.text(
                egui::pos2(x + PAD, rect.center().y),
                Align2::LEFT_CENTER,
                shown.replace(['\n', '\r', '\t'], " "),
                f,
                if is_time && n == 0 {
                    pal.text2
                } else {
                    pal.text
                },
            );
        }
    }
}

/// Move pinned columns to the front (keeping their order); returns how many.
pub fn pin_first(view: &mut Vec<usize>, pinned: impl Fn(usize) -> bool) -> usize {
    let (mut a, b): (Vec<usize>, Vec<usize>) = view.iter().partition(|&&c| pinned(c));
    let n = a.len();
    a.extend(b);
    *view = a;
    n
}

/// How far middle-button scrolling moves in `dt` seconds with the pointer
/// `offset` pixels from where it started: nothing inside a small still zone,
/// then faster the farther out, so a nudge creeps and a long pull races.
/// The unit is whatever the caller scrolls by (rows down, pixels across).
pub fn auto_scroll_step(offset: f32, dt: f32, per_px: f32) -> f32 {
    const STILL: f32 = 12.0;
    let d = offset.abs() - STILL;
    if d <= 0.0 {
        return 0.0;
    }
    offset.signum() * (d * per_px + d * d * per_px * 0.02) * dt
}

/// The columns of a block of cells, in the order shown: from where it began
/// to the focused cell, both ends included. `None` when there is no block
/// (the two are the same column, or either is not shown).
pub fn block_cols(view: &[usize], anchor: Option<usize>, focus: Option<usize>) -> Option<Vec<usize>> {
    let a = view.iter().position(|&c| Some(c) == anchor)?;
    let f = view.iter().position(|&c| Some(c) == focus)?;
    (a != f).then(|| view[a.min(f)..=a.max(f)].to_vec())
}

/// A keyboard step of the focused cell across the shown columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColStep {
    Left,
    Right,
    First,
    Last,
}

/// The focused column after a step. `view` is the shown columns in order,
/// `cur` the focused one (None, or one hidden since, starts at the first).
pub fn step_focus_col(view: &[usize], cur: Option<usize>, step: ColStep) -> Option<usize> {
    let last = view.len().checked_sub(1)?;
    let Some(at) = cur.and_then(|c| view.iter().position(|&v| v == c)) else {
        return view.first().copied();
    };
    let to = match step {
        ColStep::Left => at.saturating_sub(1),
        ColStep::Right => (at + 1).min(last),
        ColStep::First => 0,
        ColStep::Last => last,
    };
    Some(view[to])
}

/// The sideways scroll that shows a whole column, moving as little as it can.
/// `before` is the width of the scrolling columns left of it, `w` its own
/// width and `area` the width of the scrolling area. A column wider than the
/// area shows its start.
pub fn hscroll_to_show(hscroll: f32, before: f32, w: f32, area: f32) -> f32 {
    if before < hscroll || w >= area {
        before
    } else if before + w > hscroll + area {
        before + w - area
    } else {
        hscroll
    }
}

/// The first and last scrolling column (view index) with any part between
/// `left` (the pinned edge) and `right`.
pub fn visible_columns(
    xs: &[f32],
    ws: &[f32],
    npin: usize,
    left: f32,
    right: f32,
) -> Option<(usize, usize)> {
    let on: Vec<usize> = (npin..xs.len())
        .filter(|&n| xs[n] + ws[n] > left + 0.5 && xs[n] < right - 0.5)
        .collect();
    Some((*on.first()?, *on.last()?))
}

/// The row-number label: the row's own place in the file (1-based), as a
/// spreadsheet keeps it after filtering or grouping; blank until loaded.
/// What the "Found" band shows for the rows on screen.
#[derive(Default)]
pub struct FoundBand {
    /// The band's heading ("Found", "Why").
    pub title: &'static str,
    /// Wide enough for the longest line among the rows on screen (nothing is cut).
    pub w: f32,
    /// By row id: (the line shown, every keyword with its fields).
    pub rows: std::collections::HashMap<u64, (String, String)>,
    /// By row id: the whole of it as text, for the clipboard (when it says
    /// more than the pointer's note does).
    pub whole: std::collections::HashMap<u64, String>,
}

pub fn row_label(id: Option<u64>) -> String {
    id.map(|r| (r + 1).to_string()).unwrap_or_default()
}

/// Which way a sort runs, in the words that fit what the column holds:
/// times are older or newer, numbers smaller or larger, text runs A to Z.
pub fn sort_words(types: &[logseek_core::types::ColType], time_col: Option<usize>, c: Option<usize>, desc: bool) -> &'static str {
    use logseek_core::types::ColType;
    let ty = c.and_then(|c| types.get(c));
    if c.is_some() && (time_col == c || matches!(ty, Some(ColType::Time))) {
        if desc { "newest first" } else { "oldest first" }
    } else if matches!(ty, Some(ColType::Int | ColType::Float)) {
        if desc { "largest first" } else { "smallest first" }
    } else {
        if desc { "Z to A" } else { "A to Z" }
    }
}

/// The number beside a row in the plain list. By default rows are counted as
/// they are listed (1, 2, 3 in the current sort and filters), so the number
/// says how far down the list a row is. With `by_file` it is the row's own
/// number in the file, which never changes.
pub fn number_label(pos: u64, file: Option<u64>, by_file: bool) -> String {
    if by_file {
        row_label(file)
    } else {
        (pos + 1).to_string()
    }
}

/// Width of the row-number column: room for the largest number (two digits
/// at least) plus padding.
pub fn row_number_width(total: u64, digit_w: f32) -> f32 {
    let digits = total.max(1).to_string().len().max(2);
    digits as f32 * digit_w + 12.0
}
const PAD: f32 = 8.0;
const BAR: f32 = 10.0;

#[derive(Default)]
pub struct GridState {
    /// Colour rules, compiled; rebuilt by the app when the rules change.
    pub colors: crate::colors::Compiled,
    /// Columns hidden from the grid (by name; still searchable).
    pub hidden: std::collections::BTreeSet<String>,
    /// Lines between columns too (from the app's preference).
    pub lines: bool,
    /// Row height in points (0 = the default from the theme).
    pub row_h: f32,
    /// `order` minus hidden columns, recomputed every frame.
    view: Vec<usize>,
    /// The column under the last right-click on the header.
    header_ctx: Option<usize>,
    /// Show time cells in UTC (from the status bar switch).
    pub times_utc: bool,
    /// Number rows by their place in the file instead of their place in the list.
    pub number_by_file: bool,
    /// How a column's values are shown, by column name, where the user chose:
    /// `written` (exactly as in the file) or a kind of time number (`ms`, …).
    pub shown_as: std::collections::BTreeMap<String, String>,
    /// New columns made from fields are named by the field alone.
    pub short_field_names: bool,
    /// Rows on screen last frame (for the timeline's "visible" band).
    pub last_visible: u64,
    /// Some row on screen is taller than a plain row (a quiet-gap band under
    /// it, wrapped text, a group's heading): fewer rows fit than the plain
    /// count says, so the last rows are reached by scrolling to them.
    pub tall: bool,
    /// The column of reasons (or of keywords found) is folded to a narrow
    /// mark, so the rows have the room; a click on its heading opens it again.
    pub found_folded: bool,
    /// Index (within the result) of the first visible row.
    pub top: u64,
    /// Selected result index.
    pub sel: Option<u64>,
    pub hscroll: f32,
    pub order: Vec<usize>,
    /// Columns kept on the left while the rest scroll (by name). The
    /// timeline column is pinned when a file opens.
    pub pinned: std::collections::BTreeSet<String>,
    /// Pin the timeline column once it is known (set on each new file).
    pin_default: bool,
    /// (first, last, of) scrolling columns on screen last frame, 1-based.
    pub cols_on_screen: Option<(usize, usize, usize)>,
    /// Scroll this column into view on the next frame (and flash its name).
    pub goto: Option<usize>,
    /// While a list search's rows are showing: for the rows on screen, which
    /// keywords each holds (a band beside the row numbers).
    pub found: Option<FoundBand>,
    /// The rows selected (click, Shift+click, Ctrl+click, Ctrl+A), by place in the list.
    pub multi: crate::rowsel::RowSel,
    multi_gen: u64,
    /// A Follow result shown as a tree (None = the plain list).
    pub ftree: Option<crate::followtree::FollowTree>,
    /// Rows grouped by columns (None = the plain list).
    pub grouping: Option<crate::groups::Grouping>,
    flash: Option<(usize, f64)>,
    /// The order the grid chose when the file opened ("Reset order").
    pub default_order: Vec<usize>,
    pub widths: Vec<f32>,
    pub layout_for: (usize, u64),
    requested: (u64, u64, usize),
    drag_from: Option<(f32, u64)>,
    /// Columns whose long values wrap onto more lines (rows grow taller).
    pub wrap: std::collections::BTreeSet<usize>,
    /// Columns the user set to one line (never wrapped automatically again).
    no_wrap: std::collections::BTreeSet<usize>,
    /// Rows showing every line of their wrapped cells.
    expanded: std::collections::BTreeSet<u64>,
    /// Text columns held at AUTO_MAX on one line: a cell that runs past the
    /// edge ends with "+N chars" (click: the row opens in full).
    clamped: std::collections::BTreeSet<usize>,
    /// Top and height of each row drawn this frame (rows can differ in height).
    row_spans: Vec<(f32, f32)>,
    /// Top row last frame (gives the scroll direction for loading ahead).
    last_top: u64,
    /// The last few loaded blocks of this result: jumping back is instant.
    recent: Vec<std::sync::Arc<crate::bridge::PageData>>,
    /// This frame's view was not covered by loaded rows (stress: "rows waited").
    pub waited: bool,
    /// The column whose header is being dragged to a new place.
    col_drag: Option<usize>,
    /// Where it would land: (insert before this column, or at the end; gap x).
    col_drop: Option<(Option<usize>, f32)>,
    /// Dev switch: hold a drag in progress for a screenshot.
    preview_before: Option<Option<usize>>,
    /// Horizontal bar drag: (pointer x, scroll at the start).
    hdrag_from: Option<(f32, f32)>,
    /// Width of all columns last frame (decides whether a horizontal bar shows).
    content_w: f32,
    /// Column that expands to fill spare width (the main content column).
    grow_last_informative: Option<usize>,
    /// Set once the user resizes a column by hand: no more automatic growing.
    user_sized: bool,
    /// The cell under the last right-click: (result index, column, value).
    ctx_cell: Option<(u64, usize, Option<String>)>,
    /// The focused cell's column (with `sel` as its row): a click or the
    /// Left and Right keys move it, Up and Down keep it.
    pub focus_col: Option<usize>,
    /// The keyboard moved the focused column: bring it into view this frame.
    focus_scroll: bool,
    /// Where a block of cells began (Shift+click or Shift+arrows stretch it
    /// from here to the focused cell). The same column as the focus: no block.
    pub anchor_col: Option<usize>,
    /// Where a menu opened from the keyboard sits (under its cell or column
    /// name); None = at the pointer, as a right-click opens it.
    menu_at: Option<egui::Pos2>,
    header_menu_at: Option<egui::Pos2>,
    /// Dev switch (screenshots): open a menu as the keyboard would,
    /// 1 = the focused cell's, 2 = its column's.
    pub dev_key_menu: Option<u8>,
    /// Middle-button scrolling: where it was started; the pointer's distance
    /// from there sets the speed.
    auto_at: Option<egui::Pos2>,
    /// Part of a row scrolled so far (speeds below a row a frame add up).
    auto_acc: f32,
    /// The pointer left the start while the button was held: letting go ends it.
    auto_dragged: bool,
}

/// The sub-second digits exactly as the file wrote them ("123456789"), so a
/// UTC reading never loses precision (a zone change never touches them).
fn source_fraction(v: &str) -> Option<String> {
    let v = v.trim();
    let b = v.as_bytes();
    if !b.is_empty() && b.iter().all(u8::is_ascii_digit) {
        // epochs: ms, µs, ns, and 100 ns ticks (FILETIME / .NET)
        let n = match b.len() {
            13 => 3,
            16 | 17 => 6,
            18 => 7,
            19 => 9,
            _ => return None,
        };
        return Some(v[v.len() - n..].to_string());
    }
    if let Some((_, f)) = v
        .split_once('.')
        .filter(|(a, _)| !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit()))
    {
        // "1472063257.123456"
        if f.bytes().all(|c| c.is_ascii_digit()) && !f.is_empty() {
            return Some(f.to_string());
        }
    }
    for i in 3..b.len() {
        if (b[i] == b'.' || b[i] == b',')
            && b[i - 3] == b':'
            && b[i - 2].is_ascii_digit()
            && b[i - 1].is_ascii_digit()
        {
            let digits: String = v[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return (!digits.is_empty()).then_some(digits);
        }
    }
    None
}

/// A time cell read as UTC ("2016-08-24 18:27:26.123456789 UTC"), with every
/// fraction digit the file has; `None` when the text is not a time.
pub fn utc_cell(v: &str) -> Option<String> {
    let f = logseek_core::types::fmt_time(logseek_core::time::parse_time(v)?);
    let secs = f.get(..19).unwrap_or(&f);
    Some(match source_fraction(v) {
        Some(frac) => format!("{secs}.{frac} UTC"),
        None => format!("{f} UTC"),
    })
}

/// The kinds of number a time can be written as, with the words for each.
pub const TIME_KINDS: [(&str, &str); 8] = [
    ("s", "Seconds since 1970 (Unix)"),
    ("ms", "Milliseconds since 1970"),
    ("us", "Microseconds since 1970"),
    ("ns", "Nanoseconds since 1970"),
    ("filetime", "Windows FILETIME (100 ns since 1601)"),
    ("webkit", "WebKit / Chrome (microseconds since 1601)"),
    ("cocoa", "Apple Cocoa (seconds since 2001)"),
    ("days", "Spreadsheet day number"),
];

/// A number written in a cell, read as a time of the given kind and shown the
/// way time cells are. `None` when the cell is not a number or the time is out
/// of range.
pub fn number_cell_as_time(v: &str, kind: &str) -> Option<String> {
    let n: f64 = v.trim().parse().ok()?;
    let us = logseek_core::formula::number_as_time(n, kind)?;
    // years 0001 to 9999 only: anything else is not a time of this kind
    if !(-62_135_596_800_000_000..=253_402_300_799_000_000).contains(&us) {
        return None;
    }
    Some(format!("{} UTC", logseek_core::types::fmt_time(us)))
}

/// What a cell shows when it is not simply its own text: the user's choice for
/// the column first (`written` shows the text itself), then times read as
/// times. `None`: show the value as it is.
pub fn shown_cell(
    snap: &Snapshot,
    shown_as: &std::collections::BTreeMap<String, String>,
    utc: bool,
    c: usize,
    v: &str,
) -> Option<String> {
    match snap.columns.get(c).and_then(|n| shown_as.get(n)).map(String::as_str) {
        Some("written") => None,
        Some(kind) => number_cell_as_time(v, kind),
        None => (utc && reads_as_time(&snap.col_types, snap.time_col, c))
            .then(|| utc_cell(v))
            .flatten(),
    }
}

/// The columns to draw: `order` without the hidden ones (hidden by name, so
/// virtual columns and reloads keep the choice).
pub fn visible_order(
    order: &[usize],
    names: &[String],
    hidden: &std::collections::BTreeSet<String>,
) -> Vec<usize> {
    order
        .iter()
        .copied()
        .filter(|&c| names.get(c).is_none_or(|n| !hidden.contains(n)))
        .collect()
}

impl GridState {
    /// The columns of the selected block of cells, if several are selected.
    pub fn block(&self) -> Option<Vec<usize>> {
        block_cols(&self.view, self.anchor_col, self.focus_col)
    }

    /// The column whose name is being dragged, if one is.
    pub fn dragging_col(&self) -> Option<usize> {
        self.col_drag
    }

    /// The drag ended somewhere else (on the group line): no column moves.
    /// Fits a column's width again from the values on screen (after the way
    /// its values are shown changed).
    pub fn refit(&mut self, c: usize) {
        if let Some(w) = self.widths.get_mut(c) {
            *w = 64.0;
        }
    }

    pub fn cancel_col_drag(&mut self) {
        self.col_drag = None;
        self.col_drop = None;
        self.preview_before = None;
    }

    /// Dev switch (screenshots): show `col` being dragged to before `before`.
    pub fn preview_drag(&mut self, col: usize, before: Option<usize>) {
        self.col_drag = Some(col);
        self.preview_before = Some(before);
    }
}

/// Auto width of a wrapped column: long values wrap instead of widening it.
const WRAP_W: f32 = 420.0;
/// No text column grows past this on its own: longer values wrap.
const AUTO_MAX: f32 = 640.0;
/// Lines a wrapped cell shows before "+N more lines" (click to show all).
const WRAP_LINES: usize = 4;
/// Longest value that wraps; longer ones stay on one line (still whole).
const WRAP_MAX: usize = 20_000;

/// 12345 → "12,345".
fn group_n(n: usize) -> String {
    crate::moment::group(n as u64)
}

/// The row (index from the top of the view) drawn at height `y`.
pub fn row_at(spans: &[(f32, f32)], y: f32) -> Option<usize> {
    spans.iter().position(|&(t, h)| y >= t && y < t + h)
}

/// Longest cell drawn in the interface face; longer ones take the mono path,
/// which lays out only the characters on screen (still shown whole).
const PROP_MAX: usize = 4000;

/// Column `c` holds times: the timeline column, or any column whose values
/// were read as times when the file opened. All of them show the same
/// readable form (a file can hold many: created, changed, seen, closed).
pub fn reads_as_time(
    types: &[logseek_core::types::ColType],
    time_col: Option<usize>,
    c: usize,
) -> bool {
    time_col == Some(c) || matches!(types.get(c), Some(logseek_core::types::ColType::Time))
}

/// Column `c` holds machine-written values (mono face).
fn col_mono(snap: &Snapshot, c: usize) -> bool {
    use logseek_core::types::ColType;
    // times and numbers, or text whose values look machine-written (judged
    // once from the values when the file opened, never from the name)
    matches!(
        snap.col_types.get(c),
        Some(ColType::Time | ColType::Int | ColType::Float)
    ) || snap.col_machine.get(c).copied().unwrap_or(false)
}

/// Rows kept loaded beyond the view in the scroll direction (at least).
const AHEAD: u64 = 40;

/// The block of rows to load, or `None` when the loaded block `have`
/// (start, length) covers the view with enough rows to spare in the scroll
/// direction (`dir` > 0 down, < 0 up, 0 still): `AHEAD`, or ten frames' worth
/// at the current `speed` (rows per frame). Loading starts before the view
/// reaches the edge, so a steady scroll never shows waiting rows.
pub fn page_window(
    top: u64,
    visible: u64,
    total: u64,
    have: Option<(u64, usize)>,
    dir: i32,
    speed: u64,
) -> Option<(u64, usize)> {
    let ahead = AHEAD.max(speed * 10);
    if total == 0 {
        return None;
    }
    let view_end = (top + visible).min(total);
    if let Some((s, len)) = have {
        let e = s + len as u64;
        if s <= top && e >= view_end {
            let below = if e >= total { u64::MAX } else { e - view_end };
            let above = if s == 0 { u64::MAX } else { top - s };
            let enough = match dir {
                d if d > 0 => below >= ahead,
                d if d < 0 => above >= ahead,
                _ => true,
            };
            if enough {
                return None;
            }
        }
    }
    let len = visible + 300.max(speed * 30);
    let start = if dir < 0 {
        top.saturating_sub(240)
    } else {
        top.saturating_sub(60)
    };
    let end = (start + len).min(total);
    Some((start, (end - start) as usize))
}

/// The top row after the selection moved (`moved`): the selected row is
/// brought into view; otherwise the view stays where the user put it.
pub fn follow_selection(top: u64, sel: Option<u64>, visible: u64, moved: bool) -> u64 {
    match sel {
        Some(s) if moved && s < top => s,
        Some(s) if moved && s >= top + visible => s + 1 - visible,
        _ => top,
    }
}

/// Put column `col` just before column `before` (`None`: at the end).
pub fn move_before(order: &mut Vec<usize>, col: usize, before: Option<usize>) {
    if before == Some(col) {
        return;
    }
    let Some(i) = order.iter().position(|&c| c == col) else {
        return;
    };
    order.remove(i);
    let at = before
        .and_then(|b| order.iter().position(|&c| c == b))
        .unwrap_or(order.len());
    order.insert(at, col);
}

/// The gap nearest to `x` among the visible columns `spans` (column, left,
/// right): (the column it is before, or `None` at the end; the gap's x).
pub fn drop_target(spans: &[(usize, f32, f32)], x: f32) -> (Option<usize>, f32) {
    for (i, &(_, l, r)) in spans.iter().enumerate() {
        if x < (l + r) * 0.5 {
            return (Some(spans[i].0), l);
        }
    }
    (None, spans.last().map_or(0.0, |s| s.2))
}

/// Move column `col` one place left (`dir < 0`) or right in `order`.
pub fn move_col(order: &mut [usize], col: usize, dir: i32) {
    let Some(i) = order.iter().position(|&c| c == col) else {
        return;
    };
    let j = i as i64 + dir.signum() as i64;
    if j >= 0 && (j as usize) < order.len() {
        order.swap(i, j as usize);
    }
}

/// How wide the columns shown are together: what there is to scroll across.
/// Hidden columns are not in `view` and take no room, so scrolling stops at
/// the last column shown.
pub fn content_width(view: &[usize], widths: &[f32]) -> f32 {
    view.iter()
        .map(|&c| widths.get(c).copied().unwrap_or(120.0))
        .sum()
}

fn order_widest(order: &[usize], widths: &[f32], constant: &[bool]) -> Option<usize> {
    order
        .iter()
        .copied()
        .filter(|&c| !constant[c])
        .max_by(|&a, &b| widths[a].total_cmp(&widths[b]))
}

/// What a right-click menu item asked for (copy is handled in the grid).
pub enum CellAction {
    Only {
        col: String,
        value: String,
    },
    /// Colour the rows where this column has this value (a new colour rule).
    ColourLike {
        col: String,
        value: String,
    },
    Exclude {
        col: String,
        value: String,
    },
    /// Toggle tag `tag` (0-based) on global row `row`.
    Tag {
        row: u64,
        tag: usize,
    },
    /// Follow this cell's value: the rows whose `to` column holds it (and on
    /// from those, when `repeat`). `to` empty: open the Follow sheet instead.
    Follow {
        row: u64,
        col: String,
        value: String,
        to: String,
        repeat: bool,
    },
}

pub enum HeaderAction {
    /// Remove a column the user made (virtual).
    Remove(usize),
    /// Rename a column the user made.
    Rename(usize),
    /// How new columns made from fields are named: true = the field alone.
    FieldNames(bool),
    /// How this column's values are shown: `None` is the usual way.
    ShowAs(usize, Option<String>),
    /// Switch Compact / Comfortable rows.
    Density,
    /// Open "Add a column" with this column chosen.
    AddFrom(usize),
    /// Open the column chooser (show, hide, reorder).
    Choose,
    /// Lines between columns on or off.
    GridLines,
    /// Copy every value of this column (of the rows shown).
    CopyColumn(usize),
    /// Every shape this column's values have, with counts.
    Shapes(usize),
    /// The values written unlike the rest of this column.
    Strange(usize),
    /// Lines of this column folded into patterns.
    Patterns(usize),
    /// Rare pairs of this column with another.
    Pairs(usize),
    /// Sort by this column (`Some(true)` = largest first), or clear the sort.
    Sort(usize, Option<bool>),
    /// Open "Add a column" on a conversion of this column (preview first).
    Convert(usize, crate::addcol::Convert),
}

pub enum GridEvent {
    None,
    Cell(CellAction),
    Select(u64),
    Open(u64),
    /// Copy the selected rows (with the column names first?).
    CopyRows {
        names: bool,
    },
    /// Grouped view: a file row was selected / opened (by row id).
    SelectRow(u64),
    OpenRow(u64),
    /// From the header menu: things the app owns.
    Header(HeaderAction),
    /// The funnel on a column header was clicked: open its value list here.
    OpenValues {
        col: usize,
        at: egui::Pos2,
    },
}

impl GridState {
    pub fn reset(&mut self) {
        self.top = 0;
        self.sel = None;
        self.hscroll = 0.0;
        self.pinned.clear();
        self.pin_default = true;
        self.requested = (u64::MAX, 0, 0);
    }

    /// (first row index, rows on screen) as of the last frame.
    pub fn visible_range(&self) -> (u64, u64) {
        (self.top, self.last_visible.max(1))
    }

    fn ensure_layout(&mut self, snap: &Snapshot) {
        let n = snap.columns.len();
        let have_sample = snap.page.as_ref().is_some_and(|p| !p.cells.is_empty());
        let key = (n, if have_sample { snap.generation } else { 0 });
        if self.layout_for.0 == n && (self.layout_for.1 != 0 || !have_sample) {
            return;
        }
        if self.layout_for == key {
            return;
        }
        self.layout_for = key;
        // time first, then the source order — except columns that hold the
        // same value on every sampled row: they say nothing, so they go last
        // and the content (e.g. a raw event text column) moves into view
        let constant: Vec<bool> = (0..n)
            .map(|c| match &snap.page {
                Some(p) if p.cells.len() >= 20 => {
                    let mut vals = p
                        .cells
                        .iter()
                        .take(200)
                        .map(|r| r.get(c).and_then(|v| v.as_deref()));
                    let first = vals.next().flatten();
                    vals.all(|v| v == first)
                }
                _ => false,
            })
            .collect();
        let mut order: Vec<usize> = (0..n).filter(|&c| !constant[c]).collect();
        order.extend((0..n).filter(|&c| constant[c]));
        // columns the user made go after the stored key columns (host, user …) and
        // just before the main content column (e.g. _raw), so the pivots stay in view
        let ns = n.saturating_sub(snap.virtual_cols.len());
        let virt: Vec<usize> = (ns..n).collect();
        order.retain(|c| !virt.contains(c));
        let longest = |c: usize| -> usize {
            snap.page
                .as_ref()
                .map(|p| {
                    p.cells
                        .iter()
                        .take(40)
                        .filter_map(|r| r.get(c).and_then(|v| v.as_ref()).map(|v| v.len()))
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        };
        let main = order
            .iter()
            .copied()
            .filter(|&c| c < ns && !constant[c] && Some(c) != snap.time_col)
            .max_by_key(|&c| longest(c));
        let at = main
            .and_then(|m| order.iter().position(|&c| c == m))
            .unwrap_or_else(|| {
                order
                    .iter()
                    .position(|&c| constant.get(c).copied().unwrap_or(false))
                    .unwrap_or(order.len())
            });
        for (k, &v) in virt.iter().enumerate() {
            order.insert(at + k, v);
        }
        if let Some(m) = main {
            order.retain(|&c| c != m);
            order.push(m);
        }
        if let Some(t) = snap.time_col.filter(|&t| t < n) {
            order.retain(|&c| c != t);
            order.insert(0, t);
        }
        self.default_order = order.clone();
        self.order = order;
        self.widths = (0..n)
            .map(|c| {
                let head = snap.columns[c].chars().count();
                let mut longest = head;
                if let Some(p) = &snap.page {
                    // the longest value: nothing is ever trimmed
                    // the first screenful sets the width; columns grow as wider values scroll in
                    let l = p
                        .cells
                        .iter()
                        .take(25)
                        .filter_map(|r| {
                            r.get(c)
                                .and_then(|v| v.as_ref())
                                .map(|v| cell_text(v).chars().count())
                        })
                        .max()
                        .unwrap_or(0);
                    longest = longest.max(l);
                }
                (longest as f32 * 7.9 + PAD * 2.0 + 10.0).max(64.0)
            })
            .collect();
        // a text column wider than AUTO_MAX wraps instead of running on
        for c in 0..n {
            if self.widths[c] > AUTO_MAX && !col_mono(snap, c) && !self.no_wrap.contains(&c) {
                self.widths[c] = AUTO_MAX;
                self.clamped.insert(c);
            }
        }
        // the widest informative column takes the room that is left
        self.grow_last_informative = order_widest(&self.order, &self.widths, &constant);
    }
}

/// Room in a header beside the name: the sort arrow before it and the filter
/// mark after it.
const HEAD_ROOM: f32 = 44.0;

/// Width that shows a column's header and its sampled values in full (capped).
fn fit_width(p: &egui::Painter, snap: &Snapshot, c: usize) -> f32 {
    let head = p
        .layout_no_wrap(
            snap.columns.get(c).cloned().unwrap_or_default(),
            crate::fonts::strong(12.0),
            egui::Color32::WHITE,
        )
        .size()
        .x;
    let mut w = head + 26.0; // room for the funnel
    if let Some(pg) = &snap.page {
        for row in pg.cells.iter().take(120) {
            if let Some(Some(v)) = row.get(c) {
                let t = cell_text(v);
                let f = if col_mono(snap, c) || t.len() > PROP_MAX {
                    theme::data_font()
                } else {
                    theme::ui_font()
                };
                w = w.max(p.layout_no_wrap(t, f, egui::Color32::WHITE).size().x);
            }
        }
    }
    (w + PAD * 2.0).max(48.0)
}

/// The whole value as one line: every character kept, line breaks shown
/// as two spaces so a multi-line value still reads on one row. Never trimmed.
/// Where a cut cell's text starts, in characters: at its beginning, unless
/// the first search hit (`from..to`) does not fit whole in what is shown;
/// then a few characters before the hit, so the hit is what the cell shows.
pub fn hit_start(first_hit: Option<(usize, usize)>, fits: usize) -> usize {
    match first_hit {
        Some((from, to)) if to > fits => from.saturating_sub(6),
        _ => 0,
    }
}

/// True when two rows' lines give the same reason: all of the line but the
/// count of further reasons at its end, which differs from row to row.
pub fn says_the_same(a: &str, b: &str) -> bool {
    let lead = |l: &str| l.rsplit_once("  ·  +").map_or(l.to_string(), |(x, _)| x.to_string());
    !a.is_empty() && lead(a) == lead(b)
}

/// What a row's line says when the row above says the very same: that it
/// is the same, with only the count of further reasons kept when it differs
/// from row to row ("the same  ·  +23 more").
pub fn same_as_above(line: &str) -> String {
    match line.rsplit_once("  ·  +") {
        Some((_, more)) => format!("the same as above  ·  +{more}"),
        None => "the same as above".to_string(),
    }
}

/// True when a value `text_w` wide can be drawn at the kept columns' edge
/// (`lo`) and still lie whole inside its own column (`x`, `w` wide).
pub fn sticky_fits(x: f32, w: f32, lo: f32, text_w: f32) -> bool {
    x < lo && lo + PAD + text_w <= x + w - PAD
}

/// A value as it is shown on one line of a cell: every line of it, in order,
/// each without the spaces it is indented by, one space between them. Text
/// laid out over many indented lines (JSON written to be read, a stack of
/// calls) so reads as one run instead of words with wide gaps between them.
/// Nothing but line breaks and the indenting is left out; a value on one
/// line is shown exactly as it is.
pub fn cell_text(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.split(['\n', '\r'])
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        s.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    snap: &Snapshot,
    st: &mut GridState,
    bridge: &Bridge,
    keys_active: bool,
    filters: &mut Filters,
    terms: &[String],
    tags: &crate::tags::TagMap,
) -> GridEvent {
    st.ensure_layout(snap);
    // a copy for this frame: the choices are few, and the drawing below
    // borrows the rest of the grid's state
    let shown_as = st.shown_as.clone();
    // a new result moves every row's place: the selection starts over
    if st.multi_gen != snap.generation {
        st.multi.clear();
        st.multi_gen = snap.generation;
    }
    let row_h = if st.row_h > 0.0 { st.row_h } else { ROW_H };
    st.view = visible_order(&st.order, &snap.columns, &st.hidden);
    if st.pin_default {
        if let Some(name) = snap.time_col.and_then(|t| snap.columns.get(t)) {
            st.pinned.insert(name.clone());
            st.pin_default = false;
        }
    }
    let npin = {
        let pinned = &st.pinned;
        let cols = &snap.columns;
        pin_first(&mut st.view, |c| {
            cols.get(c).is_some_and(|n| pinned.contains(n))
        })
    };
    let full = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(full, Sense::click_and_drag());
    let painter = ui.painter_at(full);
    if let Some(g) = st.grouping.as_mut() {
        g.sync(snap);
    }
    let grouped = st.grouping.is_some() || st.ftree.is_some();
    let total = match (&st.grouping, &st.ftree) {
        (Some(g), _) => g.lines(),
        (None, Some(t)) => t.len(),
        (None, None) => snap.matched,
    };
    // row numbers (position in the results, like a spreadsheet) after the marks
    let digit_w = painter
        .layout_no_wrap("0".into(), crate::fonts::strong(12.0), pal.text2)
        .size()
        .x;
    let num_w = row_number_width(snap.total_rows.max(total), digit_w);
    let gutter = MARKS + num_w;
    // grouped: a tree strip after the row numbers, one step per level, so a row
    // sits one step in from its deepest group header (its first column lines up
    // with that header's value)
    let band_w = gutter;
    let tree_w = st
        .grouping
        .as_ref()
        .map_or(0.0, |g| g.by.len() as f32 * TREE_STEP + 8.0);
    // a Follow tree: a step per level (deep trees stop stepping at 12)
    let tree_w = st.ftree.as_ref().map_or(tree_w, |t| {
        (t.max_depth.min(FTREE_MAX) as f32 + 1.0) * TREE_STEP + 8.0
    });
    // a list search's rows: a band after the row numbers for what each row holds
    let found_w = if grouped {
        0.0
    } else {
        st.found
            .as_ref()
            .map_or(0.0, |f| if st.found_folded { FOUND_FOLDED_W } else { f.w })
    };
    let gutter = gutter + tree_w + found_w;
    // columns wider than the window: a horizontal bar along the bottom
    let hbar = st.content_w > full.width() - BAR - gutter + 0.5;
    let body = Rect::from_min_max(
        egui::pos2(full.left(), full.top() + HEADER_H + FILTER_H),
        egui::pos2(
            full.right() - BAR,
            full.bottom() - if hbar { BAR } else { 0.0 },
        ),
    );
    let visible = ((body.height() / row_h).floor() as u64).max(1);
    st.last_visible = visible;
    let max_top = if st.wrap.is_empty() && !st.tall {
        total.saturating_sub(visible)
    } else {
        total.saturating_sub(1)
    };
    let mut event = GridEvent::None;

    // ── input ──
    if resp.hovered() {
        let (dy, dx, shift) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.smooth_scroll_delta.x,
                i.modifiers.shift,
            )
        });
        if shift && dy != 0.0 {
            st.hscroll = (st.hscroll - dy).max(0.0);
        } else if dy != 0.0 {
            let lines = (-dy / row_h * 1.0).round() as i64;
            let lines = if lines == 0 {
                -(dy.signum() as i64)
            } else {
                lines
            };
            st.top = (st.top as i64 + lines).clamp(0, max_top as i64) as u64;
        }
        if dx != 0.0 {
            st.hscroll = (st.hscroll - dx).max(0.0);
        }
    }
    // middle button: click to start scrolling by pointer distance (click again,
    // any other click or Esc ends it), or hold and drag (letting go ends it)
    {
        use egui::PointerButton::Middle;
        let (pressed, released, held, ptr, dt, other, esc) = ui.input(|i| {
            (
                i.pointer.button_pressed(Middle),
                i.pointer.button_released(Middle),
                i.pointer.button_down(Middle),
                i.pointer.latest_pos(),
                i.stable_dt.min(0.1),
                i.pointer.primary_pressed() || i.pointer.secondary_pressed(),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if pressed {
            if st.auto_at.is_some() {
                st.auto_at = None;
            } else if ptr.is_some_and(|p| body.contains(p)) {
                st.auto_at = ptr;
                st.auto_acc = 0.0;
                st.auto_dragged = false;
            }
        } else if other || esc || (released && st.auto_dragged) {
            st.auto_at = None;
        }
        if let (Some(at), Some(p)) = (st.auto_at, ptr) {
            let d = p - at;
            if held && d.length() > 12.0 {
                st.auto_dragged = true;
            }
            // rows down: 0.5 rows a second per pixel just past the still zone
            st.auto_acc += auto_scroll_step(d.y, dt, 0.5);
            let whole = st.auto_acc.trunc();
            st.auto_acc -= whole;
            st.top = (st.top as i64 + whole as i64).clamp(0, max_top as i64) as u64;
            st.hscroll = (st.hscroll + auto_scroll_step(d.x, dt, 12.0)).max(0.0);
            // the start, marked: a ring with a dot, above everything
            let mark = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("grid-auto-scroll"),
            ));
            mark.circle_filled(at, 13.0, pal.surface);
            mark.circle_stroke(at, 13.0, Stroke::new(1.5, pal.accent_ink));
            mark.circle_filled(at, 2.5, pal.accent_ink);
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
            ui.ctx().request_repaint();
        }
    }
    if keys_active && total > 0 {
        let mut sel = st.sel.unwrap_or(st.top) as i64;
        let before = sel;
        let page = visible.saturating_sub(1).max(1) as i64;
        ui.input(|i| {
            // with Alt held, Down opens the column menu instead (below)
            if i.key_pressed(egui::Key::ArrowDown) && !i.modifiers.alt {
                sel += 1;
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                sel -= 1;
            }
            if i.key_pressed(egui::Key::PageDown) {
                sel += page;
            }
            if i.key_pressed(egui::Key::PageUp) {
                sel -= page;
            }
            if i.key_pressed(egui::Key::Home) && i.modifiers.command {
                sel = 0;
            }
            if i.key_pressed(egui::Key::End) && i.modifiers.command {
                sel = total as i64 - 1;
            }
        });
        // Left, Right, Home, End: the focused cell moves across the shown
        // columns. On a group header they fold and unfold instead (below);
        // with Alt held they always fold, so a tree keeps a keyboard path.
        let on_header = st.grouping.as_ref().is_some_and(|g| {
            matches!(
                g.tree.item_at(st.sel.unwrap_or(st.top)),
                Some(crate::groups::Item::Header(_))
            )
        });
        let step = ui.input(|i| {
            if i.modifiers.alt || i.modifiers.command || on_header {
                None
            } else if i.key_pressed(egui::Key::ArrowRight) {
                Some(ColStep::Right)
            } else if i.key_pressed(egui::Key::ArrowLeft) {
                Some(ColStep::Left)
            } else if i.key_pressed(egui::Key::Home) {
                Some(ColStep::First)
            } else if i.key_pressed(egui::Key::End) {
                Some(ColStep::Last)
            } else {
                None
            }
        });
        if let Some(step) = step {
            let to = step_focus_col(&st.view, st.focus_col, step);
            if to.is_some() {
                // Shift stretches a block of cells from where it began
                if ui.input(|i| i.modifiers.shift) {
                    st.anchor_col = st.anchor_col.or(st.focus_col);
                } else {
                    st.anchor_col = to;
                }
                st.focus_col = to;
                st.focus_scroll = true;
                if st.sel.is_none() {
                    st.sel = Some(st.top);
                }
            }
        }
        let sel = sel.clamp(0, total as i64 - 1) as u64;
        let moved = sel as i64 != before;
        if moved {
            st.sel = Some(sel);
            event = GridEvent::Select(sel);
            // Shift+arrows stretch the selection, plain arrows start it over
            if ui.input(|i| i.modifiers.shift) {
                st.multi.shift_click(sel);
            } else {
                st.multi.click(sel);
                st.anchor_col = st.focus_col;
            }
        }
        // Ctrl+A: every row of the list
        if !grouped && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) {
            st.multi.all(total);
        }
        if st.sel.is_some() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            event = GridEvent::Open(st.sel.unwrap());
        }
        // only when the keyboard moved the selection: scrolling away from a
        // selected row with the wheel or the scrollbar must stick
        st.top = follow_selection(st.top, st.sel, visible, moved);
    }
    st.top = st.top.min(max_top);

    // ── data for the window (request a margin around it, never block) ──
    // keep the last few blocks of this result; draw from the one covering the
    // most of the view (newest first)
    if let Some(p) = snap
        .page
        .as_ref()
        .filter(|p| p.generation == snap.generation)
    {
        if !st.recent.iter().any(|q| std::sync::Arc::ptr_eq(q, p)) {
            st.recent.push(p.clone());
            if st.recent.len() > 6 {
                st.recent.remove(0);
            }
        }
    }
    st.recent.retain(|q| q.generation == snap.generation);
    let view_end = (st.top + visible).min(total);
    let overlap = |q: &crate::bridge::PageData| {
        let e = q.start + q.rows.len() as u64;
        e.min(view_end).saturating_sub(q.start.max(st.top))
    };
    let mut best: Option<std::sync::Arc<crate::bridge::PageData>> = None;
    for q in st.recent.iter().rev() {
        if best.as_ref().is_none_or(|b| overlap(q) > overlap(b)) {
            best = Some(q.clone());
        }
    }
    // grouped: rows come with their groups, never from the plain list's pages
    let have = if grouped { None } else { best.as_ref() };
    let covered = have.is_some_and(|p| {
        p.start <= st.top && p.start + p.rows.len() as u64 >= (st.top + visible).min(total)
    });
    st.waited = !covered && total > 0 && !grouped;
    let moved = st.top as i64 - st.last_top as i64;
    let dir = moved.signum() as i32;
    // a jump (more than a screen at once) is not a speed
    let speed = if moved.unsigned_abs() <= visible * 2 {
        moved.unsigned_abs()
    } else {
        0
    };
    st.last_top = st.top;
    let loaded = have.map(|p| (p.start, p.rows.len()));
    if let Some(t) = st.ftree.as_mut() {
        t.sync(snap, bridge, st.top, visible);
    }
    if let Some(g) = st.grouping.as_mut() {
        g.request(snap, bridge, st.top, visible);
    } else if st.ftree.is_some() {
        // rows come by id, with the tree
    } else if let Some((start, len)) = page_window(st.top, visible, total, loaded, dir, speed) {
        // a block already on its way counts as loaded: one request at a time
        let inflight =
            (st.requested.0 == snap.generation).then_some((st.requested.1, st.requested.2));
        if page_window(st.top, visible, total, inflight, dir, speed).is_some()
            && st.requested != (snap.generation, start, len)
        {
            st.requested = (snap.generation, start, len);
            bridge.send(Cmd::Page { start, len });
        }
    }

    // ── header ──
    let header = Rect::from_min_max(full.min, egui::pos2(full.right(), full.top() + HEADER_H));
    painter.rect_filled(header, CornerRadius::ZERO, pal.surface);
    painter.line_segment(
        [header.left_bottom(), header.right_bottom()],
        Stroke::new(1.0, pal.hairline),
    );
    painter.rect_filled(
        Rect::from_min_size(header.min, egui::vec2(band_w, header.height())),
        CornerRadius::ZERO,
        pal.surface2,
    );
    painter.text(
        egui::pos2(full.left() + band_w - 8.0, header.center().y),
        egui::Align2::RIGHT_CENTER,
        "#",
        theme::small_font(),
        pal.text2,
    );
    if found_w > 0.0 {
        // the heading folds the column away and opens it again
        let title = st.found.as_ref().map_or("Found", |f| f.title);
        let head = Rect::from_min_max(
            egui::pos2(full.left() + band_w, header.top()),
            egui::pos2(full.left() + band_w + found_w, header.bottom()),
        );
        let hr = ui
            .interact(head, ui.id().with("found-head"), Sense::click())
            .on_hover_text(if st.found_folded {
                "Show this column again"
            } else {
                "Fold this column away, so the rows have the room. Resting on a row still says it all"
            });
        if hr.hovered() {
            painter.rect_filled(head.shrink(2.0), CornerRadius::same(3), pal.surface2);
        }
        painter.text(
            egui::pos2(full.left() + band_w + 8.0, header.center().y),
            egui::Align2::LEFT_CENTER,
            if st.found_folded {
                format!("{title} {}", ph::CARET_RIGHT)
            } else {
                format!("{title} {}", ph::CARET_LEFT)
            },
            theme::small_font(),
            pal.accent_ink,
        );
        if hr.clicked() {
            st.found_folded = !st.found_folded;
            ui.ctx().request_repaint();
        }
    }
    painter.line_segment(
        [
            egui::pos2(full.left() + band_w, header.top()),
            egui::pos2(full.left() + band_w, header.bottom()),
        ],
        Stroke::new(1.0, pal.hairline),
    );
    // nothing is trimmed: a column grows to the widest value on screen
    let cw = painter
        .layout_no_wrap("0".into(), theme::data_font(), pal.text)
        .size()
        .x
        .max(1.0);
    if !st.user_sized {
        // only the columns on screen (and a screen's width either side) are
        // measured: a file with hundreds of columns must not measure every one
        // of them on every frame
        let fit_cols: Vec<usize> = {
            let span = (body.width() - gutter).max(1.0);
            let (lo, hi) = (st.hscroll - span, st.hscroll + 2.0 * span);
            let mut x = 0.0f32;
            let mut v = Vec::new();
            for (n, &c) in st.view.iter().enumerate() {
                let w = st.widths.get(c).copied().unwrap_or(120.0);
                if n < npin {
                    v.push(c);
                    continue;
                }
                if x + w >= lo && x <= hi {
                    v.push(c);
                }
                x += w;
                if x > hi {
                    break;
                }
            }
            v
        };
        // a column is at least as wide as its own name, measured as it is drawn
        for &c in &fit_cols {
            let Some(name) = snap.columns.get(c) else { continue };
            let mut label = name.clone();
            let as_time = match shown_as.get(name).map(String::as_str) {
                Some("written") => false,
                Some(_) => true,
                None => st.times_utc && reads_as_time(&snap.col_types, snap.time_col, c),
            };
            if as_time {
                label.push_str("  (UTC)");
            }
            let need = painter
                .layout_no_wrap(label, crate::fonts::strong(12.0), pal.text)
                .size()
                .x
                + HEAD_ROOM
                + PAD * 2.0;
            if let Some(w) = st.widths.get_mut(c) {
                if *w < need {
                    *w = need;
                }
            }
        }
        if let Some(p) = snap
            .page
            .as_ref()
            .filter(|p| p.generation == snap.generation)
        {
            for i in 0..visible {
                let Some(row) = (st.top + i)
                    .checked_sub(p.start)
                    .and_then(|k| p.cells.get(k as usize))
                else {
                    continue;
                };
                for &c in &fit_cols {
                    let Some(Some(v)) = row.get(c) else { continue };
                    let text = shown_cell(snap, &shown_as, st.times_utc, c, v)
                        .unwrap_or_else(|| cell_text(v));
                    let need = if col_mono(snap, c) || text.len() > PROP_MAX {
                        text.chars().count() as f32 * cw
                    } else {
                        painter
                            .layout_no_wrap(text, theme::ui_font(), pal.text)
                            .size()
                            .x
                    } + PAD * 2.0
                        + 8.0;
                    let need = if st.wrap.contains(&c) {
                        need.min(WRAP_W.max(st.widths.get(c).copied().unwrap_or(0.0)))
                    } else if need > AUTO_MAX && !col_mono(snap, c) && !st.no_wrap.contains(&c) {
                        st.clamped.insert(c);
                        AUTO_MAX
                    } else {
                        need
                    };
                    if let Some(w) = st.widths.get_mut(c) {
                        if *w < need {
                            *w = need;
                        }
                    }
                }
            }
        }
    }
    let mut content_w: f32 = content_width(&st.view, &st.widths);
    // (a hidden column takes no room, and is never the one that grows)
    if let Some(g) = st
        .grow_last_informative
        .filter(|g| !st.user_sized && st.view.contains(g))
    {
        let spare = body.width() - gutter - content_w;
        if spare > 0.0 {
            st.widths[g] += spare;
            content_w += spare;
        } else if st.widths[g] < 380.0 {
            // content wider than the screen: give the main column a generous share anyway
            let want = (body.width() * 0.45).min(900.0);
            if st.widths[g] < want {
                content_w += want - st.widths[g];
                st.widths[g] = want;
            }
        }
    }
    st.hscroll = st
        .hscroll
        .min((content_w - (body.width() - gutter)).max(0.0));
    if (st.content_w - content_w).abs() > 0.5 {
        st.content_w = content_w;
        ui.ctx().request_repaint();
    }
    // pinned columns stay put after the row numbers; the rest scroll under them
    let pin_w: f32 = st.view[..npin]
        .iter()
        .map(|&c| st.widths.get(c).copied().unwrap_or(120.0))
        .sum();
    let scroll_left = full.left() + gutter + pin_w;
    if let Some(g) = st.goto.take() {
        // bring a column to the pinned edge (pinned ones are always in view)
        if let Some(n) = st.view.iter().position(|&c| c == g) {
            if n >= npin {
                let before: f32 = st.view[npin..n]
                    .iter()
                    .map(|&c| st.widths.get(c).copied().unwrap_or(120.0))
                    .sum();
                st.hscroll = before.min((content_w - (body.width() - gutter)).max(0.0));
            }
            st.flash = Some((g, ui.input(|i| i.time)));
        }
    }
    if std::mem::take(&mut st.focus_scroll) {
        if let Some(n) = st.focus_col.and_then(|f| st.view.iter().position(|&c| c == f)) {
            if n >= npin {
                let w_of = |c: usize| st.widths.get(c).copied().unwrap_or(120.0);
                let before: f32 = st.view[npin..n].iter().map(|&c| w_of(c)).sum();
                let area = (body.width() - gutter - pin_w).max(40.0);
                st.hscroll = hscroll_to_show(st.hscroll, before, w_of(st.view[n]), area);
            }
        }
    }
    let col_x: Vec<f32> = {
        let (mut a, mut b) = (full.left() + gutter, scroll_left - st.hscroll);
        st.view
            .iter()
            .enumerate()
            .map(|(n, &c)| {
                let w = st.widths.get(c).copied().unwrap_or(120.0);
                if n < npin {
                    a += w;
                    a - w
                } else {
                    b += w;
                    b - w
                }
            })
            .collect()
    };
    let lo_of = |n: usize| {
        if n < npin {
            full.left() + gutter
        } else {
            scroll_left
        }
    };
    {
        let ws: Vec<f32> = st
            .view
            .iter()
            .map(|&c| st.widths.get(c).copied().unwrap_or(120.0))
            .collect();
        st.cols_on_screen = visible_columns(&col_x, &ws, npin, scroll_left, body.right())
            .map(|(a, b)| (a + 1, b + 1, st.view.len()));
    }
    let head_font = crate::fonts::strong(12.0);
    // right-click a header: everything about that column in one menu
    {
        let hresp = ui.interact(header, ui.id().with("header-menu"), Sense::click_and_drag());
        // drag a header to move its column (resize handles and the funnel sit
        // on top and keep their own drags)
        let spans: Vec<(usize, f32, f32)> = {
            st.view
                .iter()
                .enumerate()
                .map(|(n, &c)| {
                    let w = st.widths.get(c).copied().unwrap_or(120.0);
                    let lo = lo_of(n);
                    (c, col_x[n].max(lo), (col_x[n] + w).max(lo))
                })
                .filter(|s| s.2 > s.1)
                .collect()
        };
        if let Some(b) = st.preview_before {
            let gx = spans
                .iter()
                .find(|s| Some(s.0) == b)
                .map_or(spans.last().map_or(0.0, |s| s.2), |s| s.1);
            st.col_drop = Some((b, gx));
        }
        if hresp.drag_started() {
            if let Some(p) = hresp.interact_pointer_pos() {
                st.col_drag = spans.iter().find(|s| p.x >= s.1 && p.x < s.2).map(|s| s.0);
            }
        }
        if st.col_drag.is_some() && hresp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(p) = hresp.interact_pointer_pos() {
                // near an edge: scroll, so a column can travel across a wide file
                if p.x < scroll_left + 28.0 {
                    st.hscroll = (st.hscroll - 14.0).max(0.0);
                } else if p.x > body.right() - 28.0 {
                    st.hscroll += 14.0;
                }
                st.col_drop = Some(drop_target(&spans, p.x));
                ui.ctx().request_repaint();
            }
        }
        if hresp.drag_stopped() {
            if let (Some(c), Some((before, _))) = (st.col_drag.take(), st.col_drop.take()) {
                move_before(&mut st.order, c, before);
            }
        }
        if hresp.hovered() && st.col_drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        // click a column name: sort by it, again to reverse, a third time to clear
        if hresp.clicked() && st.col_drag.is_none() {
            if let Some(p) = hresp.interact_pointer_pos() {
                for (n, &c) in st.view.iter().enumerate() {
                    let w = st.widths.get(c).copied().unwrap_or(120.0);
                    if p.x >= col_x[n].max(lo_of(n)) && p.x < col_x[n] + w {
                        let cur = snap
                            .sort
                            .as_ref()
                            .filter(|(name, _)| snap.columns.get(c) == Some(name))
                            .map(|(_, desc)| *desc);
                        let next = match cur {
                            None => Some(false),
                            Some(false) => Some(true),
                            Some(true) => None,
                        };
                        event = GridEvent::Header(HeaderAction::Sort(c, next));
                    }
                }
            }
        }
        // Alt+Down: the menu of the focused cell's column, under its name
        let key_header = ((keys_active
            && ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown)))
            || st.dev_key_menu.take_if(|m| *m == 2).is_some())
            && st
                .focus_col
                .and_then(|f| st.view.iter().position(|&c| c == f))
                .map(|n| {
                    st.header_ctx = Some(st.view[n]);
                    st.header_menu_at =
                        Some(egui::pos2(col_x[n].max(lo_of(n)), header.bottom()));
                })
                .is_some();
        if hresp.secondary_clicked() {
            st.header_menu_at = None;
            if let Some(p) = hresp.interact_pointer_pos() {
                st.header_ctx = None;
                for (n, &c) in st.view.iter().enumerate() {
                    let w = st.widths.get(c).copied().unwrap_or(120.0);
                    if p.x >= col_x[n].max(lo_of(n)) && p.x < col_x[n] + w {
                        st.header_ctx = Some(c);
                    }
                }
            }
        }
        let ctx_col = st.header_ctx;
        let ns = snap.columns.len().saturating_sub(snap.virtual_cols.len());
        let st_wrap = st.wrap.clone();
        let (fit_col, mut wrap_col, mut fit_all): (Option<usize>, Option<usize>, bool) =
            (None, None, false);
        let mut pin_col: Option<usize> = None;
        let mut group_col: Option<usize> = None;
        let pinned_now = st.pinned.clone();
        let hpop = egui::Popup::menu(&hresp).open_memory(
            if hresp.secondary_clicked() || key_header {
                Some(egui::SetOpenCommand::Bool(true))
            } else if hresp.clicked() {
                Some(egui::SetOpenCommand::Bool(false))
            } else {
                None
            },
        );
        let hpop = match st.header_menu_at {
            Some(at) => hpop.at_position(at),
            None => hpop.at_pointer_fixed(),
        };
        hpop.show(|ui| {
            // a long menu on a short window scrolls instead of running off the bottom
            let room = (ui.ctx().content_rect().height() - 56.0).max(200.0);
            egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).auto_shrink([true, true]).show(ui, |ui| {
            let Some(c) = ctx_col else {
                ui.label("Right-click a column name");
                return;
            };
            let name = snap.columns.get(c).cloned().unwrap_or_default();
            ui.label(egui::RichText::new(&name).font(crate::fonts::strong(13.0)));
            ui.separator();
            if ui
                .button(format!("{}  Values and filter", ph::FUNNEL_SIMPLE))
                .clicked()
            {
                event = GridEvent::OpenValues {
                    col: c,
                    at: egui::pos2(ui.min_rect().left(), header.bottom() + FILTER_H),
                };
                ui.close();
            }
            if ui
                .button(format!(
                    "{}  Add a column from this one",
                    ph::COLUMNS_PLUS_RIGHT
                ))
                .on_hover_text("Split, clean, decode, classify: a new column made from this one")
                .clicked()
            {
                event = GridEvent::Header(HeaderAction::AddFrom(c));
                ui.close();
            }
            if ui
                .button(format!("{}  Fit this column to its values", ph::ARROWS_OUT_LINE_HORIZONTAL))
                .clicked()
            {
                st.user_sized = true;
                let fit = fit_width(ui.painter(), snap, c);
                if let Some(w) = st.widths.get_mut(c) {
                    *w = fit;
                }
                ui.close();
            }
            if ui
                .button(format!("{}  Move left", ph::ARROW_LEFT))
                .clicked()
            {
                move_col(&mut st.order, c, -1);
                ui.close();
            }
            if ui
                .button(format!("{}  Move right", ph::ARROW_RIGHT))
                .clicked()
            {
                move_col(&mut st.order, c, 1);
                ui.close();
            }
            if ui
                .button(format!("{}  Hide column", ph::EYE_SLASH))
                .on_hover_text("Show it again from the Fields panel (eye icon)")
                .clicked()
            {
                st.hidden.insert(name.clone());
                ui.close();
            }
            if ui
                .button(format!("{}  Copy column name", ph::COPY))
                .clicked()
            {
                crate::clip::copy(ui.ctx(), name.clone(), "the column name");
                ui.close();
            }
            if c >= ns {
                ui.menu_button(format!("{}  Name new field columns", ph::TEXT_AA), |ui| {
                    if ui
                        .radio(!st.short_field_names, "column.field (response.user)")
                        .on_hover_text("Two columns with the same field never share a name")
                        .clicked()
                    {
                        event = GridEvent::Header(HeaderAction::FieldNames(false));
                        ui.close();
                    }
                    if ui
                        .radio(st.short_field_names, "the field alone (user)")
                        .on_hover_text("A second one with the same name is numbered: user (2)")
                        .clicked()
                    {
                        event = GridEvent::Header(HeaderAction::FieldNames(true));
                        ui.close();
                    }
                });
            }
            if c >= ns
                && ui
                    .button(format!("{}  Rename this column", ph::PENCIL_SIMPLE))
                    .on_hover_text("Made columns that use it follow the new name")
                    .clicked()
            {
                event = GridEvent::Header(HeaderAction::Rename(c));
                ui.close();
            }
            if c >= ns
                && ui
                    .button(format!("{}  Remove this column", ph::TRASH))
                    .on_hover_text("A column you made; the file is unchanged")
                    .clicked()
            {
                event = GridEvent::Header(HeaderAction::Remove(c));
                ui.close();
            }
            ui.menu_button(format!("{}  Show values as", ph::CLOCK), |ui| {
                ui.set_min_width(420.0);
                let name = snap.columns[c].clone();
                let chosen = st.shown_as.get(&name).cloned();
                // one value of this column from the rows on screen, to show
                // what each choice would make of it
                let sample: Option<String> = snap.page.as_ref().and_then(|p| {
                    p.cells
                        .iter()
                        .filter_map(|r| r.get(c).and_then(|v| v.clone()))
                        .find(|v| !v.trim().is_empty())
                });
                let is_time = reads_as_time(&snap.col_types, snap.time_col, c);
                ui.label(
                    egui::RichText::new("Only how the values are shown changes: the file, searches and sorting use the values as written.")
                        .font(theme::small_font())
                        .color(pal.text2),
                );
                let mut pick: Option<Option<String>> = None;
                if is_time {
                    if ui
                        .radio(chosen.is_none(), "A time, as LogSeek read it")
                        .clicked()
                    {
                        pick = Some(None);
                    }
                    let raw = sample.clone().unwrap_or_default();
                    if ui
                        .radio(
                            chosen.as_deref() == Some("written"),
                            format!("As written in the file    {raw}"),
                        )
                        .clicked()
                    {
                        pick = Some(Some("written".into()));
                    }
                } else {
                    let raw = sample.clone().unwrap_or_default();
                    if ui
                        .radio(chosen.is_none(), format!("As written in the file    {raw}"))
                        .clicked()
                    {
                        pick = Some(None);
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("A number that is a time:").color(pal.text2));
                    for (kind, words) in TIME_KINDS {
                        let made = sample.as_deref().and_then(|v| number_cell_as_time(v, kind));
                        let line = match &made {
                            Some(t) => format!("{words}    {t}"),
                            None => format!("{words}    (this value is not such a time)"),
                        };
                        if ui
                            .add_enabled(
                                made.is_some() || chosen.as_deref() == Some(kind),
                                egui::RadioButton::new(chosen.as_deref() == Some(kind), line),
                            )
                            .clicked()
                        {
                            pick = Some(Some(kind.to_string()));
                        }
                    }
                }
                if let Some(how) = pick {
                    event = GridEvent::Header(HeaderAction::ShowAs(c, how));
                    ui.close();
                }
            });
            ui.menu_button(format!("{}  Convert", ph::ARROWS_LEFT_RIGHT), |ui| {
                use crate::addcol::Convert;
                use crate::recipes::Decode;
                let items: [(&str, Convert); 12] = [
                    ("Epoch seconds to time", Convert::Time("s")),
                    ("Epoch milliseconds to time", Convert::Time("ms")),
                    ("Epoch microseconds to time", Convert::Time("us")),
                    ("Epoch nanoseconds to time", Convert::Time("ns")),
                    (
                        "Windows ticks (FILETIME) to time",
                        Convert::Time("filetime"),
                    ),
                    ("WebKit / Chrome microseconds since 1601 to time", Convert::Time("webkit")),
                    ("Apple Cocoa seconds since 2001 to time", Convert::Time("cocoa")),
                    ("Spreadsheet day number to time", Convert::Time("days")),
                    ("Decode base64", Convert::Decode(Decode::Base64)),
                    (
                        "Decode base64 (UTF-16)",
                        Convert::Decode(Decode::Base64Utf16),
                    ),
                    ("Decode hex", Convert::Decode(Decode::Hex)),
                    ("Take a field out of JSON text", Convert::Json),
                ];
                for (label, how) in items {
                    if ui
                        .button(label)
                        .on_hover_text(
                            "A new column with a live preview first; the file is unchanged",
                        )
                        .clicked()
                    {
                        event = GridEvent::Header(HeaderAction::Convert(c, how));
                        ui.close();
                    }
                }
            });
            ui.separator();
            let is_pinned = snap.columns.get(c).is_some_and(|n| pinned_now.contains(n));
            if ui
                .button(format!(
                    "{}  {}",
                    ph::PUSH_PIN,
                    if is_pinned {
                        "Let it scroll"
                    } else {
                        "Keep on the left"
                    }
                ))
                .on_hover_text(
                    "A kept column stays beside the row numbers while you scroll sideways",
                )
                .clicked()
            {
                pin_col = Some(c);
                ui.close();
            }
            for (label, how) in [
                // said for what the column holds: times, numbers or text
                (
                    format!(
                        "{}  Sort {}",
                        ph::SORT_ASCENDING,
                        sort_words(&snap.col_types, snap.time_col, Some(c), false)
                    ),
                    Some(false),
                ),
                (
                    format!(
                        "{}  Sort {}",
                        ph::SORT_DESCENDING,
                        sort_words(&snap.col_types, snap.time_col, Some(c), true)
                    ),
                    Some(true),
                ),
            ] {
                if ui.button(label).clicked() {
                    event = GridEvent::Header(HeaderAction::Sort(c, how));
                    ui.close();
                }
            }
            if snap.sort.is_some() && ui.button(format!("{}  Clear the sort", ph::X)).clicked() {
                event = GridEvent::Header(HeaderAction::Sort(c, None));
                ui.close();
            }
            ui.separator();
            let made = c >= ns;
            let group = ui
                .add_enabled(
                    !made,
                    egui::Button::new(format!("{}  Group rows by this column", ph::TREE_VIEW)),
                )
                .on_hover_text("Fold the rows into one group per value, with counts and times")
                .on_disabled_hover_text("Grouping by a column you made is not available yet");
            if group.clicked() {
                group_col = Some(c);
                ui.close();
            }
            let wrapped = st_wrap.contains(&c);
            if ui
                .button(format!(
                    "{}  {}",
                    ph::TEXT_ALIGN_LEFT,
                    if wrapped {
                        "Show on one line"
                    } else {
                        "Wrap long text"
                    }
                ))
                .on_hover_text("Long values wrap onto more lines; the row grows to show them whole")
                .clicked()
            {
                wrap_col = Some(c);
                ui.close();
            }
            if ui
                .button(format!("{}  Choose columns", ph::LIST_CHECKS))
                .on_hover_text("Show, hide and reorder every column, with a sample of each")
                .clicked()
            {
                event = GridEvent::Header(HeaderAction::Choose);
                ui.close();
            }
            // four ways to look into the column, together, each said in a line
            ui.menu_button(format!("{}  Look into this column", ph::MAGNIFYING_GLASS), |ui| {
                ui.set_min_width(420.0);
                let items: [(&str, &str, &str, HeaderAction); 4] = [
                    (
                        ph::SHAPES,
                        "Shapes",
                        "Every shape its values have (digits, letters, the marks between) and how many rows have each: the odd ones stand out.",
                        HeaderAction::Shapes(c),
                    ),
                    (
                        ph::SPARKLE,
                        "Strange text",
                        "The values written unlike the rest of the column (encoded, garbled, broken, disguised), strangest first.",
                        HeaderAction::Strange(c),
                    ),
                    (
                        ph::STACK,
                        "Patterns",
                        "Values with the same wording grouped, the words that change marked; the rare wordings first.",
                        HeaderAction::Patterns(c),
                    ),
                    (
                        ph::LINK_SIMPLE,
                        "Rare pairs",
                        "This column read with another: values rarely seen together, rarest first.",
                        HeaderAction::Pairs(c),
                    ),
                ];
                for (icon, title, line, act) in items {
                    if ui.button(format!("{icon}  {title}")).clicked() {
                        event = GridEvent::Header(act);
                        ui.close();
                    }
                    ui.indent(("look", title), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line)
                                    .font(theme::small_font())
                                    .color(pal.text2),
                            )
                            .wrap(),
                        );
                    });
                }
            });
            if ui
                .button(format!("{}  Copy this column", ph::COPY))
                .on_hover_text("Every value of this column in the rows shown, with its name first")
                .clicked()
            {
                event = GridEvent::Header(HeaderAction::CopyColumn(c));
                ui.close();
            }
            if ui
                .button(format!("{}  Fit every column to its values", ph::COLUMNS))
                .clicked()
            {
                fit_all = true;
                ui.close();
            }
            ui.separator();
            if ui
                .button(format!(
                    "{}  {}",
                    ph::GRID_FOUR,
                    if st.lines {
                        "Hide grid lines"
                    } else {
                        "Show grid lines"
                    }
                ))
                .on_hover_text("Lines between columns as well as rows")
                .clicked()
            {
                event = GridEvent::Header(HeaderAction::GridLines);
                ui.close();
            }
            let dense = row_h < ROW_H;
            if ui
                .button(format!(
                    "{}  {}",
                    ph::ROWS,
                    if dense {
                        "Comfortable rows"
                    } else {
                        "Compact rows"
                    }
                ))
                .clicked()
            {
                event = GridEvent::Header(HeaderAction::Density);
                ui.close();
            }
            });
        });
        let fit = |st: &mut GridState, c: usize| {
            let f = fit_width(&painter, snap, c);
            let f = if st.wrap.contains(&c) {
                f.min(WRAP_W)
            } else {
                f
            };
            if let Some(w) = st.widths.get_mut(c) {
                *w = f;
            }
        };
        if let Some(c) = fit_col {
            st.clamped.remove(&c);
            st.no_wrap.insert(c);
            fit(st, c);
            st.user_sized = true;
        }
        if let Some(name) = group_col.and_then(|c| snap.columns.get(c)) {
            let mut by = st
                .grouping
                .as_ref()
                .map(|g| g.by.clone())
                .unwrap_or_default();
            if !by.contains(name) {
                by.push(name.clone());
            }
            let sort = st.grouping.as_ref().map(|g| g.sort).unwrap_or_default();
            st.grouping = Some(crate::groups::Grouping::new(by, sort));
            st.top = 0;
            st.sel = None;
        }
        if let Some(name) = pin_col.and_then(|c| snap.columns.get(c)) {
            if !st.pinned.remove(name) {
                st.pinned.insert(name.clone());
            }
            st.pin_default = false;
        }
        if let Some(c) = wrap_col {
            if st.wrap.remove(&c) {
                st.no_wrap.insert(c);
                fit(st, c);
            } else {
                st.no_wrap.remove(&c);
                st.clamped.remove(&c);
                st.wrap.insert(c);
                if let Some(w) = st.widths.get_mut(c) {
                    *w = w.min(WRAP_W);
                }
            }
        }
        if fit_all {
            for c in st.view.clone() {
                fit(st, c);
            }
            st.user_sized = true;
        }
    }
    {
        let now = ui.input(|i| i.time);
        for (n, &c) in st.view.iter().enumerate() {
            let w = st.widths.get(c).copied().unwrap_or(120.0);
            let x = col_x[n];
            let lo = lo_of(n);
            let clip = painter
                .with_clip_rect(Rect::from_min_max(egui::pos2(lo, header.top()), header.max));
            if x + w >= lo && x <= full.right() {
                let cell =
                    Rect::from_min_size(egui::pos2(x, header.top()), egui::vec2(w, HEADER_H));
                let cc = clip.with_clip_rect(
                    cell.shrink2(egui::vec2(1.0, 0.0))
                        .intersect(clip.clip_rect()),
                );
                let ns = snap.columns.len().saturating_sub(snap.virtual_cols.len());
                let is_virtual = c >= ns;
                let mut label = if is_virtual {
                    format!("{} {}", ph::FUNCTION, snap.columns[c])
                } else {
                    snap.columns[c].clone()
                };
                // the sorted column says so, and which way
                let sorted = snap
                    .sort
                    .as_ref()
                    .filter(|(name, _)| *name == snap.columns[c])
                    .map(|(_, desc)| *desc);
                if let Some(desc) = sorted {
                    label = format!(
                        "{} {label}",
                        if desc { ph::ARROW_DOWN } else { ph::ARROW_UP }
                    );
                }
                match shown_as.get(&snap.columns[c]).map(String::as_str) {
                    Some("written") => {}
                    // numbers the user reads as times: always shown in UTC
                    Some(_) => label.push_str(zone_suffix(&snap.columns[c], true).unwrap_or("")),
                    None => {
                        if reads_as_time(&snap.col_types, snap.time_col, c) {
                            if let Some(z) = zone_suffix(&snap.columns[c], st.times_utc) {
                                label.push_str(z);
                            }
                        }
                    }
                }
                // just jumped to with "Go to column": the name flashes teal
                let flashing = st.flash.is_some_and(|(f, t)| f == c && now - t < 1.4);
                if flashing {
                    clip.rect_filled(cell, CornerRadius::ZERO, pal.selection);
                    ui.ctx().request_repaint();
                }
                let head_col = if flashing || sorted.is_some() {
                    pal.accent_ink
                } else {
                    pal.text2
                };
                // a wide column scrolled past its left edge keeps its name in view,
                // always inside its own column (never over a neighbour's name)
                let lx = (x + PAD).max(lo + PAD);
                let name_rect = cc.text(
                    egui::pos2(lx, header.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    head_font.clone(),
                    head_col,
                );
                if is_virtual {
                    // a made column: a dotted line under its name (never colour
                    // alone, and no width taken from the name)
                    let y = name_rect.bottom() + 1.5;
                    let mut xd = name_rect.left();
                    while xd < name_rect.right() {
                        cc.line_segment(
                            [
                                egui::pos2(xd, y),
                                egui::pos2((xd + 2.0).min(name_rect.right()), y),
                            ],
                            Stroke::new(1.0, head_col),
                        );
                        xd += 4.0;
                    }
                }
                if is_virtual {
                    let tip = snap
                        .virtual_cols
                        .get(c - ns)
                        .map(|(n, f)| {
                            // how it is made, in words: from which columns, then the formula
                            let from = logseek_core::formula::names_used(f, &snap.columns);
                            let from: Vec<&str> = from.into_iter().filter(|x| x != n).collect();
                            let src = if from.is_empty() {
                                String::new()
                            } else {
                                format!(" from {}", from.join(", "))
                            };
                            format!(
                                "{n}: made by you{src}. Saved with this file's index; the file is unchanged.\nFormula: {f}"
                            )
                        })
                        .unwrap_or_default();
                    ui.interact(
                        cell.shrink2(egui::vec2(24.0, 0.0)),
                        ui.id().with(("vhdr", c)),
                        Sense::hover(),
                    )
                    .on_hover_text(tip);
                }
                clip.line_segment(
                    [
                        egui::pos2(x + w, header.top() + 5.0),
                        egui::pos2(x + w, header.bottom() - 5.0),
                    ],
                    Stroke::new(1.0, pal.hairline),
                );
                // the column's filter: the funnel shows on hover, and always (in the
                // signal colour) when the column is filtered in any way
                let active = filters.get(&snap.columns[c]).is_some_and(|f| f.is_active());
                let fun = Rect::from_center_size(
                    egui::pos2(x + w - 16.0, header.center().y),
                    egui::vec2(24.0, 24.0),
                );
                if fun.left() > lo && fun.right() < body.right() {
                    let fr = ui.interact(fun, ui.id().with(("funnel", c)), Sense::click());
                    let cell_hover = ui.rect_contains_pointer(cell);
                    if fr.has_focus() {
                        painter.rect_stroke(
                            fun,
                            CornerRadius::same(3),
                            Stroke::new(2.0, pal.focus),
                            StrokeKind::Inside,
                        );
                    }
                    if active || cell_hover || fr.hovered() || fr.has_focus() {
                        if fr.hovered() {
                            painter.rect_filled(fun, CornerRadius::same(3), pal.surface2);
                        }
                        let col = if active { pal.accent_ink } else { pal.text2 };
                        painter.text(
                            fun.center(),
                            Align2::CENTER_CENTER,
                            ph::FUNNEL_SIMPLE,
                            FontId::new(13.0, FontFamily::Proportional),
                            col,
                        );
                    }
                    let tip = match filters.get(&snap.columns[c]).filter(|f| f.is_active()) {
                        Some(f) => format!(
                            "Filtered: {}
Values and conditions of {}",
                            crate::filters::describe(&snap.columns[c], f),
                            snap.columns[c]
                        ),
                        None => {
                            format!("Filter {}: pick values or set conditions", snap.columns[c])
                        }
                    };
                    if fr.on_hover_text(tip).clicked() {
                        event = GridEvent::OpenValues {
                            col: c,
                            at: egui::pos2(fun.left() - 220.0, header.bottom() + FILTER_H),
                        };
                    }
                }
            }
        }
    }

    // a column being dragged: its header lifts, a line shows where it lands
    if let (Some(c), Some((_, gx))) = (st.col_drag, st.col_drop) {
        for (n, &v) in st.view.iter().enumerate() {
            let w = st.widths.get(v).copied().unwrap_or(120.0);
            let x = col_x[n];
            if v == c {
                let cell =
                    Rect::from_min_size(egui::pos2(x, header.top()), egui::vec2(w, HEADER_H));
                painter.rect_filled(cell, CornerRadius::ZERO, pal.selection.gamma_multiply(0.6));
                painter.rect_stroke(
                    cell.shrink(1.0),
                    CornerRadius::same(2),
                    Stroke::new(1.0, pal.accent_ink),
                    egui::StrokeKind::Inside,
                );
                // the label stays readable on the lifted header
                if let Some(name) = snap.columns.get(c) {
                    painter.text(
                        egui::pos2(x + PAD, header.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        head_font.clone(),
                        pal.text,
                    );
                }
            }
        }
        let gx = gx.clamp(full.left() + gutter, body.right());
        painter.line_segment(
            [
                egui::pos2(gx, header.top() + 2.0),
                egui::pos2(gx, header.bottom() + FILTER_H - 2.0),
            ],
            Stroke::new(3.0, pal.accent_ink),
        );
    }

    // column borders: drag to resize, double-click to fit the content
    {
        let order = st.view.clone();
        for (n, &c) in order.iter().enumerate() {
            let w = st.widths.get(c).copied().unwrap_or(120.0);
            let edge = col_x[n] + w;
            if edge > lo_of(n) + 8.0 && edge < body.right() - 2.0 {
                let handle = Rect::from_min_max(
                    egui::pos2(edge - 4.0, header.top()),
                    egui::pos2(edge + 4.0, header.bottom()),
                );
                let hr = ui.interact(handle, ui.id().with(("resize", c)), Sense::click_and_drag());
                if hr.hovered() || hr.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    painter.line_segment(
                        [
                            egui::pos2(edge, header.top() + 2.0),
                            egui::pos2(edge, header.bottom() - 2.0),
                        ],
                        Stroke::new(2.0, pal.accent_ink),
                    );
                }
                if hr.dragged() {
                    st.user_sized = true;
                    if let Some(wc) = st.widths.get_mut(c) {
                        *wc = (*wc + hr.drag_delta().x).clamp(48.0, 2400.0);
                    }
                }
                if hr.double_clicked() {
                    st.user_sized = true;
                    let fit = fit_width(&painter, snap, c);
                    if let Some(wc) = st.widths.get_mut(c) {
                        *wc = fit;
                    }
                }
                hr.on_hover_text("Drag to resize · double-click to fit");
            }
        }
    }

    // ── rows ──
    // each row is as tall as its tallest wrapped cell on screen (one line otherwise)
    let base_h = row_h;
    let on_screen: Vec<usize> = {
        let mut v = Vec::new();
        for (n, &c) in st.view.iter().enumerate() {
            let w = st.widths.get(c).copied().unwrap_or(120.0);
            if col_x[n] + w > lo_of(n) && col_x[n] < body.right() {
                v.push(c);
            }
        }
        v
    };
    // quiet gaps: rows on screen followed by a silence far longer than the usual
    // gap between them (rows in time order only); such a row is given room below
    // it for a band with the length of the silence, so the words never cover text
    let mut gap_after: std::collections::HashMap<u64, String> = Default::default();
    if let (false, Some(p), Some(tc)) = (grouped, have, snap.time_col) {
        let ordered = time_ordered(
            &snap.sort,
            snap.columns.get(tc).map(String::as_str),
            snap.time_order,
        );
        if let Some((slack, _)) = ordered {
            let times: Vec<(u64, Option<i64>)> = (st.top..(st.top + visible + 1).min(total))
                .map(|idx| {
                    let t = idx.checked_sub(p.start).and_then(|k| {
                        let v = p.cells.get(k as usize)?.get(tc)?.as_deref()?;
                        logseek_core::time::parse_time(v)
                    });
                    (idx, t)
                })
                .collect();
            let mut gaps: Vec<i64> = times
                .windows(2)
                .filter_map(|w| Some((w[1].1? - w[0].1?).abs()))
                .collect();
            if gaps.len() >= 2 {
                let k = gaps.len() / 2;
                let typical = *gaps.select_nth_unstable(k).1;
                for w in times.windows(2) {
                    if let (Some(a), Some(b)) = (w[0].1, w[1].1) {
                        // a gap no larger than rows may be out of place says nothing
                        if (b - a).abs() <= slack {
                            continue;
                        }
                        let filtered = snap.matched < snap.total_rows;
                        if let Some(words) = quiet_gap_in(a, b, typical, filtered) {
                            gap_after.insert(w[0].0, words);
                        }
                    }
                }
            }
        }
    }
    let mut spans: Vec<(f32, f32)> = Vec::with_capacity(visible as usize);
    {
        let mut yy = body.top();
        for i in 0..visible.max(1) {
            let idx = st.top + i;
            if idx >= total || yy >= body.bottom() {
                break;
            }
            let mut h = base_h;
            let open = st.expanded.contains(&idx);
            if !grouped && (!st.wrap.is_empty() || (open && !st.clamped.is_empty())) {
                let row = have.and_then(|p| {
                    idx.checked_sub(p.start)
                        .and_then(|k| p.cells.get(k as usize))
                });
                if let Some(row) = row {
                    let tall: Vec<usize> = st
                        .wrap
                        .iter()
                        .chain(on_screen.iter().filter(|_| open))
                        .copied()
                        .filter(|c| on_screen.contains(c))
                        .collect();
                    for c in tall {
                        let Some(Some(v)) = row.get(c) else { continue };
                        let t = cell_text(v);
                        if t.len() > WRAP_MAX {
                            continue;
                        }
                        let w = st.widths.get(c).copied().unwrap_or(120.0) - PAD * 2.0 - 4.0;
                        let f = if col_mono(snap, c) {
                            theme::data_font()
                        } else {
                            theme::ui_font()
                        };
                        let g = painter.layout(t, f, pal.text, w.max(40.0));
                        let lines = g.rows.len().max(1);
                        let line_h = g.size().y / lines as f32;
                        let shown = if open {
                            lines + usize::from(lines > 1)
                        } else {
                            lines.min(WRAP_LINES)
                        };
                        h = h.max(shown as f32 * line_h + 8.0);
                    }
                }
            }
            if let Some(g) = st.grouping.as_ref() {
                if let Some(crate::groups::Item::Header(path)) = g.tree.item_at(idx) {
                    let gx = header_x(full.left(), band_w, path.len().saturating_sub(1));
                    let gal = header_galley(
                        &painter,
                        pal,
                        g,
                        &path,
                        snap.matched,
                        body.right() - gx - 12.0,
                    );
                    h = h.max(gal.size().y + 10.0);
                }
            }
            if gap_after.contains_key(&idx) {
                h += GAP_H;
            }
            spans.push((yy, h));
            yy += h;
        }
    }
    st.tall = spans.iter().any(|&(_, h)| h > row_h + 0.5);
    st.row_spans = spans.clone();
    let hover_row = resp
        .hover_pos()
        .filter(|p| body.contains(*p))
        .and_then(|p| row_at(&spans, p.y))
        .map(|i| st.top + i as u64);
    let body_clip = painter.with_clip_rect(body);
    let mut gutter_tip: Option<String> = None;
    let mut toggle_row: Option<u64> = None;
    let mut toggle_group: Option<Vec<u64>> = None;
    let mut toggle_ftree: Option<u64> = None;
    for (i, &(y, row_h)) in spans.iter().enumerate() {
        let idx = st.top + i as u64;
        if idx >= total {
            break;
        }
        // the row's own height, without the room for a quiet-gap band below it
        let own_h = if gap_after.contains_key(&idx) {
            row_h - GAP_H
        } else {
            row_h
        };
        if let Some(t) = st.ftree.as_ref() {
            let Some((row, depth)) = t.line(idx) else {
                continue;
            };
            let rect =
                Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(body.width(), row_h));
            let selected = st.sel == Some(idx);
            let d = depth.min(FTREE_MAX) as usize;
            draw_group_row(
                &body_clip,
                pal,
                snap,
                t.cells(row),
                Some(row),
                rect,
                band_w,
                &st.view,
                &st.widths,
                &col_x,
                &lo_of,
                selected,
                st.lines,
                st.times_utc,
                &shown_as,
                d,
            );
            // the fold arrow at the row's own level; past the last step, the level as a number
            let cx = rail_x(rect.left(), band_w, d);
            let kids = t.children(row);
            if kids > 0 {
                body_clip.text(
                    egui::pos2(cx, rect.center().y),
                    Align2::CENTER_CENTER,
                    if t.is_open(row) {
                        ph::CARET_DOWN
                    } else {
                        ph::CARET_RIGHT
                    },
                    FontId::new(13.0, FontFamily::Proportional),
                    pal.text2,
                );
            } else {
                body_clip.circle_filled(egui::pos2(cx, rect.center().y), 2.0, pal.text2);
            }
            let strip = Rect::from_min_max(
                egui::pos2(rect.left() + band_w, rect.top()),
                egui::pos2(rect.left() + gutter, rect.bottom()),
            );
            if resp.clicked()
                && resp
                    .interact_pointer_pos()
                    .is_some_and(|p| strip.contains(p))
            {
                toggle_ftree = Some(row);
            }
            if resp.hover_pos().is_some_and(|p| strip.contains(p)) {
                gutter_tip = Some(if kids > 0 {
                    format!(
                        "Level {} · {} linked {} beneath (click to fold or unfold)",
                        depth + 1,
                        kids,
                        if kids == 1 { "row" } else { "rows" }
                    )
                } else {
                    format!("Level {} · nothing links to this row", depth + 1)
                });
            }
            continue;
        }
        if let Some(g) = st.grouping.as_ref() {
            let rect =
                Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(body.width(), row_h));
            let selected = st.sel == Some(idx);
            match g.tree.item_at(idx) {
                Some(crate::groups::Item::Header(path)) => {
                    draw_group_header(
                        &body_clip,
                        pal,
                        g,
                        &path,
                        rect,
                        band_w,
                        selected,
                        snap.matched,
                    );
                    if resp.clicked()
                        && resp
                            .interact_pointer_pos()
                            .is_some_and(|p| rect.contains(p))
                    {
                        toggle_group = Some(path);
                    }
                }
                Some(crate::groups::Item::Row(path, r)) => {
                    let cells = g.row_cells(snap, &path, r);
                    draw_group_row(
                        &body_clip,
                        pal,
                        snap,
                        cells,
                        g.row_id(snap, idx),
                        rect,
                        band_w,
                        &st.view,
                        &st.widths,
                        &col_x,
                        &lo_of,
                        selected,
                        st.lines,
                        st.times_utc,
                        &shown_as,
                        path.len(),
                    );
                }
                None => {}
            }
            continue;
        }
        let row_rect =
            Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(body.width(), row_h));
        let selected = st.sel == Some(idx);
        let in_multi = st.multi.count() > 1 && st.multi.contains(idx);
        // a block of cells: only its cells are filled, not the whole rows
        let block = st.block();
        let in_block_row = block.is_some() && (in_multi || (st.multi.count() <= 1 && selected));
        // colour rules: checked on this row only (rows on screen), first match wins
        let tint = if st.colors.is_empty() {
            None
        } else {
            have.and_then(|p| p.cells.get(idx.checked_sub(p.start)? as usize))
                .and_then(|row| st.colors.row_match(&snap.columns, row))
                .map(|(t, name)| (t, name.to_string()))
        };
        if in_block_row {
            if let Some(cols) = &block {
                for (n, &c) in st.view.iter().enumerate() {
                    if cols.contains(&c) {
                        let w = st.widths.get(c).copied().unwrap_or(120.0);
                        let lo = lo_of(n);
                        let r = Rect::from_min_max(
                            egui::pos2(col_x[n].max(lo), y),
                            egui::pos2(col_x[n] + w, y + row_h),
                        );
                        if r.width() > 0.0 {
                            body_clip.rect_filled(r, CornerRadius::ZERO, pal.selection);
                        }
                    }
                }
            }
        } else if selected || in_multi {
            body_clip.rect_filled(row_rect, CornerRadius::ZERO, pal.selection);
        } else if let Some((t, _)) = &tint {
            let c = crate::colors::tint_color(*t, pal.dark);
            body_clip.rect_filled(
                row_rect,
                CornerRadius::ZERO,
                c.gamma_multiply(if pal.dark { 0.20 } else { 0.15 }),
            );
            if hover_row == Some(idx) {
                body_clip.rect_stroke(
                    row_rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, pal.control_border),
                    egui::StrokeKind::Inside,
                );
            }
        } else if hover_row == Some(idx) {
            body_clip.rect_filled(row_rect, CornerRadius::ZERO, pal.surface2);
        }
        body_clip.line_segment(
            [row_rect.left_bottom(), row_rect.right_bottom()],
            Stroke::new(
                1.0,
                if st.lines {
                    pal.hairline
                } else {
                    pal.hairline.gamma_multiply(0.55)
                },
            ),
        );
        // the row-number band: the header's colour, a divider on its right, so
        // it reads as part of the grid and never as a data column
        let band = Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(gutter, row_h));
        body_clip.rect_filled(
            band,
            CornerRadius::ZERO,
            if selected || in_multi {
                pal.selection
            } else {
                pal.surface2
            },
        );
        body_clip.line_segment(
            [band.right_top(), band.right_bottom()],
            Stroke::new(1.0, pal.hairline),
        );
        body_clip.line_segment(
            [band.left_bottom(), band.right_bottom()],
            Stroke::new(1.0, pal.hairline.gamma_multiply(0.55)),
        );
        // gutter: the row's tag marks, and the teal ring when selected
        let row_id = have.and_then(|p| {
            idx.checked_sub(p.start)
                .and_then(|k| p.rows.get(k as usize))
                .copied()
        });
        let gc = egui::pos2(full.left() + MARKS * 0.5, y + own_h * 0.5);
        if let Some(mask) = row_id.and_then(|r| tags.get(&r)).copied() {
            let marks: Vec<usize> = (0..9).filter(|i| mask >> i & 1 == 1).collect();
            match marks.len() {
                1 => crate::tags::draw(
                    &body_clip,
                    marks[0],
                    gc,
                    5.0,
                    crate::tags::color(marks[0], pal.dark),
                ),
                _ => {
                    for (k, &m) in marks.iter().take(2).enumerate() {
                        let c = gc + egui::vec2(if k == 0 { -4.5 } else { 4.5 }, 0.0);
                        crate::tags::draw(&body_clip, m, c, 4.0, crate::tags::color(m, pal.dark));
                    }
                }
            }
        }
        if let Some((t, name)) = &tint {
            // the marker: colour is never the only cue, and it says what it means
            let m = Rect::from_min_size(
                egui::pos2(full.left() + MARKS - 6.0, y + 3.0),
                egui::vec2(4.0, row_h - 6.0),
            );
            body_clip.rect_filled(
                m,
                CornerRadius::same(2),
                crate::colors::tint_color(*t, pal.dark),
            );
            if resp
                .hover_pos()
                .is_some_and(|p| p.y >= y && p.y < y + row_h && p.x < full.left() + gutter)
            {
                gutter_tip = Some(format!("Coloured by rule: {name}"));
            }
        }
        // which keywords of the list this row holds; all of them on hover
        if found_w > 0.0 {
            if let Some((line, all)) = row_id.and_then(|id| st.found.as_ref()?.rows.get(&id)) {
                if st.found_folded {
                    // folded: a mark that the row has something to say
                    body_clip.circle_filled(
                        egui::pos2(full.left() + gutter - found_w * 0.5, y + own_h * 0.5),
                        3.5,
                        pal.accent_ink,
                    );
                } else {
                    // a run of rows that say the same thing says it once: the
                    // rows under the first say, quietly, that they are the same
                    let above = idx
                        .checked_sub(1)
                        .filter(|_| i > 0)
                        .and_then(|prev| {
                            let p = have?;
                            let k = prev.checked_sub(p.start)? as usize;
                            p.rows.get(k).copied()
                        })
                        .and_then(|id| st.found.as_ref()?.rows.get(&id));
                    let same = above.is_some_and(|(l, _)| says_the_same(l, line));
                    body_clip.text(
                        egui::pos2(full.left() + gutter - found_w + 8.0, y + own_h * 0.5),
                        egui::Align2::LEFT_CENTER,
                        if same { same_as_above(line) } else { line.clone() },
                        theme::small_font(),
                        if same { pal.text2 } else { pal.accent_ink },
                    );
                }
                if resp.hover_pos().is_some_and(|p| {
                    p.y >= y
                        && p.y < y + row_h
                        && p.x >= full.left() + gutter - found_w
                        && p.x < full.left() + gutter
                }) {
                    // a right-click on the line copies all of it
                    if ui.input(|i| i.pointer.secondary_clicked()) {
                        let text = row_id
                            .and_then(|id| st.found.as_ref()?.whole.get(&id))
                            .unwrap_or(all)
                            .clone();
                        crate::clip::copy(ui.ctx(), text, "what this line says, in full");
                    }
                    gutter_tip = Some(format!("{all}\nRight-click to copy all of it"));
                }
            }
        }
        body_clip.text(
            egui::pos2(full.left() + gutter - found_w - 8.0, y + own_h * 0.5),
            egui::Align2::RIGHT_CENTER,
            // a tree of followed rows has no single order to count along
            number_label(idx, row_id, st.number_by_file || st.ftree.is_some()),
            if selected {
                crate::fonts::strong(12.0)
            } else {
                theme::small_font()
            },
            if selected { pal.accent_ink } else { pal.text2 },
        );
        if selected {
            // the annotation ring in the gutter: the product's signature mark
            body_clip.circle_stroke(gc, 8.0, Stroke::new(1.6, pal.accent_ink));
            body_clip.rect_filled(
                Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(3.0, row_h)),
                CornerRadius::ZERO,
                pal.accent_ink,
            );
        }
        let cells = have.and_then(|p| {
            let k = idx.checked_sub(p.start)? as usize;
            p.cells.get(k)
        });
        // made-column cells of this row that could not be worked out, and why
        let row_errors: Vec<(usize, &str)> = have
            .and_then(|p| {
                let k = idx.checked_sub(p.start)? as usize;
                Some(
                    p.errors
                        .iter()
                        .filter(|e| e.0 == k)
                        .map(|e| (e.1, e.2.as_str()))
                        .collect(),
                )
            })
            .unwrap_or_default();
        let mut hit_beyond = false;
        // where the first hit past the right edge is, on screen
        let mut hit_x: Option<f32> = None;
        // and the column it is in
        let mut hit_col: Option<usize> = None;
        for (n, &c) in st.view.iter().enumerate() {
            let w = st.widths.get(c).copied().unwrap_or(120.0);
            let x = col_x[n];
            let lo = lo_of(n);
            let cell_clip =
                body_clip.with_clip_rect(Rect::from_min_max(egui::pos2(lo, body.top()), body.max));
            if x + w >= lo && x <= body.right() {
                if st.lines {
                    cell_clip.line_segment(
                        [egui::pos2(x + w, y), egui::pos2(x + w, y + row_h)],
                        Stroke::new(1.0, pal.hairline),
                    );
                }
                let r = Rect::from_min_size(egui::pos2(x, y), egui::vec2(w - 4.0, row_h));
                let cp = cell_clip.with_clip_rect(r.intersect(cell_clip.clip_rect()));
                if selected && st.focus_col == Some(c) {
                    // the focused cell: an outline in the accent, inside the cell
                    cell_clip.rect_stroke(
                        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, row_h)).shrink(0.75),
                        CornerRadius::same(2),
                        Stroke::new(1.5, pal.accent_ink),
                        egui::StrokeKind::Inside,
                    );
                }
                match cells {
                    Some(row) => {
                        if let Some(Some(v)) = row.get(c) {
                            // a colour rule for single cells: a soft tint, and a
                            // corner mark at full strength (never colour alone)
                            if st.colors.has_cell_rules() && !selected {
                                if let Some(t) = snap
                                    .columns
                                    .get(c)
                                    .and_then(|name| st.colors.cell_match(name, v))
                                {
                                    let tc = crate::colors::tint_color(t, pal.dark);
                                    let cell =
                                        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, row_h));
                                    cp.rect_filled(
                                        cell,
                                        CornerRadius::ZERO,
                                        tc.gamma_multiply(if pal.dark { 0.26 } else { 0.20 }),
                                    );
                                    cp.add(egui::Shape::convex_polygon(
                                        vec![
                                            egui::pos2(x, y),
                                            egui::pos2(x + 7.0, y),
                                            egui::pos2(x, y + 7.0),
                                        ],
                                        tc,
                                        Stroke::NONE,
                                    ));
                                }
                            }
                            let color = if n == 0 && snap.time_col == Some(c) {
                                pal.text2
                            } else {
                                pal.text
                            };
                            let utc_text;
                            let v: &String = match shown_cell(snap, &shown_as, st.times_utc, c, v) {
                                Some(t) => {
                                    utc_text = t;
                                    &utc_text
                                }
                                None => v,
                            };
                            let text = cell_text(v);
                            let mut tx = x + PAD;
                            // the first line of every cell lines up, also in tall rows
                            let mid = y + base_h * 0.5;
                            let vis = cp.clip_rect();
                            let hl = pal
                                .accent
                                .gamma_multiply(if pal.dark { 0.32 } else { 0.45 });
                            let mono = col_mono(snap, c) || text.len() > PROP_MAX;
                            let font = if mono {
                                theme::data_font()
                            } else {
                                theme::ui_font()
                            };
                            // a wide column scrolled part-way under the kept columns:
                            // a short value stays in view, as the column's name does
                            if x < lo && text.len() <= 64 {
                                let tw = if mono && text.is_ascii() {
                                    text.len() as f32 * cw
                                } else {
                                    cp.layout_no_wrap(text.to_string(), font.clone(), color)
                                        .size()
                                        .x
                                };
                                if sticky_fits(x, w, lo, tw) {
                                    tx = lo + PAD;
                                }
                            }
                            let open_row = st.expanded.contains(&idx);
                            let clamp_here =
                                !open_row && !st.wrap.contains(&c) && text.len() <= PROP_MAX;
                            if clamp_here
                                && cp
                                    .layout_no_wrap(text.to_string(), font.clone(), color)
                                    .size()
                                    .x
                                    > w - PAD * 2.0 - 4.0
                            {
                                // one line up to the edge, then how much more there is
                                let room = w - PAD * 2.0 - 4.0;
                                let total = text.chars().count();
                                let mark_w = 86.0;
                                let g = cp.layout_no_wrap(text.to_string(), font.clone(), color);
                                let fits = g
                                    .cursor_from_pos(egui::vec2(room - mark_w, 0.0))
                                    .index
                                    .0
                                    .min(total);
                                // a search hit past what fits: the text starts just
                                // before it, and says how much comes before
                                let first_hit = if terms.is_empty() {
                                    None
                                } else {
                                    match_ranges(&text, terms)
                                        .first()
                                        .map(|r| {
                                            let from = text[..r.0].chars().count();
                                            (from, from + text[r.0..r.1].chars().count())
                                        })
                                };
                                let skip = hit_start(first_hit, fits);
                                let mut left = tx;
                                if skip > 0 {
                                    let lg = cp.layout_no_wrap(
                                        format!("+{} before", group_n(skip)),
                                        theme::small_font(),
                                        pal.accent_ink,
                                    );
                                    let at = egui::pos2(tx, mid - lg.size().y * 0.5);
                                    let r = Rect::from_min_size(at, lg.size() + egui::vec2(4.0, 2.0));
                                    left = tx + lg.size().x + 8.0;
                                    cp.galley(at, lg, pal.accent_ink);
                                    if ui
                                        .interact(r, ui.id().with(("before", idx, c)), Sense::click())
                                        .on_hover_text("Show this row in full")
                                        .clicked()
                                    {
                                        toggle_row = Some(idx);
                                    }
                                }
                                let from = text
                                    .char_indices()
                                    .nth(skip)
                                    .map_or(text.len(), |(i, _)| i);
                                let part = &text[from..];
                                let part_total = total - skip;
                                let g = cp.layout_job(crate::filters::highlighted(
                                    part,
                                    terms,
                                    font.clone(),
                                    color,
                                    hl,
                                ));
                                let text_right = tx + room - mark_w;
                                let shown = g
                                    .cursor_from_pos(egui::vec2((text_right - left).max(0.0), 0.0))
                                    .index
                                    .0
                                    .min(part_total);
                                let rest = part_total - shown;
                                let tclip = cp.with_clip_rect(
                                    Rect::from_min_max(
                                        egui::pos2(x, y),
                                        egui::pos2(
                                            if rest > 0 { text_right } else { tx + room },
                                            y + row_h,
                                        ),
                                    )
                                    .intersect(cp.clip_rect()),
                                );
                                tclip.galley(egui::pos2(left, mid - g.size().y * 0.5), g, color);
                                if rest > 0 {
                                    let label = format!("+{} chars", group_n(rest));
                                    let mg = cp.layout_no_wrap(
                                        label,
                                        theme::small_font(),
                                        pal.accent_ink,
                                    );
                                    let at = egui::pos2(text_right + 6.0, mid - mg.size().y * 0.5);
                                    let r =
                                        Rect::from_min_size(at, mg.size() + egui::vec2(4.0, 2.0));
                                    cp.galley(at, mg, pal.accent_ink);
                                    if ui
                                        .interact(r, ui.id().with(("chars", idx, c)), Sense::click())
                                        .on_hover_text("Show this row in full")
                                        .clicked()
                                    {
                                        toggle_row = Some(idx);
                                    }
                                }
                            } else if (st.wrap.contains(&c) || open_row) && text.len() <= WRAP_MAX {
                                let mut job = crate::filters::highlighted(
                                    &text,
                                    terms,
                                    font.clone(),
                                    color,
                                    hl,
                                );
                                job.wrap.max_width = (w - PAD * 2.0 - 4.0).max(40.0);
                                let g = cp.layout_job(job);
                                let lines = g.rows.len().max(1);
                                let line_h = g.size().y / lines as f32;
                                let open = open_row;
                                let cut = lines > WRAP_LINES && !open;
                                let keep = if cut { WRAP_LINES - 1 } else { lines };
                                let text_clip = cp.with_clip_rect(
                                    Rect::from_min_max(
                                        egui::pos2(x, y),
                                        egui::pos2(x + w, y + 4.0 + keep as f32 * line_h),
                                    )
                                    .intersect(cp.clip_rect()),
                                );
                                text_clip.galley(egui::pos2(tx, y + 4.0), g, color);
                                if lines > WRAP_LINES || (open && lines > 1) {
                                    // say what is not shown, and open it in one click
                                    let label = if cut {
                                        format!("+{} more lines", lines - keep)
                                    } else {
                                        "Show less".to_string()
                                    };
                                    let at = egui::pos2(tx, y + 4.0 + keep as f32 * line_h);
                                    let g = cp.layout_no_wrap(
                                        label,
                                        theme::small_font(),
                                        pal.accent_ink,
                                    );
                                    let r =
                                        Rect::from_min_size(at, g.size() + egui::vec2(4.0, 2.0));
                                    cp.galley(at, g, pal.accent_ink);
                                    if ui
                                        .interact(r, ui.id().with(("more", idx, c)), Sense::click())
                                        .on_hover_text(if cut {
                                            "Show every line of this row"
                                        } else {
                                            "Back to the first lines"
                                        })
                                        .clicked()
                                    {
                                        toggle_row = Some(idx);
                                    }
                                }
                            } else if text.is_ascii() && mono {
                                // monospace: lay out only the characters on screen
                                let n = text.len();
                                let first =
                                    (((vis.left() - tx) / cw).floor().max(0.0) as usize).min(n);
                                let last = ((((vis.right() - tx) / cw).ceil().max(0.0)) as usize
                                    + 1)
                                .min(n);
                                if !terms.is_empty() {
                                    for (a, b) in match_ranges(&text, terms) {
                                        let hr = Rect::from_min_size(
                                            egui::pos2(tx + a as f32 * cw - 1.0, y + 4.0),
                                            egui::vec2((b - a) as f32 * cw + 2.0, row_h - 8.0),
                                        );
                                        cp.rect_filled(hr, CornerRadius::same(2), hl);
                                        if hr.left() > body.right() {
                                            hit_beyond = true;
                                            hit_x.get_or_insert(hr.left());
                                        }
                                    }
                                }
                                if first < last {
                                    let g = cp.layout_no_wrap(
                                        text[first..last].to_string(),
                                        font.clone(),
                                        color,
                                    );
                                    cp.galley(
                                        egui::pos2(tx + first as f32 * cw, mid - g.size().y * 0.5),
                                        g,
                                        color,
                                    );
                                }
                            } else {
                                if !terms.is_empty() {
                                    for (a, b) in match_ranges(&text, terms) {
                                        let pre = cp
                                            .layout_no_wrap(
                                                text[..a].to_string(),
                                                font.clone(),
                                                color,
                                            )
                                            .size()
                                            .x;
                                        let wid = cp
                                            .layout_no_wrap(
                                                text[a..b].to_string(),
                                                font.clone(),
                                                color,
                                            )
                                            .size()
                                            .x;
                                        let hr = Rect::from_min_size(
                                            egui::pos2(tx + pre - 1.0, y + 4.0),
                                            egui::vec2(wid + 2.0, row_h - 8.0),
                                        );
                                        cp.rect_filled(hr, CornerRadius::same(2), hl);
                                        if hr.left() > body.right() {
                                            hit_beyond = true;
                                            hit_x.get_or_insert(hr.left());
                                        }
                                    }
                                }
                                let g = cp.layout_no_wrap(text, font.clone(), color);
                                cp.galley(egui::pos2(tx, mid - g.size().y * 0.5), g, color);
                            }
                        } else if let Some(&(_, why)) = row_errors.iter().find(|e| e.0 == c) {
                            // could not be worked out: a warning mark, the reason on hover
                            cp.text(
                                egui::pos2(x + PAD, y + base_h * 0.5),
                                egui::Align2::LEFT_CENTER,
                                ph::WARNING,
                                theme::ui_font(),
                                pal.warning,
                            );
                            if resp.hover_pos().is_some_and(|p| r.contains(p)) {
                                gutter_tip = Some(why.to_string());
                            }
                        }
                    }
                    None => {
                        // not loaded yet: a quiet placeholder, never a spinner per cell
                        let bar = Rect::from_min_size(
                            egui::pos2(x + PAD, y + own_h * 0.5 - 3.0),
                            egui::vec2((w * 0.45).min(90.0), 6.0),
                        );
                        cp.rect_filled(
                            bar,
                            CornerRadius::same(3),
                            pal.hairline.gamma_multiply(0.7),
                        );
                    }
                }
            } else if x > body.right() && !hit_beyond && !terms.is_empty() {
                // a cell off the right edge: does it hold a search hit?
                if let Some(Some(v)) = cells.and_then(|row| row.get(c)) {
                    hit_beyond = !match_ranges(v, terms).is_empty();
                    if hit_beyond {
                        hit_x.get_or_insert(x);
                        hit_col.get_or_insert(c);
                    }
                }
            }
        }
        if hit_beyond {
            // a search hit further right in this row: an arrow at the edge
            // that brings it into view
            let m = Rect::from_min_size(
                egui::pos2(body.right() - 24.0, y + 3.0),
                egui::vec2(22.0, (own_h - 6.0).max(12.0)),
            );
            let r = ui
                .interact(m, ui.id().with(("hit-right", idx)), Sense::click())
                .on_hover_text(match hit_col.and_then(|c| snap.columns.get(c)) {
                    Some(name) => format!(
                        "What you searched for is further right in this row, in {name}. Click to bring it into view"
                    ),
                    None => "What you searched for is further right in this row. Click to bring it into view"
                        .to_string(),
                });
            body_clip.rect_filled(
                m,
                CornerRadius::same(4),
                if r.hovered() { pal.accent_ink } else { pal.selection },
            );
            body_clip.rect_stroke(
                m,
                CornerRadius::same(4),
                Stroke::new(1.0, pal.accent_ink),
                egui::StrokeKind::Inside,
            );
            body_clip.text(
                m.center(),
                Align2::CENTER_CENTER,
                ph::ARROW_RIGHT,
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                if r.hovered() { pal.surface } else { pal.accent_ink },
            );
            if r.clicked() {
                if let Some(hx) = hit_x {
                    // the hit comes a little right of where the scrolling columns start
                    st.hscroll = (st.hscroll + hx - scroll_left - 80.0).max(0.0);
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    if total == 0 {
        body_clip.text(
            body.center() - egui::vec2(0.0, 40.0),
            Align2::CENTER_CENTER,
            "No rows match these filters",
            crate::fonts::strong(14.0),
            pal.text,
        );
        body_clip.text(
            body.center() - egui::vec2(0.0, 16.0),
            Align2::CENTER_CENTER,
            "Loosen a column filter above, or clear them all with the funnel at the left.",
            theme::ui_font(),
            pal.text2,
        );
    }

    if let Some(r) = toggle_row {
        if !st.expanded.remove(&r) {
            if st.expanded.len() >= 64 {
                st.expanded.clear();
            }
            st.expanded.insert(r);
        }
    }

    // the group being read keeps its header(s) pinned under the column names
    let mut sticky_n = 0usize;
    if let Some(g) = st.grouping.as_ref() {
        let path = match g.tree.item_at(st.top) {
            Some(crate::groups::Item::Row(p, _)) => Some((p.clone(), p.len())),
            Some(crate::groups::Item::Header(p)) => Some((p.clone(), p.len() - 1)),
            None => None,
        };
        if let Some((path, depth)) = path {
            for d in 0..depth {
                let anc = &path[..=d];
                if g.tree.header_line(anc).is_some_and(|l| l < st.top) {
                    let rect = Rect::from_min_size(
                        egui::pos2(full.left(), body.top() + d as f32 * base_h),
                        egui::vec2(body.width(), base_h),
                    );
                    draw_group_header(&body_clip, pal, g, anc, rect, band_w, false, snap.matched);
                    sticky_n += 1;
                    body_clip.line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0, pal.control_border),
                    );
                    if resp.clicked()
                        && resp
                            .interact_pointer_pos()
                            .is_some_and(|p| rect.contains(p))
                    {
                        toggle_group = Some(anc.to_vec());
                    }
                }
            }
        }
    }
    // ── row clicks ──
    let cell_at = |st: &GridState, pos: egui::Pos2| -> Option<(u64, usize)> {
        if !body.contains(pos) || pos.x < full.left() + gutter {
            return None;
        }
        let idx = st.top + row_at(&st.row_spans, pos.y)? as u64;
        for (n, &c) in st.view.iter().enumerate() {
            let w = st.widths.get(c).copied().unwrap_or(120.0);
            if pos.x >= col_x[n].max(lo_of(n)) && pos.x < col_x[n] + w {
                return (idx < total).then_some((idx, c));
            }
        }
        None
    };
    let value_at = |idx: u64, c: usize| -> Option<String> {
        let p = have?;
        let k = idx.checked_sub(p.start)? as usize;
        p.cells.get(k)?.get(c)?.clone()
    };
    if let Some(pos) = resp.interact_pointer_pos() {
        if body.contains(pos)
            && (resp.clicked() || resp.double_clicked() || resp.secondary_clicked())
        {
            let idx = row_at(&st.row_spans, pos.y).map_or(u64::MAX, |i| st.top + i as u64);
            if idx < total {
                st.sel = Some(idx);
                if let Some((_, c)) = cell_at(st, pos) {
                    // Shift+click stretches a block of cells to here
                    if ui.input(|i| i.modifiers.shift) {
                        st.anchor_col = st.anchor_col.or(st.focus_col);
                    } else if !resp.secondary_clicked() {
                        st.anchor_col = Some(c);
                    }
                    st.focus_col = Some(c);
                }
                let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
                if shift {
                    st.multi.shift_click(idx);
                } else if ctrl {
                    st.multi.ctrl_click(idx);
                } else if !(resp.secondary_clicked() && st.multi.contains(idx)) {
                    // a right-click on a selected row keeps the selection for its menu
                    st.multi.click(idx);
                }
                event = if resp.double_clicked() {
                    GridEvent::Open(idx)
                } else {
                    GridEvent::Select(idx)
                };
            }
        }
        if resp.secondary_clicked() {
            st.ctx_cell = cell_at(st, pos).map(|(i, c)| (i, c, value_at(i, c)));
            st.menu_at = None;
        }
    }
    let row_tsv = |st: &GridState, idx: u64| -> Option<String> {
        let p = have?;
        let row = p.cells.get(idx.checked_sub(p.start)? as usize)?;
        Some(
            st.view
                .iter()
                .map(|&c| {
                    row.get(c)
                        .cloned()
                        .flatten()
                        .unwrap_or_default()
                        .replace(['\t', '\n', '\r'], " ")
                })
                .collect::<Vec<_>>()
                .join("\t"),
        )
    };
    // Ctrl+C copies the focused cell, or the selected rows when there are
    // several (tab-separated, in the order shown);
    // Ctrl+Shift+C puts the column names first
    let with_names =
        ui.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::C));
    if keys_active
        && (with_names || ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))))
    {
        if st.multi.count() > 1 || with_names || st.block().is_some() {
            // several rows, or a block of cells: the engine writes them out
            event = GridEvent::CopyRows { names: with_names };
        } else if let Some(v) = st
            .sel
            .zip(st.focus_col)
            .and_then(|(i, c)| value_at(i, c))
        {
            // one row and a focused cell: the cell, as a spreadsheet does
            crate::clip::copy(ui.ctx(), v, "the cell");
        } else if let Some(t) = st.sel.and_then(|i| row_tsv(st, i)) {
            crate::clip::copy(ui.ctx(), t, "the row");
        }
    }
    // the quiet-gap bands, in the room left below their rows
    if let Some(tc) = snap.time_col {
        let n_tc = st.view.iter().position(|&c| c == tc);
        for (i, &(y, row_h)) in spans.iter().enumerate() {
            let Some(words) = gap_after.get(&(st.top + i as u64)) else {
                continue;
            };
            let mid = y + row_h - GAP_H * 0.5;
            body_clip.line_segment(
                [egui::pos2(full.left(), mid), egui::pos2(body.right(), mid)],
                Stroke::new(1.5, pal.info.gamma_multiply(0.7)),
            );
            let g = body_clip.layout_no_wrap(words.clone(), theme::small_font(), pal.text2);
            let size = g.size() + egui::vec2(12.0, 2.0);
            let left = n_tc
                .map(|n| col_x[n] + PAD)
                .unwrap_or(body.left() + 8.0)
                .max(body.left() + 4.0);
            let r = Rect::from_min_size(egui::pos2(left, mid - size.y * 0.5), size);
            body_clip.rect_filled(r, CornerRadius::same(4), pal.surface);
            body_clip.rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(1.0, pal.info.gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
            body_clip.galley(egui::pos2(r.left() + 6.0, r.top() + 1.0), g, pal.text2);
        }
    }
    if let Some(t) = gutter_tip.take() {
        resp.clone().on_hover_text_at_pointer(t);
    }
    let sel_line = st.sel;
    if let Some(t) = st.ftree.as_mut() {
        if let Some(r) = toggle_ftree.take() {
            t.toggle(r);
        }
        event = match event {
            GridEvent::Select(l) => t
                .line(l)
                .map_or(GridEvent::None, |(r, _)| GridEvent::SelectRow(r)),
            GridEvent::Open(l) => t
                .line(l)
                .map_or(GridEvent::None, |(r, _)| GridEvent::OpenRow(r)),
            e => e,
        };
        // Alt+→ unfolds the selected row, Alt+← folds it or goes to the row it
        // links to (plain arrows move the focused cell across the columns)
        if keys_active {
            let (right, left) = ui.input(|i| {
                (
                    i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
                    i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                )
            });
            if let Some((l, (row, _))) = sel_line.and_then(|l| t.line(l).map(|x| (l, x))) {
                let open = t.is_open(row) && t.children(row) > 0;
                if right && !open {
                    t.toggle(row);
                } else if left && open {
                    t.toggle(row);
                } else if left {
                    if let Some(p) = t.parent_line(l) {
                        st.sel = Some(p);
                    }
                }
            }
        }
    }
    if let Some(g) = st.grouping.as_mut() {
        use crate::groups::Item;
        if let Some(p) = toggle_group.take() {
            g.toggle(&p, bridge);
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        event = match event {
            GridEvent::Select(l) => g
                .row_id(snap, l)
                .map_or(GridEvent::None, GridEvent::SelectRow),
            GridEvent::Open(l) => match g.tree.item_at(l) {
                Some(Item::Header(p)) => {
                    if enter {
                        g.toggle(&p, bridge);
                    }
                    GridEvent::None
                }
                _ => g
                    .row_id(snap, l)
                    .map_or(GridEvent::None, GridEvent::OpenRow),
            },
            e => e,
        };
        // → opens the selected group, ← closes it (or goes up to its parent)
        if keys_active {
            let (right, left) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowRight),
                    i.key_pressed(egui::Key::ArrowLeft),
                )
            });
            match sel_line.and_then(|l| g.tree.item_at(l)) {
                Some(Item::Header(p)) if right && !g.tree.is_open(&p) => g.toggle(&p, bridge),
                Some(Item::Header(p)) if left && g.tree.is_open(&p) => g.toggle(&p, bridge),
                Some(Item::Row(..)) if !ui.input(|i| i.modifiers.alt) => {}
                Some(Item::Header(p)) | Some(Item::Row(p, _)) if left => {
                    let up = if matches!(g.tree.item_at(sel_line.unwrap_or(0)), Some(Item::Row(..)))
                    {
                        p.clone()
                    } else {
                        p[..p.len().saturating_sub(1)].to_vec()
                    };
                    if let Some(l) = g.tree.header_line(&up) {
                        st.sel = Some(l);
                    }
                }
                _ => {}
            }
        }
    }
    // the focused cell on screen: (row place, column, its left edge, its row's bottom)
    let focus_at = st.sel.zip(st.focus_col).and_then(|(i, c)| {
        let n = st.view.iter().position(|&v| v == c)?;
        let (y, h) = *st.row_spans.get(i.checked_sub(st.top)? as usize)?;
        Some((i, c, col_x[n].max(lo_of(n)), y + h))
    });
    let plain_key = |k: egui::Key| {
        keys_active && ui.input(|i| i.modifiers.is_none() && i.key_pressed(k))
    };
    // f: the focused column's values and filter, under its name
    if plain_key(egui::Key::F) {
        if let Some((_, c, x, _)) = focus_at {
            event = GridEvent::OpenValues {
                col: c,
                at: egui::pos2(x, header.bottom() + FILTER_H),
            };
        }
    }
    // Shift+F10: the menu of the focused cell, under it
    let key_menu = ((keys_active
        && ui.input(|i| i.modifiers.shift && i.key_pressed(egui::Key::F10)))
        || st.dev_key_menu.take_if(|m| *m == 1).is_some())
        && focus_at
            .map(|(i, c, x, y)| {
                st.ctx_cell = Some((i, c, value_at(i, c)));
                st.menu_at = Some(egui::pos2(x, y));
            })
            .is_some();
    let ctx_cell = st.ctx_cell.clone();
    let nsel = st.multi.count();
    let pop = egui::Popup::menu(&resp).open_memory(if resp.secondary_clicked() || key_menu {
        Some(egui::SetOpenCommand::Bool(true))
    } else if resp.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    });
    let pop = match st.menu_at {
        Some(at) => pop.at_position(at),
        None => pop.at_pointer_fixed(),
    };
    pop.show(|ui| {
        let Some((idx, c, val)) = ctx_cell.clone() else {
            ui.label("Right-click a cell");
            return;
        };
        if nsel > 1 {
            let n = crate::moment::group(nsel);
            if ui
                .button(format!("{}  Copy the {n} selected rows", ph::COPY))
                .on_hover_text("Tab-separated, ready to paste into a spreadsheet  (Ctrl+C)")
                .clicked()
            {
                event = GridEvent::CopyRows { names: false };
                ui.close();
            }
            if ui
                .button(format!("{}  Copy them with the column names", ph::COPY))
                .on_hover_text("Ctrl+Shift+C")
                .clicked()
            {
                event = GridEvent::CopyRows { names: true };
                ui.close();
            }
            ui.separator();
        }
        let name = snap.columns.get(c).cloned().unwrap_or_default();
        let short = |v: &str| {
            if v.chars().count() > 36 || v.contains('\n') {
                "this value".to_string()
            } else {
                v.to_string()
            }
        };
        if let Some(v) = val.as_ref().filter(|v| !v.is_empty()) {
            if ui
                .button(format!(
                    "{}  Show only  {} = {}",
                    ph::FUNNEL_SIMPLE,
                    name,
                    short(v)
                ))
                .clicked()
            {
                event = GridEvent::Cell(CellAction::Only {
                    col: name.clone(),
                    value: v.clone(),
                });
                ui.close();
            }
            if ui
                .button(format!(
                    "{}  Exclude  {} = {}",
                    ph::PROHIBIT,
                    name,
                    short(v)
                ))
                .clicked()
            {
                event = GridEvent::Cell(CellAction::Exclude {
                    col: name.clone(),
                    value: v.clone(),
                });
                ui.close();
            }
            if ui
                .button(format!(
                    "{}  Colour rows where  {} = {}",
                    ph::PALETTE,
                    name,
                    short(v)
                ))
                .on_hover_text("A colour rule: these rows get a tint and a mark in the margin, in this file and others. Change it in Colour rules.")
                .clicked()
            {
                event = GridEvent::Cell(CellAction::ColourLike {
                    col: name.clone(),
                    value: v.clone(),
                });
                ui.close();
            }
            // follow this value to the rows that hold it in a linked column:
            // the links are worked out from the file's own values
            let this_row = have.and_then(|p| {
                idx.checked_sub(p.start)
                    .and_then(|k| p.rows.get(k as usize))
                    .copied()
            });
            if let Some(row) = this_row {
                ui.menu_button(
                    format!("{}  Follow  {}", ph::TREE_STRUCTURE, short(v)),
                    |ui| {
                        ui.set_min_width(340.0);
                        let choices = crate::follow::cell_choices(c, &snap.columns, &snap.links);
                        if choices.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "No other column was found to hold the values of {name}. Choose the column yourself:"
                                ))
                                .font(theme::small_font())
                                .color(pal.text2),
                            );
                        }
                        for ch in &choices {
                            if ui.button(ch.label.as_str()).clicked() {
                                event = GridEvent::Cell(CellAction::Follow {
                                    row,
                                    col: name.clone(),
                                    value: v.clone(),
                                    to: ch.to.clone(),
                                    repeat: ch.repeat,
                                });
                                ui.close();
                            }
                        }
                        if !choices.is_empty() {
                            ui.separator();
                        }
                        if ui
                            .button("More choices")
                            .on_hover_text("Choose the columns yourself, follow within a time, or follow from many rows")
                            .clicked()
                        {
                            event = GridEvent::Cell(CellAction::Follow {
                                row,
                                col: name.clone(),
                                value: v.clone(),
                                to: String::new(),
                                repeat: false,
                            });
                            ui.close();
                        }
                    },
                );
            }
            ui.separator();
            if ui.button(format!("{}  Copy value  (Ctrl+C)", ph::COPY)).clicked() {
                crate::clip::copy(ui.ctx(), v.clone(), &format!("the value of {name}"));
                ui.close();
            }
        }
        let row_id = have.and_then(|p| {
            idx.checked_sub(p.start)
                .and_then(|k| p.rows.get(k as usize))
                .copied()
        });
        if let Some(rid) = row_id {
            ui.menu_button(format!("{}  Tag", ph::TAG), |ui| {
                let mask = tags.get(&rid).copied().unwrap_or(0);
                for i in 0..9 {
                    let on = mask >> i & 1 == 1;
                    let label = format!(
                        "{}  {}   {}",
                        if on { ph::CHECK } else { "    " },
                        crate::tags::name(i),
                        i + 1
                    );
                    if ui.button(label).clicked() {
                        event = GridEvent::Cell(CellAction::Tag { row: rid, tag: i });
                        ui.close();
                    }
                }
            });
        }
        if ui
            .button(format!("{}  Copy row", ph::ROWS))
            .clicked()
        {
            if let Some(t) = row_tsv(st, idx) {
                crate::clip::copy(ui.ctx(), t, "the row");
            }
            ui.close();
        }
        if ui
            .button(format!("{}  Open record  (Enter)", ph::ARROW_SQUARE_OUT))
            .clicked()
        {
            event = GridEvent::Open(idx);
            ui.close();
        }
    });

    let (track_col, thumb_col, hot_col) = theme::scroll_colors(pal);
    // ── horizontal scrollbar ──
    let view_w = body.width() - gutter;
    if hbar && st.content_w > view_w {
        let htrack = Rect::from_min_max(
            egui::pos2(full.left() + gutter, body.bottom()),
            egui::pos2(body.right(), full.bottom()),
        );
        painter.rect_filled(htrack, CornerRadius::ZERO, track_col);
        let span = (st.content_w - view_w).max(1.0);
        let tw = (htrack.width() * view_w / st.content_w).max(28.0);
        let pos = (st.hscroll / span).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(
            egui::pos2(
                htrack.left() + (htrack.width() - tw) * pos,
                htrack.top() + 2.0,
            ),
            egui::vec2(tw, BAR - 4.0),
        );
        let hr = ui
            .interact(htrack, ui.id().with("hbar"), Sense::click_and_drag())
            .on_hover_text("Scroll sideways  (Shift + wheel)");
        if hr.clicked() {
            if let Some(p) = hr.interact_pointer_pos().filter(|p| !thumb.contains(*p)) {
                let f = ((p.x - htrack.left() - tw * 0.5) / (htrack.width() - tw)).clamp(0.0, 1.0);
                st.hscroll = f * span;
            }
        }
        if hr.drag_started() {
            if let Some(p) = hr.interact_pointer_pos() {
                if !thumb.contains(p) {
                    let f =
                        ((p.x - htrack.left() - tw * 0.5) / (htrack.width() - tw)).clamp(0.0, 1.0);
                    st.hscroll = f * span;
                }
                st.hdrag_from = Some((p.x, st.hscroll));
            }
        }
        if hr.dragged() {
            if let (Some((x0, s0)), Some(p)) = (st.hdrag_from, hr.interact_pointer_pos()) {
                let per_px = span / (htrack.width() - tw).max(1.0);
                st.hscroll = (s0 + (p.x - x0) * per_px).clamp(0.0, span);
            }
        }
        if hr.drag_stopped() {
            st.hdrag_from = None;
        }
        let col = if hr.hovered() || hr.dragged() {
            hot_col
        } else {
            thumb_col
        };
        painter.rect_filled(thumb, CornerRadius::same(3), col);
    }

    // the pinned edge: a firm line, and a soft shadow while columns slide under it
    if npin > 0 {
        let segs: Vec<(f32, f32)> = match st.grouping.as_ref() {
            None => vec![(header.top(), body.bottom())],
            Some(g) => {
                let below = body.top() + sticky_n as f32 * base_h;
                let mut v = vec![(header.top(), body.top())];
                for (i, &(y, h)) in st.row_spans.iter().enumerate() {
                    let row = matches!(
                        g.tree.item_at(st.top + i as u64),
                        Some(crate::groups::Item::Row(..))
                    );
                    if row && y >= below - 0.5 {
                        v.push((y, (y + h).min(body.bottom())));
                    }
                }
                v
            }
        };
        for &(top, bottom) in &segs {
            painter.line_segment(
                [
                    egui::pos2(scroll_left, top),
                    egui::pos2(scroll_left, bottom),
                ],
                Stroke::new(1.0, pal.control_border),
            );
        }
        if st.hscroll > 0.5 {
            let shade: u32 = if pal.dark { 70 } else { 28 };
            for k in 0..6u32 {
                let a = (shade - k * shade / 6) as u8;
                for &(top, bottom) in &segs {
                    painter.line_segment(
                        [
                            egui::pos2(scroll_left + 1.0 + k as f32, top),
                            egui::pos2(scroll_left + 1.0 + k as f32, bottom),
                        ],
                        Stroke::new(1.0, egui::Color32::from_black_alpha(a)),
                    );
                }
            }
        }
    }
    // ── vertical scrollbar (row-index based) ──
    let track = Rect::from_min_max(
        egui::pos2(full.right() - BAR, body.top()),
        egui::pos2(full.right(), body.bottom()),
    );
    if total > visible || (max_top > 0 && st.tall) {
        painter.rect_filled(track, CornerRadius::ZERO, track_col);
        let frac = (visible as f32 / total as f32).min(0.9);
        let th = (track.height() * frac).max(28.0);
        let pos = st.top as f32 / max_top.max(1) as f32;
        let thumb = Rect::from_min_size(
            egui::pos2(
                track.left() + 2.0,
                track.top() + (track.height() - th) * pos,
            ),
            egui::vec2(BAR - 4.0, th),
        );
        let sr = ui.interact(track, ui.id().with("vbar"), Sense::click_and_drag());
        if sr.clicked() {
            if let Some(p) = sr.interact_pointer_pos().filter(|p| !thumb.contains(*p)) {
                let f = ((p.y - track.top() - th * 0.5) / (track.height() - th)).clamp(0.0, 1.0);
                st.top = (f * max_top as f32) as u64;
            }
        }
        if sr.drag_started() {
            if let Some(p) = sr.interact_pointer_pos() {
                if thumb.contains(p) {
                    st.drag_from = Some((p.y, st.top));
                } else {
                    let f =
                        ((p.y - track.top() - th * 0.5) / (track.height() - th)).clamp(0.0, 1.0);
                    st.top = (f * max_top as f32) as u64;
                    st.drag_from = Some((p.y, st.top));
                }
            }
        }
        if sr.dragged() {
            if let (Some((y0, t0)), Some(p)) = (st.drag_from, sr.interact_pointer_pos()) {
                let per_px = max_top as f32 / (track.height() - th).max(1.0);
                st.top = (t0 as f32 + (p.y - y0) * per_px).clamp(0.0, max_top as f32) as u64;
            }
        }
        if sr.drag_stopped() {
            st.drag_from = None;
        }
        let col = if sr.hovered() || sr.dragged() {
            hot_col
        } else {
            thumb_col
        };
        painter.rect_filled(thumb, CornerRadius::same(3), col);
        if sr.dragged() {
            // where you are while dragging: a bubble beside the thumb
            let label = format!(
                "Row {} of {}",
                crate::moment::group(st.top + 1),
                crate::moment::group(total)
            );
            let lp = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                ui.id().with("vbar-bubble"),
            ));
            let g = lp.layout_no_wrap(label, crate::fonts::strong(12.0), pal.text);
            let size = g.size() + egui::vec2(16.0, 10.0);
            let at = egui::pos2(
                thumb.left() - 8.0 - size.x,
                (thumb.center().y - size.y * 0.5).clamp(body.top(), body.bottom() - size.y),
            );
            let r = Rect::from_min_size(at, size);
            lp.rect_filled(r, CornerRadius::same(4), pal.surface);
            lp.rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(1.0, pal.control_border),
                egui::StrokeKind::Inside,
            );
            lp.galley(at + egui::vec2(8.0, 5.0), g, pal.text);
        }
    }
    event
}

/// Room given below a row followed by a quiet gap, for its band.
const GAP_H: f32 = 20.0;
/// The width of the column of reasons when it is folded away.
const FOUND_FOLDED_W: f32 = 58.0;

/// A band between two rows on screen whose times are far apart: the words for
/// it when the gap is at least 20 times the usual gap between the rows shown
/// (a usual gap under a second counts as a second), else nothing.
#[cfg(test)]
pub fn quiet_gap(prev_us: i64, next_us: i64, typical_us: i64) -> Option<String> {
    quiet_gap_in(prev_us, next_us, typical_us, false)
}

/// `quiet_gap`, said truly for the rows shown: with a search or filter on,
/// the log was not silent, only no row matched.
pub fn quiet_gap_in(prev_us: i64, next_us: i64, typical_us: i64, filtered: bool) -> Option<String> {
    let gap = (next_us - prev_us).abs();
    (gap >= 20 * typical_us.max(1_000_000)).then(|| {
        let d = crate::rhythmwin::dur(gap);
        if filtered {
            format!("{d} with no matching row")
        } else {
            format!("{d} quiet")
        }
    })
}
/// Whether the rows shown run in time order, and so whether two rows next to
/// each other tell how much time passed between them: sorted by the time
/// column, or in file order when the file itself was written in time order
/// (never assumed before that is known). Gives how far a row may be out of
/// place (µs) and whether the newest come first.
pub fn time_ordered(
    sort: &Option<(String, bool)>,
    time_name: Option<&str>,
    order: Option<logseek_core::query::order::TimeOrder>,
) -> Option<(i64, bool)> {
    use logseek_core::query::order::Direction;
    match sort {
        Some((name, newest)) => (Some(name.as_str()) == time_name).then_some((0, *newest)),
        None => match order?.direction {
            Direction::OldestFirst => Some((order?.slack_us, false)),
            Direction::NewestFirst => Some((order?.slack_us, true)),
            Direction::None => None,
        },
    }
}

/// What the time column's name adds to say which clock its times are shown in:
/// nothing when the name already says UTC and they are shown in UTC.
fn zone_suffix(name: &str, utc: bool) -> Option<&'static str> {
    if !utc {
        return Some("  (as in file)");
    }
    (!name.to_ascii_lowercase().contains("utc")).then_some("  (UTC)")
}
#[cfg(test)]
mod grid_tests {
    use super::*;

    #[test]
    fn a_group_header_leads_with_its_count_and_names_its_column_only_when_levels_need_it() {
        let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
        let p = header_parts(None, Some(ua), 2, 5_629, None);
        // the count and share come first, so a long value never pushes them away
        assert_eq!(p[0], "2 rows");
        assert_eq!(p[1], "under 0.1%");
        assert_eq!(p[2], ua, "the value in full");
        // one level: the chip above names the column; several: each header says its level
        let q = header_parts(Some("host"), Some("web1"), 1, 10, None);
        assert_eq!(
            q,
            vec![
                "1 row".to_string(),
                "10%".into(),
                "host".into(),
                "web1".into()
            ]
        );
        // no value said so, and the time span last
        let r = header_parts(None, None, 3, 3, Some((0, 1_000_000)));
        assert_eq!(r[2], "(no value)");
        assert!(r[3].ends_with("UTC"), "{:?}", r);
    }

    #[test]
    fn quiet_gap_words() {
        let s = 1_000_000i64;
        assert_eq!(quiet_gap(0, 90 * s, s).as_deref(), Some("1 m 30 s quiet"));
        assert_eq!(
            quiet_gap(0, 27 * 60 * s, 10 * s).as_deref(),
            Some("27 m quiet")
        );
        assert_eq!(
            quiet_gap(0, (3 * 3600 + 5 * 60) * s, 10 * s).as_deref(),
            Some("3 h 5 m quiet")
        );
        assert_eq!(
            quiet_gap(0, 2 * 86_400 * s, 60 * s).as_deref(),
            Some("2 d quiet")
        );
        // rows shown newest first: the same gap
        assert_eq!(
            quiet_gap(27 * 60 * s, 0, 10 * s).as_deref(),
            Some("27 m quiet")
        );
        // with a search or filter on, the log was not silent: no row matched
        assert_eq!(
            quiet_gap_in(0, 27 * 60 * s, 10 * s, true).as_deref(),
            Some("27 m with no matching row")
        );
    }

    #[test]
    fn hidden_columns_take_no_room_so_scrolling_stops_at_the_last_one_shown() {
        // five columns in the file, two shown
        let widths = [100.0, 200.0, 300.0, 400.0, 500.0];
        assert_eq!(content_width(&[0, 4], &widths), 600.0);
        // all shown: all counted
        assert_eq!(content_width(&[0, 1, 2, 3, 4], &widths), 1500.0);
        assert_eq!(content_width(&[], &widths), 0.0);
    }

    #[test]
    fn rows_are_in_time_order_only_when_sorted_by_time_or_written_that_way() {
        use logseek_core::query::order::{Direction, TimeOrder};
        let ord = |direction, slack_us| {
            Some(TimeOrder {
                direction,
                slack_us,
            })
        };
        let by = |n: &str, newest: bool| Some((n.to_string(), newest));
        // sorted by the time column: exactly in order, whatever the file
        assert_eq!(
            time_ordered(&by("ts", false), Some("ts"), ord(Direction::None, 0)),
            Some((0, false))
        );
        assert_eq!(
            time_ordered(&by("ts", true), Some("ts"), None),
            Some((0, true))
        );
        // sorted by another column: not
        assert_eq!(
            time_ordered(
                &by("host", false),
                Some("ts"),
                ord(Direction::OldestFirst, 0)
            ),
            None
        );
        // file order: only when the file itself runs in time order, with its slack
        assert_eq!(
            time_ordered(&None, Some("ts"), ord(Direction::OldestFirst, 9)),
            Some((9, false))
        );
        assert_eq!(
            time_ordered(&None, Some("ts"), ord(Direction::NewestFirst, 0)),
            Some((0, true))
        );
        assert_eq!(
            time_ordered(&None, Some("ts"), ord(Direction::None, 0)),
            None
        );
        // not yet known: not assumed
        assert_eq!(time_ordered(&None, Some("ts"), None), None);
    }

    #[test]
    fn no_band_for_steady_rows() {
        let s = 1_000_000i64;
        // not far beyond the usual gap
        assert_eq!(quiet_gap(0, 15 * s, 10 * s), None);
        // rows a millisecond apart: a pause under 20 s is still steady
        assert_eq!(quiet_gap(0, s / 2, 1_000), None);
        assert_eq!(quiet_gap(0, 19 * s, 0), None);
        assert_eq!(quiet_gap(0, 20 * s, 0).as_deref(), Some("20 s quiet"));
    }
    #[test]
    fn the_time_column_says_its_zone_once() {
        assert_eq!(zone_suffix("TimeCreated", true), Some("  (UTC)"));
        assert_eq!(zone_suffix("Timestamp (UTC)", true), None);
        assert_eq!(zone_suffix("created_utc", true), None);
        // shown as the file has them: always said, the name cannot know
        assert_eq!(
            zone_suffix("Timestamp (UTC)", false),
            Some("  (as in file)")
        );
    }

    #[test]
    fn row_at_follows_rows_of_different_heights() {
        let spans = [(100.0, 24.0), (124.0, 72.0), (196.0, 24.0)];
        assert_eq!(row_at(&spans, 100.0), Some(0));
        assert_eq!(row_at(&spans, 150.0), Some(1));
        assert_eq!(row_at(&spans, 195.9), Some(1));
        assert_eq!(row_at(&spans, 196.0), Some(2));
        assert_eq!(row_at(&spans, 230.0), None);
    }
    #[test]
    fn scrolling_away_from_the_selected_row_sticks() {
        // a row is selected at 100; the wheel moved the view to 500: stay there
        assert_eq!(follow_selection(500, Some(100), 30, false), 500);
        // the keyboard moved the selection to 120: bring it into view
        assert_eq!(follow_selection(500, Some(120), 30, true), 120);
        assert_eq!(follow_selection(0, Some(80), 30, true), 51);
        assert_eq!(follow_selection(40, Some(50), 30, true), 40);
        assert_eq!(follow_selection(40, None, 30, true), 40);
    }

    #[test]
    fn page_window_prefetches_before_the_edge() {
        // view 250..290, loaded 0..320: 30 rows to spare going down
        assert_eq!(
            page_window(250, 40, 10_000, Some((0, 320)), 1, 1),
            Some((190, 340))
        );
        // view 230..270: 50 rows to spare, enough
        assert_eq!(page_window(230, 40, 10_000, Some((0, 320)), 1, 1), None);
    }
    #[test]
    fn page_window_quiet_when_covered() {
        assert_eq!(page_window(100, 40, 10_000, Some((0, 320)), 1, 1), None);
        assert_eq!(page_window(100, 40, 10_000, Some((0, 320)), 0, 1), None);
    }
    #[test]
    fn page_window_up_loads_above() {
        // view 1030..1070, loaded 1000..1340: 30 rows to spare going up
        assert_eq!(
            page_window(1030, 40, 10_000, Some((1000, 340)), -1, 1),
            Some((790, 340))
        );
        // a jump up out of the loaded block
        assert_eq!(
            page_window(100, 40, 10_000, Some((1000, 340)), -1, 1),
            Some((0, 340))
        );
    }
    #[test]
    fn page_window_reaches_further_when_scrolling_fast() {
        // 41 rows a frame: keep 410 rows ahead, load 30 frames' worth
        assert_eq!(
            page_window(250, 40, 100_000, Some((0, 600)), 1, 41),
            Some((190, 1270))
        );
        assert_eq!(page_window(250, 40, 100_000, Some((0, 1000)), 1, 41), None);
    }
    #[test]
    fn page_window_clamps() {
        assert_eq!(page_window(0, 40, 50, None, 0, 1), Some((0, 50)));
        assert_eq!(
            page_window(9_980, 20, 10_000, None, 1, 1),
            Some((9_920, 80))
        );
        // everything loaded: nothing to ask for, even at the edges
        assert_eq!(page_window(30, 20, 50, Some((0, 50)), 1, 1), None);
    }

    #[test]
    fn dragging_a_header_moves_the_column_before_another() {
        let mut o = vec![0, 1, 2, 3, 4];
        move_before(&mut o, 4, Some(1));
        assert_eq!(o, [0, 4, 1, 2, 3]);
        move_before(&mut o, 0, None);
        assert_eq!(o, [4, 1, 2, 3, 0]);
        move_before(&mut o, 2, Some(2)); // dropped on itself
        assert_eq!(o, [4, 1, 2, 3, 0]);
        move_before(&mut o, 1, Some(2)); // already just before
        assert_eq!(o, [4, 1, 2, 3, 0]);
    }

    #[test]
    fn drop_target_is_the_nearest_gap() {
        // columns 7, 8, 9 at x 0..100, 100..250, 250..300
        let spans = [(7, 0.0, 100.0), (8, 100.0, 250.0), (9, 250.0, 300.0)];
        assert_eq!(drop_target(&spans, 30.0), (Some(7), 0.0));
        assert_eq!(drop_target(&spans, 80.0), (Some(8), 100.0));
        assert_eq!(drop_target(&spans, 260.0), (Some(9), 250.0));
        assert_eq!(drop_target(&spans, 290.0), (None, 300.0));
        assert_eq!(drop_target(&spans, 900.0), (None, 300.0));
        assert_eq!(drop_target(&spans, -50.0), (Some(7), 0.0));
    }

    #[test]
    fn cells_are_never_trimmed() {
        let long = "x".repeat(5000);
        assert_eq!(cell_text(&long).len(), 5000);
        let multi = "first line\nsecond line\r\nthird";
        let t = cell_text(multi);
        assert!(
            t.contains("first line") && t.contains("second line") && t.contains("third"),
            "{t}"
        );
        assert!(!t.contains('\n'));
    }

    #[test]
    fn text_laid_out_over_indented_lines_reads_as_one_run() {
        let pretty = "[\n    {\n        \"columnNumber\": 2553,\n        \"functionName\": \"sendBeacon\"\n    }\n]";
        assert_eq!(
            cell_text(pretty),
            "[ { \"columnNumber\": 2553, \"functionName\": \"sendBeacon\" } ]"
        );
        // a value on one line keeps every space it has
        assert_eq!(cell_text("a    b"), "a    b");
    }
}

/// The filter syntax table (filter-row hover and the chips bar **?**).
pub fn filter_help(ui: &mut egui::Ui, pal: &Palette) {
    ui.label(egui::RichText::new("Filter a column").font(crate::fonts::strong(13.0)));
    egui::Grid::new("filter-help")
        .num_columns(2)
        .spacing([16.0, 3.0])
        .show(ui, |ui| {
            for (syntax, meaning) in crate::filters::HELP {
                ui.label(
                    egui::RichText::new(*syntax)
                        .font(theme::data_font())
                        .color(pal.text),
                );
                ui.label(egui::RichText::new(*meaning).color(pal.text2));
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn pinned_columns_come_first_keeping_their_order() {
        let mut v = vec![5, 1, 7, 2, 9];
        let n = pin_first(&mut v, |c| c == 7 || c == 9);
        assert_eq!(v, vec![7, 9, 5, 1, 2]);
        assert_eq!(n, 2);
        let mut w = vec![3, 4];
        assert_eq!(pin_first(&mut w, |_| false), 0);
        assert_eq!(w, vec![3, 4]);
    }

    #[test]
    fn every_time_column_reads_as_a_time_not_only_the_timeline_one() {
        use logseek_core::types::ColType;
        let types = [ColType::Time, ColType::Text, ColType::Time, ColType::Int];
        // column 0 drives the timeline; column 2 is another time column
        assert!(reads_as_time(&types, Some(0), 0));
        assert!(reads_as_time(&types, Some(0), 2));
        assert!(!reads_as_time(&types, Some(0), 1));
        assert!(!reads_as_time(&types, Some(0), 3));
        // the timeline column counts even when its type is not known yet
        assert!(reads_as_time(&[], Some(5), 5));
    }

    #[test]
    fn middle_button_scrolling_is_still_near_the_start_and_faster_farther_out() {
        // inside the still zone nothing moves
        assert_eq!(auto_scroll_step(8.0, 0.016, 0.5), 0.0);
        assert_eq!(auto_scroll_step(-12.0, 0.016, 0.5), 0.0);
        // past it: down for a pointer below, up for one above, same size
        let down = auto_scroll_step(60.0, 1.0, 0.5);
        let up = auto_scroll_step(-60.0, 1.0, 0.5);
        assert!(down > 0.0 && (down + up).abs() < 1e-4);
        // farther out is faster, and more than in proportion
        let far = auto_scroll_step(300.0, 1.0, 0.5);
        assert!(far > down * 6.0, "{far} vs {down}");
        // a longer frame moves farther
        assert!(auto_scroll_step(60.0, 0.032, 0.5) > auto_scroll_step(60.0, 0.016, 0.5));
    }

    #[test]
    fn a_block_of_cells_runs_from_where_it_began_to_the_focused_cell() {
        let view = [4, 2, 7, 9];
        // stretched to the right, and to the left: the same columns, in the order shown
        assert_eq!(block_cols(&view, Some(2), Some(9)), Some(vec![2, 7, 9]));
        assert_eq!(block_cols(&view, Some(9), Some(2)), Some(vec![2, 7, 9]));
        // one cell is not a block
        assert_eq!(block_cols(&view, Some(7), Some(7)), None);
        assert_eq!(block_cols(&view, None, Some(7)), None);
        // a column hidden since: no block
        assert_eq!(block_cols(&view, Some(5), Some(7)), None);
    }

    #[test]
    fn the_focused_column_steps_through_the_shown_columns() {
        let view = [4, 2, 7];
        assert_eq!(step_focus_col(&view, None, ColStep::Right), Some(4));
        assert_eq!(step_focus_col(&view, None, ColStep::Left), Some(4));
        assert_eq!(step_focus_col(&view, Some(4), ColStep::Right), Some(2));
        assert_eq!(step_focus_col(&view, Some(7), ColStep::Right), Some(7));
        assert_eq!(step_focus_col(&view, Some(4), ColStep::Left), Some(4));
        assert_eq!(step_focus_col(&view, Some(2), ColStep::First), Some(4));
        assert_eq!(step_focus_col(&view, Some(2), ColStep::Last), Some(7));
        // the focused column was hidden since: start again from the first
        assert_eq!(step_focus_col(&view, Some(9), ColStep::Right), Some(4));
        assert_eq!(step_focus_col(&[], None, ColStep::Right), None);
    }

    #[test]
    fn scrolling_brings_the_whole_focused_column_into_view() {
        // the scrolling area is 300 wide; the column sits at 500..620 of it
        assert_eq!(hscroll_to_show(0.0, 500.0, 120.0, 300.0), 320.0);
        assert_eq!(hscroll_to_show(600.0, 500.0, 120.0, 300.0), 500.0);
        // already in view: nothing moves
        assert_eq!(hscroll_to_show(400.0, 500.0, 120.0, 300.0), 400.0);
        // wider than the area: its start shows
        assert_eq!(hscroll_to_show(0.0, 500.0, 400.0, 300.0), 500.0);
    }

    #[test]
    fn visible_columns_reports_the_range_on_screen() {
        // 4 columns 100 wide, 1 pinned; view 250 wide scrolled by 120
        // pinned: col 0 at 0..100; scrolling columns start at 100-120 = -20,
        // so column 1 (-20..80) is under the pinned one, 2 and 3 show
        let xs = [0.0, -20.0, 80.0, 180.0];
        let ws = [100.0; 4];
        assert_eq!(visible_columns(&xs, &ws, 1, 100.0, 250.0), Some((2, 3)));
        // nothing scrolls into view past the right edge
        assert_eq!(visible_columns(&xs, &ws, 4, 400.0, 250.0), None);
    }

    #[test]
    fn a_number_reads_as_a_time_in_each_of_the_ways_times_are_written_as_numbers() {
        // one moment, 2026-11-12 22:11:35.774 UTC, written eight ways
        let t = "2026-11-12 22:11:35";
        for (v, kind) in [
            ("1794521495", "s"),
            ("1794521495774", "ms"),
            ("1794521495774000", "us"),
            ("1794521495774000000", "ns"),
            ("134389950957740000", "filetime"),
            ("13438995095774000", "webkit"),
            ("816214295", "cocoa"),
        ] {
            let shown = number_cell_as_time(v, kind).unwrap_or_default();
            assert!(shown.starts_with(t), "{kind}: {v} gave {shown}");
            assert!(shown.ends_with(" UTC"), "{kind}: {shown}");
        }
        // a spreadsheet day number: 25569 is 1970-01-01, a half is noon
        assert_eq!(
            number_cell_as_time("25569.5", "days").as_deref(),
            Some("1970-01-01 12:00:00 UTC")
        );
        // text is not a number, and an absurd year is not a time
        assert_eq!(number_cell_as_time("web-01", "ms"), None);
        assert_eq!(number_cell_as_time("1794521495774000000", "s"), None);
        assert_eq!(number_cell_as_time("5", "nonsense"), None);
    }

    #[test]
    fn a_sort_is_said_in_the_words_of_what_the_column_holds() {
        use logseek_core::types::ColType;
        let types = [ColType::Time, ColType::Text, ColType::Int, ColType::Float];
        assert_eq!(sort_words(&types, Some(0), Some(0), false), "oldest first");
        assert_eq!(sort_words(&types, Some(0), Some(0), true), "newest first");
        assert_eq!(sort_words(&types, Some(0), Some(1), false), "A to Z");
        assert_eq!(sort_words(&types, Some(0), Some(1), true), "Z to A");
        assert_eq!(sort_words(&types, Some(0), Some(2), false), "smallest first");
        assert_eq!(sort_words(&types, Some(0), Some(3), true), "largest first");
        // a column you made has no type: it sorts as text unless its values are numbers
        assert_eq!(sort_words(&types, Some(0), Some(9), false), "A to Z");
    }

    #[test]
    fn a_group_of_nearly_every_row_is_not_said_to_be_all_of_them() {
        let l = header_parts(None, None, 2_997, 3_000, None);
        assert_eq!(l[1], "99.9%");
        let l = header_parts(None, None, 399_999, 400_000, None);
        assert_eq!(l[1], "over 99.9%");
        let l = header_parts(None, None, 3_000, 3_000, None);
        assert_eq!(l[1], "100%");
        let l = header_parts(None, None, 1_500, 3_000, None);
        assert_eq!(l[1], "50%");
    }

    #[test]
    fn rows_are_numbered_as_listed_unless_file_numbers_are_asked_for() {
        // the third row listed is row 19,620 of the file
        assert_eq!(number_label(2, Some(19_619), false), "3");
        assert_eq!(number_label(2, Some(19_619), true), "19620");
        // as listed, the number is known before the row itself has loaded
        assert_eq!(number_label(0, None, false), "1");
        assert_eq!(number_label(0, None, true), "");
    }

    #[test]
    fn the_row_number_is_the_rows_own_place_in_the_file() {
        // like a spreadsheet after filtering: the original number, gaps and all
        assert_eq!(row_label(Some(19_619)), "19620");
        assert_eq!(row_label(Some(0)), "1");
        // not loaded yet: nothing rather than a guess
        assert_eq!(row_label(None), "");
    }

    #[test]
    fn row_numbers_get_room_for_the_largest_number() {
        // at least two digits; 22,615 rows need five
        assert_eq!(row_number_width(9, 8.0), 2.0 * 8.0 + 12.0);
        assert_eq!(row_number_width(22_615, 8.0), 5.0 * 8.0 + 12.0);
        assert_eq!(row_number_width(0, 8.0), 2.0 * 8.0 + 12.0);
    }

    #[test]
    fn move_col_steps_one_place_and_stops_at_the_ends() {
        let mut o = vec![4, 2, 7];
        move_col(&mut o, 2, -1);
        assert_eq!(o, vec![2, 4, 7]);
        move_col(&mut o, 2, -1);
        assert_eq!(o, vec![2, 4, 7]);
        move_col(&mut o, 4, 1);
        assert_eq!(o, vec![2, 7, 4]);
        move_col(&mut o, 4, 1);
        assert_eq!(o, vec![2, 7, 4]);
        move_col(&mut o, 9, 1);
        assert_eq!(o, vec![2, 7, 4]);
    }

    #[test]
    fn utc_cell_reads_recent_epoch_numbers() {
        // 2026 epoch milliseconds and seconds (EDR exports) read as times
        assert_eq!(
            utc_cell("1794564007000").as_deref(),
            Some("2026-11-13 10:00:07.000 UTC")
        );
        assert_eq!(
            utc_cell("1794564007").as_deref(),
            Some("2026-11-13 10:00:07 UTC")
        );
    }

    #[test]
    fn utc_cell_keeps_every_fraction_digit_the_file_wrote() {
        assert_eq!(
            utc_cell("2016-08-24T12:27:37.123456789-06:00").as_deref(),
            Some("2016-08-24 18:27:37.123456789 UTC")
        );
        assert_eq!(
            utc_cell("2016-08-24T18:27:37.1234567Z").as_deref(),
            Some("2016-08-24 18:27:37.1234567 UTC")
        );
        assert_eq!(
            utc_cell("2016-08-24 18:27:37,120").as_deref(),
            Some("2016-08-24 18:27:37.120 UTC")
        );
        assert_eq!(
            utc_cell("1472063257123456789").as_deref(),
            Some("2016-08-24 18:27:37.123456789 UTC")
        );
        assert_eq!(
            utc_cell("131165368571234567").as_deref(),
            Some("2016-08-24 18:27:37.1234567 UTC")
        );
        assert_eq!(
            utc_cell("1472063257123").as_deref(),
            Some("2016-08-24 18:27:37.123 UTC")
        );
    }

    #[test]
    fn utc_cell_keeps_whole_seconds_and_millis() {
        assert_eq!(
            utc_cell("2016-08-24 12:27:26.000 MDT").as_deref(),
            Some("2016-08-24 18:27:26.000 UTC")
        );
        assert_eq!(
            utc_cell("2016-08-24T18:27:37.945Z").as_deref(),
            Some("2016-08-24 18:27:37.945 UTC")
        );
        assert_eq!(utc_cell("not a time"), None);
    }

    #[test]
    fn hide_and_move_keep_order_consistent() {
        let names: Vec<String> = ["_time", "host", "user", "cmd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let order = vec![0, 2, 1, 3];
        let hidden: BTreeSet<String> = ["user".to_string()].into();
        assert_eq!(visible_order(&order, &names, &hidden), vec![0, 1, 3]);
        let mut o = order.clone();
        move_col(&mut o, 1, -1); // host left past user
        assert_eq!(o, vec![0, 1, 2, 3]);
        move_col(&mut o, 0, -1); // already first: no change
        assert_eq!(o, vec![0, 1, 2, 3]);
        move_col(&mut o, 3, 1); // already last: no change
        assert_eq!(o, vec![0, 1, 2, 3]);
        move_col(&mut o, 0, 1);
        assert_eq!(o, vec![1, 0, 2, 3]);
    }
}

#[cfg(test)]
mod hit_tests {
    use super::{hit_start, same_as_above, says_the_same, sticky_fits};

    #[test]
    fn a_row_that_says_what_the_row_above_says_says_so_in_a_few_words() {
        assert_eq!(
            same_as_above("MAC is 7c-1e-52-a8-d8-76  ·  42 rows  ·  +24 more"),
            "the same as above  ·  +24 more"
        );
        assert_eq!(same_as_above("User is mallory  ·  1 row"), "the same as above");
        // the same reason with a different count of further ones is still the same reason
        assert!(says_the_same(
            "MAC is 7c-1e  ·  42 rows  ·  +24 more",
            "MAC is 7c-1e  ·  42 rows  ·  +23 more"
        ));
        assert!(says_the_same("User is mallory  ·  1 row", "User is mallory  ·  1 row"));
        assert!(!says_the_same("User is mallory  ·  1 row", "User is ana  ·  1 row"));
        assert!(!says_the_same("", ""));
    }

    #[test]
    fn a_short_value_follows_its_column_until_the_column_runs_out() {
        // the column starts 200 left of the kept columns' edge and is 400 wide
        assert!(sticky_fits(300.0, 400.0, 500.0, 10.0));
        // not scrolled under the edge: the value is where it always is
        assert!(!sticky_fits(520.0, 400.0, 500.0, 10.0));
        // nearly scrolled away: no room left for the value inside its column
        assert!(!sticky_fits(110.0, 400.0, 500.0, 10.0));
        // a long value does not fit in what is left
        assert!(!sticky_fits(300.0, 400.0, 500.0, 300.0));
    }

    #[test]
    fn a_cut_cell_starts_at_its_beginning_unless_the_hit_is_past_what_fits() {
        // no search, or the hit is in view: the text starts where it starts
        assert_eq!(hit_start(None, 40), 0);
        assert_eq!(hit_start(Some((12, 20)), 40), 0);
        assert_eq!(hit_start(Some((30, 40)), 40), 0);
        // the hit is cut off, wholly or in part: the text starts a few characters before it
        assert_eq!(hit_start(Some((38, 48)), 40), 32);
        assert_eq!(hit_start(Some((120, 126)), 40), 114);
        assert_eq!(hit_start(Some((3, 9)), 4), 0);
    }
}
