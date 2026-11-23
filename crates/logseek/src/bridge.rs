//! The engine lives on its own thread; the UI never waits on it. The UI sends
//! commands; the engine publishes immutable snapshots and asks for a repaint.

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use eframe::egui;
use logseek_core::budget::{Budget, Machine, Plan};
use logseek_core::ingest::pipeline::{
    start_ingest_many, IngestJob, IngestOptions, JobState, TimeChoice,
};
use logseek_core::query::agg::{run_stats, timechart};
use logseek_core::query::ast::{Agg, AggFn};
use logseek_core::query::exec::{filter_with, page_with, ChunkCache, FormulaSet, MatchSet};
pub use logseek_core::query::group::{GroupLevel, GroupRow, GroupSort};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Cmd {
    /// Open with the timeline field the user chose in "Check before opening".
    OpenWith(PathBuf, TimeChoice, Vec<String>),
    Query(String),
    /// Rows `start..start+len` of the current result.
    Page {
        start: u64,
        len: usize,
    },
    /// Every field of one row (by global row id) for the record viewer.
    Record(u64),
    /// Top values, distinct count and coverage of one column over the current result.
    /// Top (or rarest, when `rare`) values of a column over the current result.
    FieldSummary {
        col: usize,
        rare: bool,
    },
    /// Values of a column under a given query (the header value list).
    Values {
        col: usize,
        query: String,
        n: usize,
        rare: bool,
        /// By name, A to Z (over every value).
        alpha: bool,
    },
    /// How many rows a query keeps (the value list's preview before Apply).
    Count(String),
    /// Rows per hour for this query, for the calendar picker.
    Calendar(String),
    /// Group the current result and count or total a column.
    Summarise(crate::summary::SummarySpec),
    /// The cells of these file rows (any order asked; answered in file order).
    RowsById {
        key: u64,
        rows: Vec<u64>,
    },
    /// Follow from a starting set to the rows that link to it.
    Follow {
        id: u64,
        start: FollowStart,
        spec: logseek_core::query::follow::FollowSpec,
    },
    /// Groups `start..start+len` of summary `table` in `sort` order.
    SummaryPage {
        table: u64,
        sort: logseek_core::query::group::SummarySort,
        start: u64,
        len: usize,
    },
    /// Every group of summary `table` as a tab-separated table (Copy).
    SummaryCopy {
        table: u64,
        sort: logseek_core::query::group::SummarySort,
        heads: Vec<String>,
        is_time: bool,
    },
    /// Build one level of the grouped view: the current result narrowed to
    /// `path` (column = value at each level above), grouped by `by`.
    GroupLevel {
        key: u64,
        path: GroupPath,
        by: String,
        sort: GroupSort,
    },
    /// Groups `start..start+len` of a built level.
    GroupPage {
        key: u64,
        start: u64,
        len: usize,
    },
    /// Values `start..start+len` of the open value list (built by `Values`),
    /// narrowed to those containing `search` (every value is searched).
    ValuesPage {
        col: usize,
        search: String,
        start: u64,
        len: usize,
    },
    /// Rows of an open group (the result narrowed to `path`), paged.
    GroupRows {
        key: u64,
        path: GroupPath,
        start: u64,
        len: usize,
    },
    /// Write the current result to a file, page by page.
    Export {
        path: PathBuf,
        format: crate::export::Format,
        cols: Vec<usize>,
    },
    /// Add virtual (formula) columns: (name, formula). Saved per source.
    AddColumns(Vec<(String, String)>),
    RemoveColumn(String),
    /// Rename a made column; made columns that use it use the new name.
    RenameColumn(String, String),
    /// Save this source's tags (atomic write on the engine thread).
    SaveTags(Arc<crate::tags::TagMap>),
    /// Read this file (or set of files) again from the start: its index is
    /// built afresh; tags, made columns and the file itself are untouched.
    Reindex(PathBuf),
    /// Show only these rows (tagged rows) within any search; `None` = all rows.
    Restrict(Option<Arc<Vec<u64>>>),
    /// The rows at these places in the list (inclusive ranges), these columns,
    /// as tab-separated text for the clipboard.
    CopyRows {
        id: u64,
        ranges: Vec<(u64, u64)>,
        cols: Vec<usize>,
        names: bool,
    },
    /// Look for a whole list of keywords in every cell of the current
    /// result, in one pass: what each one found. `done` counts rows read.
    ListSearch {
        id: u64,
        terms: Arc<Vec<String>>,
        case: bool,
        word: bool,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// The ids of the rows the list search `id` found, to show them.
    ListRows(u64),
    /// Read the open file's column names and values for plain words.
    Lexicon,
    /// Silent gaps and steps in the rate of the rows shown.
    Signals {
        id: u64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// How regularly each value of a column comes, on the rows shown.
    Regular {
        id: u64,
        col: String,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// Every pair of values of two columns on the rows shown.
    Pairs {
        id: u64,
        a: String,
        b: String,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// Every row shown scored by how rare its values are in their columns.
    Unusual {
        id: u64,
        /// Score rows for their numbers together only.
        together: bool,
        /// The score line; `None`: the most unusual thousandth.
        line: Option<f64>,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// Lines of a column folded into patterns, on the rows shown.
    Patterns {
        id: u64,
        col: String,
        /// Share of words that must agree (0..1).
        alike: f64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// The rows of one pattern of the patterns `id`.
    PatternRows {
        id: u64,
        which: usize,
    },
    /// The values of a column written unlike the rest of it, on the rows shown.
    Strange {
        id: u64,
        col: String,
        /// Bits per pair; `None`: the strangest thousandth.
        line: Option<f64>,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// Every shape the values of a column have, on the rows shown.
    Shapes {
        id: u64,
        col: String,
        how: logseek_core::query::shape::Shape,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// What sets the rows shown apart: the values, and pairs of values, far
    /// more common in them than in the rows they are compared with.
    Contrast {
        id: u64,
        against: Against,
        min_share: f64,
        min_ratio: f64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    },
    /// Search every cell for a keyword: the rows, and the cells per column.
    Search {
        id: u64,
        text: String,
        case: bool,
        word: bool,
        /// Only in this column (by name).
        col: Option<String>,
    },
    /// Results `start..start+len` of search `id` (rows, with their matching cells).
    SearchPage {
        id: u64,
        start: u64,
        len: usize,
    },
    /// The place of each of these rows in the order the rows are sorted by
    /// (the time column when they are not sorted), for a tree of rows.
    RowKeys {
        id: u64,
        rows: Arc<Vec<u64>>,
        /// The column to order by and whether largest first; `None`: the
        /// file's time column.
        by: Option<(String, bool)>,
    },
    /// Where a file row sits in the grid right now.
    Locate {
        id: u64,
        row: u64,
    },
    /// Find the next row containing `text` (nothing is filtered out).
    Find {
        id: u64,
        text: String,
        /// The row to step from, and its place in the grid.
        after_row: Option<u64>,
        from_pos: Option<u64>,
        forward: bool,
    },
    /// Order the rows by a column (name, largest first?); `None` = file order.
    Sort(Option<(String, bool)>),
    CancelImport,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Importing {
        bytes_done: u64,
        bytes_total: u64,
        rows: u64,
    },
    Ready,
    /// Indexing stopped early (the disk is nearly full, a write failed): the
    /// rows indexed so far are kept and fully usable.
    Partial(String),
    Cancelled,
    Failed(String),
}

impl Phase {
    /// Indexing is over and the rows can be analysed.
    pub fn settled(&self) -> bool {
        matches!(self, Phase::Ready | Phase::Partial(_))
    }
}

/// The phase the app shows for an import's state.
fn phase_for(state: JobState, bytes_done: u64, bytes_total: u64, rows: u64) -> Phase {
    match state {
        JobState::Running => Phase::Importing {
            bytes_done,
            bytes_total,
            rows,
        },
        JobState::Done => Phase::Ready,
        JobState::Cancelled => Phase::Cancelled,
        JobState::Partial(r) => Phase::Partial(r),
        JobState::Failed(e) => Phase::Failed(e),
    }
}

#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
}

/// The chain of (column, value) above a group level; `None` = no value.
pub type GroupPath = Vec<(String, Option<String>)>;

/// Some values of the open value list, in its order.
#[derive(Debug)]
pub struct ValuesPage {
    pub col: usize,
    pub sort: GroupSort,
    pub search: String,
    pub start: u64,
    /// Values matching the search (all values when it is empty).
    pub total: u64,
    /// (value, rows); an empty value = rows with no value.
    pub rows: Vec<(String, u64)>,
}

/// Rows as text for the clipboard, or why not.
#[derive(Debug)]
pub struct CopyOut {
    pub id: u64,
    pub rows: u64,
    pub text: Option<String>,
    pub error: Option<String>,
}

/// What one keyword of a list found.
#[derive(Debug, Clone)]
pub struct ListTerm {
    pub rows: u64,
    pub cells: u64,
    /// (column, cells that hold it), most cells first.
    pub cols: Vec<(String, u64)>,
}

/// Timeline signals of the rows shown.
#[derive(Debug)]
pub struct SignalsOut {
    pub id: u64,
    pub gaps: Vec<logseek_core::query::rhythm::Gap>,
    pub usual_pause: i64,
    pub gap_line: i64,
    pub steps: Vec<logseek_core::query::rhythm::Step>,
    pub step_width: i64,
    pub secs: f64,
    pub error: Option<String>,
}

/// Regular repeats of a column's values, on the rows shown.
#[derive(Debug)]
pub struct RegularOut {
    pub id: u64,
    pub col: String,
    pub values: Vec<logseek_core::query::rhythm::Rhythm>,
    pub too_few: usize,
    pub secs: f64,
    pub error: Option<String>,
}

/// Rare pairs of two columns, on the rows shown.
#[derive(Debug)]
pub struct PairsOut {
    pub id: u64,
    pub a: String,
    pub b: String,
    pub unusual: Vec<logseek_core::query::pairs::PairLine>,
    pub peers: Vec<logseek_core::query::pairs::PeerLine>,
    pub a_values: u64,
    pub total: u64,
    pub secs: f64,
    pub error: Option<String>,
}

/// Unusual rows, of the rows shown.
pub struct UnusualOut {
    pub id: u64,
    pub spread: Vec<u64>,
    pub usual: f64,
    pub line: f64,
    pub rows: Arc<Vec<u64>>,
    pub model: Arc<logseek_core::query::unusual::Model>,
    pub total: u64,
    /// Columns weighed; columns left out (values nearly all different);
    /// columns weighing nothing.
    pub weighed: usize,
    pub left_out: Vec<String>,
    pub quiet: Vec<String>,
    pub secs: f64,
    pub error: Option<String>,
}

impl std::fmt::Debug for UnusualOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnusualOut({}, {} rows)", self.id, self.rows.len())
    }
}

/// Patterns in a column, on the rows shown.
#[derive(Debug)]
pub struct PatternsOut {
    pub id: u64,
    pub patterns: Vec<logseek_core::query::patterns::Pattern>,
    pub empty: u64,
    pub total: u64,
    pub secs: f64,
    pub error: Option<String>,
}

/// Strange text in a column, on the rows shown.
#[derive(Debug)]
pub struct StrangeOut {
    pub id: u64,
    pub spread: Vec<u64>,
    pub unscored: u64,
    pub usual: f64,
    pub line: f64,
    pub values: Vec<logseek_core::query::strange::StrangeValue>,
    pub rows: Arc<Vec<u64>>,
    pub secs: f64,
    pub error: Option<String>,
}

/// Every shape a column holds, on the rows shown.
#[derive(Debug)]
pub struct ShapesOut {
    pub id: u64,
    pub shapes: Vec<logseek_core::query::shape::ShapeCount>,
    pub empty: u64,
    pub total: u64,
    pub secs: f64,
    pub error: Option<String>,
}

/// What the rows shown are compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    /// Every other row of the file.
    RestOfFile,
    /// The same search at other times (when the search keeps a period).
    OtherTimes,
    /// The same search at every time before the period: what is new in it.
    Before,
    /// The same search over the same length of time just before the period.
    JustBefore,
}

/// The time range a period `(start, end)` is compared with, for the choices
/// that are a time range: everything before it, or the same length just before.
pub fn baseline_range(against: Against, (start, end): (i64, i64)) -> Option<(i64, i64)> {
    match against {
        Against::Before => Some((i64::MIN, start)),
        Against::JustBefore => Some((start.saturating_sub(end - start), start)),
        Against::RestOfFile | Against::OtherTimes => None,
    }
}

/// One thing that sets the rows apart: "column is value" (`None`: empty),
/// one or two of them, with exact counts on both sides.
#[derive(Debug, Clone)]
pub struct ContrastLine {
    pub conds: Vec<(String, Option<String>)>,
    /// For each condition, the others that hold in exactly the same rows.
    pub same: Vec<Vec<(String, Option<String>)>>,
    /// Conditions that could stand in for one of the two (its place) with
    /// the very same counts.
    pub instead: Option<(usize, Vec<(String, Option<String>)>)>,
    pub here: u64,
    pub rest: u64,
}

impl ContrastLine {
    pub fn ratio(&self, here_total: u64, rest_total: u64) -> f64 {
        let x = self.here as f64 / here_total.max(1) as f64;
        let y = self.rest as f64 / rest_total.max(1) as f64;
        if y == 0.0 {
            f64::INFINITY
        } else {
            x / y
        }
    }
}

/// The answer to "what sets these rows apart".
/// A column's values never seen before the period (see `query::newin`).
#[derive(Debug, Clone)]
pub struct NewCol {
    pub name: String,
    pub filled: u64,
    pub new_rows: u64,
    pub before_values: u64,
    pub before_filled: u64,
    /// (value, rows), most rows first.
    pub values: Vec<(String, u64)>,
}

#[derive(Debug)]
pub struct ContrastOut {
    pub id: u64,
    pub against: Against,
    pub lines: Vec<ContrastLine>,
    /// Compared with what came before: whether new values were looked for,
    /// and the columns with some (fewest different values before first).
    pub new_asked: bool,
    pub new: Vec<NewCol>,
    pub here_total: u64,
    pub rest_total: u64,
    pub columns: usize,
    pub secs: f64,
    pub error: Option<String>,
}

/// A list search's answer: one entry per keyword, in the order given.
#[derive(Debug)]
pub struct ListOut {
    pub id: u64,
    pub terms: Vec<ListTerm>,
    /// Rows with at least one keyword, and the rows that were searched.
    pub rows_any: u64,
    pub of: u64,
    /// Columns read, and those not read because the index says no keyword
    /// can be in them, over all blocks.
    pub columns_read: u64,
    pub columns_skipped: u64,
    pub error: Option<String>,
}

/// A keyword search's totals.
#[derive(Debug, Clone)]
pub struct SearchOut {
    pub id: u64,
    pub rows: u64,
    pub cells: u64,
    /// (column, matching cells), busiest first; columns with none are left out.
    pub per_col: Vec<(String, u64)>,
    pub error: Option<String>,
}

/// One row of a keyword search: its matching cells, whole.
#[derive(Debug, Clone)]
pub struct SearchRow {
    pub row: u64,
    /// The row's time as written (when the file has a timeline).
    pub time: Option<String>,
    /// (column index, value)
    pub cells: Vec<(usize, String)>,
}

#[derive(Debug)]
pub struct SearchPageOut {
    pub id: u64,
    pub start: u64,
    pub rows: Vec<SearchRow>,
}

/// What Find found.
#[derive(Debug, Clone)]
pub struct FindOut {
    pub id: u64,
    pub text: String,
    /// Rows of the current result that contain the text.
    pub total: u64,
    /// The row found: its place in the grid, and which match it is (1-based,
    /// in file order; unknown while sorted).
    pub pos: Option<u64>,
    pub index: Option<u64>,
    pub error: Option<String>,
}

/// Where a Follow starts.
#[derive(Clone, Debug)]
pub enum FollowStart {
    Rows(Arc<Vec<u64>>),
    /// Every row of the current result.
    Result,
}

/// A Follow's answer.
#[derive(Debug)]
pub struct FollowOut {
    pub id: u64,
    pub start_rows: Arc<Vec<u64>>,
    /// Per step: (row found, the row it links to).
    pub levels: Vec<Vec<(u64, u64)>>,
    /// The time (µs) of each row in the answer that has one, so the tree can
    /// list rows in the order things happened. Empty without a time column.
    pub times: Arc<std::collections::HashMap<u64, i64>>,
    pub error: Option<String>,
}

/// Some groups of a summary, in one order.
#[derive(Debug)]
pub struct SummaryPage {
    pub table: u64,
    pub sort: logseek_core::query::group::SummarySort,
    pub start: u64,
    pub rows: Vec<crate::summary::SummaryRow>,
}

/// A built group level, as the grid sees it.
#[derive(Debug, Clone)]
pub struct GroupLevelInfo {
    pub generation: u64,
    pub groups: u64,
}

/// Some groups of a level, in display order.
#[derive(Debug)]
pub struct GroupPage {
    pub generation: u64,
    pub start: u64,
    pub rows: Vec<GroupRow>,
}

#[derive(Debug)]
pub struct PageData {
    /// Which query result this page belongs to.
    pub generation: u64,
    pub start: u64,
    pub rows: Vec<u64>,
    pub cells: Vec<Vec<Option<String>>>,
    /// Made-column cells that could not be worked out: (row on this page,
    /// column, the reason in words).
    pub errors: Vec<(usize, usize, String)>,
}

/// Rows per hour (or per day for long sources) for one query: the calendar's counts.
#[derive(Debug)]
pub struct Calendar {
    pub query: String,
    pub t0_us: i64,
    pub span_us: i64,
    pub counts: Vec<u64>,
}

#[derive(Debug)]
pub struct Hist {
    pub generation: u64,
    pub t0_us: i64,
    pub span_us: i64,
    pub counts: Vec<u64>,
}

#[derive(Debug)]
pub struct LoadedTags {
    pub source: PathBuf,
    pub tags: crate::tags::TagMap,
    /// Saved when the source had a different size: rows may not line up.
    pub source_changed: bool,
}

#[derive(Debug)]
pub struct FieldSummary {
    pub generation: u64,
    /// The query these numbers were counted under (empty = current result).
    pub query: String,
    pub col: usize,
    /// Rows of the current result that have a value in this column.
    pub with_value: u64,
    pub matched: u64,
    pub distinct: u64,
    /// Most frequent values first (rarest first when `rare`): (value, rows).
    pub top: Vec<(String, u64)>,
    /// Rows of a made column whose value could not be worked out (not counted
    /// as values nor as empty).
    pub errors: u64,
    pub rare: bool,
}

#[derive(Debug)]
pub struct Record {
    pub row: u64,
    pub fields: Vec<(String, String)>,
    /// The row exactly as it is in the source file (capped at 256 KB).
    pub raw: String,
    /// Why the fields could not be read (the source changed, went offline).
    pub error: Option<String>,
}

/// A row's fields for the record view: one page of one row, so columns read
/// from the source re-parse only the window holding it (and are cached), and
/// a failure is reported in words instead of leaving fields out.
/// The time (µs) of each of `rows` (sorted, no repeats) that has one in the
/// file's time column. Empty when the file has no time column.
pub(crate) fn row_times(
    st: &Store,
    fs: &FormulaSet,
    cache: &ChunkCache,
    rows: &[u64],
) -> std::collections::HashMap<u64, i64> {
    let mut out = std::collections::HashMap::new();
    let Some(tc) = st.schema().time_col else {
        return out;
    };
    let set = MatchSet::all(st).restrict(st, rows);
    let total = set.count();
    let mut start = 0u64;
    while start < total {
        let Ok(p) = page_with(st, &set, start, 20_000, &[tc], fs, cache) else {
            break;
        };
        if p.rows.is_empty() {
            break;
        }
        for (row, cells) in p.rows.iter().zip(&p.cells) {
            if let Some(t) = cells
                .first()
                .and_then(|c| c.as_deref())
                .and_then(logseek_core::time::parse_time)
            {
                out.insert(*row, t);
            }
        }
        start += p.rows.len() as u64;
    }
    out
}

pub(crate) fn record_fields(
    st: &Store,
    fs: &FormulaSet,
    cache: &ChunkCache,
    row: u64,
) -> Result<Vec<(String, String)>, String> {
    let ns = st.schema().len();
    let one = MatchSet::all(st).restrict(st, &[row]);
    let cols: Vec<usize> = (0..ns + fs.items.len()).collect();
    let p = page_with(st, &one, 0, 1, &cols, fs, cache).map_err(|e| e.to_string())?;
    let errors = p.errors;
    let mut cells = p.cells.into_iter().next().unwrap_or_default();
    // a made column that could not be worked out says why, not nothing
    for (_, c, why) in errors {
        if let Some(cell) = cells.get_mut(c) {
            *cell = Some(format!("could not be worked out: {why}"));
        }
    }
    let names = st
        .schema()
        .columns
        .iter()
        .map(|c| c.name.clone())
        .chain(fs.items.iter().map(|(n, _)| n.clone()));
    Ok(names
        .zip(cells)
        .filter_map(|(n, v)| v.filter(|v| !v.is_empty()).map(|v| (n, v)))
        .collect())
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub source: Option<SourceInfo>,
    pub phase: Phase,
    pub columns: Vec<String>,
    pub col_types: Vec<logseek_core::types::ColType>,
    /// Text columns whose values look machine-written (drawn in the mono face).
    pub col_machine: Vec<bool>,
    /// Each stored column's role (constant, id, sparse, informative), worked
    /// out once the file is fully indexed.
    pub col_profile: Option<Arc<Vec<logseek_core::query::roles::ColumnProfile>>>,
    /// Whether the file's rows run in time order (checked once it is indexed).
    pub time_order: Option<logseek_core::query::order::TimeOrder>,
    /// Columns whose values point at another column's (for Follow), most telling first.
    pub links: Arc<Vec<logseek_core::query::links::Link>>,
    /// Oldest and newest time in the source (UTC µs), from the segment stats.
    pub time_bounds: Option<(i64, i64)>,
    /// Rows with a value, over the whole source.
    pub col_counts: Vec<u64>,
    pub time_col: Option<usize>,
    pub total_rows: u64,
    pub matched: u64,
    pub generation: u64,
    pub query: String,
    pub query_error: Option<String>,
    pub query_secs: f64,
    pub busy: bool,
    pub page: Option<Arc<PageData>>,
    pub hist: Option<Arc<Hist>>,
    pub calendar: Option<Arc<Calendar>>,
    pub summary: Option<Arc<crate::summary::SummaryResult>>,
    /// The grouped view, by level key: levels, group pages and open groups' rows.
    pub group_levels: std::collections::BTreeMap<u64, GroupLevelInfo>,
    pub group_pages: std::collections::BTreeMap<u64, Arc<GroupPage>>,
    pub group_rows: std::collections::BTreeMap<u64, Arc<PageData>>,
    pub values_page: Option<Arc<ValuesPage>>,
    pub follow: Option<Arc<FollowOut>>,
    pub find: Option<Arc<FindOut>>,
    pub search: Option<Arc<SearchOut>>,
    pub list: Option<Arc<ListOut>>,
    pub contrast: Option<Arc<ContrastOut>>,
    pub shapes: Option<Arc<ShapesOut>>,
    pub strange: Option<Arc<StrangeOut>>,
    pub patterns: Option<Arc<PatternsOut>>,
    pub unusual: Option<Arc<UnusualOut>>,
    pub pairs: Option<Arc<PairsOut>>,
    pub signals: Option<Arc<SignalsOut>>,
    /// The open file's columns and values, for reading plain words.
    pub lexicon: Option<Arc<logseek_core::query::plain::Lexicon>>,
    pub regular: Option<Arc<RegularOut>>,
    /// (patterns id, which pattern, its rows).
    pub pattern_rows: Option<(u64, usize, Arc<Vec<u64>>)>,
    /// (list search id, the ids of the rows it found).
    pub list_rows: Option<(u64, Arc<Vec<u64>>)>,
    pub copied: Option<Arc<CopyOut>>,
    pub search_page: Option<Arc<SearchPageOut>>,
    /// (request, the row's place in the grid, if it is in the current result)
    pub located: Option<(u64, Option<u64>)>,
    /// (request, each row's place in the order asked for)
    pub row_keys: Option<(u64, Arc<std::collections::HashMap<u64, i64>>)>,
    /// The column the rows are sorted by (name, largest first?), and whether
    /// the order is held on disk.
    pub sort: Option<(String, bool)>,
    pub sort_on_disk: bool,
    pub sort_error: Option<String>,
    /// Rows fetched by id for a tree: (who asked, the rows).
    pub rows_by_id: Option<Arc<(u64, PageData)>>,
    /// Pages of the open summary, and its whole table as text when copied.
    pub summary_page: Option<Arc<SummaryPage>>,
    pub summary_copy: Option<Arc<(u64, String)>>,
    pub export: Option<crate::export::ExportStatus>,
    pub record: Option<Arc<Record>>,
    pub field: Option<Arc<FieldSummary>>,
    pub values: Option<Arc<FieldSummary>>,
    pub tags: Option<Arc<LoadedTags>>,
    /// Each tagged row's time and its tags, in time order (for the time spine).
    pub tag_times: Arc<Vec<(i64, u16)>>,
    /// (query, rows it keeps) for the value list's preview.
    pub preview: Option<(String, u64)>,
    /// Virtual columns: (name, formula), numbered after the stored columns.
    pub virtual_cols: Vec<(String, String)>,
    /// Why the last column could not be added.
    pub column_error: Option<String>,
    /// What the import could not read cleanly, one sentence each (empty: all read).
    pub import_notes: Arc<Vec<String>>,
    /// A request that crashed inside the engine and was stopped, in words.
    pub engine_note: Option<String>,
    /// Made columns that cannot be shown, and why (a cycle, a missing input).
    pub column_problems: Vec<(String, String)>,
    /// Every made column saved with this file's index, working or not.
    pub saved_cols: Vec<(String, String)>,
    /// The result is restricted to tagged rows.
    pub restricted: bool,
    pub ram_used: u64,
    pub budget_cap: u64,
    pub mode: &'static str,
    pub mode_hint: Option<&'static str>,
    pub index_bytes: u64,
    /// Columns whose values are read from the source file (the disk had no
    /// room to index them).
    pub source_cols: Vec<String>,
    /// Bytes of the index held in memory (no room on any disk).
    pub mem_bytes: u64,
}

/// Status-bar words for the index: (label, tooltip).
pub fn index_words(index_bytes: u64, mem_bytes: u64, source_cols: &[String]) -> (String, String) {
    // under a megabyte in kilobytes, so a small index never reads as nothing
    let size = |b: u64| {
        if b > 0 && b < 1 << 20 {
            format!("{} KB", crate::moment::group(b.div_ceil(1 << 10)))
        } else {
            format!("{} MB", crate::moment::group(b >> 20))
        }
    };
    let idx = size(index_bytes);
    let mem = crate::moment::group(mem_bytes >> 20);
    let read_from = if source_cols.is_empty() {
        String::new()
    } else {
        format!(
            " These columns are read straight from the original file when needed: {}.",
            source_cols.join(", ")
        )
    };
    if mem_bytes > 0 && index_bytes == 0 {
        return (
            format!("Nothing written to disk · index in memory {mem} MB"),
            format!(
                "There is no room on the disk for an index, so it is kept in memory for this session, inside LogSeek's memory budget.{read_from}\nEvery row can be searched; results are exact. Opening the file again in a later session reads it once more."
            ),
        );
    }
    if mem_bytes > 0 {
        return (
            format!("Index {idx} + {mem} MB in memory"),
            format!(
                "The disk filled while indexing: the rest of the index is kept in memory for this session.{read_from}\nEvery row can be searched; results are exact. With free space, opening the file again finishes the index on disk."
            ),
        );
    }
    if source_cols.is_empty() {
        return (
            format!("Index {idx}"),
            "A compact copy of the file for fast search, kept with LogSeek's other indexes. The file itself is never changed.".into(),
        );
    }
    let list = source_cols.join(", ");
    let named = if source_cols.len() <= 3 {
        list.clone()
    } else {
        format!("{} columns", source_cols.len())
    };
    (
        format!("Index {idx} · {named} read from the file"),
        format!(
            "The disk had room for most of the index. These columns are read straight from the original file when they are needed: {list}.\nTheir values are exact; searching and filtering them is a little slower. Everything else is as fast as a full index. The file itself is never changed."
        ),
    )
}

pub struct Bridge {
    tx: Sender<Cmd>,
    snap: Arc<Mutex<Arc<Snapshot>>>,
    /// Set when rows are asked for: background work (the timeline) stops so
    /// the rows on screen are served first.
    interrupt: Arc<AtomicBool>,
}

impl Bridge {
    pub fn start(ctx: egui::Context) -> Bridge {
        // LOGSEEK_BUDGET_MB pins the RAM budget (tests and small machines)
        let pinned = crate::dev::var("LOGSEEK_BUDGET_MB")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|m| m << 20);
        let plan = Plan::for_machine(&Machine::detect(), pinned);
        // the whole app lives inside the cap: the window, fonts and drawing take
        // about UI_RESERVE, and the engine's two pools (working memory and the
        // chunk cache) share what is left, so the process never exceeds the cap
        const UI_RESERVE: u64 = 128 << 20;
        let app_cap = plan.cap;
        let engine_plan = Plan::for_resources(
            app_cap.saturating_sub(UI_RESERVE).max(160 << 20),
            Machine::detect().cores,
        );
        // the cache may grow past its share while the work in hand keeps
        // re-reading what it had to drop, as far as the machine can spare;
        // a budget the user pinned is fixed
        let cache_ceiling = if pinned.is_some() {
            engine_plan.cache_bytes
        } else {
            let spare = Machine::detect().elastic_ceiling();
            Plan::for_resources(spare.saturating_sub(UI_RESERVE).max(160 << 20), 1)
                .cache_bytes
                .max(engine_plan.cache_bytes)
        };
        let snap0 = Snapshot {
            group_levels: Default::default(),
            group_pages: Default::default(),
            group_rows: Default::default(),
            values_page: None,
            follow: None,
            find: None,
            search: None,
            list: None,
            contrast: None,
            shapes: None,
            strange: None,
            patterns: None,
            unusual: None,
            pairs: None,
            signals: None,
            lexicon: None,
            regular: None,
            pattern_rows: None,
            list_rows: None,
            copied: None,
            search_page: None,
            located: None,
            row_keys: None,
            sort: None,
            sort_on_disk: false,
            sort_error: None,
            rows_by_id: None,
            summary_page: None,
            summary_copy: None,
            source: None,
            phase: Phase::Idle,
            columns: Vec::new(),
            col_types: Vec::new(),
            col_machine: Vec::new(),
            col_profile: None,
            time_order: None,
            links: Arc::new(Vec::new()),
            time_bounds: None,
            col_counts: Vec::new(),
            time_col: None,
            total_rows: 0,
            matched: 0,
            generation: 0,
            query: String::new(),
            query_error: None,
            query_secs: 0.0,
            busy: false,
            page: None,
            hist: None,
            calendar: None,
            summary: None,
            export: None,
            record: None,
            field: None,
            values: None,
            tags: None,
            tag_times: Arc::new(Vec::new()),
            preview: None,
            virtual_cols: Vec::new(),
            column_error: None,
            import_notes: Arc::new(Vec::new()),
            engine_note: None,
            column_problems: Vec::new(),
            saved_cols: Vec::new(),
            restricted: false,
            ram_used: process_memory().private,
            budget_cap: app_cap,
            mode: plan.mode.label(),
            mode_hint: plan.unlock_hint(),
            index_bytes: 0,
            source_cols: Vec::new(),
            mem_bytes: 0,
        };
        let snap = Arc::new(Mutex::new(Arc::new(snap0)));
        let (tx, rx) = unbounded();
        let s2 = snap.clone();
        let interrupt = Arc::new(AtomicBool::new(false));
        let i2 = interrupt.clone();
        std::thread::Builder::new()
            .name("engine".into())
            // a roomy stack: the engine loop is one large function and debug
            // builds overflowed the default 2 MB once (reserved, not used, memory)
            .stack_size(16 << 20)
            .spawn(move || {
                let mut e = Engine::new(s2, ctx, engine_plan, cache_ceiling);
                e.app_cap = app_cap;
                e.grows = pinned.is_none();
                e.interrupt = i2;
                // one request that crashes must not end every later one: the
                // crash is caught, said on screen, and the engine carries on
                loop {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e.run(&rx)));
                    match r {
                        Ok(()) => break,
                        Err(p) => {
                            let why = p
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_else(|| "no details".to_string());
                            e.update(|s| s.engine_note = Some(why));
                        }
                    }
                }
            })
            .expect("spawn engine thread");
        Bridge {
            tx,
            snap,
            interrupt,
        }
    }

    pub fn send(&self, c: Cmd) {
        // someone is waiting for every request: background work steps aside
        if !matches!(c, Cmd::SaveTags(_) | Cmd::CancelImport) {
            self.interrupt
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = self.tx.send(c);
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

struct Engine {
    /// Stored text columns whose values look machine-written, judged once
    /// from the first block of each (and the column count it was judged for).
    machine: (usize, Vec<bool>),
    /// The column roles have been worked out for the open index.
    profiled: bool,
    /// Names for a headerless file's columns, for the next open.
    names: Vec<String>,
    /// The tags of the open source, to place on the time spine.
    tag_map: Arc<crate::tags::TagMap>,
    /// The last patterns worked out (id, column, the patterns), for their rows.
    last_patterns: Option<(u64, usize, logseek_core::query::patterns::Patterns)>,
    /// The cache may grow with what the machine has free (not pinned by the user).
    grows: bool,
    /// When free memory was last measured.
    measured: Instant,
    /// Rows were asked for: stop background work (see `Bridge::interrupt`).
    interrupt: Arc<AtomicBool>,
    /// The timeline for the current result still has to be built.
    hist_due: bool,
    /// Heavy requests stopped by a request for rows: run again next.
    deferred: Vec<Cmd>,
    /// When rows were last asked for: stopped work resumes only after a pause.
    last_page: Instant,
    snap: Arc<Mutex<Arc<Snapshot>>>,
    ctx: egui::Context,
    budget: Budget,
    /// The rows the last list search found (its id, the rows).
    list_found: Option<(u64, logseek_core::query::exec::MatchSet)>,
    cache: ChunkCache,
    /// What the whole app may use while the cache is at its starting size.
    app_cap: u64,
    store: Option<Store>,
    store_dir: Option<PathBuf>,
    job: Option<IngestJob>,
    matches: Option<MatchSet>,
    query: String,
    /// Grouped view: built levels and open groups' rows, by key.
    levels: std::collections::HashMap<u64, GroupLevel>,
    leaves: std::collections::HashMap<u64, MatchSet>,
    /// The open value list: every value of one column (col, its search).
    value_level: Option<(usize, GroupSort, String, GroupLevel)>,
    /// The open summary: its id, measure, current order and every group.
    summary_table: Option<(
        u64,
        crate::summary::SummarySpec,
        logseek_core::query::group::SummarySort,
        logseek_core::query::group::SummaryTable,
    )>,
    summary_ids: u64,
    /// The query's own time range: the timeline zooms to it.
    range: Option<(i64, i64)>,
    /// The sort in force and the rows' ids in that order.
    sort_spec: Option<(String, bool)>,
    order: Option<logseek_core::query::sort::SortedOrder>,
    /// The keyword search in force: its id, options, and the rows that match.
    searched: Option<(u64, logseek_core::query::search::SearchOpts, MatchSet)>,
    /// Show only these rows (tag filter).
    only: Option<Arc<Vec<u64>>>,
    /// Virtual columns (name, formula) and their compiled set for the open store.
    vcols: Vec<(String, String)>,
    fs: FormulaSet,
    source_bytes: u64,
    last_poll: Instant,
}

impl Engine {
    fn new(
        snap: Arc<Mutex<Arc<Snapshot>>>,
        ctx: egui::Context,
        plan: Plan,
        cache_ceiling: u64,
    ) -> Engine {
        // working memory and cache are carved from one cap, never added on top
        let budget = Budget::new(plan.cap.saturating_sub(plan.cache_bytes));
        Engine {
            last_patterns: None,
            tag_map: Arc::new(Default::default()),
            machine: (0, Vec::new()),
            profiled: false,
            names: Vec::new(),
            grows: false,
            measured: Instant::now(),
            levels: Default::default(),
            sort_spec: None,
            order: None,
            searched: None,
            leaves: Default::default(),
            value_level: None,
            summary_table: None,
            summary_ids: 0,
            interrupt: Arc::new(AtomicBool::new(false)),
            hist_due: false,
            deferred: Vec::new(),
            last_page: Instant::now(),
            snap,
            ctx,
            cache: ChunkCache::new(Budget::elastic(plan.cache_bytes, cache_ceiling)),
            app_cap: plan.cap,
            list_found: None,
            budget,
            store: None,
            store_dir: None,
            job: None,
            matches: None,
            query: String::new(),
            range: None,
            only: None,
            vcols: Vec::new(),
            fs: FormulaSet::default(),
            source_bytes: 0,
            last_poll: Instant::now(),
        }
    }

    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        let mut g = self.snap.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = (**g).clone();
        f(&mut s);
        s.ram_used = process_memory().private;
        // the limit in force now: more while the cache has grown, back when it gave it up
        let b = self.cache.budget();
        s.budget_cap = self.app_cap + b.cap().saturating_sub(b.base());
        *g = Arc::new(s);
        drop(g);
        self.ctx.request_repaint();
    }

    fn run(&mut self, rx: &Receiver<Cmd>) {
        loop {
            // work stopped by a request for rows comes back once scrolling has
            // paused for QUIET (newer requests of the same kind replace it)
            const QUIET: Duration = Duration::from_millis(150);
            let since = self.last_page.elapsed();
            let background = !self.deferred.is_empty() || self.hist_due;
            let quiet = since >= QUIET;
            let wait = match (background, quiet) {
                (false, _) => Duration::from_millis(120),
                (true, true) => Duration::ZERO,
                (true, false) => QUIET - since,
            };
            // nothing read for a while: the cache gives back what it grew
            if self.cache.relax(Duration::from_secs(20)) {
                self.update(|_| {});
            }
            // how far the cache may grow follows what the machine has free now:
            // measured again every few seconds, no fixed most
            if self.grows && self.measured.elapsed() >= Duration::from_secs(3) {
                self.measured = Instant::now();
                let b = self.cache.budget();
                let free = Machine::detect().available_ram / 10 * 7;
                b.set_ceiling(b.cap().saturating_add(free));
            }
            let mut fresh: Vec<Cmd> = Vec::new();
            match rx.recv_timeout(wait) {
                Ok(c) => fresh.push(c),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            fresh.extend(rx.try_iter());
            // something new asked for goes ahead of stopped old work; old work of
            // the same kind is superseded
            let kind = |c: &Cmd| std::mem::discriminant(c);
            let heavy = |c: &Cmd| {
                matches!(
                    c,
                    Cmd::FieldSummary { .. }
                        | Cmd::Values { .. }
                        | Cmd::Count(_)
                        | Cmd::Calendar(_)
                )
            };
            let fresh_kinds: Vec<_> = fresh.iter().filter(|c| heavy(c)).map(kind).collect();
            self.deferred.retain(|c| !fresh_kinds.contains(&kind(c)));
            // stopped work resumes only on a round when nothing new arrived
            let mut cmds: Vec<Cmd> = if quiet && fresh.is_empty() {
                std::mem::take(&mut self.deferred)
            } else {
                Vec::new()
            };
            // coalesce: only the newest query / page request matters
            cmds.extend(fresh);
            let mut open = None;
            let mut query = None;
            let mut pg = None;
            let mut rec = None;
            let mut field = None;
            let mut values = None;
            let mut count = None;
            let mut save_tags = None;
            let mut add_cols: Vec<(String, String)> = Vec::new();
            let mut remove_cols: Vec<String> = Vec::new();
            let mut rename_cols: Vec<(String, String)> = Vec::new();
            let mut restrict = None;
            let mut sort: Option<Option<(String, bool)>> = None;
            let mut calendar: Option<String> = None;
            let mut summarise: Option<crate::summary::SummarySpec> = None;
            let mut group_cmds: Vec<Cmd> = Vec::new();
            let mut export: Option<(PathBuf, crate::export::Format, Vec<usize>)> = None;
            for c in cmds {
                match c {
                    Cmd::OpenWith(p, t, n) => {
                        open = Some((p, t));
                        self.names = n;
                        query = None;
                        pg = None;
                    }
                    Cmd::Reindex(p) => {
                        // let go of the index before removing it, then open afresh
                        if let Some(j) = self.job.take() {
                            j.cancel();
                            j.join();
                        }
                        self.store = None;
                        self.matches = None;
                        if let Some(d) = self.store_dir.clone() {
                            logseek_core::store::memindex::remove(&d);
                            let _ = clear_index(&d);
                        }
                        open = Some((p, TimeChoice::Auto));
                        query = None;
                        pg = None;
                    }
                    Cmd::Query(q) => {
                        query = Some(q);
                        pg = None;
                    }
                    Cmd::Page { start, len } => pg = Some((start, len)),
                    Cmd::Record(r) => rec = Some(r),
                    Cmd::FieldSummary { col, rare } => field = Some((col, rare)),
                    Cmd::Values {
                        col,
                        query,
                        n,
                        rare,
                        alpha,
                    } => values = Some((col, query, n, rare, alpha)),
                    Cmd::Count(q) => count = Some(q),
                    Cmd::Calendar(q) => calendar = Some(q),
                    Cmd::Summarise(sp) => summarise = Some(sp),
                    c @ (Cmd::GroupLevel { .. }
                    | Cmd::GroupPage { .. }
                    | Cmd::GroupRows { .. }
                    | Cmd::ValuesPage { .. }
                    | Cmd::Follow { .. }
                    | Cmd::Find { .. }
                    | Cmd::Search { .. }
                    | Cmd::ListSearch { .. }
                    | Cmd::ListRows(_)
                    | Cmd::Contrast { .. }
                    | Cmd::Shapes { .. }
                    | Cmd::Strange { .. }
                    | Cmd::Patterns { .. }
                    | Cmd::Unusual { .. }
                    | Cmd::Pairs { .. }
                    | Cmd::Signals { .. }
                    | Cmd::Lexicon
                    | Cmd::Regular { .. }
                    | Cmd::PatternRows { .. }
                    | Cmd::CopyRows { .. }
                    | Cmd::SearchPage { .. }
                    | Cmd::Locate { .. }
                    | Cmd::RowKeys { .. }
                    | Cmd::RowsById { .. }
                    | Cmd::SummaryPage { .. }
                    | Cmd::SummaryCopy { .. }) => group_cmds.push(c),
                    Cmd::Export { path, format, cols } => export = Some((path, format, cols)),
                    Cmd::SaveTags(t) => save_tags = Some(t),
                    Cmd::AddColumns(c) => add_cols.extend(c),
                    Cmd::RemoveColumn(n) => remove_cols.push(n),
                    Cmd::RenameColumn(a, b) => rename_cols.push((a, b)),
                    Cmd::Restrict(r) => restrict = Some(r),
                    Cmd::Sort(s) => sort = Some(s),
                    Cmd::CancelImport => {
                        if let Some(j) = &self.job {
                            j.cancel();
                        }
                    }
                }
            }
            if let Some((p, t)) = open {
                self.open(p, t);
            }
            self.poll_import();
            if !add_cols.is_empty() || !remove_cols.is_empty() || !rename_cols.is_empty() {
                // renamed first: the name, and every made column's references to it
                // (the window checked the name; checked again here)
                let stored: Vec<String> = self
                    .store
                    .as_ref()
                    .map(|st| st.schema().columns.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                for (old, new) in rename_cols {
                    rename_made(&mut self.vcols, &old, &new, &stored);
                }
                self.vcols.retain(|(n, _)| !remove_cols.contains(n));
                let mut err = None;
                for (n, f) in add_cols {
                    if self.vcols.iter().any(|(x, _)| *x == n) {
                        continue;
                    }
                    // validate against the open store before keeping it
                    let ok = match &self.store {
                        Some(st) => {
                            // with every made column, so it may use any of them
                            let mut all = self.vcols.clone();
                            all.push((n.clone(), f.clone()));
                            let (_, errs) = FormulaSet::from_defs(st, &all);
                            match errs.into_iter().find(|(x, _)| *x == n) {
                                Some((_, e)) => Err(e.msg),
                                None => Ok(()),
                            }
                        }
                        None => Ok(()),
                    };
                    match ok {
                        Ok(()) => self.vcols.push((n, f)),
                        Err(e) => err = Some(format!("{n}: {e}")),
                    }
                }
                self.rebuild_formulas();
                if let Some(d) = self.store_dir.as_ref().filter(|_| !screenshot_session()) {
                    let _ = save_columns(d, &self.vcols);
                }
                // the columns shown were published by rebuild_formulas
                self.update(|s| s.column_error = err);
                if query.is_none() {
                    query = Some(self.query.clone());
                }
            }
            if let Some(t) = save_tags {
                if let Some(d) = self.store_dir.as_ref().filter(|_| !screenshot_session()) {
                    let _ = crate::tags::save(d, self.source_bytes, &t);
                }
                self.tag_map = t;
                self.place_tags();
            }
            if let Some(r) = restrict {
                self.only = r;
                let restricted = self.only.is_some();
                self.update(|s| s.restricted = restricted);
                if query.is_none() {
                    query = Some(self.query.clone());
                }
            }
            if let Some(s) = sort {
                self.sort_spec = s;
                if query.is_none() {
                    query = Some(self.query.clone());
                }
            }
            if let Some(q) = query {
                self.run_query(q);
            }
            if let Some((s, l)) = pg {
                self.serve_page(s, l);
                self.last_page = Instant::now();
            }
            if let Some(r) = rec {
                self.serve_record(r);
            }
            // heavy work yields to the rows on screen: wait while a request for
            // rows is queued, and stop (to run again) when one arrives
            let rows_waiting = |rx: &Receiver<Cmd>| !rx.is_empty();
            if let Some((c, rare)) = field {
                if rows_waiting(&rx) {
                    self.deferred.push(Cmd::FieldSummary { col: c, rare });
                } else {
                    self.serve_field(c, rare);
                }
            }
            if let Some((c, q, n, rare, alpha)) = values {
                if rows_waiting(&rx) {
                    self.deferred.push(Cmd::Values {
                        col: c,
                        query: q,
                        n,
                        rare,
                        alpha,
                    });
                } else {
                    self.serve_values(c, q, n, rare, alpha);
                }
            }
            if let Some(q) = count {
                self.interrupt
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                if rows_waiting(&rx) || !self.serve_count(q.clone()) {
                    self.deferred.push(Cmd::Count(q));
                }
            }
            if let Some(q) = calendar {
                self.interrupt
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                if rows_waiting(&rx) || !self.serve_calendar(q.clone()) {
                    self.deferred.push(Cmd::Calendar(q));
                }
            }
            if let Some(sp) = summarise {
                self.serve_summary(sp);
            }
            for c in group_cmds {
                match c {
                    Cmd::GroupLevel {
                        key,
                        path,
                        by,
                        sort,
                    } => self.serve_group_level(key, &path, &by, sort),
                    Cmd::GroupPage { key, start, len } => self.serve_group_page(key, start, len),
                    Cmd::ValuesPage {
                        col,
                        search,
                        start,
                        len,
                    } => self.serve_values_page(col, search, start, len),
                    Cmd::SummaryPage {
                        table,
                        sort,
                        start,
                        len,
                    } => self.serve_summary_page(table, sort, start, len),
                    Cmd::Follow { id, start, spec } => self.serve_follow(id, start, spec),
                    Cmd::Find {
                        id,
                        text,
                        after_row,
                        from_pos,
                        forward,
                    } => self.serve_find(id, text, after_row, from_pos, forward),
                    Cmd::Search {
                        id,
                        text,
                        case,
                        word,
                        col,
                    } => self.serve_search(id, text, case, word, col),
                    Cmd::SearchPage { id, start, len } => self.serve_search_page(id, start, len),
                    Cmd::ListSearch {
                        id,
                        terms,
                        case,
                        word,
                        stop,
                        done,
                    } => self.serve_list(id, terms, case, word, stop, done),
                    Cmd::ListRows(id) => self.serve_list_rows(id),
                    Cmd::Contrast {
                        id,
                        against,
                        min_share,
                        min_ratio,
                        stop,
                        done,
                    } => self.serve_contrast(id, against, min_share, min_ratio, stop, done),
                    Cmd::Shapes {
                        id,
                        col,
                        how,
                        stop,
                        done,
                    } => self.serve_shapes(id, col, how, stop, done),
                    Cmd::Strange {
                        id,
                        col,
                        line,
                        stop,
                        done,
                    } => self.serve_strange(id, col, line, stop, done),
                    Cmd::Patterns {
                        id,
                        col,
                        alike,
                        stop,
                        done,
                    } => self.serve_patterns(id, col, alike, stop, done),
                    Cmd::PatternRows { id, which } => self.serve_pattern_rows(id, which),
                    Cmd::Unusual {
                        id,
                        together,
                        line,
                        stop,
                        done,
                    } => self.serve_unusual(id, together, line, stop, done),
                    Cmd::Pairs {
                        id,
                        a,
                        b,
                        stop,
                        done,
                    } => self.serve_pairs(id, a, b, stop, done),
                    Cmd::Signals { id, stop, done } => self.serve_signals(id, stop, done),
                    Cmd::Lexicon => self.serve_lexicon(),
                    Cmd::Regular {
                        id,
                        col,
                        stop,
                        done,
                    } => self.serve_regular(id, col, stop, done),
                    Cmd::CopyRows {
                        id,
                        ranges,
                        cols,
                        names,
                    } => self.serve_copy(id, ranges, cols, names),
                    Cmd::Locate { id, row } => self.serve_locate(id, row),
                    Cmd::RowKeys { id, rows, by } => self.serve_row_keys(id, &rows, by),
                    Cmd::RowsById { key, rows } => self.serve_rows_by_id(key, rows),
                    Cmd::SummaryCopy {
                        table,
                        sort,
                        heads,
                        is_time,
                    } => self.serve_summary_copy(table, sort, heads, is_time),
                    Cmd::GroupRows {
                        key,
                        path,
                        start,
                        len,
                    } => self.serve_group_rows(key, &path, start, len),
                    _ => {}
                }
            }
            if let Some((p, f, c)) = export {
                self.serve_export(p, f, c);
            }
            // the timeline last, only while nothing waits; a request for rows
            // stops it and it starts again once they are served
            if self.hist_due && rx.is_empty() && self.last_page.elapsed() >= QUIET {
                self.interrupt
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                if self.build_hist() {
                    self.hist_due = false;
                }
            }
        }
    }

    fn open(&mut self, path: PathBuf, time: TimeChoice) {
        if let Some(j) = self.job.take() {
            j.cancel();
        }
        self.store = None;
        self.matches = None;
        // whatever was read from the index open before is let go: an index
        // rebuilt in the same folder (the file changed, or was read again)
        // must never be answered from columns of the old one
        self.cache.clear();
        // a new file starts in its own order
        self.sort_spec = None;
        self.order = None;
        self.query.clear();
        // several files as one: the set's list names them, in its own index folder
        let set = crate::fileset::is_set(&path)
            .then(|| crate::fileset::read(&path))
            .flatten();
        // a list is the app's own when it sits where the app keeps this set's
        // index. Any other list came with someone else's files: it is indexed
        // in the app's own data (never beside it), and may only name files in
        // its own folder. Decided before any listed file is touched.
        let own_list = set.is_some() && path.parent().is_some_and(in_index_home);
        if let Some(files) = set.as_ref().filter(|_| !own_list) {
            let bad = crate::fileset::outside(&path, files);
            if !bad.is_empty() {
                let why = format!(
                    "This list names {} outside its own folder, so it was not opened: {}. A list that comes with other files may only gather the files beside it. Open the files themselves instead.",
                    if bad.len() == 1 { "a file".to_string() } else { format!("{} files", bad.len()) },
                    bad.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                );
                let shown = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.update(|s| {
                    s.source = Some(SourceInfo { path: path.clone(), name: shown, bytes: 0 });
                    s.phase = Phase::Failed(why);
                    s.columns.clear();
                    s.time_col = None;
                    s.total_rows = 0;
                    s.matched = 0;
                    s.generation += 1;
                    s.page = None;
                    s.hist = None;
                });
                return;
            }
        }
        let bytes = match &set {
            Some(files) => files
                .iter()
                .map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0))
                .sum(),
            None => std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        };
        let name = match &set {
            Some(files) => crate::fileset::title(files),
            None => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let dir = match (&set, path.parent()) {
            (Some(_), Some(p)) if own_list => p.to_path_buf(),
            (Some(files), _) => store_dir_for_set(files),
            _ => store_dir_for(&path),
        };
        if let Some(old) = self.store_dir.as_ref().filter(|o| **o != dir) {
            // an index held in memory for the previous file: give its RAM back
            logseek_core::store::memindex::remove(old);
        }
        self.store_dir = Some(dir.clone());
        self.source_bytes = bytes;
        self.only = None;
        let (tags, changed) = crate::tags::load(&dir, bytes);
        self.vcols = load_columns(&dir);
        self.fs = FormulaSet::default();
        let vcols = self.vcols.clone();
        let loaded = Arc::new(LoadedTags {
            source: path.clone(),
            tags,
            source_changed: changed,
        });
        self.tag_map = Arc::new(loaded.tags.clone());
        // a new file: its columns are judged afresh
        self.machine = (0, Vec::new());
        self.profiled = false;
        let files = set.clone().unwrap_or_else(|| vec![path.clone()]);
        self.job = Some(start_ingest_many(
            &files,
            &dir,
            self.budget.clone(),
            IngestOptions {
                // never fill the drive: stop at free space − 200 MB
                disk_budget: Some(logseek_core::sysmem::disk_budget(&dir, None)),
                time,
                names: std::mem::take(&mut self.names),
                ..Default::default()
            },
        ));
        self.update(|s| {
            s.source = Some(SourceInfo {
                path: path.clone(),
                name,
                bytes,
            });
            s.phase = Phase::Importing {
                bytes_done: 0,
                bytes_total: bytes,
                rows: 0,
            };
            s.columns.clear();
            s.time_col = None;
            s.total_rows = 0;
            s.matched = 0;
            s.generation += 1;
            s.query.clear();
            s.query_error = None;
            s.page = None;
            s.hist = None;
            s.record = None;
            s.field = None;
            s.tags = Some(loaded.clone());
            s.col_profile = None;
            s.time_order = None;
            s.links = Arc::new(Vec::new());
            s.tag_times = Arc::new(Vec::new());
            // shown once the index is open and they are made; saved ones known now
            s.virtual_cols = Vec::new();
            s.saved_cols = vcols;
            s.column_error = None;
            s.import_notes = Arc::new(Vec::new());
            s.engine_note = None;
            s.restricted = false;
        });
    }

    /// While importing: publish progress, and show rows as soon as the first
    /// segments are committed (the store is readable mid-import).
    fn poll_import(&mut self) {
        let Some(job) = &self.job else { return };
        let state = job.state();
        let running = matches!(state, JobState::Running);
        if running && self.last_poll.elapsed() < Duration::from_millis(400) {
            return;
        }
        self.last_poll = Instant::now();
        let p = job.progress();
        let (done, total, rows) = (p.bytes_done(), p.bytes_total(), p.rows());
        let dir = self.store_dir.clone().unwrap();
        let had = self.store.as_ref().map(|s| s.row_count()).unwrap_or(0);
        if let Ok(st) = Store::open(&dir) {
            if st.row_count() > had || !running {
                self.store = Some(st);
                self.rebuild_formulas();
                self.place_tags();
                // refresh the visible result with the new rows
                let q = self.query.clone();
                self.run_query(q);
            }
        }
        let phase = phase_for(state, done, total, rows);
        if !running {
            self.job = None;
            // what could not be read cleanly is said above the rows, not kept quiet
            if let Some(st) = &self.store {
                let notes = Arc::new(crate::importnote::lines(&st.manifest().report));
                self.update(|s| s.import_notes = notes);
            }
            // fully indexed: what each column is (for the smart view and constants)
            if !self.profiled {
                self.profiled = true;
                if let Some(st) = &self.store {
                    let no = std::sync::atomic::AtomicBool::new(false);
                    if let Ok(p) = logseek_core::query::roles::profile(st, &self.cache, &no) {
                        let p = Arc::new(p);
                        self.update(|s| s.col_profile = Some(p));
                    }
                    // in time order or not: whether a gap between two rows is a
                    // silence, and a time a place in the rows
                    // (reading every time once; the answer is kept beside the
                    // index, so opening the file again does not read them again)
                    let finished = st.manifest().state == "done";
                    let key = order_key(st.manifest());
                    match load_order(&dir, &key).filter(|_| finished) {
                        Some(o) => self.update(|s| s.time_order = o),
                        None => {
                            if let Ok(o) =
                                logseek_core::query::order::time_order(st, &self.cache, &no)
                            {
                                if finished {
                                    save_order(&dir, &key, o);
                                }
                                self.update(|s| s.time_order = o);
                            }
                        }
                    }
                    // which columns point at which, from their values (Follow's suggestion)
                    if let Ok(l) = logseek_core::query::links::linked_columns(st, &self.cache, &no)
                    {
                        let l = Arc::new(l);
                        self.update(|s| s.links = l);
                    }
                }
            }
        }
        let idx = self.store.as_ref().map(|s| s.disk_bytes()).unwrap_or(0);
        let mem = self.store.as_ref().map(|s| s.memory_bytes()).unwrap_or(0);
        let src: Vec<String> = self
            .store
            .as_ref()
            .map(|st| {
                st.source_columns()
                    .into_iter()
                    .filter_map(|i| st.schema().columns.get(i).map(|c| c.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.update(|s| {
            s.phase = phase;
            s.index_bytes = idx;
            s.source_cols = src;
            s.mem_bytes = mem;
        });
    }

    fn run_query(&mut self, q: String) {
        let Some(st) = &self.store else {
            self.query = q;
            return;
        };
        self.query = q.clone();
        self.update(|s| s.busy = true);
        let t = Instant::now();
        let trimmed = q.trim();
        self.range = None;
        let result = if trimmed.is_empty() {
            Ok(MatchSet::all(st))
        } else {
            match parse(trimmed)
                .map_err(|e| e.msg)
                .and_then(|p| p.rows_only())
            {
                Ok(parsed) => {
                    self.range = parsed.time;
                    filter_with(st, &parsed, &self.fs, &self.cache, &AtomicBool::new(false))
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e),
            }
        };
        let secs = t.elapsed().as_secs_f64();
        let mut names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
        names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        let types: Vec<logseek_core::types::ColType> =
            st.schema().columns.iter().map(|c| c.ty).collect();
        let mut counts: Vec<u64> = st.schema().columns.iter().map(|c| c.count).collect();
        // virtual columns: coverage is shown from their value lists, not known up front
        counts.extend(self.fs.items.iter().map(|_| 0));
        let time_col = st.schema().time_col;
        let total = st.row_count();
        let result = match (result, &self.only) {
            (Ok(m), Some(rows)) => Ok(m.restrict(st, rows)),
            (r, _) => r,
        };
        let machine = self.machine_columns();
        match result {
            Ok(m) => {
                let matched = m.count();
                self.matches = Some(m);
                let sort_error = self.rebuild_order().err();
                let sort_now = self.sort_spec.clone().filter(|_| self.order.is_some());
                let on_disk = self.order.as_ref().is_some_and(|o| o.on_disk());
                self.update(|s| {
                    s.sort = sort_now;
                    s.sort_on_disk = on_disk;
                    s.sort_error = sort_error;
                });
                self.levels.clear();
                self.leaves.clear();
                self.searched = None;
                self.update(|s| {
                    s.group_levels.clear();
                    s.group_pages.clear();
                    s.group_rows.clear();
                    s.columns = names;
                    s.col_types = types;
                    s.col_machine = machine;
                    s.col_counts = counts;
                    s.time_col = time_col;
                    s.total_rows = total;
                    s.matched = matched;
                    s.generation += 1;
                    s.query = q.clone();
                    s.query_error = None;
                    s.query_secs = secs;
                    s.page = None;
                    s.busy = false;
                });
                self.serve_page(0, 80);
                // built when no rows are waiting to be shown (see run)
                self.hist_due = true;
            }
            Err(e) => self.update(|s| {
                s.columns = names;
                s.total_rows = total;
                // the error belongs to this query: never leave the previous rows
                // looking like its answer
                s.query = q.clone();
                s.query_error = Some(e);
                s.matched = 0;
                s.page = None;
                s.busy = false;
            }),
        }
    }

    fn generation(&self) -> u64 {
        self.snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }

    /// The current result narrowed to `path` (column = value at each level).
    fn narrowed(&self, path: &[(String, Option<String>)]) -> Result<MatchSet, String> {
        let st = self.store.as_ref().ok_or("no file is open")?;
        let mut q = self.query.trim().to_string();
        for (c, v) in path {
            q.push(' ');
            q.push_str(&logseek_core::query::group::narrow_term(c, v.as_deref()));
        }
        let q = q.trim();
        let m = if q.is_empty() {
            MatchSet::all(st)
        } else {
            let parsed = parse(q).map_err(|e| e.msg).and_then(|p| p.rows_only())?;
            filter_with(st, &parsed, &self.fs, &self.cache, &AtomicBool::new(false))
                .map_err(|e| e.to_string())?
        };
        Ok(match &self.only {
            Some(rows) => m.restrict(st, rows),
            None => m,
        })
    }

    fn serve_group_level(
        &mut self,
        key: u64,
        path: &[(String, Option<String>)],
        by: &str,
        sort: GroupSort,
    ) {
        let generation = self.generation();
        let built = self.narrowed(path).and_then(|m| {
            let st = self.store.as_ref().ok_or("no file is open")?;
            GroupLevel::build(st, &m, by, sort, &self.cache, &AtomicBool::new(false))
                .map_err(|e| e.to_string())
        });
        let info = match built {
            Ok(l) => {
                let groups = l.len();
                self.levels.insert(key, l);
                GroupLevelInfo {
                    generation,
                    groups,
                }
            }
            Err(_) => GroupLevelInfo {
                generation,
                groups: 0,
            },
        };
        self.update(|s| {
            s.group_levels.insert(key, info);
        });
    }

    fn serve_group_page(&mut self, key: u64, start: u64, len: usize) {
        let Some(l) = self.levels.get(&key) else {
            return;
        };
        let page = Arc::new(GroupPage {
            generation: self.generation(),
            start,
            rows: l.page(start, len),
        });
        self.update(|s| {
            s.group_pages.insert(key, page);
        });
    }

    fn serve_group_rows(
        &mut self,
        key: u64,
        path: &[(String, Option<String>)],
        start: u64,
        len: usize,
    ) {
        if !self.leaves.contains_key(&key) {
            match self.narrowed(path) {
                Ok(m) => {
                    self.leaves.insert(key, m);
                }
                Err(_) => return,
            }
        }
        let (Some(st), Some(m)) = (&self.store, self.leaves.get(&key)) else {
            return;
        };
        let cols = self.all_cols(st);
        if let Ok(p) = page_with(st, m, start, len, &cols, &self.fs, &self.cache) {
            let data = Arc::new(PageData {
                generation: self.generation(),
                start,
                rows: p.rows,
                cells: p.cells,
                errors: p.errors,
            });
            self.update(|s| {
                s.group_rows.insert(key, data);
            });
        }
    }

    /// Sort the current result by `sort_spec` (nothing to do without one).
    fn rebuild_order(&mut self) -> Result<(), String> {
        self.order = None;
        let (Some(st), Some(m), Some((name, desc))) =
            (&self.store, &self.matches, self.sort_spec.clone())
        else {
            return Ok(());
        };
        let col = logseek_core::query::exec::resolve_column_f(st, &self.fs, &name)
            .map_err(|e| e.to_string())?;
        // a made column has no type: numbers sort as numbers, the rest as text
        let ty = st
            .schema()
            .columns
            .get(col)
            .map_or(logseek_core::types::ColType::Float, |c| c.ty);
        let scratch = st.dir().join("_tmp");
        let run_bytes = (self.budget.cap() / 8) as usize;
        let mut sorter = logseek_core::query::sort::Sorter::new(&scratch, run_bytes, desc);
        let total = m.count();
        let mut start = 0u64;
        while start < total {
            let p = page_with(st, m, start, 20_000, &[col], &self.fs, &self.cache)
                .map_err(|e| e.to_string())?;
            if p.rows.is_empty() {
                break;
            }
            for (row, cells) in p.rows.iter().zip(&p.cells) {
                let v = cells.first().and_then(|v| v.as_deref());
                sorter
                    .push(logseek_core::query::sort::key_for(ty, v), *row)
                    .map_err(|e| format!("the sort could not use its scratch space: {e}"))?;
            }
            start += p.rows.len() as u64;
        }
        self.order = Some(
            sorter
                .finish()
                .map_err(|e| format!("the sort could not use its scratch space: {e}"))?,
        );
        Ok(())
    }

    fn serve_page(&mut self, start: u64, len: usize) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let cols = self.all_cols(st);
        if let Some(order) = &self.order {
            // sorted: these places' rows, fetched by id and put back in order
            let ids = order.page(start, len);
            let set = MatchSet::all(st).restrict(st, &ids);
            if let Ok(p) = page_with(st, &set, 0, ids.len(), &cols, &self.fs, &self.cache) {
                let at: std::collections::HashMap<u64, usize> =
                    p.rows.iter().enumerate().map(|(i, r)| (*r, i)).collect();
                let mut cells = p.cells;
                let mut out_rows = Vec::with_capacity(ids.len());
                let mut out_cells = Vec::with_capacity(ids.len());
                // where each fetched row lands in the sorted page
                let mut moved = vec![usize::MAX; cells.len()];
                for id in ids {
                    if let Some(&i) = at.get(&id) {
                        moved[i] = out_rows.len();
                        out_rows.push(id);
                        out_cells.push(std::mem::take(&mut cells[i]));
                    }
                }
                let errors = p
                    .errors
                    .into_iter()
                    .filter_map(|(r, c, why)| {
                        let r = *moved.get(r)?;
                        (r != usize::MAX).then_some((r, c, why))
                    })
                    .collect();
                let data = Arc::new(PageData {
                    generation: self.generation(),
                    start,
                    rows: out_rows,
                    cells: out_cells,
                    errors,
                });
                self.update(|s| s.page = Some(data));
            }
            return;
        }
        if let Ok(p) = page_with(st, m, start, len, &cols, &self.fs, &self.cache) {
            let generation = self
                .snap
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation;
            let data = Arc::new(PageData {
                generation,
                start,
                rows: p.rows,
                cells: p.cells,
                errors: p.errors,
            });
            self.update(|s| s.page = Some(data));
        }
    }

    fn serve_record(&mut self, row: u64) {
        let Some(st) = &self.store else { return };
        let (fields, error) = match record_fields(st, &self.fs, &self.cache, row) {
            Ok(f) => (f, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let raw = st
            .raw_line(row)
            .map(|b| {
                let cut = &b[..b.len().min(256 * 1024)];
                String::from_utf8_lossy(cut)
                    .trim_end_matches(char::is_control)
                    .to_string()
            })
            .unwrap_or_default();
        let rec = Arc::new(Record {
            row,
            fields,
            raw,
            error,
        });
        self.update(|s| s.record = Some(rec));
    }

    fn serve_field(&mut self, col: usize, rare: bool) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        if let Some(f) = summarize_any(st, m, col, 10, rare, &self.fs, &self.cache) {
            let generation = self
                .snap
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation;
            let f = Arc::new(FieldSummary { generation, ..f });
            self.update(|s| s.field = Some(f));
        }
    }

    fn serve_values(&mut self, col: usize, query: String, n: usize, rare: bool, alpha: bool) {
        let Some(st) = &self.store else { return };
        let q = query.trim();
        let m = if q.is_empty() {
            MatchSet::all(st)
        } else {
            match parse(q).map_err(|e| e.msg).and_then(|p| {
                filter_with(st, &p, &self.fs, &self.cache, &AtomicBool::new(false))
                    .map_err(|e| e.to_string())
            }) {
                Ok(m) => m,
                Err(_) => return,
            }
        };
        // every value, counted exactly, kept for the list to page through
        let sort = if alpha {
            GroupSort::Name
        } else if rare {
            GroupSort::FewestRows
        } else {
            GroupSort::MostRows
        };
        let ns = st.schema().len();
        let level = if col < ns {
            let name = st.schema().columns[col].name.clone();
            GroupLevel::build(st, &m, &name, sort, &self.cache, &AtomicBool::new(false)).ok()
        } else {
            let scratch = st.dir().join("_tmp");
            let _ = std::fs::create_dir_all(&scratch);
            let total = m.count();
            logseek_core::query::agg::count_values(
                "value",
                &scratch,
                &self.cache,
                &AtomicBool::new(false),
                |each| {
                    let mut start = 0u64;
                    while start < total {
                        let p = page_with(st, &m, start, 20_000, &[col], &self.fs, &self.cache)?;
                        if p.rows.is_empty() {
                            break;
                        }
                        // a value that could not be worked out is neither a value nor empty
                        let bad: std::collections::HashSet<usize> =
                            p.errors.iter().map(|e| e.0).collect();
                        for (i, cells) in p.cells.iter().enumerate() {
                            if bad.contains(&i) {
                                continue;
                            }
                            each(
                                cells
                                    .first()
                                    .and_then(|v| v.as_deref())
                                    .filter(|v| !v.is_empty()),
                            );
                        }
                        start += p.rows.len() as u64;
                    }
                    Ok(())
                },
            )
            .ok()
            .map(|r| GroupLevel::from_stats(r, sort))
        };
        let distinct = level.as_ref().map(|l| l.with_value());
        let top: Option<Vec<(String, u64)>> = level.as_ref().map(|l| {
            l.page(0, n)
                .into_iter()
                .map(|g| (g.value.unwrap_or_default(), g.rows))
                .collect()
        });
        if let Some(l) = level {
            self.value_level = Some((col, sort, String::new(), l));
        }
        if let Some(f) = summarize_any(st, &m, col, n, rare, &self.fs, &self.cache) {
            let f = FieldSummary {
                distinct: distinct.unwrap_or(f.distinct),
                top: top.unwrap_or(f.top),
                ..f
            };
            let generation = self
                .snap
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation;
            let f = Arc::new(FieldSummary {
                generation,
                query,
                ..f
            });
            self.update(|s| s.values = Some(f));
        }
    }

    fn serve_values_page(&mut self, col: usize, search: String, start: u64, len: usize) {
        let Some((c, sort, s, l)) = self.value_level.as_mut() else {
            return;
        };
        let sort = *sort;
        if *c != col {
            return;
        }
        if *s != search {
            l.search(&search);
            *s = search.clone();
        }
        let page = Arc::new(ValuesPage {
            col,
            sort,
            search,
            start,
            total: l.len(),
            rows: l
                .page(start, len)
                .into_iter()
                .map(|g| (g.value.unwrap_or_default(), g.rows))
                .collect(),
        });
        self.update(|s| s.values_page = Some(page));
    }

    fn serve_search(&mut self, id: u64, text: String, case: bool, word: bool, col: Option<String>) {
        use logseek_core::query::search::{search_all, SearchOpts};
        let (Some(st), Some(_)) = (&self.store, &self.matches) else {
            return;
        };
        let cols = self.all_cols(st);
        let mut names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
        names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        let opts = SearchOpts {
            text: text.trim().to_string(),
            case,
            word,
            col: col.as_ref().and_then(|c| names.iter().position(|n| n == c)),
        };
        // rows of the current result that have the text somewhere, then cell by cell
        let q = format!(
            "{} {}",
            self.query.trim(),
            logseek_core::formula::quote(&opts.text)
        );
        let found = parse(q.trim())
            .map_err(|e| e.msg)
            .and_then(|p| p.rows_only())
            .and_then(|p| {
                filter_with(st, &p, &self.fs, &self.cache, &AtomicBool::new(false))
                    .map_err(|e| e.to_string())
            })
            .map(|h| match &self.only {
                Some(rows) => h.restrict(st, rows),
                None => h,
            })
            .and_then(|h| {
                search_all(
                    st,
                    &h,
                    &cols,
                    &self.fs,
                    &self.cache,
                    &opts,
                    &AtomicBool::new(false),
                )
                .map_err(|e| e.to_string())
            });
        let out = match found {
            Ok(r) => {
                let mut per_col: Vec<(String, u64)> = names
                    .iter()
                    .cloned()
                    .zip(r.per_col.iter().copied())
                    .filter(|(_, n)| *n > 0)
                    .collect();
                per_col.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let out = SearchOut {
                    id,
                    rows: r.rows.count(),
                    cells: r.cells,
                    per_col,
                    error: None,
                };
                self.searched = Some((id, opts, r.rows));
                out
            }
            Err(e) => SearchOut {
                id,
                rows: 0,
                cells: 0,
                per_col: Vec::new(),
                error: Some(e),
            },
        };
        let out = Arc::new(out);
        self.update(|s| {
            s.search = Some(out);
            s.search_page = None;
        });
        self.serve_search_page(id, 0, 100);
    }

    fn serve_list(
        &mut self,
        id: u64,
        terms: Arc<Vec<String>>,
        case: bool,
        word: bool,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        use logseek_core::query::multi::{search_list, ListOpts};
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        // the rows on screen: the current search, and the tagged or followed rows if showing
        let cand = match &self.only {
            Some(rows) => m.restrict(st, rows),
            None => m.clone(),
        };
        let cols = self.all_cols(st);
        let mut names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
        names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        let of = cand.count();
        let res = search_list(
            st,
            &cand,
            &cols,
            &self.fs,
            &self.cache,
            &terms,
            &ListOpts { case, word },
            &stop,
            &done,
        );
        let out = match res {
            Ok(h) => {
                let rows_any = h.rows.count();
                let terms = h
                    .terms
                    .into_iter()
                    .map(|t| ListTerm {
                        rows: t.rows,
                        cells: t.cells,
                        cols: t
                            .cols
                            .into_iter()
                            .map(|(c, n)| (names.get(c).cloned().unwrap_or_default(), n))
                            .collect(),
                    })
                    .collect();
                let (columns_read, columns_skipped) = (h.columns_read, h.columns_skipped);
                self.list_found = Some((id, h.rows));
                ListOut {
                    id,
                    terms,
                    rows_any,
                    of,
                    columns_read,
                    columns_skipped,
                    error: None,
                }
            }
            Err(logseek_core::Error::Cancelled) => ListOut {
                id,
                terms: Vec::new(),
                rows_any: 0,
                of,
                columns_read: 0,
                columns_skipped: 0,
                error: Some("stopped".into()),
            },
            Err(e) => ListOut {
                id,
                terms: Vec::new(),
                rows_any: 0,
                of,
                columns_read: 0,
                columns_skipped: 0,
                error: Some(e.to_string()),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.list = Some(out));
    }

    fn serve_lexicon(&mut self) {
        let Some(st) = &self.store else { return };
        // stopped by a request for rows (the screen first): read again after
        match logseek_core::query::plain::build_lexicon(st, &self.fs, &self.cache, &self.interrupt)
        {
            Ok(lex) => {
                let lex = Arc::new(lex);
                self.update(|s| s.lexicon = Some(lex));
            }
            Err(logseek_core::Error::Cancelled) => self.deferred.push(Cmd::Lexicon),
            Err(_) => {}
        }
    }

    fn serve_signals(
        &mut self,
        id: u64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::rhythm::timeline_signals(st, m, &self.cache, &stop, &done);
        let secs = t.elapsed().as_secs_f64();
        let out = match res {
            Ok(s) => SignalsOut {
                id,
                gaps: s.gaps,
                usual_pause: s.usual_pause,
                gap_line: s.gap_line,
                steps: s.steps,
                step_width: s.step_width,
                secs,
                error: None,
            },
            Err(e) => SignalsOut {
                id,
                gaps: Vec::new(),
                usual_pause: 0,
                gap_line: 0,
                steps: Vec::new(),
                step_width: 0,
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "Stopped before the end.".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.signals = Some(out));
    }

    fn serve_regular(
        &mut self,
        id: u64,
        col: String,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::exec::resolve_column_f(st, &self.fs, &col).and_then(|c| {
            logseek_core::query::rhythm::regular(st, m, c, &self.fs, &self.cache, &stop, &done)
        });
        let secs = t.elapsed().as_secs_f64();
        let out = match res {
            Ok(r) => RegularOut {
                id,
                col,
                values: r.values,
                too_few: r.too_few,
                secs,
                error: None,
            },
            Err(e) => RegularOut {
                id,
                col,
                values: Vec::new(),
                too_few: 0,
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "Stopped before the end.".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.regular = Some(out));
    }

    fn serve_pairs(
        &mut self,
        id: u64,
        a: String,
        b: String,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::exec::resolve_column_f(st, &self.fs, &a).and_then(|ca| {
            let cb = logseek_core::query::exec::resolve_column_f(st, &self.fs, &b)?;
            logseek_core::query::pairs::rare_pairs(
                st,
                m,
                ca,
                cb,
                &self.fs,
                &self.cache,
                &stop,
                &done,
            )
        });
        let secs = t.elapsed().as_secs_f64();
        let total = m.count();
        let out = match res {
            Ok(p) => PairsOut {
                id,
                a,
                b,
                unusual: p.unusual,
                peers: p.peers,
                a_values: p.a_values,
                total: p.total,
                secs,
                error: None,
            },
            Err(e) => PairsOut {
                id,
                a,
                b,
                unusual: Vec::new(),
                peers: Vec::new(),
                a_values: 0,
                total,
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.pairs = Some(out));
    }

    fn serve_unusual(
        &mut self,
        id: u64,
        together: bool,
        line: Option<f64>,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let cols = self.all_cols(st);
        let mut names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
        names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        let what = if together {
            logseek_core::query::unusual::Weigh::Together
        } else {
            logseek_core::query::unusual::Weigh::All
        };
        let res = logseek_core::query::unusual::unusual_by(
            st,
            m,
            &cols,
            line,
            what,
            &self.fs,
            &self.cache,
            &stop,
            &done,
        );
        let secs = t.elapsed().as_secs_f64();
        let total = m.count();
        let out = match res {
            Ok(u) => {
                let left: Vec<usize> = u.model.left_out();
                let quiet: Vec<String> = cols
                    .iter()
                    .filter(|&&c| {
                        Some(c) != st.schema().time_col
                            && !left.contains(&c)
                            && u.model.weight(c) == 0.0
                    })
                    .map(|&c| names[c].clone())
                    .collect();
                let left_out: Vec<String> = left
                    .iter()
                    .filter(|&&c| Some(c) != st.schema().time_col)
                    .map(|&c| names[c].clone())
                    .collect();
                let weighed = cols.iter().filter(|&&c| u.model.weight(c) > 0.0).count();
                UnusualOut {
                    id,
                    spread: u.spread,
                    usual: u.usual,
                    line: u.line,
                    rows: Arc::new(u.rows),
                    model: u.model,
                    total,
                    weighed,
                    left_out,
                    quiet,
                    secs,
                    error: None,
                }
            }
            Err(e) => UnusualOut {
                id,
                spread: Vec::new(),
                usual: 0.0,
                line: line.unwrap_or(0.0),
                rows: Arc::new(Vec::new()),
                model: Arc::new(logseek_core::query::unusual::Model::empty()),
                total,
                weighed: 0,
                left_out: Vec::new(),
                quiet: Vec::new(),
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.unusual = Some(out));
    }

    fn serve_patterns(
        &mut self,
        id: u64,
        col: String,
        alike: f64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::exec::resolve_column_f(st, &self.fs, &col).and_then(|c| {
            logseek_core::query::patterns::patterns(
                st,
                m,
                c,
                &logseek_core::query::patterns::PatternOpts { alike },
                &self.fs,
                &self.cache,
                &stop,
                &done,
            )
            .map(|p| (c, p))
        });
        let secs = t.elapsed().as_secs_f64();
        let total = m.count();
        let out = match res {
            Ok((c, p)) => {
                let o = PatternsOut {
                    id,
                    patterns: p.patterns.clone(),
                    empty: p.empty,
                    total,
                    secs,
                    error: None,
                };
                self.last_patterns = Some((id, c, p));
                o
            }
            Err(e) => PatternsOut {
                id,
                patterns: Vec::new(),
                empty: 0,
                total,
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.patterns = Some(out));
    }

    fn serve_pattern_rows(&mut self, id: u64, which: usize) {
        let (Some(st), Some(m), Some((pid, col, p))) =
            (&self.store, &self.matches, &self.last_patterns)
        else {
            return;
        };
        if *pid != id {
            return;
        }
        let Ok(ids) = logseek_core::query::patterns::pattern_rows(
            st,
            m,
            *col,
            p,
            which,
            &self.fs,
            &self.cache,
            &AtomicBool::new(false),
        ) else {
            return;
        };
        let ids = Arc::new(ids);
        self.update(|s| s.pattern_rows = Some((id, which, ids)));
    }

    fn serve_strange(
        &mut self,
        id: u64,
        col: String,
        line: Option<f64>,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::exec::resolve_column_f(st, &self.fs, &col).and_then(|c| {
            logseek_core::query::strange::strange_text(
                st,
                m,
                c,
                line,
                &self.fs,
                &self.cache,
                &stop,
                &done,
            )
        });
        let secs = t.elapsed().as_secs_f64();
        let out = match res {
            Ok(s) => StrangeOut {
                id,
                spread: s.spread,
                unscored: s.unscored,
                usual: s.usual,
                line: s.line,
                values: s.values,
                rows: Arc::new(s.rows),
                secs,
                error: None,
            },
            Err(e) => StrangeOut {
                id,
                spread: Vec::new(),
                unscored: 0,
                usual: 0.0,
                line: line.unwrap_or(0.0),
                values: Vec::new(),
                rows: Arc::new(Vec::new()),
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.strange = Some(out));
    }

    fn serve_shapes(
        &mut self,
        id: u64,
        col: String,
        how: logseek_core::query::shape::Shape,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let res = logseek_core::query::exec::resolve_column_f(st, &self.fs, &col).and_then(|c| {
            logseek_core::query::shape::column_shapes(
                st,
                m,
                c,
                how,
                &self.fs,
                &self.cache,
                &stop,
                &done,
            )
        });
        let secs = t.elapsed().as_secs_f64();
        let total = m.count();
        let out = match res {
            Ok((shapes, empty)) => ShapesOut {
                id,
                shapes,
                empty,
                total,
                secs,
                error: None,
            },
            Err(e) => ShapesOut {
                id,
                shapes: Vec::new(),
                empty: 0,
                total,
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.shapes = Some(out));
    }

    fn serve_contrast(
        &mut self,
        id: u64,
        against: Against,
        min_share: f64,
        min_ratio: f64,
        stop: Arc<AtomicBool>,
        done: Arc<std::sync::atomic::AtomicU64>,
    ) {
        use logseek_core::query::contrast::{contrast, ContrastOpts};
        let (Some(st), Some(here)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        // what the rows shown are compared with
        let mut used = against;
        let base = match against {
            Against::OtherTimes | Against::Before | Against::JustBefore if self.range.is_some() => {
                // the same search, its period swapped for the one compared with
                let swap = self.range.and_then(|r| baseline_range(against, r));
                let base = parse(self.query.trim())
                    .map_err(|e| e.msg)
                    .and_then(|p| p.rows_only())
                    .and_then(|mut p| {
                        p.time = swap;
                        filter_with(st, &p, &self.fs, &self.cache, &stop).map_err(|e| e.to_string())
                    });
                match (base, &self.only) {
                    (Ok(m), Some(rows)) => Ok(m.restrict(st, rows)),
                    (r, _) => r,
                }
            }
            _ => {
                used = Against::RestOfFile;
                Ok(MatchSet::all(st))
            }
        };
        let mut names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
        names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        // every column but the timeline: times differ on every row and
        // the period itself is what is being explained
        let cols: Vec<usize> = self
            .all_cols(st)
            .into_iter()
            .filter(|&c| Some(c) != st.schema().time_col)
            .collect();
        // compared with what came before: what is new in the period, too
        let ask_new = matches!(used, Against::Before | Against::JustBefore);
        let mut new: Vec<NewCol> = Vec::new();
        let res = base.map_err(logseek_core::Error::Invalid).and_then(|base| {
            let rest = base.minus(here);
            if ask_new {
                let nv = logseek_core::query::newin::new_values(
                    st,
                    here,
                    &rest,
                    &cols,
                    &self.fs,
                    &self.cache,
                    &stop,
                    &done,
                )?;
                new = nv
                    .columns
                    .into_iter()
                    .map(|k| NewCol {
                        name: names[k.col].clone(),
                        filled: k.filled,
                        new_rows: k.new_rows,
                        before_values: k.before_values,
                        before_filled: k.before_filled,
                        values: k.values.into_iter().map(|v| (v.value, v.rows)).collect(),
                    })
                    .collect();
            }
            contrast(
                st,
                here,
                &rest,
                &cols,
                &self.fs,
                &self.cache,
                &ContrastOpts {
                    min_share,
                    min_ratio,
                },
                &stop,
                &done,
            )
        });
        let secs = t.elapsed().as_secs_f64();
        let out = match res {
            Ok(c) => ContrastOut {
                id,
                against: used,
                new_asked: ask_new,
                new: std::mem::take(&mut new),
                lines: c
                    .findings
                    .into_iter()
                    .map(|f| ContrastLine {
                        conds: f
                            .conds
                            .into_iter()
                            .map(|k| (names.get(k.col).cloned().unwrap_or_default(), k.value))
                            .collect(),
                        instead: f.instead.map(|(k, v)| {
                            (
                                k,
                                v.into_iter()
                                    .map(|c| {
                                        (names.get(c.col).cloned().unwrap_or_default(), c.value)
                                    })
                                    .collect(),
                            )
                        }),
                        same: f
                            .same
                            .into_iter()
                            .map(|v| {
                                v.into_iter()
                                    .map(|k| {
                                        (names.get(k.col).cloned().unwrap_or_default(), k.value)
                                    })
                                    .collect()
                            })
                            .collect(),
                        here: f.here,
                        rest: f.rest,
                    })
                    .collect(),
                here_total: c.here_total,
                rest_total: c.rest_total,
                columns: c.columns,
                secs,
                error: None,
            },
            Err(e) => ContrastOut {
                new_asked: false,
                new: Vec::new(),
                id,
                against: used,
                lines: Vec::new(),
                here_total: here.count(),
                rest_total: 0,
                columns: cols.len(),
                secs,
                error: Some(match e {
                    logseek_core::Error::Cancelled => "stopped".into(),
                    e => e.to_string(),
                }),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.contrast = Some(out));
    }

    fn serve_list_rows(&mut self, id: u64) {
        let (Some(st), Some((lid, rows))) = (&self.store, &self.list_found) else {
            return;
        };
        if *lid != id {
            return;
        }
        let total = rows.count();
        let mut ids: Vec<u64> = Vec::with_capacity(total as usize);
        let mut start = 0u64;
        while start < total {
            let Ok(p) = page_with(st, rows, start, 100_000, &[], &self.fs, &self.cache) else {
                return;
            };
            if p.rows.is_empty() {
                break;
            }
            start += p.rows.len() as u64;
            ids.extend(p.rows);
        }
        let ids = Arc::new(ids);
        self.update(|s| s.list_rows = Some((id, ids)));
    }

    fn serve_search_page(&mut self, id: u64, start: u64, len: usize) {
        use logseek_core::query::search::cell_spans;
        let (Some(st), Some((sid, opts, rows))) = (&self.store, &self.searched) else {
            return;
        };
        if *sid != id {
            return;
        }
        let cols = self.all_cols(st);
        let Ok(p) = page_with(st, rows, start, len, &cols, &self.fs, &self.cache) else {
            return;
        };
        let tc = st.schema().time_col;
        let out: Vec<SearchRow> = p
            .rows
            .iter()
            .zip(p.cells)
            .map(|(row, cells)| SearchRow {
                row: *row,
                time: tc.and_then(|t| cells.get(t).cloned().flatten()),
                cells: cells
                    .into_iter()
                    .enumerate()
                    .filter(|(k, _)| opts.col.is_none_or(|c| c == *k))
                    .filter_map(|(k, v)| v.map(|v| (k, v)))
                    .filter(|(_, v)| !cell_spans(v, opts).is_empty())
                    .collect(),
            })
            .collect();
        let page = Arc::new(SearchPageOut {
            id,
            start,
            rows: out,
        });
        self.update(|s| s.search_page = Some(page));
    }

    /// The cells of the rows at places `start..start+len` of the list as it
    /// is shown (sorted or not), for `cols`.
    fn cells_at(&self, start: u64, len: usize, cols: &[usize]) -> Option<Vec<Vec<Option<String>>>> {
        let (st, m) = (self.store.as_ref()?, self.matches.as_ref()?);
        match &self.order {
            None => page_with(st, m, start, len, cols, &self.fs, &self.cache)
                .ok()
                .map(|p| p.cells),
            Some(order) => {
                let ids = order.page(start, len);
                let set = MatchSet::all(st).restrict(st, &ids);
                let p = page_with(st, &set, 0, ids.len(), cols, &self.fs, &self.cache).ok()?;
                let at: std::collections::HashMap<u64, usize> =
                    p.rows.iter().enumerate().map(|(i, r)| (*r, i)).collect();
                let mut cells = p.cells;
                Some(
                    ids.iter()
                        .filter_map(|id| at.get(id).map(|&i| std::mem::take(&mut cells[i])))
                        .collect(),
                )
            }
        }
    }

    fn serve_copy(&mut self, id: u64, ranges: Vec<(u64, u64)>, cols: Vec<usize>, names: bool) {
        let Some(st) = &self.store else { return };
        // the clipboard is memory: stop before the text outgrows a share of the budget
        let limit = ((self.budget.cap() / 4) as usize).max(64 << 20);
        let mut all_names: Vec<String> =
            st.schema().columns.iter().map(|c| c.name.clone()).collect();
        all_names.extend(self.fs.items.iter().map(|(n, _)| n.clone()));
        let mut text = String::new();
        if names {
            let head: Vec<&str> = cols
                .iter()
                .map(|&c| all_names.get(c).map_or("", String::as_str))
                .collect();
            text.push_str(&head.join("\t"));
            text.push('\n');
        }
        let mut rows = 0u64;
        let mut error = None;
        'all: for (a, b) in ranges {
            let mut at = a;
            while at <= b {
                let page = logseek_core::query::exec::rows_per_page(cols.len()) as u64;
                let len = (b - at + 1).min(page) as usize;
                let Some(cells) = self.cells_at(at, len, &cols) else {
                    error = Some("the rows could not be read".to_string());
                    break 'all;
                };
                if cells.is_empty() {
                    break;
                }
                for row in &cells {
                    for (k, v) in row.iter().enumerate() {
                        if k > 0 {
                            text.push('\t');
                        }
                        if let Some(v) = v {
                            // rows copied as a table are for a spreadsheet: a value it
                            // would run as a formula is marked as text, as file export does
                            let v = crate::export::safe_cell(v);
                            // a tab or line break inside a value would break the table
                            text.extend(v.chars().map(|c| {
                                if matches!(c, '\t' | '\n' | '\r') {
                                    ' '
                                } else {
                                    c
                                }
                            }));
                        }
                    }
                    text.push('\n');
                    rows += 1;
                }
                if text.len() > limit {
                    error = Some(format!(
                        "that is more than {} MB of text, too much for the clipboard. Use Export rows for a file",
                        limit >> 20
                    ));
                    break 'all;
                }
                at += cells.len() as u64;
            }
        }
        let out = Arc::new(CopyOut {
            id,
            rows,
            text: error.is_none().then_some(text),
            error,
        });
        self.update(|s| s.copied = Some(out));
    }

    /// The place of each row in the current sort (or in time, when the rows
    /// are not sorted and the file has a time column).
    fn serve_row_keys(&mut self, id: u64, rows: &[u64], by: Option<(String, bool)>) {
        let Some(st) = &self.store else { return };
        let (col, desc) = match &by {
            Some((name, desc)) => (
                logseek_core::query::exec::resolve_column_f(st, &self.fs, name).ok(),
                *desc,
            ),
            None => (st.schema().time_col, false),
        };
        let mut values: Vec<(u64, Option<String>)> = Vec::with_capacity(rows.len());
        if let Some(col) = col {
            let set = MatchSet::all(st).restrict(st, rows);
            let total = set.count();
            let mut start = 0u64;
            while start < total {
                let Ok(p) = page_with(st, &set, start, 20_000, &[col], &self.fs, &self.cache)
                else {
                    break;
                };
                if p.rows.is_empty() {
                    break;
                }
                for (row, cells) in p.rows.iter().zip(p.cells) {
                    values.push((*row, cells.into_iter().next().flatten()));
                }
                start += p.rows.len() as u64;
            }
        }
        let keys = Arc::new(crate::followtree::rank_keys(values, desc));
        self.update(|s| s.row_keys = Some((id, keys)));
    }

    fn serve_locate(&mut self, id: u64, row: u64) {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let segs = st.segments();
        let si = segs.partition_point(|s| s.meta().row_start + s.meta().rows as u64 <= row);
        let inside = segs.get(si).is_some_and(|s| {
            row >= s.meta().row_start && m.segs[si].contains((row - s.meta().row_start) as u32)
        });
        let pos = if !inside {
            None
        } else if let Some(order) = &self.order {
            // sorted: look for the row along the order
            let n = order.len();
            let mut at = 0u64;
            let mut found = None;
            while at < n && found.is_none() {
                let ids = order.page(at, 8192);
                if ids.is_empty() {
                    break;
                }
                found = ids.iter().position(|&r| r == row).map(|k| at + k as u64);
                at += ids.len() as u64;
            }
            found
        } else {
            Some(logseek_core::query::exec::rank_of(st, m, row))
        };
        self.update(|s| s.located = Some((id, pos)));
    }

    fn serve_find(
        &mut self,
        id: u64,
        text: String,
        after_row: Option<u64>,
        from_pos: Option<u64>,
        forward: bool,
    ) {
        use logseek_core::query::exec::{next_row, rank_of};
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        // rows of the current result that contain the text, anywhere
        let q = format!(
            "{} {}",
            self.query.trim(),
            logseek_core::formula::quote(text.trim())
        );
        let hits = parse(q.trim())
            .map_err(|e| e.msg)
            .and_then(|p| p.rows_only())
            .and_then(|p| {
                filter_with(st, &p, &self.fs, &self.cache, &AtomicBool::new(false))
                    .map_err(|e| e.to_string())
            })
            .map(|h| match &self.only {
                Some(rows) => h.restrict(st, rows),
                None => h,
            });
        let out = match hits {
            Err(e) => FindOut {
                id,
                text,
                total: 0,
                pos: None,
                index: None,
                error: Some(e),
            },
            Ok(h) => {
                let total = h.count();
                let (pos, index) = if total == 0 {
                    (None, None)
                } else if let Some(order) = &self.order {
                    // sorted: walk the order from here to the next row that has it
                    let segs = st.segments();
                    let has = |row: u64| {
                        let si = segs
                            .partition_point(|s| s.meta().row_start + s.meta().rows as u64 <= row);
                        segs.get(si).is_some_and(|s| {
                            row >= s.meta().row_start
                                && h.segs[si].contains((row - s.meta().row_start) as u32)
                        })
                    };
                    let n = order.len();
                    let mut found = None;
                    let mut p = match (from_pos, forward) {
                        (Some(p), true) => (p + 1) % n,
                        (Some(p), false) => (p + n - 1) % n,
                        (None, true) => 0,
                        (None, false) => n - 1,
                    };
                    let mut left = n;
                    while left > 0 && found.is_none() {
                        // read a block in the direction of travel
                        let len = left.min(4096);
                        let start = if forward {
                            p
                        } else {
                            (p + 1).saturating_sub(len)
                        };
                        let len = if forward {
                            len.min(n - start)
                        } else {
                            p + 1 - start
                        };
                        let ids = order.page(start, len as usize);
                        if forward {
                            found = ids.iter().position(|&r| has(r)).map(|k| start + k as u64);
                            p = (start + len) % n;
                        } else {
                            found = ids.iter().rposition(|&r| has(r)).map(|k| start + k as u64);
                            p = if start == 0 { n - 1 } else { start - 1 };
                        }
                        left -= len;
                    }
                    (found, None)
                } else {
                    match next_row(st, &h, after_row, forward) {
                        Some(row) => (Some(rank_of(st, m, row)), Some(rank_of(st, &h, row) + 1)),
                        None => (None, None),
                    }
                };
                FindOut {
                    id,
                    text,
                    total,
                    pos,
                    index,
                    error: None,
                }
            }
        };
        let out = Arc::new(out);
        self.update(|s| s.find = Some(out));
    }

    fn serve_rows_by_id(&mut self, key: u64, rows: Vec<u64>) {
        let Some(st) = &self.store else { return };
        let m = MatchSet::all(st).restrict(st, &rows);
        let cols = self.all_cols(st);
        let n = m.count() as usize;
        if let Ok(p) = page_with(st, &m, 0, n, &cols, &self.fs, &self.cache) {
            let data = Arc::new((
                key,
                PageData {
                    generation: self.generation(),
                    start: 0,
                    rows: p.rows,
                    cells: p.cells,
                    errors: p.errors,
                },
            ));
            self.update(|s| s.rows_by_id = Some(data));
        }
    }

    fn serve_follow(
        &mut self,
        id: u64,
        start: FollowStart,
        spec: logseek_core::query::follow::FollowSpec,
    ) {
        let Some(st) = &self.store else { return };
        let set = match &start {
            FollowStart::Rows(r) => Some(MatchSet::all(st).restrict(st, r)),
            FollowStart::Result => None,
        };
        let set_ref = match (&set, &self.matches) {
            (Some(s), _) => s,
            (None, Some(m)) => m,
            (None, None) => return,
        };
        // the starting rows themselves, for showing and counting
        let mut start_rows: Vec<u64> = Vec::new();
        for (si, sm) in set_ref.segs.iter().enumerate() {
            let meta = st.segments()[si].meta();
            start_rows.extend(
                logseek_core::query::agg::match_rows(sm, meta.rows)
                    .map(|r| meta.row_start + r as u64),
            );
        }
        let out = match logseek_core::query::follow::follow(
            st,
            set_ref,
            &spec,
            &self.cache,
            &AtomicBool::new(false),
        ) {
            Ok(r) => {
                let mut rows: Vec<u64> = start_rows.clone();
                rows.extend(r.levels.iter().flatten().map(|(row, _)| *row));
                rows.sort_unstable();
                rows.dedup();
                let times = row_times(st, &self.fs, &self.cache, &rows);
                FollowOut {
                    id,
                    start_rows: Arc::new(start_rows),
                    levels: r.levels,
                    times: Arc::new(times),
                    error: None,
                }
            }
            Err(e) => FollowOut {
                id,
                start_rows: Arc::new(start_rows),
                levels: Vec::new(),
                times: Default::default(),
                error: Some(e.to_string()),
            },
        };
        let out = Arc::new(out);
        self.update(|s| s.follow = Some(out));
    }

    fn serve_summary_page(
        &mut self,
        table: u64,
        sort: logseek_core::query::group::SummarySort,
        start: u64,
        len: usize,
    ) {
        let Some((id, _, cur, t)) = self.summary_table.as_mut() else {
            return;
        };
        if *id != table {
            return;
        }
        if *cur != sort {
            t.sort(sort);
            *cur = sort;
        }
        let rows = t
            .page(start, len)
            .into_iter()
            .map(|l| crate::summary::SummaryRow {
                key: l.key,
                value: l.value,
                rows: l.rows,
            })
            .collect();
        let page = Arc::new(SummaryPage {
            table,
            sort,
            start,
            rows,
        });
        self.update(|s| s.summary_page = Some(page));
    }

    fn serve_summary_copy(
        &mut self,
        table: u64,
        sort: logseek_core::query::group::SummarySort,
        heads: Vec<String>,
        is_time: bool,
    ) {
        let Some((id, spec, cur, t)) = self.summary_table.as_mut() else {
            return;
        };
        if *id != table {
            return;
        }
        if *cur != sort {
            t.sort(sort);
            *cur = sort;
        }
        let rows: Vec<crate::summary::SummaryRow> = t
            .page(0, t.len() as usize)
            .into_iter()
            .map(|l| crate::summary::SummaryRow {
                key: l.key,
                value: l.value,
                rows: l.rows,
            })
            .collect();
        let text =
            crate::summary::to_tsv(&heads, spec.measure, spec.column.as_deref(), &rows, is_time);
        let copy = Arc::new((table, text));
        self.update(|s| s.summary_copy = Some(copy));
    }

    /// Recompile the virtual columns against the open store (after open / reload / edits).
    fn rebuild_formulas(&mut self) {
        self.fs = FormulaSet::default();
        let mut problems = Vec::new();
        if let Some(st) = &self.store {
            // made columns may use one another; each that cannot be made says why
            let (fs, errs) = FormulaSet::from_defs(st, &self.vcols);
            self.fs = fs;
            problems = errs.into_iter().map(|(n, e)| (n, e.msg)).collect();
        }
        // the columns shown: those made, in the engine's order (made columns
        // are numbered after the stored ones in this order)
        let working: Vec<(String, String)> = self
            .fs
            .items
            .iter()
            .filter_map(|(n, _)| self.vcols.iter().find(|(v, _)| v == n).cloned())
            .collect();
        let saved = self.vcols.clone();
        self.update(|s| {
            s.column_problems = problems;
            s.virtual_cols = working;
            s.saved_cols = saved;
        });
    }

    fn all_cols(&self, st: &Store) -> Vec<usize> {
        (0..st.schema().len() + self.fs.items.len()).collect()
    }

    /// Count a query's rows; false when a request for rows stopped it.
    /// Each tagged row's time, for the marks on the time spine: each block's
    /// time column decoded once, whatever the number of tags in it.
    fn place_tags(&mut self) {
        let times = match (
            &self.store,
            self.store.as_ref().and_then(|s| s.schema().time_col),
        ) {
            (Some(st), Some(tc)) => {
                let mut by_block: std::collections::BTreeMap<usize, Vec<(u32, u16)>> =
                    Default::default();
                for (&row, &mask) in self.tag_map.iter() {
                    if let Some((si, r)) = st.locate(row) {
                        by_block.entry(si).or_default().push((r as u32, mask));
                    }
                }
                let mut out = Vec::new();
                for (si, rows) in by_block {
                    let Ok(Some(col)) = self.cache.get(st, si, tc as u32) else {
                        continue;
                    };
                    out.extend(tag_times(&rows, |r| col.int(r as usize)));
                }
                out.sort_unstable();
                out
            }
            _ => Vec::new(),
        };
        let times = Arc::new(times);
        self.update(|s| s.tag_times = times);
    }

    /// Which stored text columns hold machine-written values, judged once per
    /// index from up to 400 values of each column's first block.
    fn machine_columns(&mut self) -> Vec<bool> {
        let Some(st) = &self.store else {
            return Vec::new();
        };
        let n = st.schema().columns.len();
        if self.machine.0 == n && self.machine.1.len() == n {
            return self.machine.1.clone();
        }
        if st.segments().is_empty() {
            return vec![false; n];
        }
        let flags: Vec<bool> = (0..n)
            .map(|c| match self.cache.get(st, 0, c as u32) {
                Ok(Some(col)) => {
                    let rows = st.segments()[0].meta().rows.min(400) as usize;
                    let texts: Vec<String> = (0..rows)
                        .filter_map(|r| col.text(r).map(|t| t.into_owned()))
                        .collect();
                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                    crate::theme::machine_written(&refs)
                }
                _ => false,
            })
            .collect();
        self.machine = (n, flags.clone());
        flags
    }

    fn serve_count(&mut self, query: String) -> bool {
        let Some(st) = &self.store else { return true };
        let q = query.trim();
        let n = if q.is_empty() {
            Some(st.row_count())
        } else {
            let Ok(p) = parse(q) else { return true };
            match filter_with(st, &p, &self.fs, &self.cache, &self.interrupt) {
                Ok(m) => Some(match &self.only {
                    Some(rows) => m.restrict(st, rows).count(),
                    None => m.count(),
                }),
                Err(logseek_core::Error::Cancelled) => return false,
                Err(_) => None,
            }
        };
        if let Some(n) = n {
            self.update(|s| s.preview = Some((query, n)));
        }
        true
    }

    /// Streams the current result to `path` in pages of 10,000 rows (memory
    /// stays flat), writing to `<path>.part` and renaming at the end, so a
    /// cancelled or failed export never leaves a half file under the real name.
    fn serve_export(&mut self, path: PathBuf, format: crate::export::Format, cols: Vec<usize>) {
        use crate::export::{csv_line, jsonl_line, ExportStatus, Format, CANCEL};
        use std::io::Write;
        use std::sync::atomic::Ordering;
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        // the UI clears CANCEL when it asks for the export, so a Cancel pressed
        // while this waited in the queue still counts
        let names: Vec<String> = {
            let s = self.snap.lock().unwrap_or_else(|e| e.into_inner());
            cols.iter()
                .map(|&c| s.columns.get(c).cloned().unwrap_or_default())
                .collect()
        };
        let total = m.count();
        let mut status = ExportStatus {
            path: path.clone(),
            total,
            ..Default::default()
        };
        let st0 = status.clone();
        self.update(|s| s.export = Some(st0));
        let mut part = path.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let result = (|| -> Result<u64, String> {
            if format == Format::Xlsx {
                // a workbook: rows streamed to sheets in constant memory
                let mut out = crate::export::XlsxOut::new(&names, &part)?;
                let mut start = 0u64;
                let mut len = 10_000usize;
                while start < total {
                    if CANCEL.load(Ordering::SeqCst) {
                        return Err("cancelled".into());
                    }
                    let p = page_with(st, m, start, len, &cols, &self.fs, &self.cache)
                        .map_err(|e| e.to_string())?;
                    if p.rows.is_empty() {
                        break;
                    }
                    let mut bytes = 0u64;
                    for cells in &p.cells {
                        bytes += cells.iter().flatten().map(|v| v.len() as u64 + 1).sum::<u64>();
                        out.push(cells)?;
                    }
                    len = crate::export::next_page_len(bytes, p.rows.len() as u64);
                    start += p.rows.len() as u64;
                    let done = start;
                    self.update(|s| {
                        if let Some(e) = s.export.as_mut() {
                            e.done = done;
                        }
                    });
                }
                out.finish()?;
                std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
                return Ok(start);
            }
            let f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
            let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
            if format == Format::Csv {
                let head: Vec<Option<String>> = names.iter().map(|n| Some(n.clone())).collect();
                w.write_all(csv_line(&head).as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            let mut start = 0u64;
            let mut len = 10_000usize;
            while start < total {
                if CANCEL.load(Ordering::SeqCst) {
                    return Err("cancelled".into());
                }
                let p = page_with(st, m, start, len, &cols, &self.fs, &self.cache)
                    .map_err(|e| e.to_string())?;
                if p.rows.is_empty() {
                    break;
                }
                let mut bytes = 0u64;
                for cells in &p.cells {
                    let line = match format {
                        Format::Csv => csv_line(cells),
                        Format::Jsonl | Format::Xlsx => jsonl_line(&names, cells),
                    };
                    bytes += line.len() as u64;
                    w.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                }
                // size the next page by bytes, so wide rows never fill memory
                len = crate::export::next_page_len(bytes, p.rows.len() as u64);
                start += p.rows.len() as u64;
                let done = start;
                self.update(|s| {
                    if let Some(e) = s.export.as_mut() {
                        e.done = done;
                    }
                });
            }
            w.flush().map_err(|e| e.to_string())?;
            drop(w);
            std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
            Ok(start)
        })();
        match result {
            Ok(n) => {
                status.done = n;
                status.finished = true;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                if e == "cancelled" {
                    status.cancelled = true;
                } else {
                    status.error = Some(e);
                }
            }
        }
        self.update(|s| s.export = Some(status));
    }

    fn serve_summary(&mut self, spec: crate::summary::SummarySpec) {
        use crate::summary::{Measure, SummaryResult};
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return;
        };
        let t = Instant::now();
        let generation = self
            .snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation;
        let ns = st.schema().len();
        let resolve = |n: &str| logseek_core::query::exec::resolve_column_f(st, &self.fs, n);
        let by: Result<Vec<usize>, _> = spec.by.iter().map(|b| resolve(b)).collect();
        let col = spec
            .column
            .as_deref()
            .filter(|_| spec.measure.needs_column())
            .map(resolve)
            .transpose();
        let (by, col) = match (by, col) {
            (Ok(b), Ok(c)) => (b, c),
            (Err(e), _) | (_, Err(e)) => {
                let r = SummaryResult {
                    spec: Some(spec),
                    generation,
                    error: Some(e.to_string()),
                    ..Default::default()
                };
                self.update(|s| s.summary = Some(Arc::new(r)));
                return;
            }
        };
        let fast =
            spec.bucket.is_none() && by.iter().all(|&c| c < ns) && col.is_none_or(|c| c < ns);
        let func = spec
            .column
            .as_ref()
            .filter(|_| spec.measure.needs_column())
            .map(|_| match spec.measure {
                Measure::Distinct => AggFn::Dc,
                Measure::Sum => AggFn::Sum,
                Measure::Avg => AggFn::Avg,
                Measure::Min => AggFn::Min,
                Measure::Max => AggFn::Max,
                Measure::Rows => AggFn::Count,
            });
        // every group, kept whole (spilled to disk when large): no limit
        let stats = if fast {
            let mut aggs = vec![Agg {
                func: AggFn::Count,
                col: None,
            }];
            if let (Some(f), Some(c)) = (func, spec.column.as_ref()) {
                aggs.push(Agg {
                    func: f,
                    col: Some(c.clone()),
                });
            }
            run_stats(st, m, &aggs, &spec.by, &self.cache, &AtomicBool::new(false))
                .map_err(|e| e.to_string())
        } else {
            // time buckets or made columns: read the rows once per pass
            let tc = st.schema().time_col.filter(|_| spec.bucket.is_some());
            let mut cols: Vec<usize> = Vec::new();
            if let Some(tc) = tc {
                cols.push(tc);
            }
            cols.extend(&by);
            if let Some(c) = col {
                cols.push(c);
            }
            let nkeys = by.len() + usize::from(tc.is_some());
            let total = m.count();
            let scratch = st.dir().join("_tmp");
            let _ = std::fs::create_dir_all(&scratch);
            let fs = &self.fs;
            let cache = &self.cache;
            let bucket = spec.bucket;
            logseek_core::query::agg::count_groups(
                nkeys,
                func,
                &scratch,
                cache,
                &crate::summary::CANCEL,
                |each| {
                    let mut start = 0u64;
                    let mut key = String::new();
                    while start < total {
                        let len = logseek_core::query::exec::rows_per_page(cols.len());
                        let p = page_with(st, m, start, len, &cols, fs, cache)?;
                        if p.rows.is_empty() {
                            break;
                        }
                        let bad: std::collections::HashSet<(usize, usize)> =
                            p.errors.iter().map(|e| (e.0, e.1)).collect();
                        for (i, cells) in p.cells.iter().enumerate() {
                            key.clear();
                            let mut k = 0;
                            if let (Some(_), Some(b)) = (tc, bucket) {
                                let us = cells
                                    .first()
                                    .and_then(|v| v.as_deref())
                                    .and_then(logseek_core::time::parse_time);
                                if let Some(u) = us {
                                    key.push_str(&logseek_core::types::fmt_time(
                                        u.div_euclid(b) * b,
                                    ));
                                }
                                k = 1;
                            }
                            for j in 0..by.len() {
                                if k + j > 0 {
                                    key.push(logseek_core::query::agg::KEY_SEP);
                                }
                                if bad.contains(&(i, k + j)) {
                                    key.push_str(crate::summary::ERROR_GROUP);
                                } else if let Some(Some(v)) = cells.get(k + j) {
                                    key.push_str(v);
                                }
                            }
                            let v = col.and_then(|_| cells.last().and_then(|v| v.as_deref()));
                            each(&key, v);
                        }
                        start += p.rows.len() as u64;
                    }
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())
        };
        let mut res = match stats {
            Ok(r) => {
                let table = logseek_core::query::group::SummaryTable::new(r);
                self.summary_ids += 1;
                let id = self.summary_ids;
                let first = table.page(0, 500);
                let res = SummaryResult {
                    rows: first
                        .into_iter()
                        .map(|l| crate::summary::SummaryRow {
                            key: l.key,
                            value: l.value,
                            rows: l.rows,
                        })
                        .collect(),
                    total_rows: m.count(),
                    groups: table.len(),
                    widths: table.widths().to_vec(),
                    table: id,
                    ..Default::default()
                };
                self.summary_table = Some((
                    id,
                    spec.clone(),
                    logseek_core::query::group::SummarySort::MostRows,
                    table,
                ));
                res
            }
            Err(e) => SummaryResult {
                error: Some(e),
                ..Default::default()
            },
        };
        let _ = &mut res;
        res.spec = Some(spec);
        res.generation = generation;
        res.secs = t.elapsed().as_secs_f64();
        let res = Arc::new(res);
        self.update(|s| s.summary = Some(res));
    }

    /// Per-hour (or per-day) counts for the calendar; false when a request
    /// for rows stopped it.
    fn serve_calendar(&mut self, query: String) -> bool {
        let Some(st) = &self.store else { return true };
        if st.schema().time_col.is_none() {
            return true;
        }
        let (lo, hi) = time_bounds(st).unwrap_or((i64::MAX, i64::MIN));
        if lo > hi {
            return true;
        }
        let q = query.trim();
        let m = if q.is_empty() {
            MatchSet::all(st)
        } else {
            let Ok(p) = parse(q) else { return true };
            match filter_with(st, &p, &self.fs, &self.cache, &self.interrupt) {
                Ok(m) => m,
                Err(logseek_core::Error::Cancelled) => return false,
                Err(_) => return true,
            }
        };
        let m = match &self.only {
            Some(rows) => m.restrict(st, rows),
            None => m,
        };
        const DAY: i64 = 86_400_000_000;
        // hourly up to ~400 days (≤ 9,600 buckets), daily beyond
        let span = if hi - lo <= 400 * DAY {
            3_600_000_000
        } else {
            DAY
        };
        let agg = Agg {
            func: AggFn::Count,
            col: None,
        };
        let series = match timechart(st, &m, span, &agg, None, &self.cache, &self.interrupt) {
            Ok(s) => s,
            Err(logseek_core::Error::Cancelled) => return false,
            Err(_) => return true,
        };
        let t0 = lo.div_euclid(DAY) * DAY;
        let n = ((hi - t0) / span + 1).clamp(1, 20_000) as usize;
        let mut counts = vec![0u64; n];
        for (b, _, v) in &series.points {
            let i = (b - t0).div_euclid(span);
            if i >= 0 && (i as usize) < n {
                counts[i as usize] += *v as u64;
            }
        }
        let cal = Arc::new(Calendar {
            query,
            t0_us: t0,
            span_us: span,
            counts,
        });
        self.update(|s| s.calendar = Some(cal));
        true
    }

    /// Build the timeline; false when a request for rows stopped it.
    fn build_hist(&mut self) -> bool {
        let (Some(st), Some(m)) = (&self.store, &self.matches) else {
            return true;
        };
        if st.schema().time_col.is_none() {
            self.update(|s| s.hist = None);
            return true;
        }
        let (lo, hi) = time_bounds(st).unwrap_or((i64::MAX, i64::MIN));
        if lo < hi {
            self.update(|s| s.time_bounds = Some((lo, hi)));
        }
        // the whole source's time, even with a period kept: the spine shows
        // the period as a band on it, never loses sight of the rest
        if lo >= hi {
            self.update(|s| s.hist = None);
            return true;
        }
        let span = nice_span((hi - lo) / 160);
        let agg = Agg {
            func: AggFn::Count,
            col: None,
        };
        let series = match timechart(st, m, span, &agg, None, &self.cache, &self.interrupt) {
            Ok(s) => s,
            Err(logseek_core::Error::Cancelled) => return false,
            Err(_) => return true,
        };
        let t0 = lo.div_euclid(span) * span;
        let n = ((hi - t0) / span + 1).clamp(1, 400) as usize;
        let mut counts = vec![0u64; n];
        for (b, _, v) in &series.points {
            let i = ((b - t0) / span) as usize;
            if i < n {
                counts[i] += *v as u64;
            }
        }
        let generation = self
            .snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation;
        let h = Arc::new(Hist {
            generation,
            t0_us: t0,
            span_us: span,
            counts,
        });
        self.update(|s| s.hist = Some(h));
        true
    }
}

/// `summarize` for stored columns; virtual columns are counted by evaluating
/// their formula over the result in chunks (at most 200,000 distinct values kept).
fn summarize_any(
    st: &Store,
    m: &MatchSet,
    col: usize,
    n: usize,
    rare: bool,
    fs: &FormulaSet,
    cache: &ChunkCache,
) -> Option<FieldSummary> {
    let ns = st.schema().len();
    if col < ns {
        return summarize(st, m, col, n, rare, cache);
    }
    let total = m.count();
    // every value counted exactly (the count spills to disk when it outgrows
    // memory, never dropping a value); each pass reads the whole result, so
    // empties and errors are counted afresh on each one
    let (mut empty, mut errors) = (0u64, 0u64);
    let scratch = st.dir().join("_tmp");
    let _ = std::fs::create_dir_all(&scratch);
    let stats = logseek_core::query::agg::count_values(
        "value",
        &scratch,
        cache,
        &AtomicBool::new(false),
        |each| {
            (empty, errors) = (0, 0);
            let mut start = 0u64;
            while start < total {
                let p = page_with(st, m, start, 20_000, &[col], fs, cache)?;
                if p.rows.is_empty() {
                    break;
                }
                let bad: std::collections::HashSet<usize> = p.errors.iter().map(|e| e.0).collect();
                for (i, cells) in p.cells.iter().enumerate() {
                    if bad.contains(&i) {
                        errors += 1;
                        continue;
                    }
                    match cells
                        .first()
                        .and_then(|v| v.as_deref())
                        .filter(|v| !v.is_empty())
                    {
                        Some(v) => each(Some(v)),
                        None => empty += 1,
                    }
                }
                start += p.rows.len() as u64;
            }
            Ok(())
        },
    )
    .ok()?;
    let level = GroupLevel::from_stats(
        stats,
        if rare {
            GroupSort::FewestRows
        } else {
            GroupSort::MostRows
        },
    );
    let distinct = level.with_value();
    let mut best: Vec<(String, u64)> = level
        .page(0, n.saturating_add(1))
        .into_iter()
        .filter_map(|g| Some((g.value?, g.rows)))
        .take(n)
        .collect();
    if empty > 0 {
        best.push((String::new(), empty));
    }
    Some(FieldSummary {
        generation: 0,
        query: String::new(),
        col,
        with_value: total - (empty + errors).min(total),
        matched: total,
        distinct,
        top: best,
        errors,
        rare,
    })
}

/// Distinct count, coverage and the `n` most frequent values of a column over `m`.
fn summarize(
    st: &Store,
    m: &MatchSet,
    col: usize,
    n: usize,
    rare: bool,
    cache: &ChunkCache,
) -> Option<FieldSummary> {
    let name = st.schema().columns.get(col).map(|c| c.name.clone())?;
    let agg = [Agg {
        func: AggFn::Count,
        col: None,
    }];
    let r = run_stats(st, m, &agg, &[name], cache, &AtomicBool::new(false)).ok()?;
    let matched = m.count();
    let (mut distinct, mut empty) = (0u64, 0u64);
    let mut best: Vec<(String, u64)> = Vec::with_capacity(n * 2 + 64);
    // most frequent first, or (stacking) the rarest first: one-off values are often the interesting ones
    let order = |a: &(String, u64), b: &(String, u64)| {
        let by = if rare { a.1.cmp(&b.1) } else { b.1.cmp(&a.1) };
        by.then_with(|| a.0.cmp(&b.0))
    };
    for (k, v) in r.iter() {
        let cnt = v[0] as u64;
        let key = k.into_iter().next().unwrap_or_default();
        if key.is_empty() {
            empty += cnt;
            continue;
        }
        distinct += 1;
        best.push((key, cnt));
        if best.len() > n * 2 + 256 {
            best.sort_unstable_by(order);
            best.truncate(n);
        }
    }
    best.sort_unstable_by(order);
    best.truncate(n);
    if empty > 0 {
        best.push((String::new(), empty));
    }
    Some(FieldSummary {
        generation: 0,
        query: String::new(),
        col,
        with_value: matched - empty.min(matched),
        errors: 0,
        matched,
        distinct,
        top: best,
        rare,
    })
}

/// Rename made column `old` to `new` in `vcols`, and every reference to it in
/// their formulas; nothing when `new` is empty or another column's name
/// (ignoring case) or `old` is not there. Says whether it renamed.
fn rename_made(vcols: &mut [(String, String)], old: &str, new: &str, stored: &[String]) -> bool {
    let new = new.trim();
    let taken = stored.iter().any(|c| c.eq_ignore_ascii_case(new))
        || vcols
            .iter()
            .any(|(n, _)| n != old && n.eq_ignore_ascii_case(new));
    if new.is_empty() || taken || !vcols.iter().any(|(n, _)| n == old) {
        return false;
    }
    for (n, src) in vcols.iter_mut() {
        if n == old {
            *n = new.to_string();
        }
        *src = logseek_core::formula::rename_refs(src, old, new);
    }
    true
}

fn columns_file(dir: &Path) -> PathBuf {
    dir.join("columns.json")
}

/// Virtual columns of a source, saved next to its index (atomic write).
fn save_columns(dir: &Path, cols: &[(String, String)]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("columns.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string(cols).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, columns_file(dir))
}

fn order_file(dir: &Path) -> PathBuf {
    dir.join("order.json")
}

/// What names one finished index for the answers kept beside it: the file it
/// was built from and how much of it is indexed. Any change gives another key.
fn order_key(m: &logseek_core::store::manifest::Manifest) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        m.version,
        m.rows,
        m.bytes_done,
        m.segments.len(),
        m.fingerprint.size,
        m.fingerprint.mtime_ns
    )
}

/// The time order kept beside an index, when it was worked out for exactly
/// this index (`key`). Working it out reads every time in the file, so it is
/// done once and kept. Outer `None`: nothing usable is kept.
fn load_order(
    dir: &Path,
    key: &str,
) -> Option<Option<logseek_core::query::order::TimeOrder>> {
    use logseek_core::query::order::{Direction, TimeOrder};
    let text = std::fs::read_to_string(order_file(dir)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if v.get("key")?.as_str()? != key {
        return None;
    }
    let direction = match v.get("direction")?.as_str()? {
        "no time" => return Some(None),
        "oldest first" => Direction::OldestFirst,
        "newest first" => Direction::NewestFirst,
        "none" => Direction::None,
        _ => return None,
    };
    Some(Some(TimeOrder {
        direction,
        slack_us: v.get("slack_us")?.as_i64()?,
    }))
}

/// Keeps the time order beside the index (atomic write). Failing to keep it
/// only means it is worked out again next time.
fn save_order(dir: &Path, key: &str, order: Option<logseek_core::query::order::TimeOrder>) {
    use logseek_core::query::order::Direction;
    let direction = match order.map(|o| o.direction) {
        None => "no time",
        Some(Direction::OldestFirst) => "oldest first",
        Some(Direction::NewestFirst) => "newest first",
        Some(Direction::None) => "none",
    };
    let v = serde_json::json!({
        "key": key,
        "direction": direction,
        "slack_us": order.map(|o| o.slack_us).unwrap_or(0),
    });
    let tmp = dir.join("order.json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, order_file(dir));
    }
}

fn load_columns(dir: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(columns_file(dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The smallest friendly bucket width ≥ `raw` µs (1 s, 5 s, 1 min, … 1 day, 7 days).
pub fn nice_span(raw: i64) -> i64 {
    const S: i64 = 1_000_000;
    const STEPS: [i64; 17] = [
        1_000,
        10_000,
        100_000,
        S,
        5 * S,
        15 * S,
        30 * S,
        60 * S,
        300 * S,
        900 * S,
        1800 * S,
        3600 * S,
        3 * 3600 * S,
        6 * 3600 * S,
        12 * 3600 * S,
        86_400 * S,
        7 * 86_400 * S,
    ];
    STEPS
        .iter()
        .copied()
        .find(|&s| s >= raw)
        .unwrap_or(30 * 86_400 * S)
}

/// Where the index for a source lives: `%LOCALAPPDATA%\LogSeek\cases\<name>-<hash>.lsidx`.
/// Earliest and latest time in the file, epoch µs (segments keep their
/// time range already in µs).
pub fn time_bounds(st: &Store) -> Option<(i64, i64)> {
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for seg in st.segments() {
        if let (Some(a), Some(b)) = (seg.meta().time_min, seg.meta().time_max) {
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    (lo <= hi).then_some((lo, hi))
}

/// The folder the user chose for indexes (set from the preferences).
static INDEX_HOME: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Keep new indexes in `p` (or in the default place when `None`).
pub fn set_index_home(p: Option<PathBuf>) {
    *INDEX_HOME.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

/// Where indexes go when nothing else is chosen.
pub fn default_cases_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LogSeek")
        .join("cases")
}

/// The folder can hold indexes: it exists or can be made, and a file can be
/// written in it (and is removed again).
pub fn index_home_usable(p: &Path) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".logseek-write-check");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The folder for new indexes: the chosen one when it can hold them, else the default.
pub fn pick_cases_dir(chosen: Option<&Path>, default: PathBuf) -> PathBuf {
    match chosen {
        Some(c) if index_home_usable(c) => c.to_path_buf(),
        _ => default,
    }
}

/// The index folder for a source named `name`: where an index for it already
/// is (so earlier tags and made columns are never left behind), else in the
/// folder for new indexes.
pub fn store_dir_in(name: &str, chosen: Option<&Path>, default: PathBuf) -> PathBuf {
    let existing = default.join(name);
    if existing
        .join(logseek_core::store::manifest::MANIFEST)
        .exists()
    {
        return existing;
    }
    if let Some(c) = chosen {
        let there = c.join(name);
        if there.join(logseek_core::store::manifest::MANIFEST).exists() {
            return there;
        }
    }
    pick_cases_dir(chosen, default).join(name)
}

/// Remove a file's index (its manifest, blocks and scratch files) so the
/// next open reads the file afresh. What the user made is kept: tags, made
/// columns, a set's list of files. The file itself is never touched.
pub fn clear_index(dir: &Path) -> std::io::Result<()> {
    let gone = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    gone(std::fs::remove_file(
        dir.join(logseek_core::store::manifest::MANIFEST),
    ))?;
    gone(std::fs::remove_dir_all(
        dir.join(logseek_core::store::SEG_DIR),
    ))?;
    gone(std::fs::remove_dir_all(dir.join("_tmp")))?;
    Ok(())
}

/// A screenshot run (LOGSEEK_SHOT): tags and made columns it creates are
/// never written to the file's index, so a screenshot leaves no trace.
fn screenshot_session() -> bool {
    crate::dev::var_os("LOGSEEK_SHOT").is_some()
}

pub fn store_dir_for(src: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let stem: String = src
        .file_stem()
        .map(|s| {
            s.to_string_lossy()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .take(40)
                .collect()
        })
        .unwrap_or_default();
    let name = format!("{stem}-{:08x}.lsidx", h as u32);
    store_dir_named(&name)
}

/// The index folder for several files opened as one, beside the other indexes.
/// `dir` is one of the app's own index folders: directly inside the default
/// index home or the one the user chose. Judged from the text of the paths.
pub fn in_index_home(dir: &Path) -> bool {
    let Some(home) = dir.parent() else {
        return false;
    };
    let mut homes = vec![default_cases_dir()];
    if let Some(c) = INDEX_HOME.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        homes.push(c);
    }
    if let Ok(d) = crate::dev::var("LOGSEEK_CASES_DIR") {
        if !d.is_empty() {
            homes.push(PathBuf::from(d));
        }
    }
    homes.iter().any(|h| crate::fileset::same_folder(h, home))
}

pub fn store_dir_for_set(files: &[PathBuf]) -> PathBuf {
    store_dir_named(&crate::fileset::folder_name(files))
}

fn store_dir_named(name: &str) -> PathBuf {
    let name = name.to_string();
    // LOGSEEK_CASES_DIR puts indexes elsewhere (a test folder)
    if let Ok(d) = crate::dev::var("LOGSEEK_CASES_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d).join(name);
        }
    }
    let chosen = INDEX_HOME.lock().unwrap_or_else(|e| e.into_inner()).clone();
    store_dir_in(&name, chosen.as_deref(), default_cases_dir())
}

/// Tagged rows (row in its block, tags) as (time, tags), rows without a time
/// left off the spine (they keep their tags).
pub fn tag_times(rows: &[(u32, u16)], time_of: impl Fn(u32) -> Option<i64>) -> Vec<(i64, u16)> {
    rows.iter()
        .filter(|(_, m)| *m != 0)
        .filter_map(|&(r, m)| Some((time_of(r)?, m)))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_time_order_kept_beside_an_index_is_used_only_for_that_same_index() {
        use logseek_core::query::order::{Direction, TimeOrder};
        let d = tempfile::tempdir().unwrap();
        let up = Some(TimeOrder { direction: Direction::OldestFirst, slack_us: 1_500 });
        // nothing kept yet
        assert_eq!(super::load_order(d.path(), "k1"), None);
        // kept, and read back whole: the order, and "no time column" too
        super::save_order(d.path(), "k1", up);
        assert_eq!(super::load_order(d.path(), "k1"), Some(up));
        super::save_order(d.path(), "k1", None);
        assert_eq!(super::load_order(d.path(), "k1"), Some(None));
        // the index changed (more rows, another file): the kept answer is not used
        super::save_order(d.path(), "k1", up);
        assert_eq!(super::load_order(d.path(), "k2"), None);
        // each direction survives the round trip
        for dir in [Direction::OldestFirst, Direction::NewestFirst, Direction::None] {
            let o = Some(TimeOrder { direction: dir, slack_us: -7 });
            super::save_order(d.path(), "k", o);
            assert_eq!(super::load_order(d.path(), "k"), Some(o));
        }
        // a damaged file is ignored, never trusted
        std::fs::write(super::order_file(d.path()), b"{\"key\":\"k\",\"direction\":\"sideways\"}").unwrap();
        assert_eq!(super::load_order(d.path(), "k"), None);
        std::fs::write(super::order_file(d.path()), b"not json").unwrap();
        assert_eq!(super::load_order(d.path(), "k"), None);
    }

    #[test]
    fn index_home_falls_back_when_it_cannot_hold_indexes() {
        let d = tempfile::tempdir().unwrap();
        let default = d.path().join("default");
        // a folder under a file can never be made
        let file = d.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let bad = file.join("indexes");
        assert_eq!(super::pick_cases_dir(Some(&bad), default.clone()), default);
        // a usable one is used, and no probe file is left in it
        let good = d.path().join("other-drive");
        assert_eq!(super::pick_cases_dir(Some(&good), default.clone()), good);
        assert_eq!(std::fs::read_dir(&good).unwrap().count(), 0);
        assert_eq!(super::pick_cases_dir(None, default.clone()), default);
    }

    #[test]
    fn a_file_indexed_before_keeps_its_index_where_it_is() {
        let d = tempfile::tempdir().unwrap();
        let default = d.path().join("default");
        let chosen = d.path().join("chosen");
        // indexed before, in the default place: stays there after the choice
        let old = default.join("old-1.lsidx");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(logseek_core::store::manifest::MANIFEST), b"{}").unwrap();
        assert_eq!(
            super::store_dir_in("old-1.lsidx", Some(&chosen), default.clone()),
            old
        );
        // never indexed: goes to the chosen folder
        assert_eq!(
            super::store_dir_in("new-2.lsidx", Some(&chosen), default.clone()),
            chosen.join("new-2.lsidx")
        );
        // indexed in the chosen folder, then the choice taken back: still found there
        let moved = chosen.join("new-2.lsidx");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(moved.join(logseek_core::store::manifest::MANIFEST), b"{}").unwrap();
        assert_eq!(
            super::store_dir_in("new-2.lsidx", Some(&chosen), default.clone()),
            moved
        );
    }

    #[test]
    fn tagged_rows_take_their_times_and_untimed_ones_stay_off_the_spine() {
        let rows = [(0u32, 0b1u16), (5, 0b10), (9, 0b101), (12, 0)];
        let t = super::tag_times(&rows, |r| (r != 5).then_some(1_000 + r as i64));
        // row 5 has no time; row 12 has no tags left
        assert_eq!(t, vec![(1_000, 0b1), (1_009, 0b101)]);
    }

    use super::*;
    use logseek_core::ingest::pipeline::start_ingest;

    #[test]
    fn reading_a_file_again_clears_the_index_and_keeps_what_the_user_made() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("x.lsidx");
        std::fs::create_dir_all(dir.join("seg")).unwrap();
        std::fs::create_dir_all(dir.join("_tmp")).unwrap();
        for f in [
            "manifest.json",
            "seg/00000000.kseg",
            "_tmp/spill",
            "tags.json",
            "columns.json",
            "files.lsset",
        ] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        clear_index(&dir).unwrap();
        // the index is gone, so the next open reads the file afresh
        assert!(!dir.join("manifest.json").exists());
        assert!(!dir.join("seg").exists());
        assert!(!dir.join("_tmp").exists());
        // the user's tags, made columns and set of files stay
        for f in ["tags.json", "columns.json", "files.lsset"] {
            assert!(dir.join(f).exists(), "{f} kept");
        }
        // a folder with no index is fine
        clear_index(&dir).unwrap();
    }
    #[test]
    fn spans_are_friendly() {
        assert_eq!(nice_span(700_000), 1_000_000);
        assert_eq!(nice_span(40 * 1_000_000), 60 * 1_000_000);
        assert_eq!(nice_span(5 * 3600 * 1_000_000), 6 * 3600 * 1_000_000);
    }
    #[test]
    fn record_reads_one_window_and_says_when_it_cannot() {
        use logseek_core::ingest::pipeline::{IngestOptions, Keep};
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("evidence.csv");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
        writeln!(f, "time,host,message").unwrap();
        for i in 0..100_000 {
            writeln!(
                f,
                "2026-11-01T00:00:{:02}Z,web{:02},\"msg {i}, ok\"",
                i % 60,
                i % 9
            )
            .unwrap();
        }
        drop(f);
        let open = |name: &str, keep: Keep| {
            let dir = d.path().join(name);
            let o = IngestOptions {
                keep,
                ..Default::default()
            };
            let job = start_ingest(&src, &dir, Budget::new(256 << 20), o);
            while matches!(job.state(), JobState::Running) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Store::open(&dir).unwrap()
        };
        let full = open("f", Keep::All);
        let inpl = open("i", Keep::Only(vec!["time".into()]));
        let cache = ChunkCache::new(Budget::new(64 << 20));
        let fs = FormulaSet::default();
        let want = record_fields(&full, &fs, &cache, 77_777).unwrap();
        let got = record_fields(&inpl, &fs, &cache, 77_777).unwrap();
        assert_eq!(want, got);
        assert!(got
            .iter()
            .any(|(k, v)| k == "message" && v == "msg 77777, ok"));
        assert!(
            inpl.rows_rebuilt() <= 1024,
            "parsed {}",
            inpl.rows_rebuilt()
        );
        // the file was edited since: say so, never show a partial record
        std::thread::sleep(Duration::from_millis(30));
        let mut f = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        writeln!(f, "x").unwrap();
        drop(f);
        let inpl2 = Store::open(&d.path().join("i")).unwrap();
        let err =
            record_fields(&inpl2, &fs, &ChunkCache::new(Budget::new(64 << 20)), 5).unwrap_err();
        assert!(
            err.contains("evidence.csv") && err.contains("changed"),
            "{err}"
        );
    }
    #[test]
    fn tier_words() {
        let (l, tip) = index_words(42 << 20, 0, &[]);
        assert_eq!(l, "Index 42 MB");
        assert!(tip.contains("never changed"), "{tip}");
        let cols = vec!["message".to_string(), "user_agent".to_string()];
        let (l, tip) = index_words(1_300 << 20, 0, &cols);
        assert_eq!(l, "Index 1,300 MB · message, user_agent read from the file");
        assert!(
            tip.contains("message, user_agent") && tip.contains("exact"),
            "{tip}"
        );
        let many: Vec<String> = (0..9).map(|i| format!("c{i}")).collect();
        let (l, tip) = index_words(5 << 20, 0, &many);
        assert_eq!(l, "Index 5 MB · 9 columns read from the file");
        assert!(tip.contains("c0, c1, c2, c3, c4, c5, c6, c7, c8"), "{tip}");
        // zero-write: nothing on disk
        let (l, tip) = index_words(0, 31 << 20, &cols);
        assert_eq!(l, "Nothing written to disk · index in memory 31 MB");
        assert!(
            tip.contains("no room") && tip.contains("message, user_agent"),
            "{tip}"
        );
        // the disk filled part-way
        let (l, _) = index_words(200 << 20, 12 << 20, &[]);
        assert_eq!(l, "Index 200 MB + 12 MB in memory");
        // an index under a megabyte is never shown as nothing
        let (l, _) = index_words(700 << 10, 0, &[]);
        assert_eq!(l, "Index 700 KB");
    }
    #[test]
    fn a_stopped_import_is_usable_not_failed() {
        let p = phase_for(JobState::Partial("the disk is nearly full".into()), 0, 0, 0);
        assert_eq!(p, Phase::Partial("the disk is nearly full".into()));
        assert!(p.settled());
        assert!(phase_for(JobState::Done, 0, 0, 0).settled());
        assert!(!phase_for(JobState::Failed("bad".into()), 0, 0, 0).settled());
        assert!(!phase_for(JobState::Running, 1, 2, 3).settled());
    }
    #[test]
    fn time_bounds_of_an_epoch_millisecond_timeline_are_real_dates() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("ms.jsonl");
        let mut f = std::fs::File::create(&src).unwrap();
        for i in 0..50u64 {
            writeln!(
                f,
                "{{\"timestamp\":{},\"e\":\"x\"}}",
                1_794_564_000_000u64 + i * 1000
            )
            .unwrap();
        }
        drop(f);
        let dir = d.path().join("ms.lsidx");
        let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
        while matches!(job.state(), JobState::Running) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let st = Store::open(&dir).unwrap();
        assert_eq!(st.schema().time_unit_us, Some(1000));
        let (lo, hi) = time_bounds(&st).expect("bounds");
        assert_eq!(logseek_core::types::fmt_time(lo), "2026-11-13 10:00:00");
        assert_eq!(logseek_core::types::fmt_time(hi), "2026-11-13 10:00:49");
    }

    #[test]
    fn made_column_errors_are_counted_apart_from_values_and_empty() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("s.csv");
        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "Size").unwrap();
        // 10 numbers, 3 that are not, 2 empty
        for i in 0..10 {
            writeln!(f, "{}", 100 + i).unwrap();
        }
        for _ in 0..3 {
            writeln!(f, "n/a").unwrap();
        }
        for _ in 0..2 {
            writeln!(f).unwrap();
        }
        drop(f);
        let dir = d.path().join("s.lsidx");
        let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
        while matches!(job.state(), JobState::Running) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let st = Store::open(&dir).unwrap();
        let mut fs = FormulaSet::default();
        let col = fs.add(&st, "kb", "num(Size) * 2").unwrap();
        let cache = ChunkCache::new(Budget::new(32 << 20));
        let m = MatchSet::all(&st);
        let total = m.count();
        let f = summarize_any(&st, &m, col, 20, false, &fs, &cache).unwrap();
        assert_eq!(f.errors, 3);
        assert_eq!(f.with_value, total - 3 - (total - 13));
        assert_eq!(f.distinct, 10);
        // the empty rows are listed as empty; the errors are not among them
        let empty = f
            .top
            .iter()
            .find(|(v, _)| v.is_empty())
            .map(|x| x.1)
            .unwrap_or(0);
        assert_eq!(empty + f.with_value + f.errors, total);
    }
    #[test]
    fn a_made_column_counts_every_different_value_however_many() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("w.csv");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
        writeln!(f, "N").unwrap();
        // more different values than any fixed number of kept keys
        let n = 200_050u64;
        for i in 0..n {
            writeln!(f, "{i}").unwrap();
        }
        writeln!(f, "n/a").unwrap();
        writeln!(f).unwrap();
        drop(f);
        let dir = d.path().join("w.lsidx");
        let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
        while matches!(job.state(), JobState::Running) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let st = Store::open(&dir).unwrap();
        let mut fs = FormulaSet::default();
        let col = fs.add(&st, "twice", "num(N) * 2").unwrap();
        let cache = ChunkCache::new(Budget::new(32 << 20));
        let m = MatchSet::all(&st);
        let f = summarize_any(&st, &m, col, 10, false, &fs, &cache).unwrap();
        assert_eq!(f.distinct, n, "every different value, counted exactly");
        assert_eq!(f.errors, 1);
        assert_eq!(f.with_value, n);
        // the rarest first: every value is on one row, so ties go by name
        let r = summarize_any(&st, &m, col, 3, true, &fs, &cache).unwrap();
        let named: Vec<&(String, u64)> = r.top.iter().filter(|x| !x.0.is_empty()).collect();
        assert_eq!(named.len(), 3);
        assert!(named.iter().all(|x| x.1 == 1));
    }

    #[test]
    fn renaming_a_made_column_carries_the_columns_that_use_it() {
        let mut v: Vec<(String, String)> = [
            ("size_kb", "num(Size) / 1024"),
            ("size_mb", "size_kb / 1024"),
            ("note", r#"concat("size_kb: ", SIZE_KB)"#),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let stored = vec!["Size".to_string(), "Time".to_string()];
        assert!(rename_made(&mut v, "size_kb", "size in kb", &stored));
        assert_eq!(v[0], ("size in kb".into(), "num(Size) / 1024".into()));
        assert_eq!(v[1].1, "`size in kb` / 1024");
        assert_eq!(v[2].1, r#"concat("size_kb: ", `size in kb`)"#);
        // refused: another column's name, nothing, or a column that is not there
        assert!(!rename_made(&mut v, "size_mb", "SIZE", &stored));
        assert!(!rename_made(&mut v, "size_mb", "note", &stored));
        assert!(!rename_made(&mut v, "size_mb", " ", &stored));
        assert!(!rename_made(&mut v, "gone", "x", &stored));
        assert_eq!(v[1].0, "size_mb");
    }

    #[test]
    fn store_dir_is_stable_and_distinct() {
        let a = store_dir_for(Path::new("C:/x/a.csv"));
        assert_eq!(a, store_dir_for(Path::new("C:/x/a.csv")));
        assert_ne!(a, store_dir_for(Path::new("C:/y/a.csv")));
        assert!(a.to_string_lossy().ends_with(".lsidx"));
    }
}
