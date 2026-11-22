//! The application shell: start screen, workspace (top bar, time spine, grid,
//! record viewer, status bar), themes and zoom. Simple first: one search box
//! and the data; everything else is one click or key away.

use crate::bridge::{Bridge, Cmd, Phase, Snapshot};
use crate::grid::{self, GridEvent, GridState};
use crate::moment::group;
use crate::theme::{self, Palette, DARK, LIGHT, STATUS_H, TOP_H, ZOOM_MAX, ZOOM_MIN};
use crate::{fonts, icon};
use eframe::egui::{
    self, Align, Align2, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, Sense,
    Stroke, StrokeKind,
};
use egui_phosphor::regular as ph;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// "This column holds XML/JSON with fields inside — make them columns?"
struct RawSuggest {
    source: PathBuf,
    col: String,
    kind: crate::inspect::Kind,
    keys: Vec<String>,
    /// Ticked keys, in the suggested order.
    pick: Vec<String>,
    dismissed: bool,
    search: String,
}

#[derive(Clone, Copy, PartialEq)]
enum LeftTab {
    Fields,
    Tags,
}

/// The header value checklist: tick the values to keep.
struct ValuesPopup {
    col: usize,
    name: String,
    at: egui::Pos2,
    search: String,
    /// Checked values; initialised from the list when it arrives.
    sel: std::collections::BTreeSet<String>,
    ready: bool,
    query: String,
    sort: ValueSort,
    /// The preview query last sent (rows the current ticks would keep).
    preview_sent: String,
    /// The Condition tab is showing instead of the value list.
    cond_tab: bool,
    /// Condition rows, folded left to right with each row's and/or.
    conds: Vec<CondRow>,
    /// The condition row whose calendar is open.
    cal_row: Option<usize>,
    cal: crate::calendar::CalState,
    /// `sel` holds the unchecked values: every value but these is kept.
    except: bool,
    /// Values loaded from the engine, by position in the list (for `cache_search`).
    cache: std::collections::HashMap<u64, (String, u64)>,
    cache_search: String,
    /// Pages already asked for (their first position).
    asked: std::collections::HashSet<u64>,
    /// Values matching the search, as the engine counted them.
    total: Option<u64>,
    /// A button pressed while not every found value was loaded: it waits
    /// until they all are (what to do, the next position to load, the page asked for).
    pending: Option<(crate::filters::Tick, u64, Option<(u64, f64)>)>,
}

/// One row of the condition builder.
#[derive(Clone)]
struct CondRow {
    op: crate::conditions::Op,
    a: String,
    b: String,
    /// How this row joins the rows above it (ignored on the first row).
    and: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ValueSort {
    Count,
    Rare,
    Alpha,
}

/// One undo/redo step of the filter state.
#[derive(Clone, PartialEq)]
struct FilterState {
    query: String,
    filters: crate::filters::Filters,
    tag_filter: Option<u16>,
}

/// The search builder while it is open: its conditions, the column search
/// typed in each, and the rows each condition (and all of them) keeps.
struct BuilderUi {
    build: crate::builder::Build,
    finds: Vec<String>,
    counts: std::collections::HashMap<String, u64>,
    /// The count asked of the engine and not answered yet.
    asked: Option<String>,
}

impl BuilderUi {
    fn new(build: crate::builder::Build) -> Self {
        BuilderUi {
            finds: vec![String::new(); build.rows.len()],
            build,
            counts: Default::default(),
            asked: None,
        }
    }
}

/// What a file looked like when it was left, to put back on return.
#[derive(Clone, Default)]
struct SavedView {
    query: String,
    filters: crate::filters::Filters,
    sort: Option<(String, bool)>,
    hidden: std::collections::BTreeSet<String>,
    /// The columns the rows were grouped by, and the order of the groups.
    group: Option<(Vec<String>, crate::bridge::GroupSort)>,
}

/// The Find bar's state.
struct FindBar {
    text: String,
    /// The text last sent to the engine (what is highlighted).
    sent: String,
    id: u64,
    /// The answer to `id` has been acted on.
    applied: bool,
    /// Bring the matching column of the found row into view once its cells load.
    pending_col: Option<u64>,
    focus: bool,
}

impl Default for FindBar {
    fn default() -> Self {
        FindBar {
            text: String::new(),
            sent: String::new(),
            id: 0,
            applied: true,
            pending_col: None,
            focus: true,
        }
    }
}

/// "3 of 1,204 rows contain it" and friends.
pub(crate) fn find_line(total: u64, index: Option<u64>) -> String {
    match (total, index) {
        (0, _) => "No row contains it".to_string(),
        (1, _) => "1 row contains it".to_string(),
        (n, Some(i)) => format!("{} of {} rows that contain it", group(i), group(n)),
        (n, None) => format!("{} rows contain it", group(n)),
    }
}

/// A named, saved filter (global, like irflow-timeline presets).
#[derive(Serialize, Deserialize, Clone)]
struct Preset {
    name: String,
    query: String,
    /// (column, filter-row text, ticked values)
    columns: Vec<(String, String, Option<Vec<String>>)>,
    /// Columns whose values are the unchecked ones (keep every value but these).
    #[serde(default)]
    except: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Recent {
    path: String,
    name: String,
    bytes: u64,
    opened: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Prefs {
    dark: bool,
    zoom: f32,
    recent: Vec<Recent>,
    /// The Fields and Tags drawer on the right (Ctrl+B); `None` until the user
    /// opens or closes it: then open where the screen has room for it.
    fields_open: Option<bool>,
    presets: Vec<Preset>,
    /// Every query that was run, newest first (the workspace's history).
    query_log: Vec<crate::workspace::Ran>,
    /// Cases: named sets of files, and the one in use.
    cases: Vec<crate::cases::Case>,
    case: usize,
    /// Saved Follows (the link and time limit, by name), for every file.
    follow_recipes: Vec<crate::follow::Recipe>,
    /// Show the exact query text under the chips (the "receipt").
    show_query: bool,
    /// Time cells in UTC instead of as written in the file.
    times_utc: bool,
    /// Compact rows (20 px) instead of comfortable (24 px).
    compact: bool,
    /// Lines between columns as well as rows (a sheet-like grid).
    grid_lines: bool,
    /// Inspector docked on the right instead of floating.
    inspector_docked: bool,
    /// Colour rules, in order (first match wins), for every source.
    colors: Vec<crate::colors::ColorRule>,
    /// Columns made from a field inside a column are named by the field alone
    /// (`user`) instead of column.field (`response.user`).
    short_field_names: bool,
    /// The `#` column shows each row's number in the file, not its place in the list.
    number_by_file: bool,
    /// Closing the window asks first.
    ask_on_close: bool,
    /// How columns are shown where the user chose, per file path, by column name.
    shown_as: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Where new indexes are kept (`None`: the app's own folder).
    index_home: Option<PathBuf>,
    /// Fields pinned to the top of the row view, by name, in pin order.
    pinned_fields: Vec<String>,
    /// Files the user wants with every column (no smart view when they open).
    all_columns_files: Vec<String>,
    /// Each file's view as it was left, put back when it is opened again.
    kept_views: Vec<crate::keep::KeptView>,
    /// The time rail folded away, so the rows take the width (Ctrl+Shift+B).
    spine_folded: bool,
    /// The analyst's names for tags 1–9 (empty: numbered).
    tag_names: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            dark: true,
            zoom: 1.0,
            recent: Vec::new(),
            fields_open: None,
            presets: Vec::new(),
            follow_recipes: Vec::new(),
            cases: Vec::new(),
            case: 0,
            query_log: Vec::new(),
            show_query: false,
            times_utc: true,
            compact: false,
            grid_lines: true,
            inspector_docked: false,
            colors: Vec::new(),
            short_field_names: false,
            number_by_file: false,
            ask_on_close: true,
            shown_as: Default::default(),
            index_home: None,
            pinned_fields: Vec::new(),
            all_columns_files: Vec::new(),
            kept_views: Vec::new(),
            spine_folded: false,
            tag_names: Vec::new(),
        }
    }
}

pub struct App {
    /// LOGSEEK_STRESS: the scripted stress run, and the settings to restore after it.
    stress: Option<crate::stress::Stress>,
    stress_prefs: Option<Prefs>,
    /// Stress run: switches between the two files' tabs made so far.
    stress_switches: u32,
    /// The export popover is open, with its choices.
    export_open: bool,
    export_fmt: crate::export::Format,
    export_visible_only: bool,
    /// The last export's status was dismissed.
    export_dismissed: bool,
    /// The status line's height: one line, or more when its items wrap.
    status_h: f32,
    /// The keyboard shortcuts overlay is open.
    keys_open: bool,
    /// The import notes the user dismissed: (which file, how many lines).
    notes_dismissed: Option<(Option<PathBuf>, usize)>,
    /// The Summarise sheet, when open.
    summary: Option<crate::summary::SummaryState>,
    /// After a saved search: (its name, how many of its column filters this file could not use).
    preset_skipped: Option<(String, usize)>,
    /// The "Add a column" sheet, when open.
    addcol: Option<crate::addcol::AddColState>,
    /// "Check before opening", while a new file waits for the user.
    check: Option<crate::check::CheckState>,
    /// Made columns as last seen, for the file they belong to.
    seen_vcols: Option<(PathBuf, Vec<(String, String)>)>,
    /// The copy asked of the engine, and a short message for the status bar
    /// (text, when, is it a problem).
    /// The group line as drawn last (a column name dropped on it joins the
    /// grouping), and the group chip being dragged to another place.
    group_rect: Option<egui::Rect>,
    group_drag: Option<usize>,
    /// "How to match" under the search box: the choices, and the column search typed.
    match_sheet: Option<BuilderUi>,
    /// The conditions the builder held when it was last closed.
    builder_last: Option<crate::builder::Build>,
    /// A workbook being opened: its sheets, and the reading of those picked.
    book: Option<crate::book::Book>,
    copy_id: u64,
    copy_done: u64,
    flash: Option<(String, f64, bool)>,
    /// Files opened since the app started, as tabs, and how each was left.
    tabs: crate::tabs::Tabs,
    views: std::collections::HashMap<u64, SavedView>,
    /// The sort in force (from the engine), to remember with the view.
    last_sort: Option<(String, bool)>,
    /// "Search a list": many keywords at once, and what each one found.
    list_win: Option<crate::listwin::ListWin>,
    /// The rows on screen are the ones a list search found (said in the status bar).
    showing_list: bool,
    /// "What sets these rows apart?"
    explain_win: Option<crate::explainwin::ExplainWin>,
    /// "Shapes in this column"
    shape_win: Option<crate::shapewin::ShapeWin>,
    /// "Strange text in this column"
    strange_win: Option<crate::strangewin::StrangeWin>,
    /// "Patterns in this column"
    pattern_win: Option<crate::patternwin::PatternWin>,
    /// "Unusual rows"
    unusual_win: Option<crate::unusualwin::UnusualWin>,
    /// "Rare pairs"
    pair_win: Option<crate::pairwin::PairWin>,
    /// "Timeline signals"
    rhythm_win: Option<crate::rhythmwin::RhythmWin>,
    /// Plain words under the search box, and the file its words were asked for.
    plain: crate::plainline::PlainLine,
    lexicon_for: Option<u64>,
    /// The unusual rows are showing: the measure that says why, for the band.
    why: Option<std::sync::Arc<logseek_core::query::unusual::Model>>,
    /// What the rows shown are, when they are a set of rows handed over
    /// (said in the status bar).
    shown_as: Option<String>,
    /// The keywords that search found, for saying which ones each row on screen
    /// holds; and the page that was last worked out (generation, start, rows).
    list_marks: Option<crate::found::Marks>,
    list_marks_page: (u64, u64, usize),
    /// The Search window (Ctrl+Shift+F): every cell that has a keyword.
    search_win: Option<crate::searchwin::SearchWin>,
    /// The query workspace (Ctrl+E).
    work: Option<crate::workspace::WorkState>,
    /// The case panel: open?, its find text, what the last folder scan left
    /// out, and each listed file's size and whether it has an index.
    case_open: bool,
    case_find: String,
    case_note: Option<String>,
    /// What the last folder scan left out, by kind, and whether every kind is listed.
    case_skipped: std::collections::BTreeMap<String, usize>,
    case_skipped_all: bool,
    case_info: std::collections::HashMap<String, (u64, bool)>,
    /// Screenshot runs change settings they must not keep.
    no_save: bool,
    /// The Find bar (Ctrl+F): text, the request waiting for its answer, and
    /// whether the found row's matching column still has to be brought into view.
    find: Option<FindBar>,
    /// The Follow sheet, and whether the grid is showing followed rows.
    follow: Option<crate::follow::FollowState>,
    follow_showing: bool,
    /// A change that was saved: what, when (UI time), and the folder it went to.
    notice: Option<(String, f64, PathBuf)>,
    colchooser: Option<crate::colchooser::ChooserState>,
    /// Rows each colour rule matches in the view, by its count query, and the
    /// count asked of the engine now.
    rule_counts: std::collections::HashMap<String, u64>,
    rule_count_asked: Option<(String, Instant)>,
    /// Screenshots: start Choose columns from no columns and pick a few.
    chooser_none: bool,
    /// The file the smart column view was considered for (once per file).
    smart_for: Option<String>,
    /// The Ctrl+K palette, when open.
    palette: Option<crate::palette::PaletteState>,
    /// The Columns view, shown in place of the rows while open.
    columns_view: Option<crate::columnsview::ColumnsViewState>,
    /// The colour rules sheet is open.
    colors_open: bool,
    /// The calendar picker opened from the timeline, and what it counts.
    cal_open: bool,
    cal: crate::calendar::CalState,
    cal_query: String,
    bridge: Bridge,
    prefs: Prefs,
    grid: GridState,
    fields: crate::fields::FieldsState,
    filters: crate::filters::Filters,
    tags: crate::tags::TagMap,
    tags_for: Option<PathBuf>,
    tags_stale: bool,
    /// Show only rows with these tags (bit mask); None = all rows.
    tag_filter: Option<u16>,
    left_tab: LeftTab,
    record_find: String,
    /// The row view shows the empty fields (instead of those with a value).
    inspect_empty: bool,
    inspect_raw: bool,
    history: Vec<FilterState>,
    hist_pos: usize,
    preset_name: Option<String>,
    /// A made column about to be removed, and the made columns that use it.
    remove_ask: Option<(String, Vec<String>)>,
    /// A file just opened: its path, and the parts of its view that came back
    /// (said in one line until dismissed).
    came_back: Option<(String, Vec<String>)>,
    /// What was saved with that file when it opened (tagged rows, made
    /// columns), held then: work done after opening never reads as saved.
    came_back_saved: Option<(usize, Vec<String>)>,
    /// "Start from scratch": what is ticked to clear (view, tags, made columns).
    scratch: Option<[bool; 4]>,
    /// A click on the time spine waiting for its answer: the count asked
    /// (rows before that time), and whether the rows run newest first.
    seek: Option<(String, bool)>,
    /// A made column being renamed, and the new name as typed.
    rename_ask: Option<(String, String)>,
    /// Columns found inside a structured column of this source.
    raw_suggest: Option<RawSuggest>,
    raw_checked_for: Option<PathBuf>,
    raw_choose: bool,
    inspect_pretty: bool,
    values: Option<ValuesPopup>,
    query: String,
    sent_query: String,
    edited_at: Option<Instant>,
    record_open: bool,
    seen_generation: u64,
    seen_source: Option<PathBuf>,
    query_id: egui::Id,
    applied: (bool, f32),
    shot: crate::shot::Shot,
    logo: Option<egui::TextureHandle>,
    /// The window title last sent (the file name, then the app's name).
    last_title: String,
    /// Frames left on which the window is asked to fill the screen. The
    /// request made when the window is created is not always honoured, so it
    /// is made again on the first frames.
    fill_screen: u8,
    /// Times left to put right a window marked maximised at an ordinary size.
    fill_tries: u8,
    /// A column just made: brought into view (its name flashes) once it is there.
    show_made: Option<String>,
    /// The row last asked for on Row view's behalf, so it is asked once.
    record_asked: Option<u64>,
    /// What each condition of the search keeps alone: (the search it is
    /// for, its conditions, the count of each as it comes, the one asked).
    cover: Option<(String, Vec<crate::coverage::Part>, Vec<Option<u64>>, Option<usize>)>,
    /// The file on screen, and the book (zip, workbook) it was read out of.
    from_book: Option<(PathBuf, Option<(PathBuf, String)>)>,
    /// A follow started from a cell's menu: shown as soon as its answer is
    /// in, without the sheet. (the run, what it is called)
    follow_quiet: Option<(crate::follow::FollowState, String)>,
    follow_seq: u64,
    /// A follow was just started from a cell: say so once.
    following_note: bool,
    /// The follow last run from a cell, kept so "Change" opens the sheet on it.
    follow_last: Option<crate::follow::FollowState>,
    /// A sheet's rows were just shown: the sheet steps aside this frame.
    hide_sheet: bool,
    /// The analysis sheet closed last, with the file it was opened on.
    closed_sheet: Option<(PathBuf, ClosedSheet)>,
    /// The selected row as last known: its file row, the result it was seen
    /// in, and the file. Kept after the selection is cleared by a new result.
    sel_mem: Option<(u64, u64, PathBuf)>,
    /// The result the row keeping last looked at.
    keep_gen: u64,
    /// The sort the tree of followed rows was last put in order for, with
    /// the tree it was: (tree, sort, request sent).
    tree_order: Option<(u64, Option<(String, bool)>, u64)>,
    /// A row being looked for in a new result, to stay on it: the request's id.
    keep_row: Option<u64>,
    keep_seq: u64,
    /// The "Close LogSeek Studio?" question is on screen.
    close_ask: bool,
    /// Closing was agreed to (or needs no question): let the window close.
    close_ok: bool,
    demo_record: bool,
}

/// The keys listed in the help sheet. Each one is read somewhere in
/// `handle_keys`, `handle_row_keys` or the grid.
pub const HELP_KEYS: &[(&str, &str)] = &[
    ("Ctrl+O", "Open a file"),
    ("Ctrl+K", "Find any command or column by name"),
    ("/", "Go to the search box"),
    ("Ctrl+F", "Find in the rows shown"),
    ("Esc", "Leave the search box, close a panel"),
    ("Up, Down, Page Up, Page Down", "Move through the rows"),
    ("Left, Right, Home, End", "Move the focused cell across the columns"),
    ("Ctrl+Home, Ctrl+End", "First or last row"),
    ("Ctrl+C", "Copy the focused cell, or the selected rows when there are several"),
    ("Shift + Up or Down, Ctrl+A", "Select rows"),
    ("Shift + Left or Right, Shift+click", "Select a block of cells; Ctrl+C copies it"),
    ("Alt+Left, Alt+Right", "Fold or unfold a row in a tree of rows"),
    ("Shift+F10", "The menu of the focused cell"),
    ("Alt+Down", "The menu of the focused cell's column"),
    ("F", "The values of the focused cell's column, to filter by"),
    ("Enter", "Open the row view for the selected row"),
    ("Space", "Show or hide the row view"),
    ("1 to 9", "Tag the row with that tag"),
    ("0", "Clear the row's tags"),
    ("Ctrl+Z, Ctrl+Y", "Step back or forward through filter changes"),
    ("Ctrl+B", "Show or hide the Fields panel"),
    ("Ctrl+G", "Choose columns"),
    ("Ctrl+2, Ctrl+1", "Columns view, back to the rows"),
    ("Ctrl+T, Ctrl+W", "New tab, close tab"),
    ("Ctrl+Tab, Ctrl+Shift+Tab", "Next tab, previous tab"),
    ("Ctrl+L", "Go to the search box"),
    ("Ctrl+E", "Query workspace"),
    ("Ctrl+Shift+F", "Search every cell"),
    ("Ctrl+Shift+C", "Copy the selected rows with the column names first"),
    ("Ctrl+Shift+B", "Fold or show the time spine"),
    ("Ctrl+Shift+Z", "Step forward through filter changes"),
    ("Ctrl + plus, Ctrl + minus, Ctrl+0", "Zoom in, zoom out, normal size"),
    ("Ctrl + mouse wheel", "Zoom"),
    ("?", "This help"),
];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        fonts::install(&cc.egui_ctx);
        let prefs: Prefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        crate::bridge::set_index_home(prefs.index_home.clone());
        crate::tags::set_names(&prefs.tag_names);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut app = App {
            stress: crate::stress::Stress::from_env(),
            stress_prefs: None,
            stress_switches: 0,
            export_open: false,
            export_fmt: crate::export::Format::Csv,
            export_visible_only: false,
            export_dismissed: false,
            status_h: STATUS_H,
            keys_open: false,
            notes_dismissed: None,
            summary: None,
            preset_skipped: None,
            addcol: None,
            check: None,
            seen_vcols: None,
            follow: None,
            follow_showing: false,
            find: None,
            case_open: false,
            work: None,
            search_win: None,
            list_win: None,
            explain_win: None,
            shape_win: None,
            strange_win: None,
            pattern_win: None,
            unusual_win: None,
            pair_win: None,
            rhythm_win: None,
            plain: Default::default(),
            lexicon_for: None,
            why: None,
            shown_as: None,
            showing_list: false,
            list_marks: None,
            list_marks_page: (u64::MAX, 0, 0),
            tabs: Default::default(),
            views: Default::default(),
            group_rect: None,
            group_drag: None,
            match_sheet: None,
            builder_last: None,
            book: None,
            copy_id: 0,
            copy_done: 0,
            flash: None,
            last_sort: None,
            case_find: String::new(),
            case_note: None,
            case_skipped: Default::default(),
            case_skipped_all: false,
            case_info: Default::default(),
            // screenshot and stress runs never write to the user's settings
            no_save: crate::dev::var_os("LOGSEEK_SHOT").is_some()
                || crate::dev::var_os("LOGSEEK_STRESS").is_some(),
            notice: None,
            colchooser: None,
            colors_open: false,
            palette: None,
            columns_view: None,
            chooser_none: false,
            smart_for: None,
            rule_counts: Default::default(),
            rule_count_asked: None,
            cal_open: false,
            cal: Default::default(),
            cal_query: String::new(),
            bridge: Bridge::start(cc.egui_ctx.clone()),
            prefs,
            grid: GridState::default(),
            fields: Default::default(),
            filters: Default::default(),
            tags: Default::default(),
            tags_for: None,
            tags_stale: false,
            tag_filter: None,
            left_tab: LeftTab::Fields,
            record_find: String::new(),
            inspect_empty: false,
            inspect_raw: crate::dev::var_os("LOGSEEK_SHOT_RAW").is_some(),
            history: Vec::new(),
            hist_pos: 0,
            preset_name: None,
            remove_ask: None,
            came_back: None,
            came_back_saved: None,
            scratch: None,
            seek: None,
            rename_ask: None,
            raw_suggest: None,
            raw_checked_for: None,
            raw_choose: false,
            inspect_pretty: true,
            values: None,
            query: String::new(),
            sent_query: String::new(),
            edited_at: None,
            record_open: false,
            seen_generation: 0,
            seen_source: None,
            query_id: egui::Id::new("query-bar"),
            applied: (false, 0.0),
            shot: crate::shot::Shot::from_env(),
            logo: icon::logo_texture(&cc.egui_ctx),
            last_title: window_title(None),
            // not for a screenshot or a timed run: those set their own size
            fill_tries: 2,
            show_made: None,
            record_asked: None,
            cover: None,
            from_book: None,
            fill_screen: if crate::dev::var_os("LOGSEEK_SHOT").is_some()
                || crate::dev::var_os("LOGSEEK_STRESS").is_some()
            {
                0
            } else {
                1
            },
            demo_record: crate::dev::var_os("LOGSEEK_SHOT_RECORD").is_some(),
            follow_quiet: None,
            follow_seq: 0,
            following_note: false,
            follow_last: None,
            hide_sheet: false,
            closed_sheet: None,
            sel_mem: None,
            keep_gen: 0,
            tree_order: None,
            keep_row: None,
            keep_seq: 0,
            close_ask: false,
            // a screenshot or a timed run closes itself: nobody is there to answer
            close_ok: crate::dev::var_os("LOGSEEK_SHOT").is_some()
                || crate::dev::var_os("LOGSEEK_STRESS").is_some(),
        };
        // command line: `logseek <file>... [--query <text>] [--show-query] [--light|--dark]` ("Open with" from the file manager)
        let args: Vec<String> = std::env::args().skip(1).collect();
        if crate::dev::var_os("LOGSEEK_SHOT").is_some() {
            // screenshots start from a known state, whatever the last session left on:
            // nothing scrolled, folded or resized from before (those are kept between runs)
            cc.egui_ctx.data_mut(|d| d.clear());
            // LOGSEEK_SHOT_FIELDS=1 shows the Fields drawer, =0 hides it
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_FIELDS") {
                app.prefs.fields_open = Some(v != "0");
            }
            app.prefs.show_query = false;
            app.prefs.times_utc = true;
            app.prefs.colors.clear();
            // LOGSEEK_SHOT_RECENT="path;path" shows exactly these as the Recent
            // list (empty: none), so a picture never shows this machine's own
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_RECENT") {
                app.prefs.recent = v
                    .split(';')
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| {
                        let path = PathBuf::from(p);
                        Recent {
                            path: p.to_string(),
                            name: crate::fileset::display_name(&path),
                            bytes: crate::fileset::bytes(&path),
                            opened: now_secs(),
                        }
                    })
                    .collect();
            }
            // LOGSEEK_SHOT_FRESH=1 opens files as never opened before (no kept view)
            if crate::dev::var_os("LOGSEEK_SHOT_FRESH").is_some() {
                app.prefs.kept_views.clear();
            }
            // LOGSEEK_SHOT_FOLDED=1 folds the time rail away
            app.prefs.spine_folded = crate::dev::var_os("LOGSEEK_SHOT_FOLDED").is_some();
            // LOGSEEK_SHOT_COLUMNSVIEW=1 opens the Columns view
            if crate::dev::var_os("LOGSEEK_SHOT_COLUMNSVIEW").is_some() {
                app.columns_view = Some(Default::default());
            }
            // LOGSEEK_SHOT_PINS="a,b" pins those fields in the row view
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_PINS") {
                app.prefs.pinned_fields = v
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
            }
            // LOGSEEK_SHOT_PALETTE="words" opens Ctrl+K with those words typed
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_PALETTE") {
                let mut st = crate::palette::PaletteState::default();
                st.typed = v;
                app.palette = Some(st);
            }
            // LOGSEEK_SHOT_CHOOSER=1 opens Choose columns; =none starts it from no columns
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_CHOOSER") {
                app.colchooser = Some(Default::default());
                if v == "none" {
                    app.chooser_none = true;
                }
            }
            // LOGSEEK_SHOT_RULES="col|value|row-or-cell;..." makes rules as the cell menu does;
            // LOGSEEK_SHOT_RULES_OPEN=1 shows the sheet
            if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_RULES") {
                for item in spec.split(';').filter(|x| !x.trim().is_empty()) {
                    let p: Vec<&str> = item.splitn(3, '|').collect();
                    if let [col, value, how] = p[..] {
                        let mut r = crate::colors::rule_from_cell(&app.prefs.colors, col, value);
                        r.cells = how == "cell";
                        app.prefs.colors.push(r);
                    }
                }
                app.colors_open = crate::dev::var_os("LOGSEEK_SHOT_RULES_OPEN").is_some();
            }
        }
        let mut i = 0;
        // several files given ("Open with" on a selection): one view
        let mut files: Vec<PathBuf> = Vec::new();
        while i < args.len() {
            match args[i].as_str() {
                "--light" => app.prefs.dark = false,
                "--show-query" => app.prefs.show_query = true,
                "--dark" => app.prefs.dark = true,
                "--query" if i + 1 < args.len() => {
                    i += 1;
                    app.query = args[i].clone();
                    app.edited_at = Some(Instant::now());
                }
                a if !a.starts_with("--") => files.push(PathBuf::from(a)),
                _ => {}
            }
            i += 1;
        }
        if !files.is_empty() {
            let q = std::mem::take(&mut app.query);
            app.open_files(files);
            // a search given here wins over the one the file was left with
            if !q.trim().is_empty() {
                app.query = q;
                app.edited_at = Some(Instant::now());
                if let Some((_, words)) = app.came_back.as_mut() {
                    words.retain(|w| w != "the search and filters");
                }
            }
        }
        app.apply_look(&cc.egui_ctx);
        app
    }

    fn pal(&self) -> Palette {
        if self.prefs.dark {
            DARK
        } else {
            LIGHT
        }
    }

    fn apply_look(&mut self, ctx: &egui::Context) {
        let want = (self.prefs.dark, self.prefs.zoom);
        if self.applied != want {
            theme::apply(ctx, &self.pal());
            ctx.set_zoom_factor(self.prefs.zoom);
            self.applied = want;
        }
    }

    /// Open a file: the first time (no index yet) show "Check before
    /// opening" so the user sees how it will be read and picks the timeline.
    /// Several files as one view: one index over them all, in time order once
    /// sorted by time, each row saying which file it came from.
    fn open_files(&mut self, files: Vec<PathBuf>) {
        match files.len() {
            0 => {}
            1 => self.open(files.into_iter().next().unwrap_or_default()),
            _ => {
                let dir = crate::bridge::store_dir_for_set(&files);
                match crate::fileset::write(&dir, &files) {
                    Ok(list) => self.open(list),
                    Err(e) => {
                        self.flash =
                            Some((format!("Could not open the files together: {e}"), 0.0, true))
                    }
                }
            }
        }
    }

    fn open(&mut self, path: PathBuf) {
        // several files as one view: opened straight away (each file is read
        // its own way; "Check before opening" is for one file)
        if crate::fileset::is_set(&path) {
            self.start_open(path, logseek_core::ingest::pipeline::TimeChoice::Auto);
            return;
        }
        // a workbook: its sheets first, each one opens as a tab
        if crate::book::handles(&path) {
            let only = crate::dev::var("LOGSEEK_SHOT_SHEET").ok();
            // the packed file or workbook itself is what was opened: it is
            // listed too, so another file inside it is one click away
            if crate::book::holds_many(&path) {
                self.remember(&path);
            }
            self.book = Some(crate::book::Book::start(path, only));
            return;
        }
        let dir = crate::bridge::store_dir_for(&path);
        let indexed = dir.join(logseek_core::store::manifest::MANIFEST).exists();
        if !indexed && crate::dev::var_os("LOGSEEK_NO_CHECK").is_none() {
            if let Ok(pv) = logseek_core::ingest::preview::preview(&path, 50) {
                let mut st = crate::check::CheckState::new(path, pv, dir);
                st.chosen_home = self.prefs.index_home.is_some();
                self.check = Some(st);
                return;
            }
        }
        self.start_open(path, logseek_core::ingest::pipeline::TimeChoice::Auto);
    }

    /// The view on screen, as it is now.
    fn view_now(&self) -> SavedView {
        SavedView {
            query: self.query.clone(),
            filters: self.filters.clone(),
            sort: self.last_sort.clone(),
            hidden: self.grid.hidden.clone(),
            group: self.grid.grouping.as_ref().map(|g| (g.by.clone(), g.sort)),
        }
    }

    /// Keep the view on screen with its tab (only when that tab's file is the one on screen).
    fn save_view(&mut self) {
        let on_screen = self
            .seen_source
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        if let Some(cur) = self.tabs.current {
            if self.tabs.path(cur).map(str::to_string) == on_screen && on_screen.is_some() {
                let v = self.view_now();
                self.views.insert(cur, v);
            }
        }
        // and for the next run: the file's view kept with the preferences
        if let Some(path) = on_screen {
            let k = crate::keep::KeptView {
                path,
                query: crate::filters::compose(&self.query, &self.filters, None),
                sort: self.last_sort.clone(),
                hidden: self.grid.hidden.iter().cloned().collect(),
                group_by: self
                    .grid
                    .grouping
                    .as_ref()
                    .map(|g| g.by.clone())
                    .unwrap_or_default(),
                group_sort: self
                    .grid
                    .grouping
                    .as_ref()
                    .map(|g| group_sort_code(g.sort))
                    .unwrap_or(0),
                at: now_secs(),
            };
            crate::keep::keep(&mut self.prefs.kept_views, k);
        }
    }

    /// Show another tab: the same file changes view in place, another file is opened.
    fn switch_tab(&mut self, id: u64) {
        if self.tabs.current == Some(id) {
            return;
        }
        self.save_view();
        let Some(path) = self.tabs.path(id).map(str::to_string) else {
            return;
        };
        let on_screen = self
            .seen_source
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        self.tabs.current = Some(id);
        if on_screen.as_deref() == Some(path.as_str()) {
            // the same file: only the view changes
            let v = self.views.get(&id).cloned().unwrap_or_default();
            self.push_history();
            self.query = v.query;
            self.filters = v.filters;
            self.grid.hidden = v.hidden;
            self.tag_filter = None;
            self.grid.ftree = None;
            self.grid.grouping = v
                .group
                .map(|(by, sort)| crate::groups::Grouping::new(by, sort));
            self.follow_showing = false;
            self.shown_as = None;
            self.why = None;
            self.grid.top = 0;
            self.grid.sel = None;
            self.bridge.send(Cmd::Restrict(None));
            self.bridge.send(Cmd::Sort(v.sort));
            self.maybe_send_query(true);
        } else {
            self.open(PathBuf::from(path));
        }
    }

    /// A second tab on this file, starting from the view on screen.
    fn new_tab(&mut self) {
        self.save_view();
        let v = self.view_now();
        if let Some(id) = self.tabs.duplicate() {
            self.views.insert(id, v);
        }
    }

    fn close_tab(&mut self, id: u64) {
        let was_current = self.tabs.current == Some(id);
        if self.tabs.list.len() < 2 {
            return;
        }
        if was_current {
            // leave it first, so its neighbour is shown properly
            let next = self.tabs.cycle(true);
            if let Some(n) = next {
                self.switch_tab(n);
            }
        }
        self.tabs.close(id);
        self.views.remove(&id);
    }

    /// Put a file first in the Recent list.
    fn remember(&mut self, path: &std::path::Path) {
        let p = path.to_string_lossy().into_owned();
        self.prefs.recent.retain(|r| r.path != p);
        self.prefs.recent.insert(
            0,
            Recent {
                path: p,
                name: crate::fileset::display_name(path),
                bytes: crate::fileset::bytes(path),
                opened: now_secs(),
            },
        );
    }

    fn start_open(&mut self, path: PathBuf, time: logseek_core::ingest::pipeline::TimeChoice) {
        self.start_open_named(path, time, Vec::new());
    }

    /// `start_open`, with names for the columns of a file that has no row of
    /// names (from "Check before opening").
    fn start_open_named(
        &mut self,
        path: PathBuf,
        time: logseek_core::ingest::pipeline::TimeChoice,
        names: Vec<String>,
    ) {
        // the tab being left remembers its search, filters, sort and hidden columns
        self.save_view();
        let key = path.to_string_lossy().into_owned();
        let tab = self.tabs.open_file(&key);
        let back = self.views.get(&tab).cloned();
        self.remember(&path);
        self.query.clear();
        self.sent_query.clear();
        self.filters.clear();
        self.values = None;
        self.tag_filter = None;
        self.preset_skipped = None;
        self.tags_for = None;
        self.grid.hidden.clear();
        self.grid.reset();
        self.record_open = false;
        self.grid.ftree = None;
        self.grid.grouping = None;
        self.follow = None;
        self.follow_showing = false;
        self.shown_as = None;
        self.why = None;
        self.last_sort = None;
        self.bridge.send(Cmd::OpenWith(path, time, names));
        // coming back to a file: its view as it was left
        if let Some(v) = back {
            self.query = v.query;
            self.filters = v.filters;
            self.grid.hidden = v.hidden;
            self.grid.grouping = v
                .group
                .map(|(by, sort)| crate::groups::Grouping::new(by, sort));
            self.edited_at = Some(Instant::now());
            if v.sort.is_some() {
                self.bridge.send(Cmd::Sort(v.sort));
            }
            self.came_back = None;
        } else {
            // opened for the first time this run: the view it was left with
            // last time, if any, and a line saying what came back
            let kept = crate::keep::find(&self.prefs.kept_views, &key).cloned();
            let words = kept
                .as_ref()
                .map(crate::keep::view_words)
                .unwrap_or_default();
            if let Some(k) = kept {
                self.query = k.query;
                self.grid.hidden = k.hidden.into_iter().collect();
                if !k.group_by.is_empty() {
                    self.grid.grouping = Some(crate::groups::Grouping::new(
                        k.group_by,
                        group_sort_from(k.group_sort),
                    ));
                }
                self.edited_at = Some(Instant::now());
                if k.sort.is_some() {
                    self.bridge.send(Cmd::Sort(k.sort));
                }
            }
            self.came_back = Some((key, words));
            self.came_back_saved = None;
        }
    }

    fn pick_file(&mut self) {
        // pick several to see them as one view (rotated logs, one export per host)
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Open log files (pick several to see them as one)")
            .add_filter(
                "Logs and workbooks (CSV, JSON, Excel, Windows event logs)",
                &[
                    "csv", "tsv", "psv", "txt", "json", "jsonl", "ndjson", "log", "xlsx", "xlsm",
                    "xlsb", "xls", "ods", "evtx", "gz", "zip",
                ],
            )
            .add_filter("All files", &["*"])
            .pick_files()
        {
            self.open_files(files);
        }
    }

    fn handle_row_keys(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        // the time spine keeps the arrow keys while it has the keyboard
        let typing =
            ctx.text_edit_focused() || self.values.is_some() || crate::spine::has_focus(ctx);
        if typing || snap.source.is_none() {
            return;
        }
        let keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        let pressed: Option<usize> = ctx.input(|i| {
            if i.modifiers.command || i.modifiers.alt {
                return None;
            }
            keys.iter().position(|k| i.key_pressed(*k))
        });
        if let (Some(t), Some(row)) = (pressed, self.selected_row_id(snap)) {
            self.toggle_tag(row, t);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Num0) && !i.modifiers.command) {
            if let Some(row) = self.selected_row_id(snap) {
                if self.tags.remove(&row).is_some() {
                    self.bridge
                        .send(Cmd::SaveTags(std::sync::Arc::new(self.tags.clone())));
                }
            }
        }
        let (undo, redo) = ctx.input(|i| {
            (
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                i.modifiers.command
                    && (i.key_pressed(egui::Key::Y)
                        || (i.modifiers.shift && i.key_pressed(egui::Key::Z))),
            )
        });
        if undo {
            self.undo_filter();
        }
        if redo {
            self.redo_filter();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.record_open = !self.record_open;
            if self.record_open {
                if self.grid.sel.is_none() {
                    self.grid.sel = Some(self.grid.top);
                }
                if let Some(r) = self.selected_row_id(snap) {
                    self.bridge.send(Cmd::Record(r));
                }
            }
        }
    }

    /// Whether the Fields and Tags drawer shows: as the user left it, else
    /// open where the screen has room for it beside the rows.
    fn fields_shown(&self, ctx: &egui::Context) -> bool {
        self.prefs
            .fields_open
            .unwrap_or_else(|| drawer_open_by_default(ctx.content_rect().width()))
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Ctrl+B: the drawer on the right; Ctrl+Shift+B: the time rail on the left
        let (drawer, rail) = ctx.input(|i| {
            let b = i.modifiers.command && i.key_pressed(egui::Key::B);
            (b && !i.modifiers.shift, b && i.modifiers.shift)
        });
        if drawer {
            self.prefs.fields_open = Some(!self.fields_shown(ctx));
        }
        if rail {
            self.prefs.spine_folded = !self.prefs.spine_folded;
        }
        // Ctrl+G: go to a column (the chooser, its find box focused)
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::G)) {
            self.colchooser = match self.colchooser {
                Some(_) => None,
                None => Some(Default::default()),
            };
        }
        let (zin, zout, zreset, open, find, esc, wheel_zoom) = ctx.input(|i| {
            let c = i.modifiers.command;
            (
                c && (i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus)),
                c && i.key_pressed(egui::Key::Minus),
                c && i.key_pressed(egui::Key::Num0),
                c && i.key_pressed(egui::Key::O),
                c && i.key_pressed(egui::Key::L),
                i.key_pressed(egui::Key::Escape),
                // Ctrl+wheel and touchpad pinch arrive as a zoom delta
                i.zoom_delta() - 1.0,
            )
        });
        let step = |z: f32, d: f32| ((z + d) * 10.0).round() / 10.0;
        if zin || wheel_zoom > 0.0 {
            self.prefs.zoom = step(self.prefs.zoom, 0.1).min(ZOOM_MAX);
        }
        if zout || wheel_zoom < 0.0 {
            self.prefs.zoom = step(self.prefs.zoom, -0.1).max(ZOOM_MIN);
        }
        if zreset {
            self.prefs.zoom = 1.0;
        }
        if open {
            self.pick_file();
        }
        // "/" jumps to the search, "?" lists every key (not while typing)
        let typing = ctx.text_edit_focused();
        let (slash, question) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Slash) && !i.modifiers.shift,
                i.key_pressed(egui::Key::Questionmark)
                    || (i.key_pressed(egui::Key::Slash) && i.modifiers.shift),
            )
        });
        // tabs: Ctrl+T another tab on this file, Ctrl+W close it, Ctrl+Tab the next one
        let (new_tab, close_tab, next_tab, prev_tab) = ctx.input(|i| {
            let c = i.modifiers.command;
            (
                c && i.key_pressed(egui::Key::T),
                c && i.key_pressed(egui::Key::W),
                c && !i.modifiers.shift && i.key_pressed(egui::Key::Tab),
                c && i.modifiers.shift && i.key_pressed(egui::Key::Tab),
            )
        });
        if new_tab {
            self.new_tab();
        }
        if close_tab {
            if let Some(id) = self.tabs.current {
                self.close_tab(id);
            }
        }
        if next_tab || prev_tab {
            if let Some(id) = self.tabs.cycle(next_tab) {
                self.switch_tab(id);
            }
        }
        // Ctrl+2: the Columns view; Ctrl+1: the rows again
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num2)) {
            self.columns_view = Some(Default::default());
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num1)) {
            self.columns_view = None;
        }
        // Ctrl+K: any action, or any column, by typing a few letters of it
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
            self.palette = match self.palette.take() {
                Some(_) => None,
                None => Some(Default::default()),
            };
        }
        // Ctrl+E: the query workspace
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::E)) {
            self.work = match self.work.take() {
                Some(_) => None,
                None => Some(crate::workspace::WorkState {
                    text: self.effective_query(),
                    focus: true,
                    ..Default::default()
                }),
            };
        }
        // Ctrl+F: find in the rows shown (jump from match to match)
        // Ctrl+Shift+F: the Search window (every cell that has a keyword)
        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::F)) {
            let text = self
                .find
                .as_ref()
                .map(|f| f.text.clone())
                .unwrap_or_default();
            match self.search_win.as_mut() {
                Some(w) => w.focus = true,
                None => self.search_win = Some(crate::searchwin::SearchWin::with_text(&text)),
            }
        } else if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            match self.find.as_mut() {
                Some(f) => f.focus = true,
                None => self.find = Some(FindBar::default()),
            }
        }
        if find || (slash && !typing && !question) {
            ctx.memory_mut(|m| m.request_focus(self.query_id));
        }
        if question && !typing {
            self.keys_open = !self.keys_open;
        }
        if esc {
            if ctx.memory(|m| m.has_focus(self.query_id)) {
                ctx.memory_mut(|m| m.surrender_focus(self.query_id));
            } else {
                self.record_open = false;
            }
        }
        // every file dropped: one opens as itself, several as one view (as the
        // file dialog does when several are picked)
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        self.open_files(dropped);
    }

    /// Toggle tag `i` on a row, save (engine thread) and refresh a tag filter.
    fn toggle_tag(&mut self, row: u64, i: usize) {
        crate::tags::toggle(&mut self.tags, row, i);
        self.bridge
            .send(Cmd::SaveTags(std::sync::Arc::new(self.tags.clone())));
        if let Some(mask) = self.tag_filter {
            self.bridge.send(Cmd::Restrict(Some(std::sync::Arc::new(
                crate::tags::rows_with(&self.tags, mask),
            ))));
        }
    }

    fn set_tag_filter(&mut self, mask: Option<u16>) {
        self.tag_filter = mask;
        // a tag filter replaces the followed rows as what the grid shows
        self.follow_showing = false;
        self.shown_as = None;
        self.why = None;
        self.grid.ftree = None;
        self.grid.reset();
        let rows = mask.map(|m| std::sync::Arc::new(crate::tags::rows_with(&self.tags, m)));
        self.bridge.send(Cmd::Restrict(rows));
    }

    /// Global row id of the selected grid row, if its page is loaded.
    fn selected_row_id(&self, snap: &Snapshot) -> Option<u64> {
        let sel = self.grid.sel?;
        let p = snap
            .page
            .as_ref()
            .filter(|p| p.generation == snap.generation)?;
        p.rows.get(sel.checked_sub(p.start)? as usize).copied()
    }

    fn filter_state(&self) -> FilterState {
        FilterState {
            query: self.query.clone(),
            filters: self.filters.clone(),
            tag_filter: self.tag_filter,
        }
    }

    /// Remember the state being applied (drops any redo steps after the current one).
    fn push_history(&mut self) {
        let now = self.filter_state();
        if self.history.get(self.hist_pos) == Some(&now) {
            return;
        }
        if !self.history.is_empty() {
            self.history.truncate(self.hist_pos + 1);
        }
        self.history.push(now);
        if self.history.len() > 200 {
            self.history.remove(0);
        }
        self.hist_pos = self.history.len() - 1;
    }

    fn restore(&mut self, st: FilterState) {
        self.query = st.query;
        self.filters = st.filters;
        if self.tag_filter != st.tag_filter {
            self.set_tag_filter(st.tag_filter);
        }
        let q = self.effective_query();
        self.sent_query = q.clone();
        self.edited_at = None;
        self.grid.reset();
        self.bridge.send(Cmd::Query(q));
    }

    /// Ctrl+Z: one filter step back.
    fn undo_filter(&mut self) {
        if self.history.is_empty() {
            return;
        }
        // the current state may not be recorded yet (an unsent edit)
        if self.history.get(self.hist_pos) != Some(&self.filter_state()) {
            self.push_history();
        }
        if self.hist_pos > 0 {
            self.hist_pos -= 1;
            let st = self.history[self.hist_pos].clone();
            self.restore(st);
        }
    }

    fn redo_filter(&mut self) {
        if self.hist_pos + 1 < self.history.len() {
            self.hist_pos += 1;
            let st = self.history[self.hist_pos].clone();
            self.restore(st);
        }
    }

    fn any_filter(&self) -> bool {
        !self.query.trim().is_empty()
            || self.filters.values().any(|f| f.is_active())
            || self.tag_filter.is_some()
            || self.follow_showing
    }

    /// Leave rows shown by a list search or a Follow: the whole file again.
    fn show_all_rows(&mut self) {
        self.grid.ftree = None;
        self.grid.reset();
        self.follow_showing = false;
        self.showing_list = false;
        self.shown_as = None;
        self.why = None;
        self.bridge.send(Cmd::Restrict(None));
    }

    fn save_preset(&mut self, name: String) {
        let columns = self
            .filters
            .iter()
            .filter(|(_, f)| f.is_active())
            .map(|(c, f)| {
                (
                    c.clone(),
                    f.text.clone(),
                    f.values.as_ref().map(|v| v.iter().cloned().collect()),
                )
            })
            .collect();
        let except = self
            .filters
            .iter()
            .filter(|(_, f)| f.is_active() && f.values_except)
            .map(|(c, _)| c.clone())
            .collect();
        self.prefs.presets.retain(|p| p.name != name);
        self.prefs.presets.push(Preset {
            name,
            query: self.query.clone(),
            columns,
            except,
        });
        self.prefs
            .presets
            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    }

    fn apply_preset(&mut self, p: &Preset, snap: &Snapshot) {
        self.push_history();
        self.query = p.query.clone();
        self.filters.clear();
        let skipped = p
            .columns
            .iter()
            .filter(|(c, _, _)| !snap.columns.iter().any(|n| n == c))
            .count();
        self.preset_skipped = (skipped > 0).then(|| (p.name.clone(), skipped));
        for (c, t, v) in &p.columns {
            // columns this source does not have are skipped (presets are global)
            if snap.columns.iter().any(|n| n == c) {
                self.filters.insert(
                    c.clone(),
                    crate::filters::ColFilter {
                        text: t.clone(),
                        values: v.as_ref().map(|v| v.iter().cloned().collect()),
                        values_except: p.except.contains(c),
                        ..Default::default()
                    },
                );
            }
        }
        self.maybe_send_query(true);
    }

    /// Chips for every filter in force, history, saved filters, copy — one 34 px strip,
    /// shown only while something filters (or saved filters exist).
    fn chips_bar(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let active = self.any_filter();
        let wide = snap.columns.len() > 12 && snap.col_profile.is_some();
        if !active
            && self.history.len() < 2
            && self.preset_skipped.is_none()
            && !wide
            && snap.virtual_cols.is_empty()
        {
            return;
        }
        let mut view_act: Option<u8> = None; // 1 smart, 2 every column, 3 choose
        let mut show_const: Option<usize> = None;
        enum Remove {
            Range,
            Search,
            Col(String),
            Tags,
            Shown,
        }
        let mut remove: Option<Remove> = None;
        let mut action: Option<&'static str> = None;
        let mut col_toggle: Option<(String, bool)> = None; // (column, true = invert, false = on/off)
        let mut dismiss_skip = false;
        let show_q = self.prefs.show_query && active;
        let failing = snap.query_error.is_some() && snap.query == self.sent_query;
        let mut edit_as_text = false;
        // the strip is as tall as its chips were on the last frame, up to three
        // lines under the controls (a panel does not grow for a scrolling area
        // inside it, so the height is carried over)
        let chips_h_id = egui::Id::new("chips-content-h");
        let chips_want: f32 = ui
            .data(|d| d.get_temp::<f32>(chips_h_id))
            .unwrap_or(34.0)
            .clamp(34.0, 136.0);
        let chips_resp = egui::Panel::top("chips")
            .min_size(chips_want)
            .resizable(false)
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin {
                left: 10,
                right: 10,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().line_segment(
                    [r.left_bottom(), r.right_bottom()],
                    Stroke::new(1.0, pal.hairline),
                );
                let row_w = ui.available_width();
                const RIGHT_W: f32 = 470.0;
                /// Three lines of chips.
                const CHIPS_MAX_H: f32 = 136.0;
                // the strip takes up to three lines of the screen; more than that
                // (a filter on a very long value) scrolls inside the strip, so
                // the rows below always keep their room
                let chips_out = egui::ScrollArea::vertical()
                    .id_salt("chips-scroll")
                    .max_height(CHIPS_MAX_H)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                // measured here: a scrollbar, when there is one, takes its width
                let left_w = (ui.available_width() - RIGHT_W).max(220.0);
                ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(left_w, 34.0), Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                    ui.set_min_width(left_w);
                    ui.set_max_width(left_w);
                    ui.add_space(0.0);
                    if wide {
                        // the column view, said: how many of how many, and how chosen
                        let total = snap.columns.len();
                        let shown = snap.columns.iter().filter(|n| !self.grid.hidden.contains(*n)).count();
                        let smart: Vec<usize> = snap
                            .col_profile
                            .as_ref()
                            .map(|p| logseek_core::query::roles::smart_columns(p, 8))
                            .unwrap_or_default();
                        let stored = snap.col_profile.as_ref().map_or(0, |p| p.len());
                        let is_smart = (0..stored).all(|c| {
                            snap.columns.get(c).is_some_and(|n| {
                                smart.contains(&c) == !self.grid.hidden.contains(n) || self.grid.pinned.contains(n)
                            })
                        });
                        let how = if shown == total {
                            "every column"
                        } else if is_smart {
                            "smart"
                        } else {
                            "chosen"
                        };
                        let label = format!(
                            "{}  Columns: {} of {}, {how}  {}",
                            ph::COLUMNS,
                            group(shown as u64),
                            group(total as u64),
                            ph::CARET_DOWN
                        );
                        ui.menu_button(RichText::new(label).font(theme::small_font()).color(pal.text), |ui| {
                            ui.set_min_width(280.0);
                            if ui.button("Smart: the most telling columns").on_hover_text("The time column and the columns that say most at a glance, judged from their own values: well filled, not the same on every row, not a different id on every row").clicked() {
                                view_act = Some(1);
                                ui.close();
                            }
                            if ui.button("Every column, in file order").clicked() {
                                view_act = Some(2);
                                ui.close();
                            }
                            if ui.button(format!("{}  Choose columns", ph::LIST_CHECKS)).clicked() {
                                view_act = Some(3);
                                ui.close();
                            }
                        });
                        // columns with one value on every row: named once, not repeated down every row
                        let consts: Vec<(usize, String)> = snap
                            .col_profile
                            .as_ref()
                            .map(|p| {
                                p.iter()
                                    .filter(|x| x.role == logseek_core::query::roles::Role::Constant)
                                    .map(|x| (x.col, x.constant.clone().unwrap_or_default()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !consts.is_empty() {
                            let label = format!(
                                "{}  Same on every row: {}  {}",
                                ph::EQUALS,
                                group(consts.len() as u64),
                                ph::CARET_DOWN
                            );
                            ui.menu_button(RichText::new(label).font(theme::small_font()).color(pal.text2), |ui| {
                                ui.set_max_width(520.0);
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.label(RichText::new("These columns hold one value on every row that has one (at least 95% of the rows):").color(pal.text2));
                                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                        for (c, v) in &consts {
                                            let name = snap.columns.get(*c).cloned().unwrap_or_default();
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(RichText::new(format!("{name} = {v}")).font(theme::data_font()));
                                                if self.grid.hidden.contains(&name) && ui.small_button("Show as a column").clicked() {
                                                    show_const = Some(*c);
                                                }
                                            });
                                        }
                                    });
                                });
                            });
                        }
                    }
                    // the columns the user made: which they are, what each is
                    // made from, and the way back (remove one, or all)
                    if !snap.virtual_cols.is_empty() {
                        let label = format!(
                            "{}  Made columns: {}  {}",
                            ph::FUNCTION,
                            group(snap.virtual_cols.len() as u64),
                            ph::CARET_DOWN
                        );
                        let mut remove: Vec<String> = Vec::new();
                        let mut remove_all = false;
                        let (made_btn, _) = egui::containers::menu::MenuButton::new(
                            RichText::new(label).font(theme::small_font()).color(pal.text2),
                        )
                        .config(
                            egui::containers::menu::MenuConfig::new()
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                        )
                        .ui(ui, |ui| {
                            ui.set_max_width(560.0);
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("Columns you made from the file's own. In the grid their names start with {} and have a dotted line under them. Removing one changes nothing in the file.", ph::FUNCTION))
                                            .color(pal.text2),
                                    )
                                    .wrap(),
                                );
                                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                    for (name, formula) in &snap.virtual_cols {
                                        let from = logseek_core::formula::names_used(formula, &snap.columns);
                                        let from: Vec<&str> =
                                            from.into_iter().filter(|x| x != name).collect();
                                        ui.separator();
                                        ui.horizontal(|ui| {
                                            if icon_button(ui, pal, ph::TRASH, "Remove this column").clicked() {
                                                remove.push(name.clone());
                                            }
                                            ui.vertical(|ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(name.as_str())
                                                            .font(fonts::strong(13.0))
                                                            .color(pal.text),
                                                    )
                                                    .wrap(),
                                                );
                                                let made = if from.is_empty() {
                                                    format!("Formula: {formula}")
                                                } else {
                                                    format!("From {}\nFormula: {formula}", from.join(", "))
                                                };
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(made)
                                                            .font(theme::small_font())
                                                            .color(pal.text2),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                        });
                                    }
                                });
                                ui.separator();
                                if snap.virtual_cols.len() > 1
                                    && ui
                                        .button(format!(
                                            "{}  Remove all {} made columns",
                                            ph::TRASH,
                                            group(snap.virtual_cols.len() as u64)
                                        ))
                                        .clicked()
                                {
                                    remove_all = true;
                                    ui.close();
                                }
                            });
                        });
                        // dev switch: LOGSEEK_SHOT_MADE=1 opens the list for a picture
                        if crate::dev::var_os("LOGSEEK_SHOT_MADE").is_some() {
                            std::env::remove_var("LOGSEEK_SHOT_MADE");
                            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&made_btn));
                        }
                        if remove_all {
                            for (name, _) in &snap.virtual_cols {
                                self.bridge.send(Cmd::RemoveColumn(name.clone()));
                            }
                        }
                        for name in remove {
                            self.ask_remove(name, &snap.saved_cols);
                        }
                    }
                    let chip_full = |ui: &mut egui::Ui, icon: &str, text: String, muted: bool| -> (bool, egui::Response) {
                        let full = format!("{icon}  {text}");
                        // the whole text, wrapped inside the chip when it is long
                        let galley = ui.painter().layout(
                            full.clone(),
                            theme::small_font(),
                            pal.text,
                            (left_w - 50.0).max(120.0),
                        );
                        let w = galley.size().x + 34.0;
                        let h = (galley.size().y + 9.0).max(24.0);
                        let (rect, body) = ui.allocate_exact_size(egui::vec2(w, h), Sense::click());
                        let radius = if h > 24.0 { 8 } else { 12 };
                        // a switched-off filter stays visible, quietly
                        let (fill, edge, ink) = if muted {
                            (pal.surface, pal.hairline, pal.muted)
                        } else {
                            (pal.selection, pal.accent_ink.gamma_multiply(0.6), pal.text)
                        };
                        ui.painter().rect_filled(rect, CornerRadius::same(radius), fill);
                        ui.painter().rect_stroke(rect, CornerRadius::same(radius), Stroke::new(1.0, edge), StrokeKind::Inside);
                        ui.painter().galley(
                            egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5),
                            galley,
                            ink,
                        );
                        let x = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 13.0, rect.top() + 12.0),
                            egui::vec2(24.0, 24.0),
                        );
                        let xr = ui.interact(x, ui.id().with(("chip-x", &full)), Sense::click());
                        if xr.hovered() {
                            ui.painter().circle_filled(x.center(), 9.0, pal.surface2);
                        }
                        ui.painter().text(
                            x.center(),
                            Align2::CENTER_CENTER,
                            ph::X,
                            FontId::new(12.0, FontFamily::Proportional),
                            pal.text2,
                        );
                        (xr.on_hover_text("Remove this filter").clicked(), body)
                    };
                    let chip = |ui: &mut egui::Ui, icon: &str, text: String| -> bool { chip_full(ui, icon, text, false).0 };
                    if let Some((a, b, rest)) = leading_range(&self.query) {
                        let label = crate::spine::period_words(a, b);
                        if chip(ui, ph::CLOCK, label) {
                            remove = Some(Remove::Range);
                        }
                        if !rest.trim().is_empty()
                            && chip(ui, ph::MAGNIFYING_GLASS, {
                                let r = crate::meaning::describe_query(rest.trim())
                                    .unwrap_or_else(|_| rest.trim().to_string());
                                r
                            })
                        {
                            remove = Some(Remove::Search);
                        }
                    } else if !self.query.trim().is_empty()
                        && chip(ui, if failing { ph::WARNING } else { ph::MAGNIFYING_GLASS }, {
                            let q = self.query.trim();
                            let r = if failing {
                                format!("cannot run: {q}")
                            } else {
                                crate::meaning::describe_query(q).unwrap_or_else(|_| q.to_string())
                            };
                            let mut r = r;
                            if let Some(h) = crate::meaning::literal_hint(q) {
                                r.push_str(&format!("  ({h})"));
                            }
                            r
                        })
                    {
                        remove = Some(Remove::Search);
                    }
                    // several conditions: what each keeps on its own
                    {
                        let whole = self.effective_query();
                        let parts = crate::coverage::parts(&self.query, &self.filters);
                        if !failing && parts.len() > 1 {
                            let (btn, _) = egui::containers::menu::MenuButton::new(
                                RichText::new(format!("{}  What each one keeps  {}", ph::LIST_NUMBERS, ph::CARET_DOWN))
                                    .font(theme::small_font())
                                    .color(pal.accent_ink),
                            )
                            .config(
                                egui::containers::menu::MenuConfig::new()
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                            )
                            .ui(ui, |ui| {
                                ui.set_max_width(620.0);
                                // counted for the search on screen, once it is asked for
                                if self.cover.as_ref().map(|c| &c.0) != Some(&whole) {
                                    let n = parts.len();
                                    self.cover = Some((whole.clone(), parts.clone(), vec![None; n], None));
                                }
                                let Some((_, parts, counts, _)) = self.cover.as_ref() else {
                                    return;
                                };
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "Together these keep {} of {} rows. Each one alone, over the whole file:",
                                                group(snap.matched),
                                                group(snap.total_rows)
                                            ))
                                            .color(pal.text2),
                                        )
                                        .wrap(),
                                    );
                                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                                        for (p, n) in parts.iter().zip(counts) {
                                            ui.separator();
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(p.words.as_str()).color(pal.text),
                                                )
                                                .wrap(),
                                            );
                                            let none = *n == Some(0);
                                            ui.label(
                                                RichText::new(if none {
                                                    format!(
                                                        "{}  keeps no rows: with this one in it the search finds nothing",
                                                        ph::WARNING
                                                    )
                                                } else {
                                                    crate::coverage::kept_words(*n)
                                                })
                                                .font(fonts::strong(13.0))
                                                .color(if none { pal.warning } else { pal.accent_ink }),
                                            );
                                        }
                                    });
                                });
                            });
                            let btn = btn.on_hover_text("How many rows each part of this search keeps by itself: which one narrows it, and which keeps none");
                            // dev switch: LOGSEEK_SHOT_COVER=1 opens the list for a picture
                            if crate::dev::var_os("LOGSEEK_SHOT_COVER").is_some() && snap.phase.settled() {
                                std::env::remove_var("LOGSEEK_SHOT_COVER");
                                egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&btn));
                            }
                        }
                    }
                    // searched words: where they are, one click away
                    if !failing && {
                        let (m, _) = crate::coverage::marks(&self.query);
                        !m.is_empty()
                    } {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{}  Where it is", ph::LIST_MAGNIFYING_GLASS))
                                        .font(theme::small_font())
                                        .color(pal.accent_ink),
                                )
                                .min_size(egui::vec2(0.0, 24.0)),
                            )
                            .on_hover_text("Each part of your search and the cells that hold it: how many, in which columns, and one click to each")
                            .clicked()
                        {
                            action = Some("where");
                        }
                    }
                    let cols: Vec<(String, crate::filters::ColFilter)> = self
                        .filters
                        .iter()
                        .filter(|(_, f)| f.is_active())
                        .map(|(c, f)| (c.clone(), f.clone()))
                        .collect();
                    for (c, f) in cols {
                        // an expression the engine cannot use is ignored — and says so
                        let unusable =
                            crate::filters::text_clause(&c, &f.text).is_err() && f.values.is_none();
                        let mut label = crate::filters::describe(&c, &f);
                        if unusable {
                            label.push_str("  · not used");
                        }
                        let icon = if unusable {
                            ph::WARNING
                        } else {
                            ph::FUNNEL_SIMPLE
                        };
                        let (x, body) = chip_full(ui, icon, label, !f.enabled);
                        if x {
                            remove = Some(Remove::Col(c.clone()));
                        }
                        body.on_hover_text("Right-click: invert or switch off").context_menu(|ui| {
                            let inv = if f.invert { "Keep the matching rows again" } else { "Invert: keep the rows that do not match" };
                            if ui.button(format!("{}  {inv}", ph::ARROWS_LEFT_RIGHT)).clicked() {
                                col_toggle = Some((c.clone(), true));
                                ui.close();
                            }
                            let onoff = if f.enabled { "Switch off (keep it, filter nothing)" } else { "Switch on" };
                            if ui.button(format!("{}  {onoff}", ph::POWER)).clicked() {
                                col_toggle = Some((c.clone(), false));
                                ui.close();
                            }
                            if ui.button(format!("{}  Remove", ph::X)).clicked() {
                                remove = Some(Remove::Col(c.clone()));
                                ui.close();
                            }
                        });
                    }
                    if let Some((name, n)) = &self.preset_skipped {
                        let what = if *n == 1 { "1 filter".to_string() } else { format!("{n} filters") };
                        if chip(ui, ph::WARNING, format!("{what} in \"{name}\" not applied: this file has no such column")) {
                            dismiss_skip = true;
                        }
                    }
                    if let Some(mask) = self.tag_filter {
                        let names: Vec<String> = (0..9)
                            .filter(|i| mask >> i & 1 == 1)
                            .map(crate::tags::name)
                            .collect();
                        let label = if mask == 0x1FF {
                            "tagged rows".to_string()
                        } else {
                            names.join(", ")
                        };
                        if chip(ui, ph::TAG, label) {
                            remove = Some(Remove::Tags);
                        }
                    }
                    // rows a list search or a Follow found: a filter like any other,
                    // with its cross (and Clear all) to leave them
                    if self.follow_showing
                        && chip(
                            ui,
                            ph::LIST_BULLETS,
                            shown_words(self.shown_as.as_deref(), self.showing_list),
                        )
                    {
                        remove = Some(Remove::Shown);
                    }
                    if !active {
                        ui.label(RichText::new("No filters").color(pal.muted));
                    }
                });
                    ui.allocate_ui_with_layout(egui::vec2(RIGHT_W, 34.0), Layout::right_to_left(Align::Center), |ui| {
                        if active
                            && ui
                                .add(
                                    egui::Button::new(RichText::new("Clear all").color(pal.accent_ink))
                                        .frame(false),
                                )
                                .clicked()
                        {
                            action = Some("clear");
                        }
                        let can_redo = self.hist_pos + 1 < self.history.len();
                        let can_undo = !self.history.is_empty()
                            && (self.hist_pos > 0
                                || self.history.get(self.hist_pos) != Some(&self.filter_state()));
                        ui.add_enabled_ui(can_redo, |ui| {
                            if icon_button(ui, pal, ph::ARROW_U_UP_RIGHT, "Forward  (Ctrl+Y)")
                                .clicked()
                            {
                                action = Some("redo");
                            }
                        });
                        ui.add_enabled_ui(can_undo, |ui| {
                            if icon_button(
                                ui,
                                pal,
                                ph::ARROW_U_UP_LEFT,
                                "Back one filter step  (Ctrl+Z)",
                            )
                            .clicked()
                            {
                                action = Some("undo");
                            }
                        });
                        // hover: the filter words at a glance; click (or ?): the help sheet
                        let help = icon_button(ui, pal, ph::QUESTION, "Help: filter words and keys  (?)");
                        if help.clicked() {
                            self.keys_open = !self.keys_open;
                        }
                        help.on_hover_ui(|ui| crate::grid::filter_help(ui, pal));
                        if active {
                            let on = self.prefs.show_query;
                            let b = egui::Button::new(RichText::new(format!("{}  Query", ph::CODE)).color(if on { pal.text } else { pal.text2 }))
                                .fill(if on { pal.selection } else { pal.bg })
                                .stroke(if on { Stroke::new(1.0, pal.accent_ink) } else { Stroke::new(1.0, pal.hairline) });
                            if ui.add(b).on_hover_text("Show the exact query these filters run — copy it, save it, or edit it as text").clicked() {
                                self.prefs.show_query = !on;
                            }
                        }
                    });
                });
                    });
                let was = ui.data(|d| d.get_temp::<f32>(chips_h_id));
                if was != Some(chips_out.content_size.y) {
                    ui.data_mut(|d| d.insert_temp(chips_h_id, chips_out.content_size.y));
                    ui.ctx().request_repaint();
                }
                let _ = row_w;
            });
        let chips_bottom = chips_resp.response.rect.bottom();
        if show_q {
            // the receipt floats under the chips (the grid never moves): exactly what the engine runs
            let q = self.view_query(&snap.columns);
            let ctx = ui.ctx().clone();
            let screen = ctx.content_rect();
            let w = (screen.width() * 0.62).clamp(420.0, 900.0);
            egui::Area::new(egui::Id::new("query-receipt")).order(egui::Order::Foreground).fixed_pos(egui::pos2(screen.right() - w - 12.0, chips_bottom + 4.0)).show(&ctx, |ui| {
                Frame::popup(ui.style()).fill(pal.surface).stroke(Stroke::new(1.0, pal.control_border)).inner_margin(Margin::same(10)).show(ui, |ui| {
                    ui.set_width(w);
                    {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Query text:").color(pal.text2));
                        let w = (ui.available_width() - 230.0).max(160.0);
                        egui::Frame::new().fill(pal.surface2).corner_radius(CornerRadius::same(4)).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.set_width(w);
                            ui.style_mut().interaction.selectable_labels = true;
                            ui.add(egui::Label::new(RichText::new(&q).font(theme::data_font()).color(pal.text)).wrap());
                        });
                        if ui.button(format!("{}  Copy", ph::COPY)).clicked() {
                            crate::clip::copy(ui.ctx(), q.clone(), "the query");
                        }
                        if ui.button(format!("{}  Edit as text", ph::PENCIL_SIMPLE)).on_hover_text("Move every filter into the search box as one query you can edit (Ctrl+Z brings the chips back)").clicked() {
                            edit_as_text = true;
                        }
                    });
                    ui.label(RichText::new("The rows, their sort and the columns shown, as one line. Send it to someone: pasted into the search box it gives the same view.").font(theme::small_font()).color(pal.text2));
                    }
                });
            });
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.prefs.show_query = false;
            }
        }
        if edit_as_text {
            self.push_history();
            self.query = self.effective_query();
            self.filters.clear();
            self.maybe_send_query(true);
            self.prefs.show_query = false;
        }
        match remove {
            Some(Remove::Range) => {
                if let Some((_, _, rest)) = leading_range(&self.query) {
                    self.query = rest.trim_start().to_string();
                }
                self.maybe_send_query(true);
            }
            Some(Remove::Search) => {
                self.query = leading_range(&self.query)
                    .map(|(a, b, _)| format!("@{}..{}", iso(a), iso(b)))
                    .unwrap_or_default();
                self.maybe_send_query(true);
            }
            Some(Remove::Col(c)) => {
                self.filters.remove(&c);
                self.maybe_send_query(true);
            }
            Some(Remove::Tags) => self.set_tag_filter(None),
            Some(Remove::Shown) => self.show_all_rows(),
            None => {}
        }
        match action {
            Some("where") => {
                let (marks, others) = crate::coverage::marks(&self.query);
                let mut w = crate::searchwin::SearchWin::with_text(
                    marks.first().map_or("", |m| m.text.as_str()),
                );
                w.col = marks.first().and_then(|m| m.col.clone());
                w.from_search = true;
                w.focus = false;
                w.marks = marks.into_iter().map(|m| (m.label, m.text, m.col)).collect();
                w.others = others;
                self.search_win = Some(w);
            }
            Some("clear") => {
                self.push_history();
                if self.follow_showing {
                    self.show_all_rows();
                }
                self.query.clear();
                self.filters.clear();
                if self.tag_filter.is_some() {
                    self.set_tag_filter(None);
                }
                self.maybe_send_query(true);
            }
            Some("copy") => crate::clip::copy(ui.ctx(), self.effective_query(), "the query"),
            Some("save") => self.preset_name = Some(String::new()),
            Some("undo") => self.undo_filter(),
            Some("redo") => self.redo_filter(),
            _ => {}
        }
        if let Some((c, invert)) = col_toggle {
            if let Some(f) = self.filters.get_mut(&c) {
                if invert {
                    f.invert = !f.invert;
                } else {
                    f.enabled = !f.enabled;
                }
            }
            self.maybe_send_query(true);
        }
        match view_act {
            Some(1) => {
                if let Some(src) = &snap.source {
                    let key = src.path.to_string_lossy().into_owned();
                    self.prefs.all_columns_files.retain(|k| *k != key);
                }
                self.apply_smart(snap);
            }
            Some(2) => {
                // every column, in file order; remembered for this file
                self.grid.hidden.clear();
                self.grid.order = self.grid.default_order.clone();
                if let Some(src) = &snap.source {
                    let key = src.path.to_string_lossy().into_owned();
                    if !self.prefs.all_columns_files.contains(&key) {
                        self.prefs.all_columns_files.push(key);
                    }
                }
            }
            Some(3) => self.colchooser = Some(Default::default()),
            _ => {}
        }
        if let Some(c) = show_const {
            crate::colchooser::add_to_end(
                &mut self.grid.order,
                &mut self.grid.hidden,
                &snap.columns,
                c,
            );
        }
        if dismiss_skip {
            self.preset_skipped = None;
        }
        // the name prompt for Save
        if let Some(mut name) = self.preset_name.take() {
            let mut keep = true;
            let ctx = ui.ctx().clone();
            egui::Window::new("Save filters")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
                .show(&ctx, |ui| {
                    ui.label("A name for these filters (saved filters work on any file with the same columns):");
                    let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text("A name you will know it by").desired_width(320.0));
                    r.request_focus();
                    ui.horizontal(|ui| {
                        let ok = !name.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() || (ok && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                            self.save_preset(name.trim().to_string());
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                    });
                });
            if keep {
                self.preset_name = Some(name);
            }
        }
    }

    /// The smart column view: the anchor, then the most telling informative
    /// columns (by their own numbers), first; every other stored column hidden,
    /// one click away. Columns the user made stay shown.
    fn apply_smart(&mut self, snap: &Snapshot) {
        let Some(prof) = snap.col_profile.as_ref() else {
            return;
        };
        let smart = logseek_core::query::roles::smart_columns(prof, 8);
        let stored = prof.len();
        for (c, name) in snap.columns.iter().enumerate() {
            if c < stored && !smart.contains(&c) && !self.grid.pinned.contains(name) {
                self.grid.hidden.insert(name.clone());
            } else {
                self.grid.hidden.remove(name);
            }
        }
        let mut order: Vec<usize> = smart.clone();
        order.extend(
            self.grid
                .default_order
                .iter()
                .copied()
                .filter(|c| !smart.contains(c)),
        );
        self.grid.order = order;
    }

    /// Under the search, once a file opened again: what came back with it,
    /// in one line, with "Start from scratch" beside it.
    fn came_back_line(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some((path, words)) = self.came_back.clone() else {
            return;
        };
        let on_screen = snap
            .source
            .as_ref()
            .map(|s| s.path.to_string_lossy().into_owned());
        if on_screen.as_deref() != Some(path.as_str()) || !snap.phase.settled() {
            return;
        }
        // the tags read from disk at opening (not the ones made since), and
        // the made columns as they were when the file settled
        let Some(loaded) = snap
            .tags
            .as_ref()
            .filter(|t| t.source.to_string_lossy() == path)
        else {
            return;
        };
        let (tagged, made) = self
            .came_back_saved
            .get_or_insert_with(|| {
                (
                    loaded.tags.values().filter(|m| **m != 0).count(),
                    snap.saved_cols.iter().map(|(n, _)| n.clone()).collect(),
                )
            })
            .clone();
        let Some(line) = crate::keep::came_back_line(&words, tagged, &made) else {
            return;
        };
        egui::Panel::top("came-back")
            .frame(
                Frame::new()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{}  {line}", ph::CLOCK_COUNTER_CLOCKWISE))
                            .color(pal.text),
                    );
                    if ui
                        .button("Start from scratch")
                        .on_hover_text("Choose what to clear. The file itself is never changed.")
                        .clicked()
                    {
                        self.scratch = Some([!words.is_empty(), false, false, false]);
                    }
                    if ui.button("OK").on_hover_text("Hide this line").clicked() {
                        self.came_back = None;
                    }
                });
            });
    }

    /// "Start this file from scratch": what is kept with the file, each with a
    /// tick; the ticked ones are cleared. The search can be brought back with
    /// Undo; tags and made columns cannot, and the window says so.
    fn scratch_window(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(mut pick) = self.scratch else {
            return;
        };
        let made: Vec<String> = snap.saved_cols.iter().map(|(n, _)| n.clone()).collect();
        let tagged = self.tags.len();
        let view = self.view_now();
        let has_view = !self.query.trim().is_empty()
            || self.filters.values().any(|f| f.is_active())
            || view.sort.is_some()
            || !view.hidden.is_empty()
            || view.group.is_some();
        let name = snap
            .source
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let (mut go, mut close) = (false, false);
        let ctx = ui.ctx().clone();
        egui::Window::new("Start from scratch")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
            .show(&ctx, |ui| {
                ui.set_max_width(520.0);
                ui.add(
                    egui::Label::new(format!(
                        "Tick what to clear for {name}. The file itself is never changed; only what you did with it here is cleared."
                    ))
                    .wrap(),
                );
                ui.add_space(8.0);
                let rows: [(bool, String, &str); 4] = [
                    (
                        has_view,
                        "The search, filters, sort, hidden columns and grouping".to_string(),
                        "Undo brings it back",
                    ),
                    (
                        tagged > 0,
                        match tagged {
                            0 => "Tags".to_string(),
                            1 => "Tags on 1 row".to_string(),
                            n => format!("Tags on {} rows", group(n as u64)),
                        },
                        "cannot be undone",
                    ),
                    (
                        !made.is_empty(),
                        if made.is_empty() {
                            "Made columns".to_string()
                        } else {
                            format!(
                                "{} ({})",
                                if made.len() == 1 {
                                    "1 made column".to_string()
                                } else {
                                    format!("{} made columns", made.len())
                                },
                                made.join(", ")
                            )
                        },
                        "cannot be undone",
                    ),
                    (
                        snap.source.is_some(),
                        "Read the file again from the start. Use this when the file changed, or after a LogSeek update that reads times or columns better.".to_string(),
                        "your tags and the columns you made are kept",
                    ),
                ];
                for (i, (any, label, note)) in rows.iter().enumerate() {
                    ui.add_enabled_ui(*any, |ui| {
                        let mut on = pick[i] && *any;
                        let text = if *any {
                            format!("{label}  ({note})")
                        } else {
                            format!("{label}: nothing set")
                        };
                        if ui.add(egui::Checkbox::new(&mut on, text)).changed() {
                            pick[i] = on;
                        }
                    });
                }
                ui.add_space(10.0);
                let n = (0..4).filter(|&i| pick[i] && rows[i].0).count();
                ui.horizontal_wrapped(|ui| {
                    // the action reads as the main one once something is ticked
                    let clear = if n > 0 {
                        primary_button(ui, &self.pal(), "Clear what is ticked")
                    } else {
                        ui.add_enabled(false, egui::Button::new("Clear what is ticked"))
                    };
                    if clear.clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        close = true;
                    }
                });
                pick = [
                    pick[0] && rows[0].0,
                    pick[1] && rows[1].0,
                    pick[2] && rows[2].0,
                    pick[3] && rows[3].0,
                ];
            });
        if go {
            if pick[0] {
                self.push_history();
                self.query.clear();
                self.filters.clear();
                self.grid.hidden.clear();
                self.grid.grouping = None;
                self.tag_filter = None;
                self.last_sort = None;
                self.bridge.send(Cmd::Sort(None));
                if let Some(cur) = self.tabs.current {
                    self.views.remove(&cur);
                }
                if let Some(s) = &snap.source {
                    crate::keep::forget(&mut self.prefs.kept_views, &s.path.to_string_lossy());
                }
                self.maybe_send_query(true);
            }
            if pick[1] {
                self.tags.clear();
                self.tag_filter = None;
                self.bridge
                    .send(Cmd::SaveTags(std::sync::Arc::new(self.tags.clone())));
            }
            if pick[2] {
                for c in &made {
                    self.bridge.send(Cmd::RemoveColumn(c.clone()));
                }
            }
            let mut done: Vec<String> = Vec::new();
            if pick[0] {
                done.push("the search and view".to_string());
            }
            if pick[1] {
                done.push(if tagged == 1 {
                    "tags on 1 row".to_string()
                } else {
                    format!("tags on {} rows", group(tagged as u64))
                });
            }
            if pick[2] {
                done.push(if made.len() == 1 {
                    "1 made column".to_string()
                } else {
                    format!("{} made columns", made.len())
                });
            }
            if pick[3] {
                if let Some(s) = &snap.source {
                    self.bridge.send(Cmd::Reindex(s.path.clone()));
                }
                done.push("the index (the file is being read again)".to_string());
            }
            let now = ctx.input(|i| i.time);
            self.flash = Some((
                format!(
                    "Cleared {}. The file itself is unchanged.",
                    crate::keep::and_list(&done)
                ),
                now,
                false,
            ));
            self.came_back = None;
            close = true;
        }
        self.scratch = if close { None } else { Some(pick) };
    }

    /// Remove a made column; when other made columns use it, ask first.
    fn ask_remove(&mut self, name: String, saved: &[(String, String)]) {
        let users = logseek_core::query::exec::made_dependents(saved, &name);
        if users.is_empty() {
            self.bridge.send(Cmd::RemoveColumn(name));
        } else {
            self.remove_ask = Some((name, users));
        }
    }

    /// "Remove a made column that others use?": remove it alone (they stay saved
    /// and say why they are not shown), remove them all, or keep it.
    fn remove_window(&mut self, ui: &mut egui::Ui, _saved: &[(String, String)]) {
        let Some((name, users)) = self.remove_ask.clone() else {
            return;
        };
        let mut keep = true;
        let ctx = ui.ctx().clone();
        egui::Window::new("Remove a made column")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
            .show(&ctx, |ui| {
                ui.set_max_width(460.0);
                // "a", "a and b", "a, b and c"
                let them = match users.split_last() {
                    Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
                    _ => users.join(""),
                };
                let verb = if users.len() == 1 { "uses" } else { "use" };
                ui.add(
                    egui::Label::new(format!(
                        "{them} {verb} {name}. Without {name} {} cannot be worked out; {} stay saved with this file's index and say why {} not shown.",
                        if users.len() == 1 { "it" } else { "they" },
                        if users.len() == 1 { "it would" } else { "they would" },
                        if users.len() == 1 { "it is" } else { "they are" },
                    ))
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button(format!("Remove {name} only")).clicked() {
                        self.bridge.send(Cmd::RemoveColumn(name.clone()));
                        keep = false;
                    }
                    let all = if users.len() == 1 {
                        "Remove both".to_string()
                    } else {
                        format!("Remove all {}", users.len() + 1)
                    };
                    if ui
                        .button(all)
                        .on_hover_text(format!("Removes {name} and {them} (the file is unchanged)"))
                        .clicked()
                    {
                        self.bridge.send(Cmd::RemoveColumn(name.clone()));
                        for u in &users {
                            self.bridge.send(Cmd::RemoveColumn(u.clone()));
                        }
                        keep = false;
                    }
                    if ui.button("Keep").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                });
            });
        if !keep {
            self.remove_ask = None;
        }
    }

    /// Rename a made column: the new name checked while typed; the made columns
    /// that use it follow the new name, and so do its column filters.
    fn rename_window(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some((old, mut text)) = self.rename_ask.take() else {
            return;
        };
        let mut keep = true;
        let ctx = ui.ctx().clone();
        let problem = rename_problem(&text, &old, &snap.columns);
        let users = logseek_core::query::exec::made_dependents(&snap.saved_cols, &old);
        egui::Window::new("Rename a made column")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
            .show(&ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(format!("A new name for {old}:"));
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(320.0));
                r.request_focus();
                if let Some(p) = &problem {
                    ui.label(egui::RichText::new(p).color(ui.visuals().error_fg_color));
                }
                if !users.is_empty() {
                    let them = match users.split_last() {
                        Some((last, rest)) if !rest.is_empty() => {
                            format!("{} and {last}", rest.join(", "))
                        }
                        _ => users.join(""),
                    };
                    let verb = if users.len() == 1 { "uses" } else { "use" };
                    ui.add(
                        egui::Label::new(format!(
                            "{them} {verb} {old}; {} the new name.",
                            if users.len() == 1 {
                                "it will use"
                            } else {
                                "they will use"
                            }
                        ))
                        .wrap(),
                    );
                }
                ui.add(
                    egui::Label::new(format!(
                        "A search typed with {old} in it needs the new name written in."
                    ))
                    .wrap(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = problem.is_none() && text.trim() != old;
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() || (ok && enter) {
                        let new = text.trim().to_string();
                        self.bridge
                            .send(Cmd::RenameColumn(old.clone(), new.clone()));
                        // its column filter follows it
                        if let Some(f) = self.filters.remove(&old) {
                            self.filters.insert(new, f);
                        }
                        keep = false;
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        keep = false;
                    }
                });
            });
        if keep {
            self.rename_ask = Some((old, text));
        }
    }

    /// Detect once per source a structured column (event XML in a raw-text column, JSON) and
    /// offer its inner fields as columns — one click, never automatic.
    fn raw_columns_strip(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(src) = snap.source.as_ref().map(|s| s.path.clone()) else {
            return;
        };
        if self.raw_checked_for.as_ref() != Some(&src) {
            if let Some(p) = snap
                .page
                .as_ref()
                .filter(|p| p.generation == snap.generation && !p.cells.is_empty())
            {
                self.raw_checked_for = Some(src.clone());
                self.raw_suggest = None;
                let ns = snap.columns.len().saturating_sub(snap.virtual_cols.len());
                for c in 0..ns {
                    let samples: Vec<&str> = p
                        .cells
                        .iter()
                        .take(40)
                        .filter_map(|r| r.get(c).and_then(|v| v.as_deref()))
                        .collect();
                    if let Some(s) = crate::rawcols::suggest(&samples) {
                        self.raw_suggest = Some(RawSuggest {
                            source: src.clone(),
                            col: snap.columns[c].clone(),
                            kind: s.kind,
                            keys: s.keys.clone(),
                            // nothing varies in the rows looked at: every field is offered
                            pick: if s.pick.is_empty() {
                                s.keys.iter().cloned().collect()
                            } else {
                                s.pick.into_iter().collect()
                            },
                            dismissed: false,
                            search: String::new(),
                        });
                        break;
                    }
                }
            }
        }
        let Some(rs) = self
            .raw_suggest
            .as_mut()
            .filter(|r| r.source == src && !r.dismissed)
        else {
            return;
        };
        // already made (some of) these columns: nothing to suggest
        if !snap.virtual_cols.is_empty() && !self.raw_choose {
            return;
        }
        let mut add: Option<Vec<String>> = None;
        let mut dismiss = false;
        let mut choose = false;
        if snap.virtual_cols.is_empty() {
            egui::Panel::top("raw-suggest")
                .min_size(36.0)
                .frame(Frame::new().fill(pal.selection).inner_margin(Margin {
                    left: 12,
                    right: 10,
                    top: 6,
                    bottom: 6,
                }))
                .show(ui, |ui| {
                    // buttons first, at the right, so they always show; the
                    // sentence wraps in the room that is left (never cut)
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, ph::X, "Not now").clicked() {
                            dismiss = true;
                        }
                        if ui.button("Choose columns").clicked() {
                            choose = true;
                        }
                        let label = if rs.pick.len() < rs.keys.len() {
                            format!("Add the {} that vary (of {})", rs.pick.len(), rs.keys.len())
                        } else {
                            format!(
                                "Add {}",
                                crate::keep::plural(rs.pick.len(), "column", "columns")
                            )
                        };
                        if primary_button(ui, pal, &label)
                            .on_hover_text(format!(
                                "New columns read from inside each row (the file is unchanged):
{}",
                                rs.pick.iter().cloned().collect::<Vec<_>>().join(", ")
                            ))
                            .clicked()
                        {
                            add = Some(rs.pick.iter().cloned().collect());
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(
                                RichText::new(ph::SPARKLE)
                                    .color(pal.accent_ink)
                                    .font(FontId::new(15.0, FontFamily::Proportional)),
                            );
                            const SHOWN: usize = 3;
                            let names: Vec<&str> =
                                rs.pick.iter().take(SHOWN).map(|s| s.as_str()).collect();
                            let kind = if rs.kind == crate::inspect::Kind::Xml {
                                "XML"
                            } else {
                                "JSON"
                            };
                            // a few names fit the line whole; a longer list is on the
                            // button's tip in full, never cut short here
                            let listed = if rs.pick.len() > SHOWN {
                                format!(
                                    "The button makes {} columns; hover it for every name",
                                    rs.pick.len()
                                )
                            } else {
                                format!("Make columns: {}", names.join(", "))
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} holds {kind} with {} inside. {listed}.",
                                        rs.col,
                                        crate::keep::plural(rs.keys.len(), "field", "fields"),
                                    ))
                                    .color(pal.text),
                                )
                                .wrap(),
                            )
                            .on_hover_text(format!("Suggested columns: {}", rs.pick.join(", ")));
                        });
                    });
                });
        }
        if choose {
            self.raw_choose = true;
        }
        // dev switch for visual checks: accept the suggestion once
        if add.is_none()
            && snap.virtual_cols.is_empty()
            && crate::dev::var_os("LOGSEEK_SHOT_RAWCOLS").is_some()
        {
            std::env::remove_var("LOGSEEK_SHOT_RAWCOLS");
            add = self
                .raw_suggest
                .as_ref()
                .map(|r| r.pick.iter().cloned().collect());
        }
        if self.raw_choose {
            let mut open = true;
            let ctx = ui.ctx().clone();
            let rs = self.raw_suggest.as_mut().unwrap();
            // a short fixed title (window titles are cut to fit); the column's
            // name goes in the body, where it wraps
            egui::Window::new("Make columns").open(&mut open).collapsible(false).resizable(true).default_size([380.0, 460.0]).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0)).show(&ctx, |ui| {
                ui.add(egui::Label::new(RichText::new(format!("Fields inside {}", rs.col)).font(fonts::strong(14.0)).color(pal.text)).wrap());
                ui.label(RichText::new("Tick the fields to show as columns. They are read from each row; the file is never changed.").color(pal.text2));
                ui.add(egui::TextEdit::singleline(&mut rs.search).hint_text(format!("{}  Find a field", ph::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    if ui.small_button("Select all").clicked() {
                        rs.pick = rs.keys.clone();
                    }
                    if ui.small_button("Clear").clicked() {
                        rs.pick.clear();
                    }
                });
                let needle = rs.search.to_lowercase();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for k in &rs.keys {
                        if !needle.is_empty() && !k.to_lowercase().contains(&needle) {
                            continue;
                        }
                        let mut on = rs.pick.contains(k);
                        if ui.checkbox(&mut on, k).changed() {
                            if on {
                                rs.pick.push(k.clone());
                                // keep the suggested order
                                let order = rs.keys.clone();
                                rs.pick.sort_by_key(|x| order.iter().position(|o| o == x).unwrap_or(usize::MAX));
                            } else {
                                rs.pick.retain(|x| x != k);
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                if primary_button(ui, pal, &format!("Add {}", crate::keep::plural(rs.pick.len(), "column", "columns"))).clicked() {
                    add = Some(rs.pick.iter().cloned().collect());
                }
            });
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx) {
                open = false;
            }
            if !open {
                self.raw_choose = false;
            }
        }
        if let Some(keys) = add {
            let rs = self.raw_suggest.as_ref().unwrap();
            let existing: Vec<String> = snap.columns.clone();
            let mut taken = existing.clone();
            let cols: Vec<(String, String)> = keys
                .iter()
                .map(|k| {
                    let name = field_column_name(k, &rs.col, &taken, self.prefs.short_field_names);
                    taken.push(name.clone());
                    (name, crate::rawcols::extract_formula(&rs.col, rs.kind, k))
                })
                .collect();
            self.show_made = cols.first().map(|(n, _)| n.clone());
            self.bridge.send(Cmd::AddColumns(cols));
            self.raw_choose = false;
        }
        if dismiss {
            if let Some(r) = self.raw_suggest.as_mut() {
                r.dismissed = true;
            }
        }
    }

    /// Time span (UTC µs) of the rows on screen, to shade on the timeline.
    fn visible_window(&self, snap: &Snapshot) -> Option<(i64, i64)> {
        let tc = snap.time_col?;
        let p = snap
            .page
            .as_ref()
            .filter(|p| p.generation == snap.generation)?;
        let (top, n) = self.grid.visible_range();
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for i in top..top + n {
            let Some(row) = i.checked_sub(p.start).and_then(|k| p.cells.get(k as usize)) else {
                continue;
            };
            if let Some(us) = row
                .get(tc)
                .and_then(|v| v.as_deref())
                .and_then(logseek_core::time::parse_time)
            {
                lo = lo.min(us);
                hi = hi.max(us);
            }
        }
        (lo <= hi).then_some((lo, hi))
    }

    /// The query the engine runs: the global search AND every column filter.
    fn effective_query(&self) -> String {
        crate::filters::compose(&self.query, &self.filters, None)
    }

    /// The whole view as one line: the rows, the sort, and the columns shown
    /// (only when some are hidden or moved).
    fn view_query(&self, columns: &[String]) -> String {
        let vis = crate::grid::visible_order(&self.grid.order, columns, &self.grid.hidden);
        let natural = vis.len() == columns.len() && vis.iter().enumerate().all(|(i, &c)| i == c);
        let names: Vec<String> = vis
            .iter()
            .filter_map(|&c| columns.get(c).cloned())
            .collect();
        crate::viewquery::write(
            &self.effective_query(),
            self.last_sort.as_ref().map(|(c, d)| (c.as_str(), *d)),
            (!natural && !vis.is_empty()).then_some(names.as_slice()),
        )
    }

    /// A pasted view ("... | sort -bytes | fields a, b"): the sort and the
    /// columns are applied to the grid and leave the search box. Only when
    /// every column it names exists (so it waits while one is being typed).
    /// Says `true` when it must wait: the file's columns are not known yet.
    fn take_view_steps(&mut self) -> bool {
        if !self.query.contains('|') {
            return false;
        }
        let Some(v) = crate::viewquery::read(&self.query) else {
            return false;
        };
        let snap = self.bridge.snapshot();
        if snap.columns.is_empty() {
            return true;
        }
        let has = |n: &String| snap.columns.contains(n);
        if !v.sort.as_ref().is_none_or(|(c, _)| has(c))
            || !v.fields.as_ref().is_none_or(|f| f.iter().all(has))
        {
            return false;
        }
        self.query = v.query;
        if let Some(sort) = v.sort {
            self.bridge.send(Cmd::Sort(Some(sort)));
        }
        if let Some(f) = v.fields {
            let idx: Vec<usize> = f
                .iter()
                .filter_map(|n| snap.columns.iter().position(|c| c == n))
                .collect();
            let mut order = idx.clone();
            order.extend((0..snap.columns.len()).filter(|c| !idx.contains(c)));
            self.grid.order = order;
            self.grid.hidden = snap
                .columns
                .iter()
                .filter(|c| !f.contains(c))
                .cloned()
                .collect();
        }
        false
    }

    /// Follows a cell's value from its menu. With a column to look in (`to`)
    /// it runs at once and the answer is shown as soon as it is in; with none
    /// the Follow sheet opens, started from that row and that column.
    fn follow_from_cell(
        &mut self,
        snap: &Snapshot,
        row: u64,
        col: String,
        value: String,
        to: String,
        repeat: bool,
    ) {
        let mut st = self.new_follow(snap);
        st.start = crate::follow::StartKind::Selected;
        st.from = col.clone();
        st.also = None;
        st.when = 0;
        if to.is_empty() {
            self.follow = Some(st);
            return;
        }
        st.to = to.clone();
        st.repeat = repeat;
        let Some(spec) = st.spec() else {
            self.follow = Some(st);
            return;
        };
        // its own run numbers, far from the sheet's
        self.follow_seq += 1;
        st.run = (1u64 << 40) + self.follow_seq;
        self.bridge.send(Cmd::Follow {
            id: st.run,
            start: crate::bridge::FollowStart::Rows(std::sync::Arc::new(vec![row])),
            spec,
        });
        let words = crate::follow::followed_words(&col, &value, &to, repeat);
        self.follow_quiet = Some((st, words));
        self.following_note = true;
    }

    /// Keeps a sheet that was just closed, for "Back to …".
    fn keep_closed(&mut self, sheet: ClosedSheet, snap: &Snapshot) {
        if let Some(src) = snap.source.as_ref() {
            self.closed_sheet = Some((src.path.clone(), sheet));
        }
    }

    /// Puts a kept sheet back on screen as it was left.
    fn reopen(&mut self, sheet: ClosedSheet) {
        match sheet {
            ClosedSheet::List(w) => self.list_win = Some(w),
            ClosedSheet::Rhythm(w) => self.rhythm_win = Some(w),
            ClosedSheet::Pair(w) => self.pair_win = Some(w),
            ClosedSheet::Pattern(w) => self.pattern_win = Some(w),
            ClosedSheet::Strange(w) => self.strange_win = Some(w),
            ClosedSheet::Shape(w) => self.shape_win = Some(w),
            ClosedSheet::Explain(w) => self.explain_win = Some(w),
        }
    }

    fn maybe_send_query(&mut self, force: bool) {
        if self.take_view_steps() {
            // the file is still opening: try again shortly, send nothing yet
            self.edited_at = Some(Instant::now());
            return;
        }
        let due = self
            .edited_at
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(260));
        let q = self.effective_query();
        if (force || due) && q != self.sent_query {
            self.push_history();
            self.sent_query = q.clone();
            self.edited_at = None;
            self.grid.reset();
            self.bridge.send(Cmd::Query(q));
        }
        if force || due {
            self.edited_at = None;
        }
    }
}

/// A leading `@a..b` time range in the query: (start µs, end µs, rest of the query).
fn leading_range(q: &str) -> Option<(i64, i64, &str)> {
    let t = q.trim_start();
    let tok_end = t.find(char::is_whitespace).unwrap_or(t.len());
    let (x, y) = t[..tok_end].strip_prefix('@')?.split_once("..")?;
    let (a, b) = (
        logseek_core::time::parse_time(x)?,
        logseek_core::time::parse_time(y)?,
    );
    Some((a.min(b), a.max(b), &t[tok_end..]))
}

/// µs → `2016-08-24T18:27:39Z` (seconds precision is plenty for a dragged range).
fn iso(us: i64) -> String {
    let full = logseek_core::types::fmt_time(us);
    // every digit kept: a range drawn on the timeline is never rounded
    format!("{}Z", full.replace(' ', "T"))
}

/// The name of a column made from field `key` inside column `source`:
/// `source.key` (two JSON columns with the same field never share a name), or
/// the field's own name inside `_raw` (the whole line) or when the user prefers
/// short names (`short`); numbered if taken.
fn field_column_name(key: &str, source: &str, existing: &[String], short: bool) -> String {
    if short || source == "_raw" {
        unique_name(key, existing)
    } else {
        unique_name(&format!("{source}.{key}"), existing)
    }
}

/// `name`, or `name (2)`, … if a column of that name already exists.
fn unique_name(name: &str, existing: &[String]) -> String {
    // the engine matches column names ignoring case, so names must differ by more than case
    let taken = |n: &str| existing.iter().any(|e| e.eq_ignore_ascii_case(n));
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|i| format!("{name} ({i})"))
        .find(|n| !taken(n))
        .unwrap()
}

/// Indent every line by four spaces (for nested values in Pretty).
fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A grouping's order as kept between runs.
fn group_sort_code(g: crate::bridge::GroupSort) -> u8 {
    use crate::bridge::GroupSort as G;
    match g {
        G::MostRows => 0,
        G::FewestRows => 1,
        G::FirstSeen => 2,
        G::LastSeen => 3,
        G::Name => 4,
    }
}

fn group_sort_from(c: u8) -> crate::bridge::GroupSort {
    use crate::bridge::GroupSort as G;
    match c {
        1 => G::FewestRows,
        2 => G::FirstSeen,
        3 => G::LastSeen,
        4 => G::Name,
        _ => G::MostRows,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn human_bytes(b: u64) -> String {
    let f = b as f64;
    if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.0} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{b} B")
    }
}

fn ago(secs: u64) -> String {
    let d = now_secs().saturating_sub(secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", d / 86_400),
    }
}

/// A flat icon button with a visible hover state, a 28 px target and a tooltip.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    glyph: &str,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click());
    // the glyph is painted, so name the button for screen readers (AccessKit)
    let name = tip.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone()));
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), pal.surface2);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(2.0, pal.focus),
            StrokeKind::Inside,
        );
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(16.0, FontFamily::Proportional),
        pal.text2,
    );
    resp.on_hover_text(tip)
}

/// Made columns added and removed between two lists (by name).
pub(crate) fn column_changes(
    before: &[(String, String)],
    after: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let has = |l: &[(String, String)], n: &str| l.iter().any(|(x, _)| x == n);
    let added = after
        .iter()
        .filter(|(n, _)| !has(before, n))
        .map(|(n, _)| n.clone())
        .collect();
    let removed = before
        .iter()
        .filter(|(n, _)| !has(after, n))
        .map(|(n, _)| n.clone())
        .collect();
    (added, removed)
}

/// A colour's dot followed by its name: the dot carries the colour, the name
/// stays in readable text colour (a tint is never used as text).
pub(crate) fn swatch_label(
    dot: egui::Color32,
    name: &str,
    text: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = theme::ui_font();
    job.append(ph::CIRCLE, 0.0, egui::TextFormat::simple(font.clone(), dot));
    job.append(
        &format!("  {name}"),
        0.0,
        egui::TextFormat::simple(font, text),
    );
    job
}

pub(crate) fn primary_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
    let b = egui::Button::new(
        RichText::new(label)
            .color(pal.on_accent)
            .font(fonts::strong(13.0)),
    )
    .fill(pal.accent)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(4))
    .min_size(egui::vec2(0.0, 32.0));
    ui.add(b)
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.no_save {
            return;
        }
        self.save_view();
        eframe::set_value(storage, eframe::APP_KEY, &self.prefs);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        if let Some(st) = self.stress.as_mut() {
            st.begin_frame(frame.info().cpu_usage);
        }
        self.shot.tick(ctx);
        self.handle_keys(ctx);
        self.apply_look(ctx);
        self.maybe_send_query(false);
        if self.edited_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let pal = self.pal();
        if let Some(mut b) = self.book.take() {
            let mut act = b.show(ctx, &pal);
            // dev switch: LOGSEEK_SHOT_SHEET="*" opens every sheet
            if crate::dev::var("LOGSEEK_SHOT_SHEET").as_deref() == Ok("*") && b.pick_all() {
                std::env::remove_var("LOGSEEK_SHOT_SHEET");
                act = crate::book::BookAction::None;
            }
            match act {
                crate::book::BookAction::None => self.book = Some(b),
                crate::book::BookAction::Close => {}
                crate::book::BookAction::Open(list) => {
                    // what was found about formulas, where the user will see it
                    let mut notes: Vec<String> = list
                        .iter()
                        .filter_map(|r| crate::book::formula_line(&r.sheet, &r.out))
                        .collect();
                    let titled: Vec<String> = list
                        .iter()
                        .filter_map(|r| crate::book::title_line(&r.sheet, &r.out))
                        .collect();
                    if notes.is_empty() && !titled.is_empty() {
                        let short = if titled.len() == 1 {
                            format!("{}  A sheet has a title above its column names", ph::INFO)
                        } else {
                            format!(
                                "{}  {} sheets have a title above their column names",
                                ph::INFO,
                                titled.len()
                            )
                        };
                        self.flash = Some((
                            format!("{short}\n{}", titled.join("\n")),
                            ctx.input(|i| i.time) + 52.0,
                            false,
                        ));
                    }
                    let formula_notes = !notes.is_empty();
                    notes.extend(titled);
                    let notes: Vec<String> = if formula_notes { notes } else { Vec::new() };
                    let damaged: u64 = list.iter().map(|r| r.damaged).sum();
                    if let Some(line) = crate::book::damaged_line(damaged) {
                        self.flash = Some((
                            format!("{}  {line}", ph::WARNING),
                            ctx.input(|i| i.time) + 52.0,
                            true,
                        ));
                    }
                    if !notes.is_empty() {
                        let bad = list.iter().any(|r| r.out.formulas_without_result > 0);
                        // short in the status bar, the whole of it on hover (after the line break)
                        let (f, e): (u64, u64) = list.iter().fold((0, 0), |a, r| {
                            (a.0 + r.out.formulas, a.1 + r.out.formulas_without_result)
                        });
                        let short = if e > 0 {
                            format!(
                                "{}  {} formula cells have no saved result and are empty",
                                ph::WARNING,
                                group(e)
                            )
                        } else {
                            format!(
                                "{}  {} formula cells show their saved results",
                                ph::INFO,
                                group(f)
                            )
                        };
                        self.flash = Some((
                            format!("{short}\n{}", notes.join("\n")),
                            ctx.input(|i| i.time) + 52.0,
                            bad,
                        ));
                    }
                    // a tab for each sheet, in the workbook's order; the first is shown
                    self.save_view();
                    for r in &list {
                        self.tabs.open_file(&r.path.to_string_lossy());
                    }
                    if let Some(first) = list.first() {
                        self.open(first.path.clone());
                    }
                }
            }
        }
        if let Some(mut st) = self.check.take() {
            let mut act = crate::check::show(ctx, &pal, &mut st);
            // dev switch: LOGSEEK_SHOT_CHECK_PICK="field" (or "none") picks and opens
            if let Ok(want) = crate::dev::var("LOGSEEK_SHOT_CHECK_PICK") {
                std::env::remove_var("LOGSEEK_SHOT_CHECK_PICK");
                st.time = st.preview.time.iter().position(|t| t.name == want);
                act = crate::check::CheckAction::Open;
            }
            match act {
                crate::check::CheckAction::Open => {
                    let t = st.choice();
                    let names = st.names.clone();
                    self.start_open_named(st.path, t, names);
                }
                crate::check::CheckAction::Cancel => {}
                crate::check::CheckAction::ChooseIndexHome => {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Keep indexes in this folder")
                        .pick_folder()
                    {
                        if crate::bridge::index_home_usable(&dir) {
                            self.prefs.index_home = Some(dir.clone());
                            crate::bridge::set_index_home(Some(dir.clone()));
                            st.index_dir = crate::bridge::store_dir_for(&st.path);
                            st.chosen_home = true;
                            st.home_note = Some(format!(
                                "New indexes are kept in {}. Files indexed before keep theirs where they are.",
                                dir.display()
                            ));
                        } else {
                            st.home_note = Some(format!(
                                "{} cannot hold indexes (nothing can be written there), so they stay in {}.",
                                dir.display(),
                                st.index_dir.parent().map(|p| p.display().to_string()).unwrap_or_default()
                            ));
                        }
                    }
                    self.check = Some(st);
                }
                crate::check::CheckAction::DefaultIndexHome => {
                    self.prefs.index_home = None;
                    crate::bridge::set_index_home(None);
                    st.index_dir = crate::bridge::store_dir_for(&st.path);
                    st.chosen_home = false;
                    st.home_note =
                        Some("New indexes are kept in the app's own folder again.".to_string());
                    self.check = Some(st);
                }
                crate::check::CheckAction::None => self.check = Some(st),
            }
        }
        let snap = self.bridge.snapshot();
        // a made column saved or removed: say where, and that the file is unchanged
        if let Some(src) = snap.source.as_ref() {
            match self.seen_vcols.as_ref() {
                Some((p, before)) if *p == src.path && *before != snap.saved_cols => {
                    let (added, removed) = column_changes(before, &snap.saved_cols);
                    let list = |v: &[String]| v.join(", ");
                    let msg = if !added.is_empty() {
                        format!(
                            "{}  Column {} saved with this file's index, ready next time. Your file is unchanged.",
                            ph::CHECK_CIRCLE,
                            list(&added)
                        )
                    } else if !removed.is_empty() {
                        format!(
                            "{}  Column {} removed from this file's index. Your file is unchanged.",
                            ph::CHECK_CIRCLE,
                            list(&removed)
                        )
                    } else {
                        format!(
                            "{}  Made columns saved. Your file is unchanged.",
                            ph::CHECK_CIRCLE
                        )
                    };
                    let now = ctx.input(|i| i.time);
                    self.notice = Some((msg, now, crate::bridge::store_dir_for(&src.path)));
                    self.seen_vcols = Some((src.path.clone(), snap.saved_cols.clone()));
                }
                Some((p, _)) if *p == src.path => {}
                _ => self.seen_vcols = Some((src.path.clone(), snap.saved_cols.clone())),
            }
        }
        // a wide file, fully profiled, its columns not arranged by the user: the
        // smart view (the anchor and the most telling columns), said in a pill
        if let (Some(_), Some(src)) = (snap.col_profile.as_ref(), snap.source.as_ref()) {
            let key = src.path.to_string_lossy().into_owned();
            let ready = self.grid.order.len() == snap.columns.len() && !snap.columns.is_empty();
            if ready && self.smart_for.as_deref() != Some(key.as_str()) {
                self.smart_for = Some(key.clone());
                let arranged =
                    !self.grid.hidden.is_empty() || self.grid.order != self.grid.default_order;
                if snap.columns.len() > 12
                    && !arranged
                    && !self.prefs.all_columns_files.contains(&key)
                {
                    self.apply_smart(&snap);
                }
            }
        }
        if snap.source.as_ref().map(|s| &s.path) != self.seen_source.as_ref() {
            self.seen_source = snap.source.as_ref().map(|s| s.path.clone());
        }
        if let Some((q, newest_first)) = self.seek.clone() {
            if let Some((_, n)) = snap.preview.as_ref().filter(|(pq, _)| *pq == q) {
                let last = snap.matched.saturating_sub(1);
                let top = if newest_first {
                    snap.matched.saturating_sub(*n)
                } else {
                    *n
                };
                self.grid.top = top.min(last);
                self.seek = None;
            }
        }
        if snap.generation != self.seen_generation {
            // a query just ran: it goes in the workspace's history
            if let (Some(src), None) = (snap.source.as_ref(), snap.query_error.as_ref()) {
                crate::workspace::note(
                    &mut self.prefs.query_log,
                    crate::workspace::Ran {
                        query: snap.query.trim().to_string(),
                        rows: snap.matched,
                        of: snap.total_rows,
                        file: src.name.clone(),
                        at: now_secs(),
                    },
                );
            }
            self.seen_generation = snap.generation;
        }
        if let Some(t) = snap.tags.as_ref() {
            if self.tags_for.as_ref() != Some(&t.source) {
                self.tags = t.tags.clone();
                self.tags_stale = t.source_changed;
                self.tags_for = Some(t.source.clone());
            }
        }
        let has_source = snap.source.is_some();
        // set anew by any "show these rows" this frame
        self.hide_sheet = false;
        self.stress_drive(ctx, &snap);
        self.handle_row_keys(ctx, &snap);

        // Row view always shows the selected row: when the rows changed under
        // it (a filter, a sort, another list of rows) the row is asked again
        if self.record_open {
            if let Some(id) = self.selected_row_id(&snap) {
                if snap.record.as_ref().map(|r| r.row) != Some(id) && self.record_asked != Some(id) {
                    self.record_asked = Some(id);
                    self.bridge.send(Cmd::Record(id));
                }
            }
        }
        // what each condition keeps alone: asked one at a time, for the search on screen
        if self.cover.as_ref().is_some_and(|c| c.0 != self.effective_query()) {
            self.cover = None;
        }
        if let Some((_, parts, counts, asked)) = self.cover.as_mut() {
            if let (Some(i), Some((q, n))) = (*asked, snap.preview.as_ref()) {
                if parts.get(i).is_some_and(|p| p.query == *q) {
                    counts[i] = Some(*n);
                    *asked = None;
                }
            }
            if asked.is_none() {
                if let Some(i) = counts.iter().position(|c| c.is_none()) {
                    *asked = Some(i);
                    self.bridge.send(Cmd::Count(parts[i].query.clone()));
                }
            }
        }
        // a column just made is shown: the grid scrolls to it and its name flashes
        if let Some(name) = self.show_made.as_ref() {
            if let Some(c) = snap.columns.iter().position(|n| n == name) {
                self.grid.hidden.remove(name);
                self.grid.goto = Some(c);
                self.show_made = None;
            }
        }
        // the first seconds: the window is asked to fill the screen until it does
        if self.fill_screen > 0 {
            if ctx.input(|i| i.time) > 4.0 {
                self.fill_screen = 0;
            }
            let (max, outer, monitor) = ctx.input(|i| {
                let v = i.viewport();
                (
                    v.maximized.unwrap_or(false),
                    v.outer_rect.map(|r| r.size()),
                    v.monitor_size,
                )
            });
            if !max {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            } else if let (Some(o), Some(m)) = (outer, monitor) {
                // marked maximised at an ordinary size: taken back to a plain
                // window, so the next frame's request really maximises it
                if self.fill_tries > 0 && crate::fillscreen::short_of_the_screen(o, m) {
                    self.fill_tries -= 1;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                }
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        // closing the window asks first, so a slip of the mouse does not end a
        // long look through a file (the question can be switched off)
        if ctx.input(|i| i.viewport().close_requested())
            && !self.close_ok
            && self.prefs.ask_on_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_ask = true;
        }
        if self.close_ask {
            let pal = self.pal();
            let (mut close, mut stay) = (false, false);
            let mut never = !self.prefs.ask_on_close;
            egui::Modal::new(egui::Id::new("close-ask")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new("Close LogSeek Studio?").font(fonts::strong(15.0)));
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("Your search, filters, tags and the columns you made are kept, and come back when you open the file again.")
                            .color(pal.text2),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let b = primary_button(ui, &pal, "Close");
                    // Enter closes, so the button holds the keyboard from the start
                    b.request_focus();
                    close = b.clicked();
                    stay = ui.button("Stay").clicked();
                });
                ui.add_space(6.0);
                ui.checkbox(&mut never, "Do not ask again")
                    .on_hover_text("The window then closes at once");
            });
            self.prefs.ask_on_close = !never;
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                stay = true;
            }
            if close {
                self.close_ask = false;
                self.close_ok = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if stay {
                self.close_ask = false;
            }
        }
        let title = window_title(snap.source.as_ref().map(|s| s.name.as_str()));
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        self.top_bar(ui, &pal, &snap);
        self.last_sort = snap.sort.clone();
        // a copy the engine finished: onto the clipboard, with a word in the status bar
        if let Some(c) = snap
            .copied
            .as_ref()
            .filter(|c| c.id == self.copy_id && c.id != self.copy_done)
        {
            self.copy_done = c.id;
            let now = ctx.input(|i| i.time);
            self.flash = Some(match (&c.text, &c.error) {
                (Some(t), _) => {
                    // dev switch: the copied text into a file instead of the clipboard
                    match crate::dev::var("LOGSEEK_SHOT_COPYTO") {
                        Ok(f) => drop(std::fs::write(f, t)),
                        Err(_) => ctx.copy_text(t.clone()),
                    }
                    (
                        format!(
                            "{}  {} {} copied",
                            ph::CHECK_CIRCLE,
                            group(c.rows),
                            if c.rows == 1 { "row" } else { "rows" }
                        ),
                        now,
                        false,
                    )
                }
                (None, e) => (
                    format!(
                        "Not copied: {}",
                        e.as_deref().unwrap_or("it could not be read")
                    ),
                    now,
                    true,
                ),
            });
        }
        self.tab_strip(ui, &pal, &snap);
        if has_source {
            self.status_bar(ui, &pal, &snap);
            self.notices(&ctx, &pal, &snap);
            // plain words: the file's words asked for once per file (in the
            // background), then the line under the search box
            if snap.total_rows > 0 && snap.phase.settled() {
                let key = snap.total_rows ^ (snap.columns.len() as u64) << 40;
                if self.lexicon_for != Some(key) {
                    self.lexicon_for = Some(key);
                    self.bridge.send(Cmd::Lexicon);
                }
            }
            let q = self.query.clone();
            if let crate::plainline::PlainAction::Use(search, summary) =
                self.plain.show(ui, &pal, &snap, &self.bridge, &q)
            {
                self.push_history();
                self.query = search;
                self.maybe_send_query(true);
                if let Some(s) = summary.filter(|s| !s.by.is_empty()) {
                    let m = match s.measure.as_str() {
                        "different" => 1,
                        "total" => 2,
                        "average" => 3,
                        "smallest" => 4,
                        "largest" => 5,
                        _ => 0,
                    };
                    let spec = format!(
                        "{}|{}|{}|{}|{}",
                        s.by[0],
                        s.by.get(1).cloned().unwrap_or_default(),
                        m,
                        s.column.clone().unwrap_or_default(),
                        s.per
                            .map(|p| (p / 1_000_000).to_string())
                            .unwrap_or_default()
                    );
                    self.summary = Some(crate::summary::SummaryState::from_spec(
                        &spec,
                        &snap.columns,
                    ));
                }
            }
            self.chips_bar(ui, &pal, &snap);
            self.remove_window(ui, &snap.saved_cols);
            self.rename_window(ui, &snap);
            // dev switch: LOGSEEK_SHOT_REINDEX reads the file again once it is in
            if crate::dev::var_os("LOGSEEK_SHOT_REINDEX").is_some() && snap.phase.settled() {
                std::env::remove_var("LOGSEEK_SHOT_REINDEX");
                if let Some(s) = &snap.source {
                    self.bridge.send(Cmd::Reindex(s.path.clone()));
                }
            }
            // dev switch: LOGSEEK_SHOT_SCRATCH opens "Start from scratch" once the file is in
            if crate::dev::var_os("LOGSEEK_SHOT_SCRATCH").is_some() && snap.phase.settled() {
                std::env::remove_var("LOGSEEK_SHOT_SCRATCH");
                self.scratch = Some([true, false, false, false]);
            }
            self.came_back_line(ui, &pal, &snap);
            self.scratch_window(ui, &snap);
            // dev switch: LOGSEEK_SHOT_RENAME="old|new" opens the rename window
            if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_RENAME") {
                let (old, new) = spec.split_once('|').unwrap_or((spec.as_str(), ""));
                if snap.saved_cols.iter().any(|(c, _)| c == old) && snap.phase.settled() {
                    self.rename_ask = Some((old.to_string(), new.to_string()));
                    std::env::remove_var("LOGSEEK_SHOT_RENAME");
                }
            }
            // dev switch: LOGSEEK_SHOT_REMOVE="column" asks to remove it once the columns are in
            if let Ok(n) = crate::dev::var("LOGSEEK_SHOT_REMOVE") {
                if snap.saved_cols.iter().any(|(c, _)| *c == n) && snap.phase.settled() {
                    std::env::remove_var("LOGSEEK_SHOT_REMOVE");
                    self.ask_remove(n, &snap.saved_cols);
                }
            }
            self.raw_columns_strip(ui, &pal, &snap);
            if !snap.column_problems.is_empty() {
                // made columns left out of the view, each with its reason in full
                egui::Panel::top("col-problems")
                    .frame(
                        Frame::new()
                            .fill(pal.bg)
                            .inner_margin(Margin::symmetric(12, 4)),
                    )
                    .show(ui, |ui| {
                        for (name, why) in &snap.column_problems {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{}  Made column {name} is not shown: {why}",
                                        ph::WARNING
                                    ))
                                    .color(pal.danger),
                                );
                                if ui
                                    .small_button("Remove")
                                    .on_hover_text(format!(
                                        "Remove the made column {name} (the file is unchanged)"
                                    ))
                                    .clicked()
                                {
                                    self.ask_remove(name.clone(), &snap.saved_cols);
                                }
                            });
                        }
                    });
            }
            // the import met something it could not read cleanly, or a request
            // crashed inside the engine: said in full above the rows
            let mut said: Vec<(String, bool)> = snap
                .import_notes
                .iter()
                .map(|n| (n.clone(), false))
                .collect();
            if let Some(e) = snap.engine_note.as_ref() {
                said.insert(
                    0,
                    (
                        format!("The last request stopped with an error inside LogSeek and was dropped ({e}). Your file is untouched. Try it again; if it stops again, reopen the file."),
                        true,
                    ),
                );
            }
            // per file, not per search: a dismissed note stays away while the file is open
            let key = (snap.source.as_ref().map(|s| s.path.clone()), said.len());
            if !said.is_empty() && self.notes_dismissed.as_ref() != Some(&key) {
                egui::Panel::top("import-notes")
                    .frame(
                        Frame::new()
                            .fill(pal.surface2)
                            .inner_margin(Margin::symmetric(12, 6)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new(ph::WARNING).color(pal.warning));
                            let room = ui.available_width() - 110.0;
                            ui.vertical(|ui| {
                                ui.set_max_width(room.max(200.0));
                                if said.iter().any(|s| !s.1) {
                                    ui.label(
                                        RichText::new("Not everything in this file was read cleanly:")
                                            .color(pal.text),
                                    );
                                }
                                for (line, serious) in &said {
                                    ui.add(
                                        egui::Label::new(RichText::new(line).color(if *serious {
                                            pal.danger
                                        } else {
                                            pal.text2
                                        }))
                                        .wrap(),
                                    );
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                if ui
                                    .button("Dismiss")
                                    .on_hover_text("Hide this note for this file")
                                    .clicked()
                                {
                                    self.notes_dismissed = Some(key);
                                }
                            });
                        });
                    });
            }
            if let Some(e) = snap.column_error.as_ref() {
                egui::Panel::top("col-error")
                    .exact_size(26.0)
                    .frame(
                        Frame::new()
                            .fill(pal.bg)
                            .inner_margin(Margin::symmetric(12, 0)),
                    )
                    .show(ui, |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{}  Column not added — {e}", ph::WARNING))
                                    .color(pal.danger),
                            );
                        });
                    });
            }
            // dev switch: LOGSEEK_SHOT_PICK="only|col|value" or "exclude|col|value" picks as a click does
            if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_PICK") {
                if snap.phase.settled() && !snap.columns.is_empty() {
                    std::env::remove_var("LOGSEEK_SHOT_PICK");
                    let parts: Vec<&str> = spec.splitn(3, '|').collect();
                    if let [how, col, value] = parts[..] {
                        let pick = if how == "exclude" {
                            crate::fields::Pick::Exclude(col.into(), value.into())
                        } else {
                            crate::fields::Pick::Only(col.into(), value.into())
                        };
                        crate::fields::apply(&pick, &mut self.filters);
                        self.maybe_send_query(true);
                    }
                }
            }
            if let Ok(name) = crate::dev::var("LOGSEEK_SHOT_FIELD") {
                // screenshots only (preferences are not saved then): the panel shown
                self.prefs.fields_open = Some(true);
                if self.fields.open.is_none() {
                    self.fields.open = snap.columns.iter().position(|c| *c == name);
                }
            }
            if self.fields_shown(ui.ctx()) && !snap.columns.is_empty() {
                let mut pick = None;
                // a drawer on the right: the rows keep the left edge
                egui::Panel::right("fields")
                    .resizable(true)
                    .default_size(270.0)
                    .min_size(200.0)
                    .max_size(460.0)
                    .frame(Frame::new().fill(pal.surface).inner_margin(Margin {
                        left: 10,
                        right: 6,
                        top: 0,
                        bottom: 6,
                    }))
                    .show(ui, |ui| {
                        let r = ui.max_rect();
                        ui.painter().line_segment(
                            [
                                r.left_top() - egui::vec2(10.0, 0.0),
                                r.left_bottom() + egui::vec2(-10.0, 6.0),
                            ],
                            Stroke::new(1.0, pal.hairline),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let n_tagged = self.tags.len();
                            for (tab, label) in [
                                (LeftTab::Fields, "Fields".to_string()),
                                (LeftTab::Tags, format!("Tags  {}", group(n_tagged as u64))),
                            ] {
                                let on = self.left_tab == tab;
                                let b = egui::Button::new(RichText::new(label).color(if on {
                                    pal.text
                                } else {
                                    pal.text2
                                }))
                                .fill(if on { pal.selection } else { pal.surface })
                                .stroke(if on {
                                    Stroke::new(1.0, pal.accent_ink)
                                } else {
                                    Stroke::new(1.0, pal.hairline)
                                })
                                .min_size(egui::vec2(0.0, 26.0));
                                if ui.add(b).clicked() {
                                    self.left_tab = tab;
                                }
                            }
                        });
                        match self.left_tab {
                            LeftTab::Fields => {
                                pick = crate::fields::show(
                                    ui,
                                    &pal,
                                    &snap,
                                    &mut self.fields,
                                    &self.bridge,
                                    &self.grid.order,
                                    &mut self.grid.hidden,
                                )
                            }
                            LeftTab::Tags => self.tags_panel(ui, &pal, &snap),
                        }
                    });
                if let Some(crate::fields::Pick::EveryValue { col, rare }) = pick {
                    // the full list opens beside the drawer, where the pointer is
                    let at = ui
                        .ctx()
                        .pointer_latest_pos()
                        .map(|p| p - egui::vec2(360.0, 0.0))
                        .unwrap_or(egui::pos2(400.0, 160.0));
                    self.open_values(&snap, col, at, rare);
                } else if let Some(p) = pick {
                    // the column's filter (a sentence chip), never search text
                    crate::fields::apply(&p, &mut self.filters);
                    self.maybe_send_query(true);
                }
            }
            // the time spine: time runs down a rail beside the rows (none
            // without a time column — the rows get the room)
            let has_time = snap.time_col.is_some() || snap.columns.is_empty();
            if has_time {
                egui::Panel::left("spine")
                    .exact_size(crate::spine::rail_width(self.prefs.spine_folded))
                    .resizable(false)
                    .frame(Frame::new().fill(pal.bg))
                    .show(ui, |ui| {
                        let r = ui.max_rect();
                        ui.painter().line_segment(
                            [r.right_top(), r.right_bottom()],
                            Stroke::new(1.0, pal.hairline),
                        );
                        // folded away: only the button that brings it back
                        if self.prefs.spine_folded {
                            let b = egui::Rect::from_min_size(
                                egui::pos2(r.left() + 3.0, r.top() + 4.0),
                                egui::vec2(28.0, 24.0),
                            );
                            let mut ui_f = ui.new_child(egui::UiBuilder::new().max_rect(b));
                            if icon_button(
                                &mut ui_f,
                                &pal,
                                ph::CARET_DOUBLE_RIGHT,
                                "Show the time rail  (Ctrl+Shift+B)",
                            )
                            .clicked()
                            {
                                self.prefs.spine_folded = false;
                            }
                            return;
                        }
                        // fold it away, at the rail's top right
                        let fb = egui::Rect::from_min_size(
                            egui::pos2(r.right() - 34.0, r.top() + 4.0),
                            egui::vec2(28.0, 24.0),
                        );
                        let mut ui_fb = ui.new_child(egui::UiBuilder::new().max_rect(fb));
                        if icon_button(
                            &mut ui_fb,
                            &pal,
                            ph::CARET_DOUBLE_LEFT,
                            "Fold the time rail away, so the rows take the width  (Ctrl+Shift+B)",
                        )
                        .clicked()
                        {
                            self.prefs.spine_folded = true;
                        }
                        // the rail's tools: timeline signals, the calendar
                        let sb = egui::Rect::from_min_size(
                            egui::pos2(r.left() + 6.0, r.top() + 4.0),
                            egui::vec2(28.0, 24.0),
                        );
                        let mut ui_s = ui.new_child(egui::UiBuilder::new().max_rect(sb));
                        if icon_button(
                            &mut ui_s,
                            &pal,
                            ph::WAVEFORM,
                            "Timeline signals: silences far longer than usual, lasting changes in the rate of rows, values that come like clockwork",
                        )
                        .clicked()
                        {
                            self.rhythm_win = if self.rhythm_win.is_some() {
                                None
                            } else {
                                Some(Default::default())
                            };
                        }
                        let b = egui::Rect::from_min_size(
                            egui::pos2(r.left() + 38.0, r.top() + 4.0),
                            egui::vec2(28.0, 24.0),
                        );
                        let mut ui_b = ui.new_child(egui::UiBuilder::new().max_rect(b));
                        if icon_button(
                            &mut ui_b,
                            &pal,
                            ph::CALENDAR_DOTS,
                            "Pick days on a calendar (rows per day)",
                        )
                        .clicked()
                        {
                            self.cal_open = !self.cal_open;
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(egui::Id::new("calendar-anchor"), b.right_top())
                            });
                            if self.cal_open {
                                let rest = leading_range(&self.query)
                                    .map(|(_, _, r)| r.to_string())
                                    .unwrap_or_else(|| self.query.clone());
                                self.cal_query =
                                    crate::filters::compose(rest.trim(), &self.filters, None);
                                self.cal = Default::default();
                                self.bridge.send(Cmd::Calendar(self.cal_query.clone()));
                            }
                        }
                        let rail = egui::Rect::from_min_max(
                            egui::pos2(r.left(), r.top() + crate::spine::SPINE_TOOLS_H),
                            r.max,
                        );
                        let active = leading_range(&self.query).map(|(a, b, _)| (a, b));
                        // the rows run in time order (file order or sorted by
                        // time): a time on the rail is a place in the rows
                        let tc_name = snap.time_col.and_then(|c| snap.columns.get(c));
                        // a time is a place in the rows only when they run in
                        // time order (sorted by time, or written that way)
                        let (seekable, newest_first) = match crate::grid::time_ordered(
                            &snap.sort,
                            tc_name.map(String::as_str),
                            snap.time_order,
                        ) {
                            Some((_, newest)) => (true, newest),
                            None => (false, false),
                        };
                        // dev switch: LOGSEEK_SHOT_SPINE_KEYS=<stretch> gives the rail the keyboard
                        if let Ok(k) = crate::dev::var("LOGSEEK_SHOT_SPINE_KEYS") {
                            if snap.hist.is_some() && snap.phase.settled() {
                                std::env::remove_var("LOGSEEK_SHOT_SPINE_KEYS");
                                let k: usize = k.parse().unwrap_or(0);
                                ui.ctx().memory_mut(|m| m.request_focus(crate::spine::rail_id()));
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(crate::spine::rail_id().with("cursor"), k)
                                });
                            }
                        }
                        // dev switch: LOGSEEK_SHOT_SPINE_SEEK=<time> clicks the rail at that time once
                        let shot_seek = crate::dev::var("LOGSEEK_SHOT_SPINE_SEEK")
                            .ok()
                            .filter(|_| snap.hist.is_some() && snap.phase.settled())
                            .and_then(|t| {
                                std::env::remove_var("LOGSEEK_SHOT_SPINE_SEEK");
                                logseek_core::time::parse_time(&t)
                            });
                        match shot_seek.map(crate::spine::SpineAction::Seek).unwrap_or_else(|| crate::spine::show(
                            ui,
                            &pal,
                            snap.hist.as_deref().filter(|h| {
                                h.generation <= snap.generation && snap.query_error.is_none()
                            }),
                            snap.matched,
                            snap.time_col.is_some(),
                            active,
                            // where the rows on screen sit: only a place when they run in time order
                            seekable.then(|| self.visible_window(&snap)).flatten(),
                            seekable,
                            &snap.tag_times,
                            rail,
                        )) {
                            crate::spine::SpineAction::Seek(t) => {
                                // rows before `t` in this result: that many rows down
                                // is the first row at or after it
                                let eff = self.effective_query();
                                let (lo, rest) = match leading_range(&eff) {
                                    Some((a, _, r)) => (a, r.to_string()),
                                    None => (
                                        snap.hist.as_ref().map(|h| h.t0_us).unwrap_or(t),
                                        eff.clone(),
                                    ),
                                };
                                if t <= lo {
                                    self.grid.top = if newest_first {
                                        snap.matched.saturating_sub(1)
                                    } else {
                                        0
                                    };
                                } else {
                                    let q = format!("@{}..{} {}", iso(lo), iso(t - 1), rest.trim())
                                        .trim_end()
                                        .to_string();
                                    self.bridge.send(Cmd::Count(q.clone()));
                                    self.seek = Some((q, newest_first));
                                }
                            }
                            crate::spine::SpineAction::Scroll(r) => {
                                let id = egui::Id::new("spine-scroll-left-over");
                                let acc: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0) + r;
                                let whole = acc.trunc();
                                ui.ctx().data_mut(|d| d.insert_temp(id, acc - whole));
                                let last = snap.matched.saturating_sub(1) as i64;
                                self.grid.top = (self.grid.top as i64 + whole as i64).clamp(0, last.max(0)) as u64;
                            }
                            crate::spine::SpineAction::Range(a, b) => {
                                let rest = leading_range(&self.query)
                                    .map(|(_, _, r)| r.to_string())
                                    .unwrap_or_else(|| self.query.clone());
                                self.query =
                                    format!("@{}..{} {}", iso(a), iso(b), rest.trim_start())
                                        .trim_end()
                                        .to_string();
                                self.maybe_send_query(true);
                            }
                            crate::spine::SpineAction::Clear => {
                                if let Some((_, _, rest)) = leading_range(&self.query) {
                                    self.query = rest.trim_start().to_string();
                                    self.maybe_send_query(true);
                                }
                            }
                            crate::spine::SpineAction::None => {}
                        }
                    });
            }
            if self.record_open && self.prefs.inspector_docked {
                self.record_viewer(ui, &pal, &snap);
            }
            egui::CentralPanel::default()
                .frame(Frame::new().fill(pal.bg))
                .show(ui, |ui| {
                    if let Some(mut st) = self.columns_view.take() {
                        match crate::columnsview::show(
                            ui,
                            &pal,
                            &snap,
                            &mut self.grid.hidden,
                            &mut st,
                        ) {
                            crate::columnsview::Act::Back => {}
                            crate::columnsview::Act::GoTo(c) => {
                                if let Some(n) = snap.columns.get(c) {
                                    self.grid.hidden.remove(n);
                                }
                                self.grid.goto = Some(c);
                            }
                            crate::columnsview::Act::None => self.columns_view = Some(st),
                        }
                    } else {
                        self.workspace(ui, &pal, &snap)
                    }
                });
            if self.record_open && !self.prefs.inspector_docked {
                self.record_viewer(ui, &pal, &snap);
            }
        } else {
            egui::CentralPanel::default()
                .frame(Frame::new().fill(pal.bg))
                .show(ui, |ui| {
                    self.start_screen(ui, &pal, &snap);
                    self.case_sheet(ui, &pal, &snap);
                });
        }
        if matches!(snap.phase, Phase::Importing { .. }) || snap.busy {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if let Some(st) = self.stress.as_mut() {
            st.end_frame(self.grid.waited);
        }
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Arc<Snapshot>) {
        egui::Panel::top("top")
            .exact_size(TOP_H)
            .frame(
                Frame::new()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().line_segment(
                    [full.left_bottom(), full.right_bottom()],
                    Stroke::new(1.0, pal.hairline),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    // the window's own title bar already names the app (and the
                    // file): the bar here starts with the mark, not a second title
                    let (r, logo) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::hover());
                    if let Some(t) = &self.logo {
                        ui.painter().image(
                            t.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    logo.on_hover_text("LogSeek Studio");
                    ui.add_space(6.0);
                    let (sr, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), Sense::hover());
                    ui.painter()
                        .rect_filled(sr, CornerRadius::ZERO, pal.hairline);
                    ui.add_space(4.0);
                    // source switcher (today: shows the source, click opens another)
                    let in_case = self
                        .prefs
                        .cases
                        .get(self.prefs.case)
                        .filter(|c| !c.files.is_empty())
                        .map(|c| format!("  ·  {} ({})", c.name, crate::keep::plural(c.files.len(), "file", "files")))
                        .unwrap_or_default();
                    // read out of a zip or a workbook: named by what it is inside it (the
                    // packed file's own name is on hover and on the button beside it)
                    let src_path = snap.source.as_ref().map(|s| s.path.clone());
                    if self.from_book.as_ref().map(|(p, _)| p) != src_path.as_ref() {
                        self.from_book = src_path.map(|p| {
                            let from = crate::book::origin(&p);
                            (p, from)
                        });
                    }
                    // with several files open, the name is what tells this file
                    // from the others (as on its tab); the full name is on hover
                    let shown_name = match (&snap.source, self.tabs.current) {
                        (Some(_), Some(id)) if self.tabs.list.len() > 1 => {
                            let l = self.tabs.label(id);
                            (!l.is_empty()).then_some(l)
                        }
                        _ => self
                            .from_book
                            .as_ref()
                            .and_then(|(_, from)| from.as_ref().map(|(_, inner)| inner.clone()))
                            .or_else(|| {
                                snap.source.as_ref().and_then(|s| crate::book::inner_name(&s.path))
                            }),
                    };
                    let label = match &snap.source {
                        Some(s) => format!(
                            "{}  {}  ·  {}{in_case}  {}",
                            ph::FILE_TEXT,
                            shown_name.as_deref().unwrap_or(&s.name),
                            human_bytes(s.bytes),
                            ph::CARET_DOWN
                        ),
                        None => format!("{}  Open a file{in_case}", ph::FOLDER_OPEN),
                    };
                    let has_file = snap.source.is_some();
                    let full_name = snap.source.as_ref().map(|s| s.name.clone());
                    // the name takes at most two fifths of the bar and wraps
                    // inside that, so the search box and the buttons never lose
                    // their place to a long file name
                    let name_room = (ui.available_width() * 0.4).max(180.0);
                    let src_btn = ui
                        .scope(|ui| {
                            ui.set_max_width(name_room);
                            ui.add(
                                egui::Button::new(RichText::new(label).color(pal.text))
                                    .frame(false)
                                    .wrap_mode(egui::TextWrapMode::Wrap)
                                    .min_size(egui::vec2(0.0, 28.0)),
                            )
                        })
                        .inner;
                    // several files as one view: every one of them on hover
                    let set_files = snap
                        .source
                        .as_ref()
                        .filter(|s| crate::fileset::is_set(&s.path))
                        .map(|s| s.path.clone());
                    if src_btn
                        .on_hover_ui(|ui| {
                            if let Some(n) = &full_name {
                                ui.label(n.as_str());
                                ui.separator();
                            }
                            if let Some(p) = &set_files {
                                ui.label(format!(
                                    "Open as one view: {}\nEach row's file is in the _file column.",
                                    crate::fileset::display_name(p)
                                ));
                                ui.separator();
                            }
                            ui.label(if has_file {
                                "This case's files, and opening another file  (Ctrl+O opens one directly; pick several to see them as one)"
                            } else {
                                "Choose a file to open  (Ctrl+O; pick several to see them as one)"
                            });
                        })
                        .clicked()
                    {
                        if has_file {
                            self.case_open = !self.case_open;
                        } else {
                            self.pick_file();
                        }
                    }
                    // read out of a zip or a workbook: the rest of it is one click away
                    let more_from = self
                        .from_book
                        .as_ref()
                        .and_then(|(_, from)| from.as_ref().map(|(b, _)| b.clone()))
                        .filter(|b| crate::book::holds_many(b) && b.exists());
                    if let Some(book) = more_from {
                        let workbook = logseek_core::ingest::sheets::is_workbook(&book);
                        let doc = crate::book::is_json_doc(&book);
                        let words = if workbook {
                            "More sheets"
                        } else if doc {
                            "More from this file"
                        } else {
                            "More from this zip"
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{}  {words}", ph::FOLDER_OPEN))
                                        .color(pal.text),
                                )
                                .min_size(egui::vec2(0.0, 28.0)),
                            )
                            .on_hover_text(format!(
                                "Open another {} of {}\nEach one opens as its own tab.",
                                if workbook {
                                    "sheet"
                                } else if doc {
                                    "part"
                                } else {
                                    "file"
                                },
                                book.display()
                            ))
                            .clicked()
                        {
                            self.open(book);
                        }
                    }
                    ui.add_space(8.0);
                    // right-side controls first so the query bar takes the rest
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (glyph, tip) = if self.prefs.dark {
                            (ph::SUN, "Light theme")
                        } else {
                            (ph::MOON, "Dark theme")
                        };
                        if icon_button(ui, pal, glyph, tip).clicked() {
                            self.prefs.dark = !self.prefs.dark;
                        }
                        if snap.source.is_some() {
                            let mut run: Option<crate::palette::Command> = None;
                            use crate::palette::Command as C;
                            // the view: theme aside, everything about how things look
                            ui.menu_button(RichText::new(format!("View  {}", ph::CARET_DOWN)).color(pal.text), |ui| {
                                ui.set_min_width(260.0);
                                if ui.button(format!("{}  Colour rules", ph::PALETTE)).clicked() {
                                    run = Some(C::ColourRules);
                                    ui.close();
                                }
                                ui.menu_button(format!("{}  Saved searches", ph::BOOKMARK_SIMPLE), |ui| {
                                    self.saved_searches_menu(ui, pal, snap);
                                });
                                if ui.button(format!("{}  Query workspace   Ctrl+E", ph::TERMINAL_WINDOW)).clicked() {
                                    run = Some(C::QueryWorkspace);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Search every cell   Ctrl+Shift+F", ph::LIST_MAGNIFYING_GLASS)).clicked() {
                                    run = Some(C::SearchCells);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(if self.prefs.compact { "Comfortable rows" } else { "Compact rows" }).clicked() {
                                    run = Some(C::Density);
                                }
                                if ui.button(if self.prefs.grid_lines { "Hide grid lines" } else { "Show grid lines" }).clicked() {
                                    run = Some(C::GridLines);
                                }
                                if ui
                                    .button(if self.prefs.number_by_file {
                                        "Number rows as listed (1, 2, 3)"
                                    } else {
                                        "Number rows as in the file"
                                    })
                                    .on_hover_text("The # column: count the rows as they are listed now, in the current sort and filters, or show each row's own number in the file. Row view always says both.")
                                    .clicked()
                                {
                                    self.prefs.number_by_file = !self.prefs.number_by_file;
                                    ui.close();
                                }
                                if ui
                                    .button(if self.prefs.spine_folded {
                                        "Show the time rail   Ctrl+Shift+B"
                                    } else {
                                        "Fold the time rail away   Ctrl+Shift+B"
                                    })
                                    .clicked()
                                {
                                    run = Some(C::FoldTimeRail);
                                }
                                ui.separator();
                                ui.label(RichText::new("Any action, by name:  Ctrl+K").font(theme::small_font()).color(pal.text2));
                            });
                            let shown = self.fields_shown(ui.ctx());
                            if ui
                                .selectable_label(shown, format!("{}  Fields", ph::SIDEBAR_SIMPLE))
                                .on_hover_text("The fields and tags panel  (Ctrl+B)")
                                .clicked()
                            {
                                run = Some(C::Fields);
                            }
                            if ui
                                .selectable_label(self.record_open, format!("{}  Row view", ph::ROWS))
                                .on_hover_text("Every field of the selected row  (Space)")
                                .clicked()
                            {
                                run = Some(C::RowView);
                            }
                            ui.menu_button(RichText::new(format!("Columns  {}", ph::CARET_DOWN)).color(pal.text), |ui| {
                                ui.set_min_width(280.0);
                                if snap.col_profile.is_some() && ui.button("Smart: the most telling columns").clicked() {
                                    run = Some(C::SmartColumns);
                                    ui.close();
                                }
                                if ui.button("Every column, in file order").clicked() {
                                    run = Some(C::EveryColumn);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Choose columns", ph::LIST_CHECKS)).clicked() {
                                    run = Some(C::ChooseColumns);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Columns view: every column at a glance   Ctrl+2", ph::TABLE)).clicked() {
                                    run = Some(C::ColumnsView);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(format!("{}  Add a column: split, combine, convert", ph::COLUMNS_PLUS_RIGHT)).clicked() {
                                    run = Some(C::AddColumn);
                                    ui.close();
                                }
                            });
                            if ui
                                .button(format!("{}  Summarise", ph::TABLE))
                                .on_hover_text("Group the rows by one or two columns and count, total or average")
                                .clicked()
                            {
                                run = Some(C::Summarise);
                            }
                            ui.menu_button(RichText::new(format!("Analyse  {}", ph::CARET_DOWN)).color(pal.text), |ui| {
                                ui.set_min_width(320.0);
                                ui.label(RichText::new("Compare and spot the unusual").font(theme::small_font()).color(pal.text2));
                                for (c, icon, words) in [
                                    (C::Unusual, ph::SEAL_WARNING, "Unusual rows"),
                                    (C::Explain, ph::SCALES, "What sets these rows apart?"),
                                    (C::WhatsNew, ph::SPARKLE, "What's new in this period?"),
                                ] {
                                    if ui.button(format!("{icon}  {words}")).clicked() {
                                        run = Some(c);
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                ui.label(RichText::new("Time").font(theme::small_font()).color(pal.text2));
                                if ui.button(format!("{}  Timeline signals", ph::WAVEFORM)).clicked() {
                                    run = Some(C::TimelineSignals);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Calendar", ph::CALENDAR_DOTS)).clicked() {
                                    run = Some(C::Calendar);
                                    ui.close();
                                }
                                ui.separator();
                                ui.label(RichText::new("Across rows").font(theme::small_font()).color(pal.text2));
                                if ui.button(format!("{}  Follow related rows", ph::TREE_STRUCTURE)).clicked() {
                                    run = Some(C::Follow);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Search a whole list of keywords", ph::LIST_BULLETS)).clicked() {
                                    run = Some(C::SearchList);
                                    ui.close();
                                }
                                ui.separator();
                                ui.label(RichText::new("One column: right-click its header (patterns, shapes, strange text, rare pairs)").font(theme::small_font()).color(pal.text2));
                            });
                            if let Some(c) = run {
                                let now = ui.input(|i| i.time);
                                self.run(c, snap, now);
                            }
                        }
                        ui.add_space(6.0);
                        if snap.source.is_some() {
                            self.query_bar(ui, pal, snap);
                        }
                    });
                });
            });
    }

    /// Saved searches: save the one in force, load or delete one.
    fn saved_searches_menu(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        ui.set_min_width(300.0);
        if self.any_filter()
            && ui
                .button(format!(
                    "{}  Save the current search and filters",
                    ph::FLOPPY_DISK
                ))
                .clicked()
        {
            self.preset_name = Some(String::new());
            ui.close();
        }
        ui.label(
            RichText::new("Your saved searches")
                .font(theme::small_font())
                .color(pal.text2),
        );
        if self.prefs.presets.is_empty() {
            ui.label(RichText::new("None yet").color(pal.muted));
        }
        let mut load = None;
        let mut delete = None;
        for pr in &self.prefs.presets {
            ui.horizontal(|ui| {
                if ui.button(&pr.name).clicked() {
                    load = Some(pr.clone());
                    ui.close();
                }
                if ui
                    .small_button(ph::TRASH)
                    .on_hover_text("Delete this saved search")
                    .clicked()
                {
                    delete = Some(pr.name.clone());
                }
            });
        }
        if let Some(pr) = load {
            self.apply_preset(&pr, snap);
        }
        if let Some(n) = delete {
            self.prefs.presets.retain(|x| x.name != n);
        }
    }

    /// Do one of the app's actions, from a menu or from Ctrl+K.
    /// A column's every value, counted exactly and paged: tick to keep or
    /// leave out, search, or write a condition (most rows first, or rarest).
    fn open_values(&mut self, snap: &Snapshot, col: usize, at: egui::Pos2, rare: bool) {
        let Some(name) = snap.columns.get(col).cloned() else {
            return;
        };
        let query = crate::filters::compose(&self.query, &self.filters, Some(&name));
        self.bridge.send(Cmd::Values {
            col,
            query: query.clone(),
            n: 500,
            rare,
            alpha: false,
        });
        self.values = Some(ValuesPopup {
            col,
            name,
            at,
            search: String::new(),
            sel: Default::default(),
            ready: false,
            query,
            sort: if rare {
                ValueSort::Rare
            } else {
                ValueSort::Count
            },
            preview_sent: String::new(),
            // a column of times opens on its conditions (after, before, on a
            // day, between): a list of every single moment is rarely the way in
            cond_tab: crate::grid::reads_as_time(&snap.col_types, snap.time_col, col),
            conds: Vec::new(),
            cal_row: None,
            cal: Default::default(),
            except: true,
            cache: Default::default(),
            cache_search: String::new(),
            asked: Default::default(),
            total: None,
            pending: None,
        });
    }

    fn run(&mut self, c: crate::palette::Command, snap: &Snapshot, now: f64) {
        use crate::palette::Command as C;
        match c {
            C::OpenFile => self.pick_file(),
            C::Summarise => {
                self.summary = match self.summary {
                    Some(_) => None,
                    None => Some(crate::summary::SummaryState::new(None, &snap.columns)),
                }
            }
            C::ChooseColumns => {
                self.colchooser = match self.colchooser {
                    Some(_) => None,
                    None => Some(Default::default()),
                }
            }
            C::FoldTimeRail => self.prefs.spine_folded = !self.prefs.spine_folded,
            C::ColumnsView => {
                self.columns_view = match self.columns_view {
                    Some(_) => None,
                    None => Some(Default::default()),
                }
            }
            C::SmartColumns => {
                if let Some(src) = &snap.source {
                    let key = src.path.to_string_lossy().into_owned();
                    self.prefs.all_columns_files.retain(|k| *k != key);
                }
                self.apply_smart(snap);
            }
            C::EveryColumn => {
                self.grid.hidden.clear();
                self.grid.order = self.grid.default_order.clone();
                if let Some(src) = &snap.source {
                    let key = src.path.to_string_lossy().into_owned();
                    if !self.prefs.all_columns_files.contains(&key) {
                        self.prefs.all_columns_files.push(key);
                    }
                }
            }
            C::AddColumn => {
                self.addcol = match self.addcol {
                    Some(_) => None,
                    None => Some(crate::addcol::AddColState::new(None, &snap.columns)),
                }
            }
            C::RowView => {
                self.record_open = !self.record_open;
                if self.record_open {
                    if self.grid.sel.is_none() {
                        self.grid.sel = Some(self.grid.top);
                    }
                    if let Some(r) = self.selected_row_id(snap) {
                        self.bridge.send(Cmd::Record(r));
                    }
                }
            }
            C::Fields => {
                let shown = self.prefs.fields_open.unwrap_or(false);
                self.prefs.fields_open = Some(!shown);
            }
            C::Unusual => {
                if snap.matched > 0 {
                    self.unusual_win = Some(Default::default());
                }
            }
            C::Explain | C::WhatsNew => {
                let period = logseek_core::query::parse(snap.query.trim())
                    .map(|q| q.time.is_some())
                    .unwrap_or(false);
                if c == C::WhatsNew && !period {
                    self.flash = Some((
                        "Keep a period first (drag down the time spine), then ask what is new in it.".to_string(),
                        now,
                        false,
                    ));
                } else {
                    let mut w = crate::explainwin::ExplainWin::default();
                    if c == C::WhatsNew {
                        w.against = crate::bridge::Against::Before;
                    } else if period {
                        w.against = crate::bridge::Against::OtherTimes;
                    }
                    self.explain_win = Some(w);
                }
            }
            C::TimelineSignals => {
                self.rhythm_win = if self.rhythm_win.is_some() {
                    None
                } else {
                    Some(Default::default())
                };
            }
            C::Calendar => {
                self.cal_open = !self.cal_open;
                if self.cal_open {
                    let rest = leading_range(&self.query)
                        .map(|(_, _, r)| r.to_string())
                        .unwrap_or_else(|| self.query.clone());
                    self.cal_query = crate::filters::compose(rest.trim(), &self.filters, None);
                    self.cal = Default::default();
                    self.bridge.send(Cmd::Calendar(self.cal_query.clone()));
                }
            }
            C::Follow => {
                self.follow = match self.follow {
                    Some(_) => None,
                    None => Some(self.new_follow(snap)),
                }
            }
            C::SearchList => self.list_win = Some(Default::default()),
            C::SearchCells => {
                self.search_win = match self.search_win.take() {
                    Some(_) => None,
                    None => {
                        // the words searched for at the top are offered in it
                        let terms = crate::filters::highlight_terms(&self.query);
                        let mut w = crate::searchwin::SearchWin::with_text(
                            terms.first().map_or("", String::as_str),
                        );
                        w.terms = terms;
                        Some(w)
                    }
                }
            }
            C::QueryWorkspace => {
                self.work = match self.work.take() {
                    Some(_) => None,
                    None => Some(crate::workspace::WorkState {
                        text: self.effective_query(),
                        focus: true,
                        ..Default::default()
                    }),
                }
            }
            C::ColourRules => self.colors_open = !self.colors_open,
            C::SaveSearch => {
                if self.any_filter() {
                    self.preset_name = Some(String::new());
                } else {
                    self.flash = Some((
                        "Nothing to save yet: search or filter first.".to_string(),
                        now,
                        false,
                    ));
                }
            }
            C::Theme => self.prefs.dark = !self.prefs.dark,
            C::Density => self.prefs.compact = !self.prefs.compact,
            C::GridLines => self.prefs.grid_lines = !self.prefs.grid_lines,
            C::Export => self.export_open = true,
        }
    }

    fn query_bar(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let w = ui.available_width().min(760.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 30.0), Sense::hover());
        let focused = ui.memory(|m| m.has_focus(self.query_id));
        let err = snap.query_error.is_some() && snap.query == self.sent_query;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), pal.bg);
        let border = if err {
            Stroke::new(1.5, pal.danger)
        } else if focused {
            Stroke::new(2.0, pal.focus)
        } else {
            Stroke::new(1.0, pal.control_border)
        };
        ui.painter()
            .rect_stroke(rect, CornerRadius::same(4), border, StrokeKind::Inside);
        ui.painter().text(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            ph::MAGNIFYING_GLASS,
            FontId::new(15.0, FontFamily::Proportional),
            pal.muted,
        );
        let inner = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 30.0, rect.top() + 3.0),
            egui::pos2(rect.right() - 58.0, rect.bottom() - 3.0),
        );
        // how to match: capitals, part or whole, a word, the start or the end, one column
        {
            let b = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 43.0, rect.center().y),
                egui::vec2(24.0, 24.0),
            );
            let on = self.match_sheet.is_some();
            let r = ui.interact(b, self.query_id.with("match"), Sense::click());
            if r.hovered() || on {
                ui.painter().rect_filled(
                    b,
                    CornerRadius::same(4),
                    if on { pal.selection } else { pal.surface2 },
                );
            }
            ui.painter().text(
                b.center(),
                Align2::CENTER_CENTER,
                ph::SLIDERS_HORIZONTAL,
                FontId::new(14.0, FontFamily::Proportional),
                if on { pal.accent_ink } else { pal.text2 },
            );
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Build a search")
            });
            if r
                .on_hover_text("Build a search: several conditions, each chosen, none written (starts with, is one of, is shaped like an example, more than, empty)")
                .clicked()
            {
                self.match_sheet = match self.match_sheet.take() {
                    Some(b) => {
                        self.builder_last = Some(b.build);
                        None
                    }
                    None => {
                        // the conditions as they were left; else one condition, started
                        // from the search box when it holds one plain piece of text
                        let q = leading_range(&self.query)
                            .map(|(_, _, r)| r)
                            .unwrap_or(&self.query)
                            .trim();
                        let plain = !q.is_empty()
                            && !q.contains(['=', ':', '~', '<', '>', '|', '(', ')', '"', '`', '@']);
                        let build = self.builder_last.clone().unwrap_or_else(|| {
                            let mut b = crate::builder::Build::default();
                            if plain {
                                b.rows[0].text = q.to_string();
                            }
                            b
                        });
                        Some(BuilderUi::new(build))
                    }
                };
            }
        }
        self.match_panel(ui, pal, snap, rect);
        // the box holds only what was typed: a kept period lives in its chip
        // (and on the spine), never as syntax to read or edit
        let period = leading_range(&self.query).map(|(a, b, _)| format!("@{}..{}", iso(a), iso(b)));
        let mut typed = match leading_range(&self.query) {
            Some((_, _, rest)) => rest.trim_start().to_string(),
            None => self.query.clone(),
        };
        let edit = egui::TextEdit::singleline(&mut typed)
            .id(self.query_id)
            .frame(Frame::NONE)
            .font(theme::data_font())
            .vertical_align(Align::Center)
            .hint_text(RichText::new("Search every field").color(pal.muted))
            .desired_width(inner.width());
        let resp = ui.put(inner, edit);
        if resp.changed() {
            self.query = match &period {
                Some(p) => format!("{p} {typed}").trim_end().to_string(),
                None => typed.clone(),
            };
            self.edited_at = Some(Instant::now());
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.maybe_send_query(true);
        }
        if !typed.is_empty() {
            let x = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 15.0, rect.center().y),
                egui::vec2(24.0, 24.0),
            );
            let r = ui.interact(x, self.query_id.with("clear"), Sense::click());
            if r.hovered() {
                ui.painter()
                    .rect_filled(x, CornerRadius::same(4), pal.surface2);
            }
            ui.painter().text(
                x.center(),
                Align2::CENTER_CENTER,
                ph::X,
                FontId::new(13.0, FontFamily::Proportional),
                pal.text2,
            );
            if r.on_hover_text("Clear what is typed (a kept period stays; its chip removes it)")
                .clicked()
            {
                self.query = period.clone().unwrap_or_default();
                self.maybe_send_query(true);
            }
        }
        if err {
            resp.on_hover_text(snap.query_error.clone().unwrap_or_default());
        } else if !typed.trim().is_empty() {
            let q = typed.trim().to_string();
            resp.on_hover_ui(|ui| {
                ui.label(RichText::new("This search means").font(fonts::strong(12.5)));
                ui.label(crate::meaning::describe_query(&q).unwrap_or_default());
                if let Some(h) = crate::meaning::literal_hint(&q) {
                    ui.label(RichText::new(h).color(pal.warning));
                }
            });
        }
    }

    /// The search builder, under the search box: several conditions, each
    /// chosen and none written, with the rows each one keeps counted as it is
    /// made. What it builds goes into the search box as an ordinary search.
    fn match_panel(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        snap: &Snapshot,
        anchor: egui::Rect,
    ) {
        use crate::builder::{How, Row, Shape};
        // dev switch: LOGSEEK_SHOT_BUILD="[any;]col|how#|text|kind#|not|case;...[;go]"
        // opens the builder with those conditions (col empty = every field)
        let mut go_now = false;
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_BUILD") {
            if !snap.columns.is_empty() {
                std::env::remove_var("LOGSEEK_SHOT_BUILD");
                let mut build = crate::builder::Build {
                    rows: Vec::new(),
                    all: true,
                };
                for part in spec.split(';') {
                    match part {
                        "any" => build.all = false,
                        "go" => go_now = true,
                        _ => {
                            let f: Vec<&str> = part.split('|').collect();
                            let n = |k: usize| {
                                f.get(k).and_then(|x| x.parse::<usize>().ok()).unwrap_or(0)
                            };
                            build.rows.push(Row {
                                            time: false,
                                col: f.first().filter(|c| !c.is_empty()).map(|c| c.to_string()),
                                how: How::ALL[n(1).min(How::ALL.len() - 1)],
                                text: f.get(2).unwrap_or(&"").replace("\\n", "\n"),
                                to: String::new(),
                                shape: Shape::ALL[n(3).min(Shape::ALL.len() - 1)],
                                not: f.get(4) == Some(&"1"),
                                case: f.get(5) == Some(&"1"),
                            });
                        }
                    }
                }
                self.match_sheet = Some(BuilderUi::new(build));
            }
        }
        let Some(mut b) = self.match_sheet.take() else {
            return;
        };
        // counts: one condition at a time, then all of them together
        if let Some((q, n)) = &snap.preview {
            if b.asked.as_deref() == Some(q.as_str()) {
                b.counts.insert(q.clone(), *n);
                b.asked = None;
            }
        }
        let whole = b.build.query();
        let mut wanted: Vec<String> = b.build.terms();
        if wanted.len() > 1 {
            wanted.extend(whole.clone());
        }
        if b.asked.as_ref().is_some_and(|a| !wanted.contains(a)) {
            b.asked = None;
        }
        if b.asked.is_none() {
            if let Some(next) = wanted
                .iter()
                .find(|t| !b.counts.contains_key(*t) && logseek_core::query::parse(t).is_ok())
            {
                self.bridge.send(Cmd::Count(next.clone()));
                b.asked = Some(next.clone());
            }
        }
        let ctx = ui.ctx().clone();
        if b.asked.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let screen = ctx.content_rect();
        let w = (screen.width() - 24.0).min(900.0).max(420.0);
        let left = anchor
            .left()
            .min(screen.right() - w - 12.0)
            .max(screen.left() + 12.0);
        let (mut close, mut search, mut add, mut clear) = (false, go_now, false, false);
        let mut open_list = false;
        let mut remove: Option<usize> = None;
        let count_text = |b: &BuilderUi, term: &Option<String>| -> (String, bool) {
            match term {
                None => (String::new(), false),
                Some(t) => match b.counts.get(t) {
                    Some(n) => (
                        format!("{} {}", group(*n), if *n == 1 { "row" } else { "rows" }),
                        true,
                    ),
                    None if logseek_core::query::parse(t).is_err() => {
                        ("cannot be searched".to_string(), false)
                    }
                    None => ("counting".to_string(), false),
                },
            }
        };
        egui::Area::new(egui::Id::new("match-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(left, anchor.bottom() + 4.0))
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Build a search").font(fonts::strong(14.0)));
                            ui.label(
                                RichText::new("choose each part; nothing has to be written")
                                    .color(pal.text2),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        if b.build.rows.len() > 1 {
                            // how the conditions join, as two plain choices always in
                            // sight; the word between the conditions below says the same
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Find rows that match").color(pal.text2));
                                ui.selectable_value(&mut b.build.all, true, "every condition  (and)")
                                    .on_hover_text("A row is kept only when all the conditions below are true of it");
                                ui.selectable_value(&mut b.build.all, false, "any one condition  (or)")
                                    .on_hover_text("A row is kept when at least one of the conditions below is true of it");
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new("One word joins all the conditions here. To mix and with or, search with one set, then build the next and choose Add to the search.")
                                        .font(theme::small_font())
                                        .color(pal.text2),
                                )
                                .wrap(),
                            );
                        } else {
                            ui.label(RichText::new("Find rows where").color(pal.text2));
                        }
                        ui.add_space(2.0);
                        let n_rows = b.build.rows.len();
                        let max_h = (screen.height() - anchor.bottom() - 190.0).max(120.0);
                        egui::ScrollArea::vertical()
                            .max_height(max_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for i in 0..n_rows {
                                    // the word that joins this condition to the one above
                                    if i > 0 {
                                        ui.horizontal(|ui| {
                                            ui.add_space(14.0);
                                            let word = if b.build.all { "and" } else { "or" };
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new(word)
                                                            .font(fonts::strong(13.0))
                                                            .color(pal.accent_ink),
                                                    )
                                                    .min_size(egui::vec2(44.0, 22.0)),
                                                )
                                                .on_hover_text(if b.build.all {
                                                    "Every condition must be true of a row. Click to change to: any one of them"
                                                } else {
                                                    "Any one condition is enough. Click to change to: every one of them"
                                                })
                                                .clicked()
                                            {
                                                b.build.all = !b.build.all;
                                            }
                                        });
                                    }
                                    let term = b.build.rows[i].term();
                                    let (count, known) = count_text(&b, &term);
                                    let missing = b.build.rows[i].missing();
                                    let mut find = std::mem::take(&mut b.finds[i]);
                                    let row = &mut b.build.rows[i];
                                    Frame::new()
                                        .fill(pal.surface2)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(8, 6))
                                        .show(ui, |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                // where
                                                let where_label = match &row.col {
                                                    Some(c) => c.clone(),
                                                    None => "any field".to_string(),
                                                };
                                                // this menu holds a find box: a click in it must not
                                                // close the menu (menus close on any click inside by
                                                // default). It closes when a column is picked, or on a
                                                // click anywhere else.
                                                egui::containers::menu::MenuButton::new(
                                                    RichText::new(format!("{where_label}  {}", ph::CARET_DOWN))
                                                        .color(pal.accent_ink)
                                                        .strong(),
                                                )
                                                .config(
                                                    egui::containers::menu::MenuConfig::new()
                                                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                                                )
                                                .ui(
                                                    ui,
                                                    |ui| {
                                                        ui.set_min_width(300.0);
                                                        // the find box takes the cursor without scrolling the sheet behind
                                                        crate::colpick::keeps_its_scrolling_to_itself(ui, ("build-find", i), |ui| {
                                                        ui.add(
                                                                egui::TextEdit::singleline(&mut find)
                                                                    .hint_text(format!(
                                                                        "{}  Find a column ({} in this file)",
                                                                        ph::MAGNIFYING_GLASS,
                                                                        group(snap.columns.len() as u64)
                                                                    ))
                                                                    .desired_width(f32::INFINITY),
                                                            )
                                                        });
                                                        if !row.how.needs_column()
                                                            && ui
                                                                .selectable_label(row.col.is_none(), "any field")
                                                                .on_hover_text("Look in every column")
                                                                .clicked()
                                                        {
                                                            row.col = None;
                                                            ui.close();
                                                        }
                                                        // names that start with what is typed come first
                                                        let names = crate::colpick::matching(&snap.columns, &find);
                                                        if names.is_empty() {
                                                            ui.label(RichText::new("No column has that in its name").color(pal.muted));
                                                        }
                                                        // a row is as tall as it is drawn (the list shows only the
                                                        // rows in view, so a wrong height would cut rows in half),
                                                        // and each thing typed starts its list from the top
                                                        let rh = (ui.text_style_height(&egui::TextStyle::Body)
                                                            + 2.0 * ui.spacing().button_padding.y)
                                                            .max(ui.spacing().interact_size.y);
                                                        egui::ScrollArea::vertical()
                                                            .id_salt(("build-col-list", i, find.to_lowercase()))
                                                            .max_height(260.0)
                                                            .auto_shrink([false, true])
                                                            .show_rows(ui, rh, names.len(), |ui, range| {
                                                                for k in range {
                                                                    let n = names[k];
                                                                    if ui.selectable_label(row.col.as_ref() == Some(n), n).clicked() {
                                                                        row.col = Some(n.clone());
                                                                        ui.close();
                                                                    }
                                                                }
                                                            });
                                                    },
                                                );
                                                // a column of times is asked about in time: after,
                                                // before, between, with a date and a clock to fill in
                                                let col_at = row
                                                    .col
                                                    .as_ref()
                                                    .and_then(|c| snap.columns.iter().position(|n| n == c));
                                                let is_time = col_at.is_some_and(|c| {
                                                    crate::grid::reads_as_time(&snap.col_types, snap.time_col, c)
                                                });
                                                if is_time != row.time {
                                                    row.time = is_time;
                                                    row.text.clear();
                                                    row.to.clear();
                                                }
                                                if row.time && !How::choices(true).contains(&row.how) {
                                                    row.how = How::Between;
                                                }
                                                let timed = row.time
                                                    && matches!(row.how, How::MoreThan | How::LessThan | How::Between);
                                                if timed && row.text.trim().is_empty() {
                                                    // start from a time the column really holds: the
                                                    // first one on screen, to the second
                                                    let first = col_at.and_then(|c| {
                                                        snap.page.as_ref()?.cells.iter().find_map(|r| {
                                                            let v = r.get(c)?.as_deref()?;
                                                            crate::grid::utc_cell(v)
                                                        })
                                                    });
                                                    if let Some(t) = first {
                                                        let (d, clock) = crate::calendar::split_when(&t);
                                                        row.text = crate::calendar::join_when(&d, clock);
                                                        if row.to.trim().is_empty() {
                                                            row.to = row.text.clone();
                                                        }
                                                    }
                                                }
                                                // how
                                                egui::ComboBox::from_id_salt(("build-how", i))
                                                    .selected_text(row.how.label_for(row.time))
                                                    .height(420.0)
                                                    .show_ui(ui, |ui| {
                                                        for &h in How::choices(row.time) {
                                                            ui.selectable_value(&mut row.how, h, h.label_for(row.time))
                                                                .on_hover_text(h.hint());
                                                        }
                                                    });
                                                // what
                                                match row.how {
                                                    How::MoreThan | How::LessThan | How::Between if row.time => {
                                                        crate::calendar::when_entry(ui, pal, &mut row.text);
                                                        if row.how == How::Between {
                                                            ui.label(RichText::new("and").color(pal.text2));
                                                            crate::calendar::when_entry(ui, pal, &mut row.to);
                                                        }
                                                        ui.label(RichText::new("UTC").color(pal.text2));
                                                    }
                                                    How::Empty | How::HasValue => {}
                                                    How::Shaped | How::HasShaped => {
                                                        ui.add(
                                                            egui::TextEdit::singleline(&mut row.text)
                                                                .font(theme::data_font())
                                                                .hint_text("one example value, as it looks in the data")
                                                                .desired_width(220.0),
                                                        );
                                                        egui::ComboBox::from_id_salt(("build-shape", i))
                                                            .selected_text(row.shape.label())
                                                            .show_ui(ui, |ui| {
                                                                for sh in Shape::ALL {
                                                                    ui.selectable_value(&mut row.shape, sh, sh.label())
                                                                        .on_hover_text(sh.hint());
                                                                }
                                                            });
                                                        // the shape read from the example, in words
                                                        if let Some(w) = crate::builder::shape_pattern(row.text.trim(), row.shape)
                                                            .and_then(|p| crate::builder::shape_words(&p))
                                                        {
                                                            ui.label(RichText::new(w).color(pal.text2));
                                                        }
                                                    }
                                                    How::OneOf => {
                                                        ui.add(
                                                            egui::TextEdit::multiline(&mut row.text)
                                                                .font(theme::data_font())
                                                                .hint_text("one value per line; paste a list")
                                                                .desired_rows(2)
                                                                .desired_width(260.0),
                                                        );
                                                        let n = crate::builder::list_values(&row.text).len();
                                                        if n > 0 {
                                                            ui.label(
                                                                RichText::new(format!(
                                                                    "{} {}",
                                                                    group(n as u64),
                                                                    if n == 1 { "value" } else { "values" }
                                                                ))
                                                                .color(pal.text2),
                                                            );
                                                        }
                                                    }
                                                    How::Between => {
                                                        ui.add(
                                                            egui::TextEdit::singleline(&mut row.text)
                                                                .font(theme::data_font())
                                                                .hint_text("from")
                                                                .desired_width(110.0),
                                                        );
                                                        ui.label(RichText::new("and").color(pal.text2));
                                                        ui.add(
                                                            egui::TextEdit::singleline(&mut row.to)
                                                                .font(theme::data_font())
                                                                .hint_text("to")
                                                                .desired_width(110.0),
                                                        );
                                                    }
                                                    _ => {
                                                        let r = ui.add(
                                                            egui::TextEdit::singleline(&mut row.text)
                                                                .font(theme::data_font())
                                                                .hint_text(match row.how {
                                                                    How::MoreThan | How::LessThan => "a number or a date",
                                                                    How::Pattern => "a regular expression",
                                                                    _ => "text, exactly as typed",
                                                                })
                                                                .desired_width(240.0),
                                                        );
                                                        if r.lost_focus() && ui.input(|x| x.key_pressed(egui::Key::Enter)) {
                                                            search = true;
                                                        }
                                                    }
                                                }
                                                if row.how.has_case() {
                                                    ui.toggle_value(&mut row.case, "Aa")
                                                        .on_hover_text("Capitals matter: on, \"Bob\" does not find \"bob\"");
                                                }
                                                ui.toggle_value(&mut row.not, "not")
                                                    .on_hover_text("Keep the rows where this is NOT so");
                                                // what it keeps, or what is missing
                                                match missing {
                                                    Some(m) => {
                                                        ui.label(RichText::new(m).color(pal.text2));
                                                    }
                                                    None => {
                                                        ui.label(RichText::new(count).color(if known {
                                                            pal.accent_ink
                                                        } else {
                                                            pal.text2
                                                        }));
                                                    }
                                                }
                                                if n_rows > 1
                                                    && icon_button(ui, pal, ph::X, "Remove this condition").clicked()
                                                {
                                                    remove = Some(i);
                                                }
                                            });
                                        });
                                    // a number test or "empty" cannot look in every field
                                    let row = &mut b.build.rows[i];
                                    if row.how.needs_column() && row.col.is_none() {
                                        row.col = None;
                                    }
                                    b.finds[i] = find;
                                    ui.add_space(4.0);
                                }
                            });
                        ui.horizontal(|ui| {
                            if ui
                                .button(format!("{}  Add a condition", ph::PLUS))
                                .clicked()
                            {
                                b.build.rows.push(Row::default());
                                b.finds.push(String::new());
                            }
                            if ui
                                .button(format!("{}  Search a whole list", ph::LIST_MAGNIFYING_GLASS))
                                .on_hover_text("Paste hundreds or thousands of keywords and see which ones are in the file, how often and in which columns")
                                .clicked()
                            {
                                open_list = true;
                            }
                        });
                        ui.separator();
                        match &whole {
                            Some(q) => {
                                let ready = b.build.terms().len();
                                let words: Vec<String> = b
                                    .build
                                    .rows
                                    .iter()
                                    .filter(|r| r.term().is_some())
                                    .map(|r| r.words())
                                    .collect();
                                let joiner = if b.build.all { "  and  " } else { "  or  " };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("Finds rows where {}.", words.join(joiner)))
                                            .color(pal.text),
                                    )
                                    .wrap(),
                                );
                                if ready > 1 {
                                    let (c, known) = count_text(&b, &whole);
                                    ui.label(
                                        RichText::new(format!("Together: {c}"))
                                            .strong()
                                            .color(if known { pal.accent_ink } else { pal.text2 }),
                                    );
                                }
                                ui.collapsing("As a search (for sharing or editing by hand)", |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(q).font(theme::data_font())).wrap(),
                                    );
                                });
                            }
                            None => {
                                ui.label(
                                    RichText::new("Fill in a condition to see what it finds.")
                                        .color(pal.text2),
                                );
                            }
                        }
                        ui.horizontal(|ui| {
                            let ready = whole.is_some();
                            if ui
                                .add_enabled(
                                    ready,
                                    egui::Button::new(
                                        RichText::new(format!("{}  Search", ph::MAGNIFYING_GLASS))
                                            .color(pal.on_accent),
                                    )
                                    .fill(pal.accent),
                                )
                                .on_hover_text("Replace the search with this")
                                .clicked()
                            {
                                search = true;
                            }
                            if ui
                                .add_enabled(
                                    ready && !self.query.trim().is_empty(),
                                    egui::Button::new("Add to the search"),
                                )
                                .on_hover_text("Keep the search as it is and also ask for this")
                                .clicked()
                            {
                                add = true;
                            }
                            if ui
                                .button("Start over")
                                .on_hover_text("Back to one empty condition")
                                .clicked()
                            {
                                clear = true;
                            }
                        });
                    });
            });
        if let Some(i) = remove {
            b.build.rows.remove(i);
            b.finds.remove(i);
        }
        if open_list {
            if self.list_win.is_none() {
                self.list_win = Some(Default::default());
            }
            close = true;
        }
        if clear {
            b = BuilderUi::new(Default::default());
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx) {
            close = true;
        }
        if let (true, Some(term)) = (search || add, b.build.query()) {
            self.push_history();
            self.query = if add {
                let cur = crate::querylines::build(&self.query).query;
                // an OR in what is there must not swallow the new conditions
                if cur.contains(" OR ") {
                    format!("({cur}) {term}")
                } else {
                    format!("{cur} {term}")
                }
            } else {
                term
            };
            self.maybe_send_query(true);
            close = true;
        }
        if close {
            self.builder_last = Some(b.build);
        } else {
            self.match_sheet = Some(b);
        }
    }

    /// One-off messages above the status line (copied, saved, an export's
    /// result), each wrapping to its full length so a long path is never cut
    /// and never runs into the status line; each can be dismissed.
    fn notices(&mut self, ctx: &egui::Context, pal: &Palette, snap: &Snapshot) {
        let now = ctx.input(|i| i.time);
        let flash = self.flash.clone().filter(|f| now - f.1 < 8.0);
        if flash.is_none() {
            self.flash = None;
        }
        let notice = self.notice.clone().filter(|n| now - n.1 < 8.0);
        if notice.is_none() {
            self.notice = None;
        }
        let export = snap
            .export
            .as_ref()
            .filter(|e| !self.export_dismissed && (e.finished || e.cancelled || e.error.is_some()));
        let copied = crate::clip::last(ctx).filter(|c| now - c.1 < 4.0);
        if copied.is_none() {
            crate::clip::forget(ctx);
        }
        if flash.is_none() && notice.is_none() && export.is_none() && copied.is_none() {
            return;
        }
        if flash.is_some() || notice.is_some() || copied.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let screen = ctx.content_rect();
        let w = (screen.width() - 24.0).min(640.0);
        egui::Area::new(egui::Id::new("notices"))
            .order(egui::Order::Foreground)
            .anchor(
                egui::Align2::LEFT_BOTTOM,
                egui::vec2(12.0, -(self.status_h + 8.0)),
            )
            .show(ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(w);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            if let Some((msg, _)) = &copied {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!("{}  {msg}", ph::CHECK_CIRCLE))
                                                .color(pal.accent_ink),
                                        )
                                        .wrap(),
                                    );
                                    if ui.small_button(ph::X).on_hover_text("Dismiss").clicked() {
                                        crate::clip::forget(ui.ctx());
                                    }
                                });
                            }
                            if let Some((msg, _, bad)) = &flash {
                                ui.horizontal(|ui| {
                                    let r = ui.add(
                                        egui::Label::new(
                                            RichText::new(msg.lines().next().unwrap_or("")).color(
                                                if *bad { pal.danger } else { pal.accent_ink },
                                            ),
                                        )
                                        .wrap(),
                                    );
                                    if let Some((_, more)) = msg.split_once('\n') {
                                        r.on_hover_text(more);
                                    }
                                    if ui.small_button(ph::X).on_hover_text("Dismiss").clicked() {
                                        self.flash = None;
                                    }
                                });
                            }
                            if let Some((msg, _, dir)) = &notice {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(msg).color(pal.accent_ink))
                                            .wrap(),
                                    );
                                    if ui
                                        .small_button(format!(
                                            "{}  Open the folder",
                                            ph::FOLDER_OPEN
                                        ))
                                        .on_hover_text(format!("Saved in {}", dir.display()))
                                        .clicked()
                                    {
                                        let _ =
                                            std::process::Command::new("explorer").arg(dir).spawn();
                                    }
                                    if ui.small_button(ph::X).on_hover_text("Dismiss").clicked() {
                                        self.notice = None;
                                    }
                                });
                            }
                            if let Some(e) = export {
                                let (msg, col) = if let Some(err) = &e.error {
                                    (format!("Export failed: {err}"), pal.danger)
                                } else if e.cancelled {
                                    (
                                        "Export cancelled; nothing was written".to_string(),
                                        pal.text2,
                                    )
                                } else {
                                    (
                                        format!(
                                            "Exported {} to {}",
                                            crate::keep::plural(e.done as usize, "row", "rows"),
                                            e.path.display()
                                        ),
                                        pal.text,
                                    )
                                };
                                ui.add(egui::Label::new(RichText::new(msg).color(col)).wrap());
                                ui.horizontal(|ui| {
                                    if e.finished
                                        && ui
                                            .small_button(format!(
                                                "{}  Show in folder",
                                                ph::FOLDER_OPEN
                                            ))
                                            .clicked()
                                    {
                                        // the path goes in quotes, so a comma or a space
                                        // in it cannot be read as another argument
                                        use std::os::windows::process::CommandExt;
                                        let _ = std::process::Command::new("explorer")
                                            .raw_arg(format!("/select,\"{}\"", e.path.display()))
                                            .spawn();
                                    }
                                    if ui.small_button(format!("{}  Dismiss", ph::X)).clicked() {
                                        self.export_dismissed = true;
                                    }
                                });
                            }
                        });
                    });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        egui::Panel::bottom("status")
            .exact_size(self.status_h)
            .frame(
                Frame::new()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().line_segment(
                    [full.left_top(), full.right_top()],
                    Stroke::new(1.0, pal.hairline),
                );
                // the steady items on the right first, in their own strip; then
                // the left ones in the room left, wrapping onto another line (the
                // status line grows) rather than running under the right side
                let mut right = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(full)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                {
                    let ui = &mut right;
                    ui.style_mut().override_font_id = Some(theme::small_font());
                    let t = |s: String| RichText::new(s).color(pal.text2);
                    if (self.prefs.zoom - 1.0).abs() > 0.01 {
                        if ui
                            .add(
                                egui::Button::new(t(format!(
                                    "{:.0} %",
                                    self.prefs.zoom * 100.0
                                )))
                                .frame(false),
                            )
                            .on_hover_text("Zoom — click to reset (Ctrl+0)")
                            .clicked()
                        {
                            self.prefs.zoom = 1.0;
                        }
                        ui.label(t("·".into()));
                    }
                    if snap.time_col.is_some() {
                        // one switch for how times read: as the file wrote them, or all in UTC
                        let label = if self.prefs.times_utc {
                            "Times: UTC"
                        } else {
                            "Times: as in file (timeline UTC)"
                        };
                        ui.menu_button(t(format!("{}  {label}", ph::CLOCK)), |ui| {
                            if ui
                                .radio(
                                    !self.prefs.times_utc,
                                    "As written in the file (e.g. MDT)",
                                )
                                .clicked()
                            {
                                self.prefs.times_utc = false;
                                ui.close();
                            }
                            if ui
                                .radio(
                                    self.prefs.times_utc,
                                    "All in UTC (same as the timeline)",
                                )
                                .clicked()
                            {
                                self.prefs.times_utc = true;
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text(
                            "The timeline is always UTC. Choose how time cells read.",
                        );
                    }
                    // just what is in use; the limit is in the tip for whoever wants it
                    let ram = ui.label(t(format!("Memory {} MB", group(snap.ram_used >> 20))));
                    let tip = format!(
                        "Memory LogSeek is using now. It keeps to about 500 MB; its limit is {} MB right now (mode {}): it takes more only while what you are looking at keeps needing it, as far as this computer can spare, and gives it back when you pause.{}",
                        group(snap.budget_cap >> 20),
                        snap.mode,
                        snap.mode_hint.map(|h| format!("\n{h}")).unwrap_or_default()
                    );
                    ram.on_hover_text(tip);
                    if snap.index_bytes > 0 || snap.mem_bytes > 0 {
                        let (label, tip) = crate::bridge::index_words(
                            snap.index_bytes,
                            snap.mem_bytes,
                            &snap.source_cols,
                        );
                        ui.separator();
                        // the index note opens where indexes are kept, and lets
                        // that move to a drive with more room
                        let in_memory = snap.mem_bytes > 0;
                        let here = snap
                            .source
                            .as_ref()
                            .map(|s| crate::bridge::store_dir_for(&s.path));
                        let mut choose = false;
                        let mut back = false;
                        ui.menu_button(t(label), |ui| {
                            ui.set_max_width(460.0);
                            // left-aligned, never justified: a menu stretches wrapped
                            // lines to its width, which pulls a line with a long path
                            // apart letter by letter
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.add(egui::Label::new(tip.clone()).wrap());
                            if let Some(h) = &here {
                                ui.add_space(4.0);
                                ui.label(RichText::new("This file's index").color(pal.text2));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(h.display().to_string())
                                            .font(theme::data_font())
                                            .color(pal.text),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(4.0);
                            }
                            if in_memory {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Choose a folder on a drive with room, then open the file again: its index is saved there.")
                                            .color(pal.text2),
                                    )
                                    .wrap(),
                                );
                            }
                            if ui
                                .button(format!("{}  Keep new indexes in another folder", ph::FOLDER_OPEN))
                                .clicked()
                            {
                                choose = true;
                                ui.close();
                            }
                            if self.prefs.index_home.is_some()
                                && ui.button("Use the usual folder again").clicked()
                            {
                                back = true;
                                ui.close();
                            }
                            });
                        })
                        .response
                        .on_hover_text(tip);
                        let now = ui.input(|i| i.time);
                        if choose {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("Keep new indexes in this folder")
                                .pick_folder()
                            {
                                let msg = if crate::bridge::index_home_usable(&dir) {
                                    self.prefs.index_home = Some(dir.clone());
                                    crate::bridge::set_index_home(Some(dir.clone()));
                                    (format!("New indexes are kept in {}. Files indexed before keep theirs where they are.", dir.display()), false)
                                } else {
                                    (format!("{} cannot hold indexes (nothing can be written there); nothing changed.", dir.display()), true)
                                };
                                self.flash = Some((msg.0, now, msg.1));
                            }
                        }
                        if back {
                            self.prefs.index_home = None;
                            crate::bridge::set_index_home(None);
                            self.flash = Some(("New indexes are kept in the app's own folder again.".to_string(), now, false));
                        }
                    }
                }
                let right_w = right.min_rect().width();
                let pad = ((STATUS_H - 18.0) * 0.5).max(2.0);
                let left_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left(), full.top() + pad),
                    // one line tall: a wrapped line then starts right under the first
                    egui::pos2((full.right() - right_w - 16.0).max(full.left() + 120.0), full.top() + pad + 18.0),
                );
                let mut left = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(left_rect)
                        .layout(Layout::left_to_right(Align::Min).with_main_wrap(true)),
                );
                {
                    let ui = &mut left;
                    ui.style_mut().override_font_id = Some(theme::small_font());
                    let t = |s: String| RichText::new(s).color(pal.text2);
                    if let Some(r) = crate::spine::keyboard_readout(ui.ctx()) {
                        // the time spine has the keyboard: where its cursor is
                        ui.label(
                            RichText::new(format!("{}  {r}", ph::CLOCK)).color(pal.text),
                        );
                    } else if snap.query_error.is_some() && snap.query == self.sent_query {
                        // the page explains it; the status bar stays quiet
                    } else if snap.total_rows > 0 {
                        let rows = if let Some(t) = self.grid.ftree.as_ref() {
{
                            // which column puts the tree in order is said, so it
                            // is never a guess
                            let order = match self.tree_order.as_ref().and_then(|o| o.1.as_ref()) {
                                Some((col, desc)) => format!(
                                    ", in order of {col} ({})",
                                    crate::grid::sort_words(
                                        &snap.col_types,
                                        snap.time_col,
                                        snap.columns.iter().position(|c| c == col),
                                        *desc
                                    )
                                ),
                                None => ", in the order of the file".to_string(),
                            };
                            match &self.shown_as {
                                // followed from a cell: what was followed, in words
                                Some(what) => format!(
                                    "{} rows as a tree{order}: {what}  ·  of {}",
                                    group(t.len()),
                                    group(snap.total_rows)
                                ),
                                None => format!(
                                    "{} followed rows of {}, as a tree{order}",
                                    group(t.len()),
                                    group(snap.total_rows)
                                ),
                            }
                            }
                        } else if self.follow_showing {
                            format!(
                                "{} {} of {}",
                                group(snap.matched),
                                match &self.shown_as {
                                    Some(s) => format!("{s},"),
                                    None if self.showing_list => "rows with a keyword of the list,".to_string(),
                                    None => "followed rows".to_string(),
                                },
                                group(snap.total_rows)
                            )
                        } else if snap.query.trim().is_empty() && !snap.restricted {
                            crate::keep::plural(snap.total_rows as usize, "row", "rows")
                        } else {
                            format!("{} of {} rows", group(snap.matched), group(snap.total_rows))
                        };
                        ui.label(RichText::new(rows).color(pal.text));
                        if self.follow_showing
                            && ui
                                .add(egui::Button::new(t("·  Show all rows".to_string())).frame(false))
                                .on_hover_text("Leave the followed rows and show the whole file again")
                                .clicked()
                        {
                            self.show_all_rows();
                        }
                        // a follow run from a cell can be changed: the sheet opens on it
                        if self.follow_showing
                            && self.follow_last.is_some()
                            && self.follow.is_none()
                            && ui
                                .add(egui::Button::new(t("·  Change what is followed".to_string())).frame(false))
                                .on_hover_text("Opens Follow on what was just followed: another column, a time limit, the whole tree")
                                .clicked()
                        {
                            self.follow = self.follow_last.take();
                        }
                        // rows selected, and the word on the last copy
                        let nsel = self.grid.multi.count();
                        if nsel > 1 {
                            ui.label(t(match self.grid.block() {
                                Some(cols) => format!(
                                    "·  {} rows by {} columns selected",
                                    group(nsel),
                                    cols.len()
                                ),
                                None => format!("·  {} rows selected", group(nsel)),
                            }));
                        }
                        // one-off messages (copied, saved, exported) are shown
                        // above this line, where a long path can wrap: see `notices`
                        // sorted: by what, which way, and the way back to file order
                        // the analysis sheet closed last can be put back as it was
                        let any_open = self.list_win.is_some()
                            || self.rhythm_win.is_some()
                            || self.pair_win.is_some()
                            || self.pattern_win.is_some()
                            || self.strange_win.is_some()
                            || self.shape_win.is_some()
                            || self.explain_win.is_some();
                        let back = self
                            .closed_sheet
                            .as_ref()
                            .filter(|(path, _)| {
                                !any_open && snap.source.as_ref().is_some_and(|s| s.path == *path)
                            })
                            .map(|(_, sheet)| sheet.name());
                        if let Some(name) = back {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("{}  Back to {name}", ph::ARROW_U_UP_LEFT))
                                            .font(theme::small_font())
                                            .color(pal.accent_ink),
                                    )
                                    .stroke(Stroke::new(1.0, pal.accent_ink)),
                                )
                                .on_hover_text("Opens it again as you left it")
                                .clicked()
                            {
                                if let Some((_, sheet)) = self.closed_sheet.take() {
                                    self.reopen(sheet);
                                }
                            }
                        }
                        if let Some((name, desc)) = snap.sort.as_ref() {
                            let how = crate::grid::sort_words(
                                &snap.col_types,
                                snap.time_col,
                                snap.columns.iter().position(|c| c == name),
                                *desc,
                            );
                            ui.label(t(format!("·  Sorted by {name}, {how}")));
                            if ui
                                .add(egui::Button::new(t("Clear sort".to_string())).frame(false))
                                .on_hover_text("Back to the order in the file")
                                .clicked()
                            {
                                self.grid.top = 0;
                                self.grid.sel = None;
                                self.bridge.send(Cmd::Sort(None));
                            }
                        }
                        if let Some(e) = snap.sort_error.as_ref() {
                            ui.label(RichText::new(format!("·  Could not sort: {e}")).color(pal.danger));
                        }

                        // sideways: which columns are on screen, when not all of them fit
                        let kept = self.grid.pinned.len();
                        if let Some((a, b, of)) = self
                            .grid
                            .cols_on_screen
                            .filter(|&(a, b, of)| !(a <= kept + 1 && b == of))
                        {
                            let label = if kept > 0 {
                                format!("·  Showing columns {a} to {b} of {of}; {kept} stays on the left")
                            } else {
                                format!("·  Columns {a}–{b} of {of}")
                            };
                            if ui
                                .add(egui::Button::new(t(label)).frame(false))
                                .on_hover_text("Go to a column (Ctrl+G)")
                                .clicked()
                            {
                                self.colchooser = Some(Default::default());
                            }
                        }
                        if ui
                            .add(egui::Button::new(RichText::new(format!("{}  Export rows", ph::EXPORT)).color(pal.text2)).frame(false))
                            .on_hover_text("Save the rows this search keeps as CSV or JSON Lines")
                            .clicked()
                        {
                            self.export_open = !self.export_open;
                        }
                        if let Some(e) = snap.export.as_ref().filter(|_| !self.export_dismissed) {
                            if !e.finished && !e.cancelled && e.error.is_none() {
                                ui.label(RichText::new(format!("·  Exporting {} of {} rows", group(e.done), group(e.total))).color(pal.text));
                                if ui.small_button("Cancel").clicked() {
                                    crate::export::CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        }
                        if let Some(mask) = self.tag_filter {
                            let names: Vec<String> = (0..9)
                                .filter(|i| mask >> i & 1 == 1)
                                .map(crate::tags::name)
                                .collect();
                            let label = if mask == 0x1FF {
                                "·  tagged rows only".to_string()
                            } else {
                                format!("·  tagged {} only", names.join(", "))
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("{label}  {}", ph::X))
                                            .color(pal.accent_ink),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("Show all rows again")
                                .clicked()
                            {
                                self.set_tag_filter(None);
                            }
                        }
                        if self.tags_stale {
                            ui.label(
                                RichText::new(format!(
                                    "·  {}  tags were saved for a different version of this file",
                                    ph::WARNING
                                ))
                                .color(pal.warning),
                            );
                        }
                        let nf = self
                            .filters
                            .iter()
                            .filter(|(c, f)| {
                                f.is_active()
                                    && (f.values.is_some()
                                        || crate::filters::text_clause(c, &f.text).is_ok())
                            })
                            .count();
                        if nf > 0 {
                            let label =
                                format!("·  {nf} column filter{}", if nf == 1 { "" } else { "s" });
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(label).color(pal.accent_ink))
                                        .frame(false),
                                )
                                .on_hover_text("Clear column filters")
                                .clicked()
                            {
                                self.filters.clear();
                                self.maybe_send_query(true);
                            }
                        }
                        // timing lives in the rows tooltip, not on the line
                    }
                    if let Phase::Importing {
                        bytes_done,
                        bytes_total,
                        rows,
                    } = snap.phase
                    {
                        ui.add_space(10.0);
                        let f = if bytes_total > 0 {
                            bytes_done as f32 / bytes_total as f32
                        } else {
                            0.0
                        };
                        let (r, _) = ui.allocate_exact_size(egui::vec2(120.0, 6.0), Sense::hover());
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(3), pal.hairline);
                        let mut done = r;
                        done.set_width(r.width() * f.clamp(0.0, 1.0));
                        ui.painter()
                            .rect_filled(done, CornerRadius::same(3), pal.info);
                        // nothing read yet: the file is being opened (an index
                        // made before only needs loading), not indexed from 0 %
                        ui.label(t(if bytes_done == 0 && rows == 0 {
                            "Opening".to_string()
                        } else {
                            format!("Indexing {:.0} %  ·  {} rows", f * 100.0, group(rows))
                        }));
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Cancel").color(pal.accent_ink))
                                    .frame(false),
                            )
                            .clicked()
                        {
                            self.bridge.send(Cmd::CancelImport);
                        }
                    }
                    match &snap.phase {
                        Phase::Failed(e) => {
                            ui.label(
                                RichText::new(format!("{}  {e}", ph::WARNING)).color(pal.warning),
                            );
                        }
                        Phase::Partial(r) => {
                            ui.label(
                                RichText::new(format!(
                                    "{}  Indexing stopped early: {r}. The rows indexed so far can be searched.",
                                    ph::WARNING
                                ))
                                .color(pal.warning),
                            );
                        }
                        Phase::Cancelled => {
                            ui.label(t("Indexing stopped — rows so far are shown".into()));
                        }
                        _ => {}
                    }
                }
                // the height the left items took sets the status line's height next frame
                // lines used: where the next item would go, plus one line (a horizontal
                // group reports all the height it was given, so its size cannot be used)
                let lines_h = left.cursor().min.y - left_rect.top() + 18.0;
                self.status_h = (lines_h + pad * 2.0).max(STATUS_H);
            });
    }

    fn tags_panel(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        ui.add_space(8.0);
        let counts = crate::tags::counts(&self.tags);
        ui.label(
            RichText::new(
                "Press 1–9 on a row to tag it, 0 to clear. Click a tag to show only its rows.",
            )
            .font(theme::small_font())
            .color(pal.text2),
        );
        ui.add_space(6.0);
        // tags mean what the analyst says: each is named in its own row, in
        // their own words. The switch sits above the list, always in reach.
        let naming_id = egui::Id::new("naming-tags");
        let mut naming = ui.data(|d| d.get_temp::<bool>(naming_id)).unwrap_or(false);
        if ui
            .button(if naming {
                format!("{}  Done naming", ph::CHECK)
            } else {
                format!("{}  Name the tags", ph::PENCIL_SIMPLE)
            })
            .on_hover_text("Give each tag your own name, in its row (kept for every file); an empty name goes back to its number")
            .clicked()
        {
            naming = !naming;
        }
        ui.data_mut(|d| d.insert_temp(naming_id, naming));
        if naming {
            self.prefs.tag_names.resize(9, String::new());
        }
        let mut renamed = false;
        ui.add_space(4.0);
        let mut set: Option<Option<u16>> = None;
        let w = ui.available_width();
        // "all tagged" row
        {
            let on = self.tag_filter == Some(0x1FF);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 28.0), Sense::click());
            if on {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(4), pal.selection);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(4), pal.surface2);
            }
            ui.painter().text(
                egui::pos2(r.left() + 10.0, r.center().y),
                Align2::LEFT_CENTER,
                ph::TAG,
                FontId::new(14.0, FontFamily::Proportional),
                pal.text2,
            );
            ui.painter().text(
                egui::pos2(r.left() + 32.0, r.center().y),
                Align2::LEFT_CENTER,
                "All tagged rows",
                theme::ui_font(),
                pal.text,
            );
            ui.painter().text(
                egui::pos2(r.right() - 8.0, r.center().y),
                Align2::RIGHT_CENTER,
                group(self.tags.len() as u64),
                theme::small_font(),
                pal.text2,
            );
            if resp.clicked() && !self.tags.is_empty() {
                set = Some(if on { None } else { Some(0x1FF) });
            }
        }
        ui.add_space(2.0);
        for i in 0..9 {
            let bit = 1u16 << i;
            let on = self.tag_filter == Some(bit);
            if naming {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::hover());
                    crate::tags::draw(
                        ui.painter(),
                        i,
                        egui::pos2(r.left() + 16.0, r.center().y),
                        5.5,
                        crate::tags::color(i, pal.dark),
                    );
                    let tail = format!("{}   key {}", group(counts[i]), i + 1);
                    // the count and the key keep their place at the right
                    let tail_w = 96.0;
                    renamed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.prefs.tag_names[i])
                                .hint_text(format!("Tag {}", i + 1))
                                .desired_width((ui.available_width() - tail_w).max(80.0)),
                        )
                        .changed();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(tail).font(theme::small_font()).color(pal.text2));
                    });
                });
                continue;
            }
            let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 28.0), Sense::click());
            if on {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(4), pal.selection);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(4), pal.surface2);
            }
            crate::tags::draw(
                ui.painter(),
                i,
                egui::pos2(r.left() + 16.0, r.center().y),
                5.5,
                crate::tags::color(i, pal.dark),
            );
            ui.painter().text(
                egui::pos2(r.left() + 32.0, r.center().y),
                Align2::LEFT_CENTER,
                crate::tags::name(i),
                theme::ui_font(),
                if counts[i] > 0 { pal.text } else { pal.text2 },
            );
            ui.painter().text(
                egui::pos2(r.right() - 8.0, r.center().y),
                Align2::RIGHT_CENTER,
                format!("{}   key {}", group(counts[i]), i + 1),
                theme::small_font(),
                pal.text2,
            );
            if resp
                .on_hover_text(format!("Show only rows tagged {}", crate::tags::name(i)))
                .clicked()
                && counts[i] > 0
            {
                set = Some(if on { None } else { Some(bit) });
            }
        }
        if renamed {
            crate::tags::set_names(&self.prefs.tag_names);
        }
        if self.tag_filter.is_some() {
            ui.add_space(8.0);
            if ui.button(format!("{}  Show all rows", ph::X)).clicked() {
                set = Some(None);
            }
        }
        if let Some(m) = set {
            self.set_tag_filter(m);
        }
        let _ = snap;
    }

    /// The row inspector: a floating, movable, resizable window over the grid
    /// (the grid keeps all its rows). Fields, unpacked XML/JSON, and the raw line.
    fn record_viewer(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let ctx = ui.ctx().clone();
        let mut step: i64 = 0;
        let mut pick: Option<crate::fields::Pick> = None;
        let mut toggle: Option<(u64, usize)> = None;
        let mut make_col: Option<(String, String)> = None;
        let short_names = self.prefs.short_field_names;
        let mut open = self.record_open;
        let screen = ctx.content_rect();
        let w = (screen.width() * 0.56).clamp(560.0, 980.0);
        let h = (screen.height() * 0.78).clamp(380.0, 900.0);
        let docked = self.prefs.inspector_docked;
        let shown_names: Vec<String> = self
            .grid
            .order
            .iter()
            .filter_map(|&c| snap.columns.get(c))
            .filter(|n| !self.grid.hidden.contains(*n))
            .cloned()
            .collect();
        let mut pin_act: Option<(String, bool)> = None; // (field, pin?)
        let time_name: Option<String> = snap.time_col.and_then(|c| snap.columns.get(c)).cloned();
        let times_utc = self.prefs.times_utc;
        let mut body = |ui: &mut egui::Ui| {
            // ── header: drag here to move ──
            Frame::new()
                .fill(pal.surface2)
                .inner_margin(Margin {
                    left: 12,
                    right: 8,
                    top: 8,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if icon_button(ui, pal, ph::CARET_UP, "Previous row  (↑)").clicked() {
                            step = -1;
                        }
                        if icon_button(ui, pal, ph::CARET_DOWN, "Next row  (↓)").clicked() {
                            step = 1;
                        }
                        let Some(rec) = &snap.record else {
                            ui.label(RichText::new("Select a row to inspect it").color(pal.muted));
                            return;
                        };
                        ui.label(
                            RichText::new(
                                match self.grid.sel.filter(|_| snap.matched < snap.total_rows) {
                                    Some(i) => format!(
                                        "Match {} of {}  ·  row {} in the file",
                                        group(i + 1),
                                        group(snap.matched),
                                        group(rec.row + 1)
                                    ),
                                    None => format!(
                                        "Row {} of {}",
                                        group(rec.row + 1),
                                        group(snap.total_rows)
                                    ),
                                },
                            )
                            .font(fonts::strong(14.0)),
                        );
                        ui.add_space(6.0);
                        let mask = self.tags.get(&rec.row).copied().unwrap_or(0);
                        ui.menu_button(format!("{}  Tag", ph::TAG), |ui| {
                            for t in 0..9 {
                                let on = mask >> t & 1 == 1;
                                let label = format!(
                                    "{}  {}   key {}",
                                    if on { ph::CHECK } else { "    " },
                                    crate::tags::name(t),
                                    t + 1
                                );
                                if ui.button(label).clicked() {
                                    toggle = Some((rec.row, t));
                                    ui.close();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Tag this row (keys 1–9; 0 clears)");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let (g, tip) = if self.prefs.inspector_docked {
                                (ph::ARROW_SQUARE_OUT, "Float over the grid")
                            } else {
                                (
                                    ph::SIDEBAR_SIMPLE,
                                    "Dock on the right (the grid narrows instead of being covered)",
                                )
                            };
                            if icon_button(ui, pal, g, tip).clicked() {
                                self.prefs.inspector_docked = !self.prefs.inspector_docked;
                            }
                            if icon_button(ui, pal, ph::X, "Close  (Space or Esc)").clicked() {
                                self.record_open = false;
                            }
                        });
                    });
                    // this row's tags, named, on a line of their own: however
                    // many there are they wrap, and the buttons above keep their place
                    let mask = snap
                        .record
                        .as_ref()
                        .and_then(|r| self.tags.get(&r.row).copied())
                        .unwrap_or(0);
                    if mask != 0 {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Tags").font(theme::small_font()).color(pal.text2));
                            for t in (0..9).filter(|t| mask >> t & 1 == 1) {
                                let (r, _) =
                                    ui.allocate_exact_size(egui::vec2(14.0, 20.0), Sense::hover());
                                crate::tags::draw(
                                    ui.painter(),
                                    t,
                                    r.center(),
                                    5.0,
                                    crate::tags::color(t, pal.dark),
                                );
                                ui.label(
                                    RichText::new(crate::tags::name(t))
                                        .font(theme::small_font())
                                        .color(pal.text),
                                );
                                ui.add_space(6.0);
                            }
                        });
                    }
                });
            let Some(rec) = snap.record.clone() else {
                return;
            };
            // an unusual row: every reason it stands out, said in full
            // (worked out here from the row's own cells: the band beside the
            // rows is rebuilt as the grid is drawn)
            let reasons: Option<String> = self
                .why
                .as_ref()
                .filter(|_| self.follow_showing)
                .and_then(|model| {
                    let page = snap.page.as_ref()?;
                    let k = page.rows.iter().position(|r| *r == rec.row)?;
                    let why = model.why(page.cells.get(k)?);
                    (!why.is_empty()).then(|| crate::unusualwin::full(&why, &snap.columns))
                });
            if let Some(reasons) = reasons.as_ref() {
                Frame::new()
                    .fill(pal.selection)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Why this row stands out").font(fonts::strong(13.0)));
                        ui.add(egui::Label::new(RichText::new(reasons.as_str()).color(pal.text)).wrap());
                    });
            }
            // ── tabs + tools ──
            Frame::new()
                    .inner_margin(Margin {
                        left: 12,
                        right: 12,
                        top: 8,
                        bottom: 4,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let arr = crate::inspect::arrange(&rec.fields, &shown_names, &self.prefs.pinned_fields, &snap.columns);
                            let n_filled = arr.filled();
                            let n_empty = arr.empty.len();
                            let tab_now = if self.inspect_raw { 2 } else if self.inspect_empty { 1 } else { 0 };
                            for (tab, label) in [
                                // a row holds only its own fields: the other columns of
                                // the file are named apart, never shown as if the row had them
                                (0u8, format!("This row's fields  {}", group(n_filled as u64))),
                                (1u8, format!("Other columns, no value here  {}", group(n_empty as u64))),
                                (2u8, "Raw line".to_string()),
                            ] {
                                let on = tab_now == tab;
                                let raw = tab == 2;
                                let b = egui::Button::new(RichText::new(label).color(if on {
                                    pal.text
                                } else {
                                    pal.text2
                                }))
                                .fill(if on { pal.selection } else { pal.surface })
                                .stroke(if on {
                                    Stroke::new(1.0, pal.accent_ink)
                                } else {
                                    Stroke::new(1.0, pal.hairline)
                                })
                                .min_size(egui::vec2(0.0, 26.0));
                                let tip = match tab {
                                    0 => "The fields this row holds, with their values",
                                    1 => "Columns that other rows of the file have and this row does not hold, or holds blank. Where each line has its own fields, most columns are here for any one row.",
                                    _ => "The row exactly as it is written in the file",
                                };
                                if ui.add(b).on_hover_text(tip).clicked() {
                                    self.inspect_raw = raw;
                                    self.inspect_empty = tab == 1;
                                }
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if self.inspect_raw {
                                    if ui.button(format!("{}  Copy", ph::COPY)).clicked() {
                                        crate::clip::copy(ui.ctx(), rec.raw.clone(), "the raw line");
                                    }
                                    ui.checkbox(&mut self.inspect_pretty, "Pretty")
                                        .on_hover_text("One field per line, XML and JSON indented. Off = the exact line from the file.");
                                } else {
                                    ui.menu_button(format!("{}  Copy row", ph::COPY), |ui| {
                                        if ui.button("As JSON").clicked() {
                                            let mut m = serde_json::Map::new();
                                            for (k, v) in &rec.fields {
                                                m.insert(k.clone(), serde_json::Value::String(v.clone()));
                                            }
                                            crate::clip::copy(ui.ctx(), serde_json::to_string_pretty(&m).unwrap_or_default(), "the row as JSON");
                                            ui.close();
                                        }
                                        if ui.button("As text (name = value)").clicked() {
                                            let t: Vec<String> =
                                                rec.fields.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                                            crate::clip::copy(ui.ctx(), t.join("\n"), "the row as text");
                                            ui.close();
                                        }
                                        if ui.button("As CSV (names, then values)").clicked() {
                                            let q = |s: &str| {
                                                if s.contains([',', '"', '\n', '\r']) {
                                                    format!("\"{}\"", s.replace('"', "\"\""))
                                                } else {
                                                    s.to_string()
                                                }
                                            };
                                            let names: Vec<String> = rec.fields.iter().map(|(k, _)| q(k)).collect();
                                            let vals: Vec<String> = rec.fields.iter().map(|(_, v)| q(v)).collect();
                                            crate::clip::copy(ui.ctx(), format!("{}\n{}", names.join(","), vals.join(",")), "the row as CSV");
                                            ui.close();
                                        }
                                        if ui.button("The line from the file").clicked() {
                                            crate::clip::copy(ui.ctx(), rec.raw.clone(), "the raw line");
                                            ui.close();
                                        }
                                    });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.record_find)
                                            .hint_text(
                                                RichText::new(format!(
                                                    "{}  Find a field or value",
                                                    ph::MAGNIFYING_GLASS
                                                ))
                                                .color(pal.muted),
                                            )
                                            .desired_width(240.0),
                                    );
                                }
                            });
                        });
                    });
            ui.add_space(2.0);
            let body_margin = Margin {
                left: 14,
                right: 10,
                top: 4,
                bottom: 10,
            };
            if self.inspect_raw {
                Frame::new().inner_margin(body_margin).show(ui, |ui| {
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.style_mut().interaction.selectable_labels = true;
                            let text = match (self.inspect_pretty, crate::inspect::kind(&rec.raw)) {
                                (true, Some(crate::inspect::Kind::Json)) => {
                                    crate::inspect::pretty_json(&rec.raw)
                                }
                                (true, Some(crate::inspect::Kind::Xml)) => {
                                    crate::inspect::pretty_xml(&rec.raw)
                                }
                                (true, None) => {
                                    let mut out = String::new();
                                    for (k, v) in &rec.fields {
                                        match crate::inspect::kind(v) {
                                            Some(crate::inspect::Kind::Xml) => {
                                                out.push_str(&format!(
                                                    "{k}:\n{}\n",
                                                    indent(&crate::inspect::pretty_xml(v))
                                                ))
                                            }
                                            Some(crate::inspect::Kind::Json) => {
                                                out.push_str(&format!(
                                                    "{k}:\n{}\n",
                                                    indent(&crate::inspect::pretty_json(v))
                                                ))
                                            }
                                            None => out.push_str(&format!("{k}: {v}\n")),
                                        }
                                    }
                                    out
                                }
                                _ => rec.raw.clone(),
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text).font(theme::data_font()).color(pal.text),
                                )
                                .wrap(),
                            );
                        });
                });
                return;
            }
            let needle = self.record_find.to_lowercase();
            let keep = |k: &str, v: &str| {
                needle.is_empty()
                    || k.to_lowercase().contains(&needle)
                    || v.to_lowercase().contains(&needle)
            };
            let pins = self.prefs.pinned_fields.clone();
            // what the search found, marked in the values as in the grid
            let mut terms = crate::filters::highlight_terms(&self.query);
            if let Some(f) = self.find.as_ref().filter(|f| !f.sent.trim().is_empty()) {
                terms.push(f.sent.trim().to_string());
            }
            // rows a list search found: its keywords are marked too
            if self.showing_list {
                if let Some(m) = &self.list_marks {
                    terms.extend(m.terms.iter().cloned());
                }
            }
            let hl = pal
                .accent
                .gamma_multiply(if pal.dark { 0.32 } else { 0.45 });
            let arr = crate::inspect::arrange(&rec.fields, &shown_names, &pins, &snap.columns);
            Frame::new().inner_margin(body_margin).show(ui, |ui| {
                if let Some(e) = &rec.error {
                    ui.label(
                        RichText::new(format!("{}  The fields could not be read: {e}", ph::WARNING))
                            .color(pal.warning),
                    );
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if self.inspect_empty {
                        // the empty fields: named, so nothing is hidden, but out of the way
                        let empty: Vec<&str> = arr.empty.iter().map(String::as_str).filter(|k| keep(k, "")).collect();
                        ui.label(
                            RichText::new(format!(
                                "{} columns of the file have no value in this row (the row does not hold them, or holds them blank):",
                                group(empty.len() as u64)
                            ))
                            .color(pal.text2),
                        );
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(14.0, 6.0);
                            for k in empty {
                                ui.label(RichText::new(k).font(theme::data_font()).color(pal.text2));
                            }
                        });
                        return;
                    }
                    let longest = rec.fields.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(8).min(28) as f32;
                    let key_w = (longest * 7.2 + 26.0).clamp(110.0, ui.available_width() * 0.34);
                    // one field as a row of the table: the name in a shaded cell, the value
                    // beside it, a line under; pin and copy on the row under the mouse
                    let mut row = |ui: &mut egui::Ui, k: &str, v: &str, parent: Option<(&str, crate::inspect::Kind)>| {
                        let indent = parent.is_some();
                        let pinned = !indent && pins.iter().any(|p| p == k);
                        let left = ui.max_rect().left();
                        let right = ui.max_rect().right();
                        let top = ui.cursor().top();
                        let cell = ui.painter().add(egui::Shape::Noop);
                        ui.add_space(5.0);
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(egui::vec2(key_w, 20.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_width(key_w);
                                ui.horizontal_top(|ui| {
                                    ui.add_space(if indent { 20.0 } else { 6.0 });
                                    if pinned {
                                        ui.label(RichText::new(ph::PUSH_PIN).color(pal.accent_ink));
                                    }
                                    ui.add(egui::Label::new(RichText::new(k).font(theme::ui_font()).color(pal.text2)).wrap());
                                });
                            });
                            ui.add_space(8.0);
                            let value_w = (ui.available_width() - 60.0).max(80.0);
                            ui.allocate_ui_with_layout(egui::vec2(value_w, 20.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_width(value_w);
                                // the time field reads as in the grid, its raw value beside it
                                let as_time = (!indent && times_utc && time_name.as_deref() == Some(k))
                                    .then(|| crate::grid::utc_cell(v))
                                    .flatten()
                                    .filter(|t| t != v)
                                    .map(|t| format!("{t}   ({v})"));
                                let (text, col) = if v.trim().is_empty() {
                                    ("empty".to_string(), pal.muted)
                                } else {
                                    (as_time.unwrap_or_else(|| v.to_string()), pal.text)
                                };
                                let resp = ui.add(
                                    egui::Label::new(crate::filters::highlighted(&text, &terms, theme::data_font(), col, hl))
                                        .wrap()
                                        .sense(Sense::click()),
                                );
                                resp.context_menu(|ui| {
                                    let short: String = if v.chars().count() > 36 || v.contains('\n') {
                                        "this value".to_string()
                                    } else {
                                        v.to_string()
                                    };
                                    if !indent && !v.trim().is_empty() {
                                        if ui.button(format!("{}  Show only  {k} = {short}", ph::FUNNEL_SIMPLE)).clicked() {
                                            pick = Some(crate::fields::Pick::Only(k.to_string(), v.to_string()));
                                            ui.close();
                                        }
                                        if ui.button(format!("{}  Exclude  {k} = {short}", ph::PROHIBIT)).clicked() {
                                            pick = Some(crate::fields::Pick::Exclude(k.to_string(), v.to_string()));
                                            ui.close();
                                        }
                                        ui.separator();
                                    }
                                    if let Some((pc, kind)) = parent {
                                        // the name it will have, said before it is made
                                        let made = field_column_name(k, pc, &snap.columns, short_names);
                                        if ui.button(format!("{}  Make column  {made}", ph::FUNCTION)).on_hover_text(format!("A new column {made}: the field {k} inside {pc}, for every row (the file is unchanged)")).clicked() {
                                            make_col = Some((made, crate::rawcols::extract_formula(pc, kind, k)));
                                            ui.close();
                                        }
                                        ui.separator();
                                    }
                                    if !indent {
                                        let label = if pinned { "Unpin from the top" } else { "Pin to the top" };
                                        if ui.button(format!("{}  {label}", ph::PUSH_PIN)).clicked() {
                                            pin_act = Some((k.to_string(), !pinned));
                                            ui.close();
                                        }
                                    }
                                    if ui.button(format!("{}  Copy value", ph::COPY)).clicked() {
                                        crate::clip::copy(ui.ctx(), v.to_string(), &format!("the value of {k}"));
                                        ui.close();
                                    }
                                    if ui.button(format!("{}  Copy  {k} = value", ph::COPY)).clicked() {
                                        crate::clip::copy(ui.ctx(), format!("{k} = {v}"), &format!("{k} = value"));
                                        ui.close();
                                    }
                                });
                            });
                        });
                        ui.add_space(5.0);
                        let bottom = ui.cursor().top();
                        let full = egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom));
                        ui.painter().set(
                            cell,
                            egui::Shape::rect_filled(
                                egui::Rect::from_min_max(full.min, egui::pos2(left + key_w + 4.0, bottom)),
                                CornerRadius::ZERO,
                                pal.surface2,
                            ),
                        );
                        ui.painter().line_segment([full.left_bottom(), full.right_bottom()], Stroke::new(1.0, pal.hairline));
                        // pin and copy on every row, faint until the row is under the pointer,
                        // so copying a value is never a hidden trick
                        {
                            let ink = if ui.rect_contains_pointer(full) { pal.text2 } else { pal.muted };
                            let b = |dx: f32| egui::Rect::from_min_size(egui::pos2(right - dx, top + 3.0), egui::vec2(24.0, 24.0));
                            if !indent {
                                let (g, tip) = if pinned {
                                    (ph::PUSH_PIN_SLASH, "Unpin from the top")
                                } else {
                                    (ph::PUSH_PIN, "Pin to the top: it stays first on every row")
                                };
                                if ui.put(b(54.0), egui::Button::new(RichText::new(g).color(ink)).frame(false)).on_hover_text(tip).clicked() {
                                    pin_act = Some((k.to_string(), !pinned));
                                }
                            }
                            if ui.put(b(28.0), egui::Button::new(RichText::new(ph::COPY).color(ink)).frame(false)).on_hover_text("Copy this value").clicked() {
                                crate::clip::copy(ui.ctx(), v.to_string(), &format!("the value of {k}"));
                            }
                        }
                        // the buttons sit at the row's top, and placing them left the
                        // next row starting just under them: a value of several lines
                        // was then drawn over by the rows after it. The whole row is
                        // claimed, so the next one starts below it however tall it is.
                        ui.allocate_rect(full, Sense::hover());
                    };
                    let section =|ui: &mut egui::Ui, title: &str, n: usize| {
                        ui.add_space(10.0);
                        ui.label(RichText::new(format!("{title}  {}", group(n as u64))).font(fonts::strong(12.5)).color(pal.text));
                        ui.add_space(2.0);
                        let r = ui.cursor();
                        ui.painter().line_segment(
                            [egui::pos2(ui.max_rect().left(), r.top()), egui::pos2(ui.max_rect().right(), r.top())],
                            Stroke::new(1.0, pal.hairline),
                        );
                    };
                    let structured = |v: &str| crate::inspect::kind(v).is_some();
                    let groups: [(&str, Vec<&(String, String)>); 3] = [
                        ("Pinned", arr.pinned.iter().copied().filter(|(k, v)| keep(k, v)).collect()),
                        ("In the grid", arr.in_grid.iter().copied().filter(|(k, v)| !structured(v) && keep(k, v)).collect()),
                        ("Also with a value", arr.also.iter().copied().filter(|(k, v)| !structured(v) && keep(k, v)).collect()),
                    ];
                    for (title, items) in groups.iter() {
                        if items.is_empty() {
                            continue;
                        }
                        section(ui, title, items.len());
                        for (k, v) in items {
                            row(ui, k, v, None);
                        }
                    }
                    // structured cells: their own readable section each
                    for (k, v) in arr.in_grid.iter().chain(arr.also.iter()).copied() {
                        let Some(kind) = crate::inspect::kind(v) else {
                            continue;
                        };
                        let inner = match kind {
                            crate::inspect::Kind::Xml => crate::inspect::xml_fields(v),
                            crate::inspect::Kind::Json => crate::inspect::json_fields(v),
                        };
                        // fields you already made into columns are listed above, not twice
                        let made: Vec<&str> = snap.virtual_cols.iter().map(|(n, _)| n.as_str()).collect();
                        let shown: Vec<&(String, String)> = inner
                            .iter()
                            .filter(|(a, _)| !made.contains(&a.as_str()))
                            .filter(|(a, b)| keep(a, b) || keep(k, b))
                            .collect();
                        if shown.is_empty() {
                            continue;
                        }
                        ui.add_space(10.0);
                        let label = format!(
                            "{}  {k}  ·  {} · {} values",
                            ph::CODE,
                            if kind == crate::inspect::Kind::Xml { "XML" } else { "JSON" },
                            inner.len()
                        );
                        egui::CollapsingHeader::new(RichText::new(label).font(fonts::strong(13.0)).color(pal.text))
                            .id_salt(("struct", k))
                            .default_open(true)
                            .show(ui, |ui| {
                                for (a, b) in shown {
                                    row(ui, a, b, Some((k.as_str(), kind)));
                                }
                            });
                    }
                });
            });
        };
        if docked {
            egui::Panel::right("inspector-dock")
                .resizable(true)
                .default_size(440.0)
                .min_size(320.0)
                .frame(Frame::new().fill(pal.surface).inner_margin(Margin::same(0)))
                .show(ui, |ui| body(ui));
        } else {
            egui::Window::new("Row inspector")
                .id(egui::Id::new("row-inspector"))
                .open(&mut open)
                .title_bar(false)
                .resizable(true)
                .collapsible(false)
                .default_size([w, h])
                .default_pos(egui::pos2(screen.right() - w - 24.0, screen.top() + 60.0))
                .constrain(true)
                .frame(
                    Frame::window(&ui.style())
                        .fill(pal.surface)
                        .stroke(Stroke::new(1.0, pal.control_border))
                        .inner_margin(Margin::same(0))
                        .corner_radius(CornerRadius::same(8)),
                )
                .show(&ctx, |ui| body(ui));
        }
        if !open {
            self.record_open = false;
        }
        if let Some((row, i)) = toggle {
            self.toggle_tag(row, i);
        }
        if let Some((k, pin)) = pin_act {
            self.prefs.pinned_fields.retain(|p| *p != k);
            if pin {
                self.prefs.pinned_fields.push(k);
            }
        }
        if let Some((name, f)) = make_col {
            self.bridge.send(Cmd::AddColumns(vec![(name, f)]));
        }
        if let Some(p) = pick {
            // the column's filter (a sentence chip), never search text
            crate::fields::apply(&p, &mut self.filters);
            self.maybe_send_query(true);
        }
        if step != 0 && snap.matched > 0 {
            let cur = self.grid.sel.unwrap_or(self.grid.top) as i64;
            let next = (cur + step).clamp(0, snap.matched as i64 - 1) as u64;
            self.grid.sel = Some(next);
            self.request_record(snap, next);
        }
    }

    fn workspace(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let rect = ui.max_rect();
        // a search that cannot run says so here — no stale rows pretending to answer it
        if let Some(err) = snap
            .query_error
            .as_ref()
            .filter(|_| snap.query == self.sent_query)
        {
            // "unknown column X" where X lives inside a structured column: offer to make it
            let missing = err
                .strip_prefix("unknown column ")
                .and_then(|r| r.split([' ', '—']).next())
                .map(|s| s.trim().to_string());
            let offer = missing.as_ref().and_then(|m| {
                self.raw_suggest
                    .as_ref()
                    .filter(|r| r.keys.iter().any(|k| k == m))
                    .map(|r| (m.clone(), r.col.clone(), r.kind))
            });
            // "did you mean X?" is a button, and the search can always run as plain text
            let suggestion = err
                .split("did you mean ")
                .nth(1)
                .map(|r| r.trim_end_matches('?').trim().to_string());
            let suggestion = suggestion.filter(|sg| {
                missing
                    .as_ref()
                    .is_some_and(|m| crate::filters::similar(m, sg))
            });
            let shown = if suggestion.is_some() {
                err.as_str()
            } else {
                err.split(" — did you mean").next().unwrap_or(err)
            };
            let c = rect.center();
            ui.painter().text(
                c - egui::vec2(0.0, 40.0),
                Align2::CENTER_CENTER,
                format!("{}  This search cannot run", ph::WARNING),
                fonts::strong(16.0),
                pal.danger,
            );
            ui.painter().text(
                c - egui::vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                shown,
                theme::ui_font(),
                pal.text,
            );
            let mut y = 26.0;
            if missing.is_some() && suggestion.is_none() {
                let b =
                    egui::Rect::from_center_size(c + egui::vec2(0.0, y), egui::vec2(300.0, 30.0));
                if ui
                    .put(b, egui::Button::new("Search it as plain text"))
                    .on_hover_text("Find the words anywhere in the rows instead")
                    .clicked()
                {
                    let q = self.query.trim().replace('"', "");
                    self.query = format!("\"{q}\"");
                    self.maybe_send_query(true);
                }
                y += 40.0;
            }
            if let Some(m) = missing.as_ref().filter(|_| offer.is_none()) {
                let b =
                    egui::Rect::from_center_size(c + egui::vec2(0.0, y), egui::vec2(300.0, 30.0));
                let mut ranked: Vec<&String> = snap.columns.iter().collect();
                ranked.sort_by_key(|n| crate::filters::distance(m, n));
                let mut ui2 = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(b)
                        .layout(Layout::top_down(Align::Center)),
                );
                ui2.menu_button(
                    format!("Use another column instead of {m}  {}", ph::CARET_DOWN),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for n in ranked {
                                    if ui.button(n.as_str()).clicked() {
                                        self.query = self.query.replace(m.as_str(), n.as_str());
                                        self.maybe_send_query(true);
                                        ui.close();
                                    }
                                }
                            });
                    },
                );
                y += 40.0;
            }
            if let (Some(m), Some(sug)) = (missing.as_ref(), suggestion.as_ref()) {
                let b = egui::Rect::from_center_size(
                    c + egui::vec2(-130.0, y),
                    egui::vec2(240.0, 30.0),
                );
                if ui
                    .put(b, egui::Button::new(format!("Use {sug} instead of {m}")))
                    .clicked()
                {
                    self.query = self.query.replace(m.as_str(), sug.as_str());
                    self.maybe_send_query(true);
                }
                let b2 =
                    egui::Rect::from_center_size(c + egui::vec2(130.0, y), egui::vec2(240.0, 30.0));
                if ui
                    .put(b2, egui::Button::new("Search it as plain text"))
                    .on_hover_text("Find the words anywhere in the rows instead")
                    .clicked()
                {
                    let q = self.query.trim().replace('"', "");
                    self.query = format!("\"{q}\"");
                    self.maybe_send_query(true);
                }
                y += 40.0;
            }
            if let Some((m, col, kind)) = offer {
                let b =
                    egui::Rect::from_center_size(c + egui::vec2(0.0, y), egui::vec2(340.0, 32.0));
                let label = format!("{}  Make {m} a column from {col}", ph::FUNCTION);
                if ui
                    .put(b, |ui: &mut egui::Ui| primary_button(ui, pal, &label))
                    .clicked()
                {
                    let name =
                        field_column_name(&m, &col, &snap.columns, self.prefs.short_field_names);
                    self.bridge.send(Cmd::AddColumns(vec![(
                        name,
                        crate::rawcols::extract_formula(&col, kind, &m),
                    )]));
                }
            }
            return;
        }
        if snap.total_rows == 0 {
            let msg = match &snap.phase {
                Phase::Importing {
                    bytes_done,
                    bytes_total,
                    ..
                } => {
                    let f = if *bytes_total > 0 {
                        *bytes_done as f64 / *bytes_total as f64 * 100.0
                    } else {
                        0.0
                    };
                    if *bytes_done == 0 {
                        "Opening the file.\nThe first rows appear in a moment.".to_string()
                    } else {
                        format!("Reading the file: {f:.0} %\nThe first rows appear in a moment.")
                    }
                }
                Phase::Failed(e) => format!("This file could not be indexed.\n{e}"),
                Phase::Partial(r) => format!("No rows could be indexed yet:\n{r}."),
                _ => "This file has no rows.".into(),
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                msg,
                theme::ui_font(),
                pal.text2,
            );
            return;
        }
        // "tagged rows only" with nothing tagged (or nothing tagged that also
        // matches the search): say that, not something about search words
        if snap.matched == 0
            && snap.query_error.is_none()
            && !snap.busy
            && self.tag_filter.is_some()
        {
            let c = rect.center();
            let searching = !snap.query.trim().is_empty() || !self.filters.is_empty();
            ui.painter().text(
                c - egui::vec2(0.0, 18.0),
                Align2::CENTER_CENTER,
                if searching {
                    "No tagged rows match this search"
                } else {
                    "No rows have this tag yet"
                },
                fonts::strong(15.0),
                pal.text,
            );
            ui.painter().text(
                c + egui::vec2(0.0, 8.0),
                Align2::CENTER_CENTER,
                "Only tagged rows are showing. Tag a row with the keys 1 to 9, or show every row again.",
                theme::ui_font(),
                pal.text2,
            );
            let b =
                egui::Rect::from_center_size(c + egui::vec2(0.0, 44.0), egui::vec2(150.0, 30.0));
            if ui.put(b, egui::Button::new("Show every row")).clicked() {
                self.set_tag_filter(None);
            }
            return;
        }
        if snap.matched == 0 && snap.query_error.is_none() && !snap.busy && self.filters.is_empty()
        {
            // both lines wrap inside the area: a long search or a long
            // sentence is never cut off at its edges
            let wrap = (rect.width() - 48.0).max(200.0);
            let centred = |text: String, font: egui::FontId, color: egui::Color32| {
                let mut job = egui::text::LayoutJob::simple(text, font, color, wrap);
                job.halign = egui::Align::Center;
                ui.painter().layout_job(job)
            };
            let head = centred(
                format!("No rows match {}", snap.query.trim()),
                fonts::strong(15.0),
                pal.text,
            );
            let advice = centred(
                no_rows_advice(snap.query.trim()).to_string(),
                theme::ui_font(),
                pal.text2,
            );
            let c = rect.center();
            let total = head.size().y + 8.0 + advice.size().y + 14.0 + 30.0;
            let mut y = c.y - total * 0.5;
            ui.painter().galley(egui::pos2(c.x, y), head.clone(), pal.text);
            y += head.size().y + 8.0;
            ui.painter().galley(egui::pos2(c.x, y), advice.clone(), pal.text2);
            y += advice.size().y + 14.0;
            let b = egui::Rect::from_center_size(egui::pos2(c.x, y + 15.0), egui::vec2(120.0, 30.0));
            if ui.put(b, egui::Button::new("Clear search")).clicked() {
                self.query.clear();
                self.maybe_send_query(true);
            }
            return;
        }
        if self.demo_record
            && snap
                .page
                .as_ref()
                .is_some_and(|p| p.generation == snap.generation && !p.rows.is_empty())
            && snap.phase.settled()
            && !snap.busy
            && snap.query == self.sent_query
            && self.edited_at.is_none()
        {
            self.demo_record = false;
            self.grid.sel = Some(2);
            self.record_open = true;
            self.request_record(snap, 2);
        }
        // developer switches for visual checks: LOGSEEK_SHOT_FILTER="col=text", LOGSEEK_SHOT_VALUES="col"
        // LOGSEEK_SHOT_FILTER="col=text;!col=text;~col=text" (! inverted, ~ switched off)
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_FILTER") {
            for item in spec.split(';') {
                let (invert, off, item) = match item.as_bytes().first() {
                    Some(b'!') => (true, false, &item[1..]),
                    Some(b'~') => (false, true, &item[1..]),
                    _ => (false, false, item),
                };
                if let Some((c, t)) = item.split_once('=') {
                    if !self.filters.contains_key(c) && snap.columns.iter().any(|n| n == c) {
                        self.filters.insert(
                            c.to_string(),
                            crate::filters::ColFilter {
                                text: t.to_string(),
                                invert,
                                enabled: !off,
                                ..Default::default()
                            },
                        );
                        self.edited_at = Some(Instant::now());
                    }
                }
            }
        }
        // LOGSEEK_SHOT_GRIDPOS="hscroll,row" scrolls sideways and selects a row (screenshots)
        if snap.page.is_some() && ui.ctx().input(|i| i.time) > 3.0 {
            if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_GRIDPOS") {
                std::env::remove_var("LOGSEEK_SHOT_GRIDPOS");
                let mut it = v.split(',').map(|x| x.trim().parse::<u64>().unwrap_or(0));
                self.grid.hscroll = it.next().unwrap_or(0) as f32;
                self.grid.top = 0;
                self.grid.sel = it.next();
                // a third number focuses that shown column (0 = the first)
                if let Some(n) = it.next() {
                    self.grid.focus_col = self.grid.order.get(n as usize).copied();
                }
                // a fourth: open a menu as the keyboard would (1 cell, 2 column)
                self.grid.dev_key_menu = it.next().map(|m| m as u8);
                // a fifth: a block of cells from that shown column, four rows deep
                if let (Some(n), Some(sel)) = (it.next(), self.grid.sel) {
                    self.grid.anchor_col = self.grid.order.get(n as usize).copied();
                    self.grid.multi.click(sel);
                    self.grid.multi.shift_click(sel + 3);
                    self.grid.sel = Some(sel + 3);
                }
            }
        }
        // LOGSEEK_SHOT_SELECT="3-8,12" selects those rows (places, from 0) and copies them
        // with the column names; "col:NAME" copies that whole column instead
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SELECT") {
            if snap.page.is_some() && ui.ctx().input(|i| i.time) > 4.0 {
                std::env::remove_var("LOGSEEK_SHOT_SELECT");
                let visible =
                    crate::grid::visible_order(&self.grid.order, &snap.columns, &self.grid.hidden);
                let (ranges, cols) = if let Some(name) = v.strip_prefix("col:") {
                    let c = snap.columns.iter().position(|x| x == name).unwrap_or(0);
                    (vec![(0, snap.matched.saturating_sub(1))], vec![c])
                } else {
                    for part in v.split(',') {
                        let (a, b) = part.split_once('-').unwrap_or((part, part));
                        let a: u64 = a.trim().parse().unwrap_or(0);
                        let b: u64 = b.trim().parse().unwrap_or(a);
                        for r in a..=b {
                            self.grid.multi.ctrl_click(r);
                        }
                    }
                    (self.grid.multi.ranges().to_vec(), visible)
                };
                self.copy_id += 1;
                self.bridge.send(Cmd::CopyRows {
                    id: self.copy_id,
                    ranges,
                    cols,
                    names: true,
                });
            }
        }
        // LOGSEEK_SHOT_SORT="column" or "column,desc" sorts once the columns are known
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SORT") {
            if snap.page.is_some() {
                std::env::remove_var("LOGSEEK_SHOT_SORT");
                let (name, how) = v.split_once(',').unwrap_or((v.as_str(), ""));
                self.bridge
                    .send(Cmd::Sort(Some((name.to_string(), how == "desc"))));
            }
        }
        // LOGSEEK_SHOT_FIND="text" opens Find and steps to the second match (screenshots)
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_FIND") {
            if snap.page.is_some() && ui.ctx().input(|i| i.time) > 3.0 {
                let done = self.find.as_ref().is_some_and(|f| f.id >= 2);
                let ready = self.find.as_ref().is_none_or(|f| f.applied);
                if done {
                    std::env::remove_var("LOGSEEK_SHOT_FIND");
                } else if ready {
                    let mut f = self.find.take().unwrap_or_default();
                    let same = f.sent == v;
                    f.text = v.clone();
                    f.sent = v.clone();
                    f.id += 1;
                    f.applied = false;
                    self.bridge.send(Cmd::Find {
                        id: f.id,
                        text: v,
                        after_row: if same {
                            self.selected_row_id(snap)
                        } else {
                            None
                        },
                        from_pos: self.grid.sel.filter(|_| same),
                        forward: true,
                    });
                    self.find = Some(f);
                }
            }
        }
        // LOGSEEK_SHOT_CHOOSER=1 opens "Choose columns" once the columns are known
        if !snap.columns.is_empty() && crate::dev::var_os("LOGSEEK_SHOT_CHOOSER").is_some() {
            std::env::remove_var("LOGSEEK_SHOT_CHOOSER");
            self.colchooser = Some(Default::default());
        }
        // LOGSEEK_SHOT_WRAP="column" wraps a column's long values (screenshots)
        if let Ok(name) = crate::dev::var("LOGSEEK_SHOT_WRAP") {
            if let Some(c) = snap.columns.iter().position(|x| *x == name) {
                std::env::remove_var("LOGSEEK_SHOT_WRAP");
                self.grid.wrap.insert(c);
                if let Some(w) = self.grid.widths.get_mut(c) {
                    *w = w.min(420.0);
                }
            }
        }
        // LOGSEEK_SHOT_COLDRAG="column|before column" holds a header drag for a screenshot
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_COLDRAG") {
            if snap.phase.settled() {
                let mut it = spec.split('|');
                let pos =
                    |n: Option<&str>| n.and_then(|n| snap.columns.iter().position(|c| c == n));
                if let Some(c) = pos(it.next()) {
                    std::env::remove_var("LOGSEEK_SHOT_COLDRAG");
                    self.grid.preview_drag(c, pos(it.next()));
                }
            }
        }
        // LOGSEEK_SHOT_PRESET=1 applies a saved search that names a column this file lacks
        if crate::dev::var_os("LOGSEEK_SHOT_PRESET").is_some()
            && snap.phase.settled()
            && !snap.columns.is_empty()
        {
            std::env::remove_var("LOGSEEK_SHOT_PRESET");
            let pr = Preset {
                name: "Web triage".into(),
                query: String::new(),
                columns: vec![
                    ("status".into(), "404".into(), None),
                    ("host".into(), "^=we1".into(), None),
                ],
                except: Vec::new(),
            };
            self.apply_preset(&pr, snap);
        }
        // LOGSEEK_SHOT_ADDCOL="card|column" opens "Add a column" on a card
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_ADDCOL") {
            let ready = snap
                .page
                .as_ref()
                .is_some_and(|p| p.generation == snap.generation && !p.cells.is_empty());
            if self.addcol.is_none() && snap.phase.settled() && ready {
                std::env::remove_var("LOGSEEK_SHOT_ADDCOL");
                let (card, col) = spec.split_once('|').unwrap_or((spec.as_str(), ""));
                let mut st = crate::addcol::AddColState::new(
                    (!col.is_empty()).then(|| col.to_string()),
                    &snap.columns,
                );
                st.card = card.parse().unwrap_or(0);
                self.addcol = Some(st);
            }
        }
        // LOGSEEK_SHOT_SUMMARY="by1|by2|measure#|column|bucket seconds" opens Summarise
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_SUMMARY") {
            if self.summary.is_none() && snap.phase.settled() && !self.sent_query.is_empty()
                || (self.summary.is_none() && snap.phase.settled() && self.query.is_empty())
            {
                std::env::remove_var("LOGSEEK_SHOT_SUMMARY");
                self.summary = Some(crate::summary::SummaryState::from_spec(
                    &spec,
                    &snap.columns,
                ));
            }
        }
        // LOGSEEK_SHOT_EXPORT="path|csv or jsonl" exports the current result once (no dialog)
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_EXPORT") {
            if snap.phase.settled()
                && snap.query == self.sent_query
                && !self.sent_query.is_empty()
                && snap.page.is_some()
            {
                std::env::remove_var("LOGSEEK_SHOT_EXPORT");
                let (path, fmt) = spec.split_once('|').unwrap_or((spec.as_str(), "csv"));
                let format = if fmt == "jsonl" {
                    crate::export::Format::Jsonl
                } else if fmt == "xlsx" {
                    crate::export::Format::Xlsx
                } else {
                    crate::export::Format::Csv
                };
                crate::export::CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
                self.bridge.send(Cmd::Export {
                    path: PathBuf::from(path),
                    format,
                    cols: (0..snap.columns.len()).collect(),
                });
            }
        }
        if crate::dev::var_os("LOGSEEK_SHOT_KEYS").is_some() {
            std::env::remove_var("LOGSEEK_SHOT_KEYS");
            self.keys_open = true;
        }
        // LOGSEEK_SHOT_CAL="2016-08-24[..2016-08-25][@6-9]" opens the calendar with a choice
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_CAL") {
            if !self.cal_open && snap.phase.settled() && snap.time_col.is_some() {
                std::env::remove_var("LOGSEEK_SHOT_CAL");
                let rest = leading_range(&self.query)
                    .map(|(_, _, r)| r.to_string())
                    .unwrap_or_else(|| self.query.clone());
                self.cal_query = crate::filters::compose(rest.trim(), &self.filters, None);
                self.bridge.send(Cmd::Calendar(self.cal_query.clone()));
                self.cal_open = true;
                let (days, hours) = spec
                    .split_once('@')
                    .map(|(d, h)| (d, Some(h)))
                    .unwrap_or((spec.as_str(), None));
                let day = |t: &str| {
                    logseek_core::time::parse_time(t.trim()).map(|us| us.div_euclid(86_400_000_000))
                };
                let (a, b) = days
                    .split_once("..")
                    .map(|(a, b)| (day(a), day(b)))
                    .unwrap_or((day(days), day(days)));
                self.cal = Default::default();
                self.cal.a = a;
                self.cal.b = b;
                self.cal.hours = hours
                    .and_then(|h| h.split_once('-'))
                    .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)));
            }
        }
        if let Ok(c) = crate::dev::var("LOGSEEK_SHOT_VALUES") {
            if self.values.is_none() && snap.phase.settled() && !self.sent_query.is_empty() {
                if let Some(col) = snap.columns.iter().position(|n| *n == c) {
                    // dev-only: open the list once, then clear the switch
                    std::env::remove_var("LOGSEEK_SHOT_VALUES");
                    let query = crate::filters::compose(&self.query, &self.filters, Some(&c));
                    let rare = crate::dev::var("LOGSEEK_SHOT_VALUES_SORT").as_deref() == Ok("rare");
                    self.bridge.send(Cmd::Values {
                        col,
                        query: query.clone(),
                        n: 500,
                        rare,
                        alpha: false,
                    });
                    self.values = Some(ValuesPopup {
                        col,
                        name: c,
                        at: ui.max_rect().center_top() + egui::vec2(-60.0, 50.0),
                        // LOGSEEK_SHOT_VALUES_FIND="text" starts with that search typed
                        search: crate::dev::var("LOGSEEK_SHOT_VALUES_FIND").unwrap_or_default(),
                        sel: Default::default(),
                        ready: false,
                        query,
                        sort: if rare {
                            ValueSort::Rare
                        } else {
                            ValueSort::Count
                        },
                        preview_sent: String::new(),
                        cond_tab: false,
                        conds: Vec::new(),
                        cal_row: None,
                        cal: Default::default(),
                        except: true,
                        cache: Default::default(),
                        cache_search: String::new(),
                        asked: Default::default(),
                        total: None,
                        pending: None,
                    });
                    // LOGSEEK_SHOT_COND="op#|a|b|or;op#|a" opens the Condition tab with rows
                    if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_COND") {
                        let kind = crate::conditions::ColKind::of(
                            snap.col_types.get(col),
                            snap.time_col == Some(col),
                        );
                        let ops = crate::conditions::ops_for(kind);
                        if let Some(vp) = self.values.as_mut() {
                            vp.cond_tab = true;
                            vp.conds = spec
                                .split(';')
                                .filter_map(|r| {
                                    let f: Vec<&str> = r.split('|').collect();
                                    let op = *ops.get(f.first()?.parse::<usize>().ok()?)?;
                                    Some(CondRow {
                                        op,
                                        a: f.get(1).unwrap_or(&"").to_string(),
                                        b: f.get(2).unwrap_or(&"").to_string(),
                                        and: f.get(3) != Some(&"or"),
                                    })
                                })
                                .collect();
                            if crate::dev::var_os("LOGSEEK_SHOT_CONDCAL").is_some() {
                                vp.cal_row = Some(0);
                                vp.cal.compact = true;
                                self.bridge.send(Cmd::Calendar(vp.query.clone()));
                            }
                        }
                    }
                }
            }
        }
        // dev switch: LOGSEEK_SHOT_TAGS="idx:tag,…" tags result rows once (tag 1-9), LOGSEEK_SHOT_TAB=tags
        let settled = snap.phase.settled()
            && !snap.busy
            && snap.query == self.sent_query
            && self.edited_at.is_none();
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_TAGS") {
            if settled {
                if let Some(p) = snap
                    .page
                    .as_ref()
                    .filter(|p| p.generation == snap.generation && p.start == 0)
                {
                    std::env::remove_var("LOGSEEK_SHOT_TAGS");
                    for part in spec.split(',') {
                        if let Some((i, t)) = part.split_once(':') {
                            if let (Ok(i), Ok(t)) = (i.parse::<usize>(), t.parse::<usize>()) {
                                if let Some(&row) = p.rows.get(i) {
                                    self.toggle_tag(row, t.saturating_sub(1).min(8));
                                }
                            }
                        }
                    }
                    if crate::dev::var("LOGSEEK_SHOT_TAB").as_deref() == Ok("tags") {
                        self.left_tab = LeftTab::Tags;
                        self.prefs.fields_open = Some(true);
                        // LOGSEEK_SHOT_NAMING=1 also switches "Name the tags" on
                        if crate::dev::var_os("LOGSEEK_SHOT_NAMING").is_some() {
                            ui.data_mut(|d| d.insert_temp(egui::Id::new("naming-tags"), true));
                        }
                    }
                    if crate::dev::var_os("LOGSEEK_SHOT_TAGGED").is_some() {
                        self.set_tag_filter(Some(0x1FF));
                    }
                }
            }
        }
        let keys = !ui.ctx().text_edit_focused() && self.values.is_none();
        let mut terms = crate::filters::highlight_terms(&self.query);
        if let Some(f) = self.find.as_ref().filter(|f| !f.sent.trim().is_empty()) {
            terms.push(f.sent.trim().to_string());
        }
        self.find_bar(ui, pal, snap);
        // dev switch: LOGSEEK_SHOT_SEARCH="keyword" opens the Search window on it
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SEARCH") {
            if snap.page.is_some() {
                std::env::remove_var("LOGSEEK_SHOT_SEARCH");
                let (marks, others) = crate::coverage::marks(&self.query);
                let mut w = crate::searchwin::SearchWin::with_text(&v);
                if !marks.is_empty() {
                    w.from_search = true;
                    w.focus = false;
                    w.text = marks[0].text.clone();
                    w.col = marks[0].col.clone();
                    w.marks = marks.into_iter().map(|m| (m.label, m.text, m.col)).collect();
                    w.others = others;
                }
                self.search_win = Some(w);
            }
        }
        // dev switch: LOGSEEK_SHOT_LIST="file with one keyword per line" opens Search a list and runs it
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_LIST") {
            if snap.page.is_some() && snap.phase.settled() {
                std::env::remove_var("LOGSEEK_SHOT_LIST");
                let mut w = crate::listwin::ListWin::default();
                // a table waits for its column to be chosen; a plain list runs at once
                w.load_file(std::path::Path::new(&v));
                w.run_now = !w.text.is_empty();
                self.list_win = Some(w);
            }
        }
        if let Some(mut w) = self.list_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            match act {
                crate::listwin::ListAction::None => {}
                crate::listwin::ListAction::Cells(text) => {
                    self.search_win = Some(crate::searchwin::SearchWin::with_text(&text));
                }
                crate::listwin::ListAction::Search(q) => {
                    self.push_history();
                    self.query = q;
                    self.maybe_send_query(true);
                }
                crate::listwin::ListAction::Show {
                    ids,
                    found,
                    case,
                    word,
                } => {
                    self.list_marks = crate::found::Marks::new(found, case, word);
                    self.list_marks_page = (u64::MAX, 0, 0);
                    // the rows that hold a keyword, in the plain list
                    self.tag_filter = None;
                    self.grid.ftree = None;
                    self.grid.grouping = None;
                    self.grid.reset();
                    self.follow_showing = true;
                    self.shown_as = None;
                    self.why = None;
                    self.showing_list = true;
                    self.bridge.send(Cmd::Restrict(Some(ids)));
                    self.hide_sheet = true;
                }
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.list_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::List(w), snap);
            }
        }
        // dev switch: LOGSEEK_SHOT_EXPLAIN=1 (or "times") opens "What sets these rows apart?"
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_EXPLAIN") {
            if snap.page.is_some()
                && snap.phase.settled()
                && snap.query == self.sent_query
                && snap.query.trim() == self.query.trim()
            {
                std::env::remove_var("LOGSEEK_SHOT_EXPLAIN");
                let mut w = crate::explainwin::ExplainWin::default();
                w.against = match v.as_str() {
                    "times" => crate::bridge::Against::OtherTimes,
                    "before" => crate::bridge::Against::Before,
                    "just" => crate::bridge::Against::JustBefore,
                    _ => w.against,
                };
                self.explain_win = Some(w);
            }
        }
        // dev switch: LOGSEEK_SHOT_SHAPES="column" opens "Shapes in this column"
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SHAPES") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_SHAPES");
                self.shape_win = Some(crate::shapewin::ShapeWin::new(v));
            }
        }
        // dev switch: LOGSEEK_SHOT_STRANGE="column" opens "Strange text in this column"
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_STRANGE") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_STRANGE");
                self.strange_win = Some(crate::strangewin::StrangeWin::new(v));
            }
        }
        // dev switch: LOGSEEK_SHOT_PATTERNS="column" opens "Patterns in this column"
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_PATTERNS") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_PATTERNS");
                self.pattern_win = Some(crate::patternwin::PatternWin::new(v));
            }
        }
        // dev switch: LOGSEEK_SHOT_UNUSUAL=1 opens "Unusual rows" (=show also shows them)
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_UNUSUAL") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_UNUSUAL");
                let mut w = crate::unusualwin::UnusualWin::default();
                w.show_now = v == "show";
                self.unusual_win = Some(w);
            }
        }
        // dev switch: LOGSEEK_SHOT_PAIRS="column a|column b" opens "Rare pairs" on them
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_PAIRS") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_PAIRS");
                let (a, b) = v.split_once('|').unwrap_or((v.as_str(), ""));
                self.pair_win = Some(if b.is_empty() {
                    crate::pairwin::PairWin::new(a.to_string())
                } else {
                    crate::pairwin::PairWin::with(a.to_string(), b.to_string())
                });
            }
        }
        // dev switch: LOGSEEK_SHOT_SIGNALS="0" / "1" / "2:column" opens "Timeline signals" on a tab
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SIGNALS") {
            if snap.page.is_some() && snap.phase.settled() && snap.query == self.sent_query {
                std::env::remove_var("LOGSEEK_SHOT_SIGNALS");
                let mut w = crate::rhythmwin::RhythmWin::default();
                let (tab, col) = v.split_once(':').unwrap_or((v.as_str(), ""));
                w.tab = tab.parse().unwrap_or(0);
                if !col.is_empty() {
                    w.col = Some(col.to_string());
                }
                self.rhythm_win = Some(w);
            }
        }
        if let Some(mut w) = self.rhythm_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            match act {
                crate::rhythmwin::RhythmAction::None => {}
                crate::rhythmwin::RhythmAction::AddToSearch(t) => {
                    self.push_history();
                    let cur = self.query.trim().to_string();
                    self.query = if cur.is_empty() {
                        t
                    } else if cur.contains(" OR ") {
                        format!("({cur}) {t}")
                    } else {
                        format!("{cur} {t}")
                    };
                    self.maybe_send_query(true);
                }
                crate::rhythmwin::RhythmAction::Period(a, b) => {
                    self.push_history();
                    let rest = leading_range(&self.query)
                        .map(|(_, _, r)| r.trim().to_string())
                        .unwrap_or_else(|| self.query.trim().to_string());
                    let range = format!(
                        "@{}..{}",
                        crate::rhythmwin::stamp(a),
                        crate::rhythmwin::stamp(b)
                    );
                    self.query = if rest.is_empty() {
                        range
                    } else {
                        format!("{range} {rest}")
                    };
                    self.maybe_send_query(true);
                }
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.rhythm_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Rhythm(w), snap);
            }
        }
        if let Some(mut w) = self.pair_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            if let crate::pairwin::PairAction::AddToSearch(t) = act {
                self.push_history();
                let cur = self.query.trim().to_string();
                self.query = if cur.is_empty() {
                    t
                } else if cur.contains(" OR ") {
                    format!("({cur}) {t}")
                } else {
                    format!("{cur} {t}")
                };
                self.maybe_send_query(true);
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.pair_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Pair(w), snap);
            }
        }
        if let Some(mut w) = self.unusual_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            let mut close = !open;
            if let crate::unusualwin::UnusualAction::ShowRows(ids, model) = act {
                self.tag_filter = None;
                self.grid.ftree = None;
                self.grid.grouping = None;
                self.grid.reset();
                self.follow_showing = true;
                self.showing_list = false;
                self.shown_as = Some("unusual rows".to_string());
                self.why = Some(model);
                self.list_marks_page = (u64::MAX, 0, 0);
                self.bridge.send(Cmd::Restrict(Some(ids)));
                self.hide_sheet = true;
                close = true;
            }
            if !close {
                self.unusual_win = Some(w);
            }
        }
        if let Some(mut w) = self.pattern_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            if let crate::patternwin::PatternAction::ShowRows(ids, what) = act {
                self.tag_filter = None;
                self.grid.ftree = None;
                self.grid.grouping = None;
                self.grid.reset();
                self.follow_showing = true;
                self.showing_list = false;
                self.shown_as = Some(what);
                self.bridge.send(Cmd::Restrict(Some(ids)));
                self.hide_sheet = true;
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.pattern_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Pattern(w), snap);
            }
        }
        if let Some(mut w) = self.strange_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            match act {
                crate::strangewin::StrangeAction::None => {}
                crate::strangewin::StrangeAction::AddToSearch(t) => {
                    self.push_history();
                    let cur = self.query.trim().to_string();
                    self.query = if cur.is_empty() {
                        t
                    } else if cur.contains(" OR ") {
                        format!("({cur}) {t}")
                    } else {
                        format!("{cur} {t}")
                    };
                    self.maybe_send_query(true);
                }
                crate::strangewin::StrangeAction::ShowRows(ids) => {
                    self.tag_filter = None;
                    self.grid.ftree = None;
                    self.grid.grouping = None;
                    self.grid.reset();
                    self.follow_showing = true;
                    self.showing_list = false;
                    self.shown_as = Some(format!("rows with strange text in {}", w.col));
                    self.bridge.send(Cmd::Restrict(Some(ids)));
                    self.hide_sheet = true;
                }
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.strange_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Strange(w), snap);
            }
        }
        if let Some(mut w) = self.shape_win.take() {
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge);
            if let crate::shapewin::ShapeAction::AddToSearch(t) = act {
                self.push_history();
                let cur = self.query.trim().to_string();
                self.query = if cur.is_empty() {
                    t
                } else if cur.contains(" OR ") {
                    format!("({cur}) {t}")
                } else {
                    format!("{cur} {t}")
                };
                self.maybe_send_query(true);
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.shape_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Shape(w), snap);
            }
        }
        if let Some(mut w) = self.explain_win.take() {
            let period = logseek_core::query::parse(snap.query.trim())
                .map(|q| q.time.is_some())
                .unwrap_or(false);
            let (open, act) = w.show(ui.ctx(), pal, snap, &self.bridge, period);
            if let crate::explainwin::ExplainAction::AddToSearch(t) = act {
                self.push_history();
                let cur = self.query.trim().to_string();
                self.query = if cur.is_empty() {
                    t
                } else if cur.contains(" OR ") {
                    format!("({cur}) {t}")
                } else {
                    format!("{cur} {t}")
                };
                self.maybe_send_query(true);
            }
            // showing its rows puts the sheet aside, so the rows can be seen;
            // "Back to …" in the status bar brings it back as it was
            if open && !std::mem::take(&mut self.hide_sheet) {
                self.explain_win = Some(w);
            } else {
                self.keep_closed(ClosedSheet::Explain(w), snap);
            }
        }
        if let Some(mut w) = self.search_win.take() {
            // opened from the search: it follows the search as it changes
            if w.from_search && w.for_query != self.query {
                let (marks, others) = crate::coverage::marks(&self.query);
                let keep = marks
                    .iter()
                    .any(|m| m.text == w.text.trim() && m.col == w.col);
                if !keep {
                    w.text = marks.first().map(|m| m.text.clone()).unwrap_or_default();
                    w.col = marks.first().and_then(|m| m.col.clone());
                }
                w.marks = marks.into_iter().map(|m| (m.label, m.text, m.col)).collect();
                w.others = others;
                w.for_query = self.query.clone();
            }
            let (open, jump) = w.show(ui.ctx(), pal, snap, &self.bridge);
            if let Some(j) = jump {
                // the match's row and cell, in the plain list
                self.grid.grouping = None;
                self.grid.ftree = None;
                if let Some(pos) = j.pos {
                    self.grid.sel = Some(pos);
                    let (top, vis) = self.grid.visible_range();
                    if pos < top || pos >= top + vis.saturating_sub(1) {
                        self.grid.top = pos.saturating_sub(4);
                    }
                }
                if let Some(n) = snap.columns.get(j.col) {
                    self.grid.hidden.remove(n);
                }
                self.grid.goto = Some(j.col);
            }
            if open {
                self.search_win = Some(w);
            }
        }
        // a list search's rows are showing: say, for the rows on screen, which
        // keywords each holds (from the cells already here; nothing is asked of the engine)
        match (&self.list_marks, snap.page.as_ref()) {
            // the unusual rows: why each row on screen is there
            (_, Some(page)) if self.follow_showing && self.why.is_some() => {
                let key = (page.generation, page.start, page.rows.len());
                if self.list_marks_page != key || self.grid.found.is_none() {
                    self.list_marks_page = key;
                    let model = self.why.clone().unwrap();
                    let mut band = crate::grid::FoundBand {
                        title: "Why",
                        ..Default::default()
                    };
                    let font = theme::small_font();
                    for (id, cells) in page.rows.iter().zip(&page.cells) {
                        let why = model.why(cells);
                        if why.is_empty() {
                            continue;
                        }
                        let line = crate::unusualwin::short(&why, &snap.columns);
                        let w = ui
                            .painter()
                            .layout_no_wrap(line.clone(), font.clone(), pal.text)
                            .size()
                            .x;
                        band.w = band.w.max(w + 20.0);
                        band.rows
                            .insert(*id, (line, crate::unusualwin::full(&why, &snap.columns)));
                    }
                    band.w = band.w.max(72.0);
                    self.grid.found = Some(band);
                }
            }
            (Some(m), Some(page)) if self.follow_showing && self.showing_list => {
                let key = (page.generation, page.start, page.rows.len());
                if self.list_marks_page != key || self.grid.found.is_none() {
                    self.list_marks_page = key;
                    let mut band = crate::grid::FoundBand {
                        title: "Found",
                        ..Default::default()
                    };
                    let font = theme::small_font();
                    for (id, cells) in page.rows.iter().zip(&page.cells) {
                        let hits = m.of_row(cells);
                        if hits.is_empty() {
                            continue;
                        }
                        let line = crate::found::short(&hits, &m.terms);
                        let w = ui
                            .painter()
                            .layout_no_wrap(line.clone(), font.clone(), pal.text)
                            .size()
                            .x;
                        band.w = band.w.max(w + 20.0);
                        band.rows.insert(
                            *id,
                            (line, crate::found::full(&hits, &m.terms, &snap.columns)),
                        );
                    }
                    // room for the heading even when a page has short lines
                    band.w = band.w.max(72.0);
                    self.grid.found = Some(band);
                }
            }
            _ => {
                self.grid.found = None;
                self.list_marks_page = (u64::MAX, 0, 0);
            }
        }
        self.grid.times_utc = self.prefs.times_utc;
        self.grid.short_field_names = self.prefs.short_field_names;
        self.grid.number_by_file = self.prefs.number_by_file;
        // a tree of followed rows reads in the order the rows are sorted by:
        // asked for when the tree is made, and again whenever the sort changes
        if let Some(t) = self.grid.ftree.as_ref() {
            // the column the rows are sorted by; when they are not sorted, the
            // first column on screen that is shown as a time (the time you
            // can see, which need not be the file's own timeline column)
            let by = snap.sort.clone().or_else(|| {
                crate::grid::visible_order(&self.grid.order, &snap.columns, &self.grid.hidden)
                    .into_iter()
                    .find(|&c| {
                        match snap.columns.get(c).and_then(|n| self.grid.shown_as.get(n)).map(String::as_str) {
                            Some("written") => false,
                            Some(_) => true,
                            None => crate::grid::reads_as_time(&snap.col_types, snap.time_col, c),
                        }
                    })
                    .and_then(|c| snap.columns.get(c).cloned())
                    .map(|n| (n, false))
            });
            let want = (t.id, by);
            let have = self.tree_order.as_ref().map(|(i, s, _)| (*i, s.clone()));
            if have != Some(want.clone()) {
                self.keep_seq += 1;
                let id = (1u64 << 61) + self.keep_seq;
                self.tree_order = Some((want.0, want.1, id));
                self.bridge.send(Cmd::RowKeys {
                    id,
                    rows: std::sync::Arc::new(t.all_rows()),
                    by: self.tree_order.as_ref().and_then(|o| o.1.clone()),
                });
            }
        } else {
            self.tree_order = None;
        }
        if let (Some((_, _, want)), Some((id, keys))) = (self.tree_order.as_ref(), snap.row_keys.as_ref()) {
            if id == want {
                if let Some(t) = self.grid.ftree.as_mut() {
                    // applied once: the request id is cleared so it is not redone
                    t.reorder(keys);
                }
                if let Some(o) = self.tree_order.as_mut() {
                    o.2 = 0;
                }
            }
        }
        // the row asked to be kept has been found in the new result: select it
        // and bring it into view, with room above for what came before it
        if let (Some(want), Some((id, pos))) = (self.keep_row, snap.located) {
            if id == want {
                self.keep_row = None;
                if let (Some(pos), true) = (
                    pos,
                    self.grid.grouping.is_none() && self.grid.ftree.is_none(),
                ) {
                    self.grid.sel = Some(pos);
                    let (_, vis) = self.grid.visible_range();
                    self.grid.top = top_for_kept_row(pos, vis);
                    if self.record_open {
                        self.request_record(snap, pos);
                    }
                }
            }
        }
        // a new result (a filter removed, Show all rows, a sort): the row you
        // were on is looked for in it, so you stay on it and see the rows
        // around it. Only when it was selected in the result just replaced,
        // in this same file, and nothing is selected yet in the new one.
        if snap.generation != self.keep_gen {
            let was = self.keep_gen;
            self.keep_gen = snap.generation;
            let here = snap.source.as_ref().map(|s| &s.path);
            if let Some((row, seen, path)) = self.sel_mem.as_ref() {
                if *seen == was && here == Some(path) && self.grid.sel.is_none() {
                    self.keep_seq += 1;
                    let id = (1u64 << 62) + self.keep_seq;
                    self.keep_row = Some(id);
                    self.bridge.send(Cmd::Locate { id, row: *row });
                }
            }
        }
        // which file row is selected, noted while its page is loaded
        let now = self.grid.sel.and_then(|i| {
            let p = snap.page.as_ref().filter(|p| p.generation == snap.generation)?;
            p.rows.get(i.checked_sub(p.start)? as usize).copied()
        });
        if let (Some(row), Some(src)) = (now, snap.source.as_ref()) {
            self.sel_mem = Some((row, snap.generation, src.path.clone()));
        }
        // how each column of this file is shown, where the user chose
        let shown = snap
            .source
            .as_ref()
            .and_then(|s| self.prefs.shown_as.get(&s.path.to_string_lossy().into_owned()));
        if shown != Some(&self.grid.shown_as) {
            self.grid.shown_as = shown.cloned().unwrap_or_default();
        }
        self.grid.row_h = if self.prefs.compact { 20.0 } else { 24.0 };
        self.grid.lines = self.prefs.grid_lines;
        if self.grid.colors.source != self.prefs.colors {
            self.grid.colors = crate::colors::Compiled::new(&self.prefs.colors);
        }
        // dev switch: LOGSEEK_SHOT_GROUP="col[,col]" groups; LOGSEEK_SHOT_GROUP_OPEN=1
        // opens the first group at each level (screenshots)
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_GROUP") {
            if !snap.columns.is_empty() {
                std::env::remove_var("LOGSEEK_SHOT_GROUP");
                let by: Vec<String> = v.split(',').map(|x| x.trim().to_string()).collect();
                self.grid.grouping = Some(crate::groups::Grouping::new(by, Default::default()));
            }
        }
        if crate::dev::var_os("LOGSEEK_SHOT_GROUP_OPEN").is_some() {
            if let Some(g) = self.grid.grouping.as_mut() {
                let mut path = vec![0u64];
                while g.tree.is_open(&path) {
                    path.push(0);
                }
                if path.len() > g.by.len() {
                    std::env::remove_var("LOGSEEK_SHOT_GROUP_OPEN");
                } else if g.group(&path).is_some() {
                    g.toggle(&path, &self.bridge);
                }
            }
        }
        // the Follow tree shows the followed rows as found; a search, a filter,
        // a tag filter or grouping works on them as a plain list instead
        if self.grid.ftree.is_some()
            && (!self.follow_showing
                || !self.query.trim().is_empty()
                || self.filters.values().any(|f| f.is_active())
                || self.tag_filter.is_some()
                || self.grid.grouping.is_some())
        {
            self.grid.ftree = None;
        }
        self.group_drop(ui, pal, snap);
        if self.grid.grouping.is_some() {
            self.group_line(ui, pal);
        } else {
            self.group_rect = None;
        }
        let ev = grid::show(
            ui,
            pal,
            snap,
            &mut self.grid,
            &self.bridge,
            keys,
            &mut self.filters,
            &terms,
            &self.tags,
        );
        self.values_popup(ui, pal, snap);
        self.calendar_popup(ui, pal, snap);
        self.colors_sheet(ui, pal, snap);
        if let Some(mut st) = self.palette.take() {
            let (item, open) = crate::palette::show(ui.ctx(), pal, &mut st, &snap.columns);
            if open {
                self.palette = Some(st);
            }
            match item {
                Some(crate::palette::Item::Run(c)) => {
                    let now = ui.input(|i| i.time);
                    self.run(c, snap, now);
                }
                Some(crate::palette::Item::Column(c)) => {
                    if let Some(n) = snap.columns.get(c) {
                        self.grid.hidden.remove(n);
                    }
                    self.grid.goto = Some(c);
                }
                None => {}
            }
        }
        self.case_sheet(ui, pal, snap);
        self.work_sheet(ui, pal, snap);
        self.follow_sheet(ui, pal, snap);
        self.addcol_sheet(ui, pal, snap);
        // dev switch: start the chooser from no columns, then pick four (screenshots)
        if self.chooser_none && !snap.columns.is_empty() && snap.page.is_some() {
            self.chooser_none = false;
            crate::colchooser::show_none(&mut self.grid.hidden, &snap.columns, &self.grid.pinned);
            for c in [5usize, 2, 9, 3] {
                if c < snap.columns.len() {
                    crate::colchooser::add_to_end(
                        &mut self.grid.order,
                        &mut self.grid.hidden,
                        &snap.columns,
                        c,
                    );
                }
            }
        }
        if let Some(mut st) = self.colchooser.take() {
            let keep = crate::colchooser::show(ui.ctx(), pal, &mut st, snap, &mut self.grid);
            if keep && !ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                self.colchooser = Some(st);
            }
        }
        self.summary_sheet(ui, pal, snap);
        self.export_popover(ui, pal, snap);
        self.keys_overlay(ui, pal);
        // filters that leave nothing: offer the way back right where the eye is
        if snap.matched == 0
            && !snap.busy
            && snap.query == self.sent_query
            && self.any_filter()
            && !self.history.is_empty()
        {
            let c = ui.max_rect().center();
            let b =
                egui::Rect::from_center_size(c + egui::vec2(0.0, 30.0), egui::vec2(220.0, 32.0));
            if ui
                .put(
                    b,
                    egui::Button::new(format!(
                        "{}  Undo last filter  (Ctrl+Z)",
                        ph::ARROW_U_UP_LEFT
                    )),
                )
                .clicked()
            {
                self.undo_filter();
            }
        }
        match ev {
            GridEvent::Cell(a) => {
                use crate::fields::Pick;
                let pick = match a {
                    grid::CellAction::Follow { row, col, value, to, repeat } => {
                        self.follow_from_cell(snap, row, col, value, to, repeat);
                        return;
                    }
                    grid::CellAction::Only { col, value } => Pick::Only(col, value),
                    grid::CellAction::Exclude { col, value } => Pick::Exclude(col, value),
                    grid::CellAction::ColourLike { col, value } => {
                        // a new colour rule from the cell, then the sheet to see or change it
                        let r = crate::colors::rule_from_cell(&self.prefs.colors, &col, &value);
                        let tint = crate::colors::TINTS[r.tint % crate::colors::TINTS.len()];
                        self.prefs.colors.push(r);
                        self.colors_open = true;
                        let now = ui.input(|i| i.time);
                        self.flash = Some((
                            format!("Rows where {col} is {value} are coloured {tint}. Change or remove it in Colour rules."),
                            now,
                            false,
                        ));
                        return;
                    }
                    grid::CellAction::Tag { row, tag } => {
                        self.toggle_tag(row, tag);
                        return;
                    }
                };
                // the column's filter (a sentence chip), never search text
                crate::fields::apply(&pick, &mut self.filters);
                self.maybe_send_query(true);
            }
            GridEvent::Header(a) => match a {
                grid::HeaderAction::Remove(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.ask_remove(name.clone(), &snap.saved_cols);
                    }
                }
                grid::HeaderAction::Rename(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.rename_ask = Some((name.clone(), name.clone()));
                    }
                }
                grid::HeaderAction::Density => self.prefs.compact = !self.prefs.compact,
                grid::HeaderAction::FieldNames(short) => self.prefs.short_field_names = short,
                grid::HeaderAction::ShowAs(c, how) => {
                    if let (Some(src), Some(name)) = (snap.source.as_ref(), snap.columns.get(c)) {
                        let key = src.path.to_string_lossy().into_owned();
                        let of_file = self.prefs.shown_as.entry(key.clone()).or_default();
                        match how {
                            Some(h) => {
                                of_file.insert(name.clone(), h);
                            }
                            None => {
                                of_file.remove(name);
                            }
                        }
                        if of_file.is_empty() {
                            self.prefs.shown_as.remove(&key);
                        }
                        // widths were fitted to the old way of showing the values
                        self.grid.refit(c);
                    }
                }
                grid::HeaderAction::GridLines => self.prefs.grid_lines = !self.prefs.grid_lines,
                grid::HeaderAction::Choose => self.colchooser = Some(Default::default()),
                grid::HeaderAction::Pairs(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.pair_win = Some(crate::pairwin::PairWin::new(name.clone()));
                    }
                }
                grid::HeaderAction::Patterns(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.pattern_win = Some(crate::patternwin::PatternWin::new(name.clone()));
                    }
                }
                grid::HeaderAction::Strange(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.strange_win = Some(crate::strangewin::StrangeWin::new(name.clone()));
                    }
                }
                grid::HeaderAction::Shapes(c) => {
                    if let Some(name) = snap.columns.get(c) {
                        self.shape_win = Some(crate::shapewin::ShapeWin::new(name.clone()));
                    }
                }
                grid::HeaderAction::CopyColumn(c) => {
                    if snap.matched > 0 {
                        self.copy_id += 1;
                        self.bridge.send(Cmd::CopyRows {
                            id: self.copy_id,
                            ranges: vec![(0, snap.matched - 1)],
                            cols: vec![c],
                            names: true,
                        });
                    }
                }
                grid::HeaderAction::Sort(c, how) => {
                    self.grid.top = 0;
                    self.grid.sel = None;
                    let spec = how.and_then(|desc| snap.columns.get(c).map(|n| (n.clone(), desc)));
                    self.bridge.send(Cmd::Sort(spec));
                }
                grid::HeaderAction::AddFrom(c) => {
                    self.addcol = Some(crate::addcol::AddColState::new(
                        snap.columns.get(c).cloned(),
                        &snap.columns,
                    ));
                }
                grid::HeaderAction::Convert(c, how) => {
                    self.addcol = Some(crate::addcol::AddColState::convert(
                        snap.columns.get(c).cloned(),
                        &snap.columns,
                        how,
                    ));
                }
            },
            GridEvent::OpenValues { col, at } => self.open_values(snap, col, at, false),
            GridEvent::Open(i) => {
                self.record_open = true;
                self.request_record(snap, i);
            }
            GridEvent::Select(i) => {
                if self.record_open {
                    self.request_record(snap, i);
                }
            }
            GridEvent::CopyRows { names } => {
                // a block of cells copies its own columns; rows copy every shown one
                let cols = self.grid.block().unwrap_or_else(|| {
                    crate::grid::visible_order(&self.grid.order, &snap.columns, &self.grid.hidden)
                });
                let mut ranges = self.grid.multi.ranges().to_vec();
                if ranges.is_empty() {
                    ranges.extend(self.grid.sel.map(|s| (s, s)));
                }
                if !ranges.is_empty() {
                    self.copy_id += 1;
                    self.bridge.send(Cmd::CopyRows {
                        id: self.copy_id,
                        ranges,
                        cols,
                        names,
                    });
                }
            }
            // grouped view: the grid already knows the file row
            GridEvent::OpenRow(row) => {
                self.record_open = true;
                self.bridge.send(Cmd::Record(row));
            }
            GridEvent::SelectRow(row) => {
                if self.record_open {
                    self.bridge.send(Cmd::Record(row));
                }
            }
            GridEvent::None => {}
        }
    }

    fn values_popup(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(mut vp) = self.values.take() else {
            return;
        };
        let want_rare = vp.sort == ValueSort::Rare;
        let list = snap
            .values
            .as_ref()
            .filter(|v| v.col == vp.col && v.query == vp.query && v.rare == want_rare)
            .cloned();
        if let (Some(l), false) = (&list, vp.ready) {
            vp.ready = true;
            let _ = l;
            match self.filters.get(&vp.name).filter(|f| f.values.is_some()) {
                Some(f) => {
                    vp.sel = f.values.clone().unwrap_or_default();
                    vp.except = f.values_except;
                }
                None => {
                    vp.sel.clear();
                    vp.except = true;
                }
            }
        }
        // what Apply would do, counted before it is applied: every value
        // checked = no value filter; otherwise every value but the unchecked
        // ones, or only the checked ones
        let candidate_values = if vp.except && vp.sel.is_empty() {
            None
        } else {
            Some(vp.sel.clone())
        };
        // the condition builder writes the column's filter-row text
        let kind = crate::conditions::ColKind::of(
            snap.col_types.get(vp.col),
            snap.time_col == Some(vp.col),
        );
        if vp.conds.is_empty() {
            vp.conds.push(CondRow {
                op: crate::conditions::ops_for(kind)[0],
                a: String::new(),
                b: String::new(),
                and: true,
            });
        }
        let newest = snap.time_bounds.map(|(_, hi)| hi);
        let row_texts: Vec<Result<String, String>> = vp
            .conds
            .iter()
            .map(|r| crate::conditions::to_filter_text(r.op, &r.a, &r.b, newest))
            .collect();
        let cond_text: Option<String> = if row_texts.iter().all(|r| r.is_ok()) {
            let mut acc: Option<String> = None;
            for (r, t) in vp.conds.iter().zip(row_texts.iter()) {
                let t = t.as_ref().unwrap();
                acc = Some(match acc {
                    None => t.clone(),
                    Some(x) => crate::conditions::combine(&x, r.and, t),
                });
            }
            acc
        } else {
            None
        };
        let mut candidate = self.filters.clone();
        {
            let e = candidate.entry(vp.name.clone()).or_default();
            if vp.cond_tab {
                if let Some(t) = &cond_text {
                    e.text = t.clone();
                }
            } else {
                e.values = candidate_values.clone();
                e.values_except = vp.except;
            }
            if !e.is_active() {
                candidate.remove(&vp.name);
            }
        }
        let preview_q = crate::filters::compose(&self.query, &candidate, None);
        if (vp.ready || vp.cond_tab) && vp.preview_sent != preview_q {
            vp.preview_sent = preview_q.clone();
            self.bridge.send(Cmd::Count(preview_q.clone()));
        }
        let unchanged = preview_q == self.effective_query();
        let preview = snap
            .preview
            .as_ref()
            .filter(|(q, _)| *q == preview_q)
            .map(|(_, n)| *n);

        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let popup_w = if vp.cond_tab && vp.cal_row.is_some() {
            450.0
        } else {
            330.0
        };
        let at = egui::pos2(
            vp.at.x.clamp(
                screen.left() + 8.0,
                (screen.right() - popup_w - 8.0).max(screen.left() + 8.0),
            ),
            vp.at.y,
        );
        let (mut apply, mut close, mut remove_filter, mut resort) = (false, false, false, None);
        let mut send_cal = false;
        let area = egui::Area::new(egui::Id::new("values-popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(popup_w);
                        ui.label(
                            RichText::new(format!("Filter {}", vp.name)).font(fonts::strong(13.0)),
                        );
                        // two ways to filter one column: tick values, or describe a condition
                        ui.horizontal(|ui| {
                            for (cond, label, tip) in [
                                (false, "Values", "Tick the values to keep"),
                                (
                                    true,
                                    "Condition",
                                    "Describe the rows to keep: contains, between, after",
                                ),
                            ] {
                                let on = vp.cond_tab == cond;
                                let b = egui::Button::new(RichText::new(label).color(if on {
                                    pal.text
                                } else {
                                    pal.text2
                                }))
                                .fill(if on { pal.selection } else { pal.surface })
                                .stroke(if on {
                                    Stroke::new(1.0, pal.accent_ink)
                                } else {
                                    Stroke::new(1.0, pal.hairline)
                                });
                                if ui.add(b).on_hover_text(tip).clicked() {
                                    vp.cond_tab = cond;
                                }
                            }
                        });
                        ui.separator();
                        if vp.cond_tab {
                            let ops = crate::conditions::ops_for(kind);
                            let mut remove_row = None;
                            let mut cal_toggle: Option<usize> = None;
                            let n_rows = vp.conds.len();
                            for (i, row) in vp.conds.iter_mut().enumerate() {
                                if i > 0 {
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(&mut row.and, true, "and");
                                        ui.selectable_value(&mut row.and, false, "or");
                                    });
                                }
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(("cond-op", i))
                                        .selected_text(crate::conditions::op_label(row.op))
                                        .width(170.0)
                                        .show_ui(ui, |ui| {
                                            for op in ops {
                                                ui.selectable_value(
                                                    &mut row.op,
                                                    *op,
                                                    crate::conditions::op_label(*op),
                                                );
                                            }
                                        });
                                    if n_rows > 1
                                        && icon_button(ui, pal, ph::X, "Remove this condition")
                                            .clicked()
                                    {
                                        remove_row = Some(i);
                                    }
                                });
                                let need = crate::conditions::values_needed(row.op);
                                if need >= 1 {
                                    ui.horizontal(|ui| {
                                        let w = if need == 2 { 140.0 } else { 300.0 };
                                        ui.add(
                                            egui::TextEdit::singleline(&mut row.a)
                                                .hint_text(crate::conditions::value_hint(row.op))
                                                .desired_width(w),
                                        );
                                        if need == 2 {
                                            ui.label("and");
                                            ui.add(
                                                egui::TextEdit::singleline(&mut row.b)
                                                    .desired_width(w),
                                            );
                                        }
                                        if kind == crate::conditions::ColKind::Time
                                            && !matches!(row.op, crate::conditions::Op::LastN(_))
                                            && icon_button(
                                                ui,
                                                pal,
                                                ph::CALENDAR_DOTS,
                                                "Pick on a calendar (rows per day)",
                                            )
                                            .clicked()
                                        {
                                            cal_toggle = Some(i);
                                        }
                                    });
                                    // suggestions from the column's own values
                                    if kind == crate::conditions::ColKind::Text && row.a.is_empty()
                                    {
                                        if let Some(l) = &list {
                                            ui.horizontal_wrapped(|ui| {
                                                for (v, _) in l
                                                    .top
                                                    .iter()
                                                    .filter(|(v, _)| !v.is_empty())
                                                    .take(5)
                                                {
                                                    if ui
                                                        .small_button(v.as_str())
                                                        .on_hover_text("Use this value")
                                                        .clicked()
                                                    {
                                                        row.a = v.clone();
                                                    }
                                                }
                                            });
                                        }
                                    }
                                }
                                // each row read back in plain words
                                match &row_texts[i] {
                                    Ok(_) => {
                                        let reading = crate::conditions::reading(
                                            &vp.name, row.op, &row.a, &row.b,
                                        );
                                        ui.label(
                                            RichText::new(reading)
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                        );
                                    }
                                    Err(_)
                                        if crate::conditions::values_needed(row.op) >= 1
                                            && row.a.trim().is_empty() =>
                                    {
                                        // nothing typed yet: a hint, not an error
                                        let hint = if kind == crate::conditions::ColKind::Time {
                                            "Type a date or pick one on the calendar"
                                        } else {
                                            "Type a value"
                                        };
                                        ui.label(
                                            RichText::new(hint)
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                        );
                                    }
                                    Err(e) => {
                                        ui.label(
                                            RichText::new(e)
                                                .font(theme::small_font())
                                                .color(pal.danger),
                                        );
                                    }
                                }
                                ui.add_space(4.0);
                            }
                            if let Some(i) = remove_row {
                                vp.conds.remove(i);
                                vp.cal_row = None;
                            }
                            if let Some(i) = cal_toggle {
                                vp.cal_row = if vp.cal_row == Some(i) { None } else { Some(i) };
                                vp.cal = Default::default();
                                send_cal = vp.cal_row.is_some();
                            }
                            if let Some(ri) = vp.cal_row.filter(|ri| *ri < vp.conds.len()) {
                                ui.separator();
                                let cal = snap.calendar.as_deref().filter(|c| c.query == vp.query);
                                match crate::calendar::show(ui, pal, cal, &mut vp.cal) {
                                    crate::calendar::CalAction::Use { a, b, whole_days } => {
                                        let row = &mut vp.conds[ri];
                                        let t = logseek_core::types::fmt_time;
                                        if whole_days && b - a == 86_400_000_000 {
                                            row.op = crate::conditions::Op::OnDay;
                                            row.a = t(a).get(..10).unwrap_or_default().to_string();
                                            row.b.clear();
                                        } else {
                                            // the exact span: "between" includes both ends
                                            row.op = crate::conditions::Op::Between;
                                            row.a = t(a);
                                            row.b = t(b - 1);
                                        }
                                        vp.cal_row = None;
                                    }
                                    crate::calendar::CalAction::Cancel => vp.cal_row = None,
                                    crate::calendar::CalAction::None => {}
                                }
                                ui.separator();
                            }
                            if ui
                                .button(format!("{}  Add a condition", ph::PLUS))
                                .clicked()
                            {
                                vp.conds.push(CondRow {
                                    op: ops[0],
                                    a: String::new(),
                                    b: String::new(),
                                    and: true,
                                });
                            }
                            if let Some(t) = &cond_text {
                                ui.label(
                                    RichText::new(format!("The same, as text you can type in the search box:  {t}"))
                                        .font(theme::small_font())
                                        .color(pal.muted),
                                );
                            }
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let ok = cond_text.is_some() && !unchanged;
                                let label = match (cond_text.is_some(), preview) {
                                    (false, _) => "Fill in the condition".to_string(),
                                    (true, _) if unchanged => "No change yet".to_string(),
                                    (true, Some(k)) => {
                                        format!("Apply · keeps {}", group(k))
                                    }
                                    (true, None) => "Apply".to_string(),
                                };
                                if ui
                                    .add_enabled_ui(ok, |ui| primary_button(ui, pal, &label))
                                    .inner
                                    .clicked()
                                {
                                    apply = true;
                                }
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                            });

                            return;
                        }
                        // sort: most frequent, rarest first (stacking), A→Z
                        ui.horizontal(|ui| {
                            for (mode, label, tip) in [
                                (ValueSort::Count, "Most", "Most frequent first"),
                                (
                                    ValueSort::Rare,
                                    "Rarest",
                                    "Rarest first: one-off values are often the interesting ones",
                                ),
                                (ValueSort::Alpha, "A→Z", "Alphabetical"),
                            ] {
                                let on = vp.sort == mode;
                                let b =
                                    egui::Button::new(
                                        RichText::new(label)
                                            .font(theme::small_font())
                                            .color(if on { pal.text } else { pal.text2 }),
                                    )
                                    .fill(if on { pal.selection } else { pal.surface })
                                    .stroke(if on {
                                        Stroke::new(1.0, pal.accent_ink)
                                    } else {
                                        Stroke::new(1.0, pal.hairline)
                                    })
                                    .min_size(egui::vec2(0.0, 22.0));
                                if ui.add(b).on_hover_text(tip).clicked() && !on {
                                    resort = Some(mode);
                                }
                            }
                        });
                        let Some(l) = &list else {
                            ui.label(RichText::new("Counting values").color(pal.muted));
                            return;
                        };
                        let limit_note = String::new();
                        ui.label(
                            RichText::new(format!(
                                "{} distinct in {} rows{limit_note}",
                                group(l.distinct),
                                group(l.matched)
                            ))
                            .font(theme::small_font())
                            .color(pal.text2),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut vp.search)
                                .hint_text(
                                    RichText::new(format!(
                                        "{}  Find a value",
                                        ph::MAGNIFYING_GLASS
                                    ))
                                    .color(pal.muted),
                                )
                                .desired_width(f32::INFINITY),
                        );
                        let want = match vp.sort {
                            ValueSort::Alpha => crate::bridge::GroupSort::Name,
                            ValueSort::Rare => crate::bridge::GroupSort::FewestRows,
                            _ => crate::bridge::GroupSort::MostRows,
                        };
                        if vp.cache_search != vp.search {
                            vp.cache_search = vp.search.clone();
                            vp.cache.clear();
                            vp.asked.clear();
                            vp.total = None;
                            vp.pending = None;
                        }
                        if let Some(pg) = snap
                            .values_page
                            .as_ref()
                            .filter(|p| p.col == vp.col && p.sort == want && p.search == vp.search)
                        {
                            vp.total = Some(pg.total);
                            for (k, r) in pg.rows.iter().enumerate() {
                                vp.cache
                                    .entry(pg.start + k as u64)
                                    .or_insert_with(|| r.clone());
                            }
                        }
                        let first_list = vp.search.is_empty() && vp.sort != ValueSort::Alpha;
                        if vp.total.is_none() && first_list {
                            for (k, r) in l.top.iter().enumerate() {
                                vp.cache.entry(k as u64).or_insert_with(|| r.clone());
                            }
                        }
                        if vp.total.is_none() && vp.asked.insert(0) {
                            self.bridge.send(Cmd::ValuesPage {
                                col: vp.col,
                                search: vp.search.clone(),
                                start: 0,
                                len: 400,
                            });
                        }
                        let total =
                            vp.total
                                .unwrap_or(if first_list { l.top.len() as u64 } else { 0 });
                        // with a search, the buttons act on every value it found, loaded or not
                        let searching = !vp.search.trim().is_empty();
                        let found = if searching {
                            format!(" the {} found", group(total))
                        } else {
                            String::new()
                        };
                        let mut press: Option<crate::filters::Tick> = None;
                        ui.horizontal(|ui| {
                            if ui
                                .small_button(format!("Select all{found}"))
                                .on_hover_text(
                                    "Check every value (with a search: every value it found, not only the ones on screen)",
                                )
                                .clicked()
                            {
                                if searching {
                                    press = Some(crate::filters::Tick::All);
                                } else {
                                    vp.except = true;
                                    vp.sel.clear();
                                }
                            }
                            if ui
                                .small_button(format!("Clear{found}"))
                                .on_hover_text(
                                    "Uncheck every value (with a search: every value it found)",
                                )
                                .clicked()
                            {
                                if searching {
                                    press = Some(crate::filters::Tick::None);
                                } else {
                                    vp.except = false;
                                    vp.sel.clear();
                                }
                            }
                            if ui
                                .small_button("Keep checked only")
                                .on_hover_text("Keep only the values matching the search")
                                .clicked()
                            {
                                press = Some(crate::filters::Tick::Only);
                            }
                        });
                        // dev switch: LOGSEEK_SHOT_VALUES_TICK="all|none|only" presses that button once
                        if vp.total.is_some() {
                            if let Ok(t) = crate::dev::var("LOGSEEK_SHOT_VALUES_TICK") {
                                std::env::remove_var("LOGSEEK_SHOT_VALUES_TICK");
                                press = Some(match t.as_str() {
                                    "none" => crate::filters::Tick::None,
                                    "only" => crate::filters::Tick::Only,
                                    _ => crate::filters::Tick::All,
                                });
                            }
                        }
                        if let Some(how) = press {
                            vp.pending = Some((how, 0, None));
                        }
                        if let Some((how, mut next, mut asked)) = vp.pending {
                            // every found value first: page through the ones not loaded yet
                            while vp.cache.contains_key(&next) {
                                next += 1;
                            }
                            match vp.total {
                                Some(t) if next >= t => {
                                    let (mut sel, mut except) =
                                        (std::mem::take(&mut vp.sel), vp.except);
                                    crate::filters::tick(
                                        &mut sel,
                                        &mut except,
                                        vp.cache.values().map(|(v, _)| v),
                                        how,
                                    );
                                    vp.sel = sel;
                                    vp.except = except;
                                    vp.pending = None;
                                }
                                Some(t) => {
                                    // ask for the next page; again if it has not come in a second
                                    // (another page of this list may have taken its place)
                                    let now = ui.input(|i| i.time);
                                    if asked.is_none_or(|(at, when)| at != next || now - when > 1.0) {
                                        asked = Some((next, now));
                                        self.bridge.send(Cmd::ValuesPage {
                                            col: vp.col,
                                            search: vp.search.clone(),
                                            start: next,
                                            len: 20_000,
                                        });
                                    }
                                    vp.pending = Some((how, next, asked));
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new(format!(
                                                "Reading every value found: {} of {}",
                                                group(next),
                                                group(t)
                                            ))
                                            .color(pal.text2),
                                        );
                                    });
                                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                                }
                                None => {
                                    vp.pending = Some((how, next, asked));
                                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                                }
                            }
                        }
                        let matched = l.matched.max(1);
                        let row_h = ui.text_style_height(&egui::TextStyle::Body).max(18.0) + 2.0;
                        let col = vp.col;
                        let search = vp.search.clone();
                        // a dense list: one value per line, little air between lines
                        ui.spacing_mut().item_spacing.y = 2.0;
                        egui::ScrollArea::both()
                            .max_height(300.0)
                            .auto_shrink([false, true])
                            .show_rows(ui, row_h, total as usize, |ui, range| {
                                for i in range {
                                    let i = i as u64;
                                    let Some((v, n)) = vp.cache.get(&i).cloned() else {
                                        let start = i / 200 * 200;
                                        if vp.asked.insert(start) {
                                            self.bridge.send(Cmd::ValuesPage {
                                                col,
                                                search: search.clone(),
                                                start,
                                                len: 400,
                                            });
                                        }
                                        ui.add_sized(
                                            [ui.available_width(), row_h],
                                            egui::Label::new(
                                                RichText::new("reading").color(pal.text2),
                                            ),
                                        );
                                        continue;
                                    };
                                    let mut on = if vp.except {
                                        !vp.sel.contains(&v)
                                    } else {
                                        vp.sel.contains(&v)
                                    };
                                    let row = ui.horizontal(|ui| {
                                        // a value reads here as it does in the grid: a time
                                        // as a time, not as the number it is written as
                                        let shown = crate::grid::shown_cell(
                                            snap,
                                            &self.grid.shown_as,
                                            self.prefs.times_utc,
                                            col,
                                            &v,
                                        );
                                        let label = if v.is_empty() {
                                            RichText::new("(no value)").color(pal.text2)
                                        } else {
                                            RichText::new(shown.as_deref().unwrap_or(&v)).font(theme::data_font())
                                        };
                                        let tick = ui.checkbox(&mut on, label);
                                        let tick = match &shown {
                                            Some(_) => tick.on_hover_text(format!("As written in the file: {v}")),
                                            None => tick,
                                        };
                                        if tick.changed() {
                                            // unchecked joins the "except" set, checked the "only" set
                                            if on != vp.except {
                                                vp.sel.insert(v.clone());
                                            } else {
                                                vp.sel.remove(&v);
                                            }
                                        }
                                        ui.label(
                                            RichText::new(group(n))
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                        );
                                    });
                                    // a quiet share bar under each value (the count is printed too)
                                    let r = row.response.rect;
                                    let track = egui::Rect::from_min_size(
                                        egui::pos2(r.left() + 24.0, r.bottom() - 2.0),
                                        egui::vec2((r.width() - 24.0).max(0.0), 2.0),
                                    );
                                    let mut bar = track;
                                    bar.set_width(track.width() * (n as f32 / matched as f32));
                                    ui.painter().rect_filled(
                                        bar,
                                        CornerRadius::same(1),
                                        pal.info.gamma_multiply(0.6),
                                    );
                                }
                            });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let label = match preview {
                                _ if unchanged => "No change yet".to_string(),
                                Some(k) => format!("Apply · keeps {}", group(k)),
                                None => "Apply".to_string(),
                            };
                            let applied = ui
                                .add_enabled_ui(!unchanged, |ui| primary_button(ui, pal, &label))
                                .inner;
                            if applied
                                .on_disabled_hover_text("Uncheck a value to filter it out")
                                .clicked()
                            {
                                apply = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if self
                                .filters
                                .get(&vp.name)
                                .is_some_and(|f| f.values.is_some())
                                && ui.button("Remove filter").clicked()
                            {
                                remove_filter = true;
                            }
                        });
                    });
            });
        if send_cal {
            self.bridge.send(Cmd::Calendar(vp.query.clone()));
        }
        if let Some(mode) = resort {
            vp.sort = mode;
            {
                // the engine orders every value (most, rarest, or A to Z)
                vp.cache.clear();
                vp.asked.clear();
                vp.total = None;
                self.bridge.send(Cmd::Values {
                    col: vp.col,
                    query: vp.query.clone(),
                    n: 500,
                    rare: mode == ValueSort::Rare,
                    alpha: mode == ValueSort::Alpha,
                });
            }
        }
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && !egui::Popup::is_any_open(&ctx)
            && ctx
                .pointer_interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || clicked_outside {
            close = true;
        }
        if apply {
            self.filters = candidate;
            self.maybe_send_query(true);
            return;
        }
        if remove_filter {
            if let Some(f) = self.filters.get_mut(&vp.name) {
                f.values = None;
                if !f.is_active() {
                    self.filters.remove(&vp.name);
                }
            }
            self.maybe_send_query(true);
            return;
        }
        if !close {
            self.values = Some(vp);
        }
    }

    /// Shows only rows in `a`..`b` (UTC µs, end exclusive): the same range a
    /// drag on the timeline writes.
    fn set_range(&mut self, a: i64, b: i64) {
        let rest = leading_range(&self.query)
            .map(|(_, _, r)| r.to_string())
            .unwrap_or_else(|| self.query.clone());
        self.query = format!("@{}..{} {}", iso(a), iso(b), rest.trim_start())
            .trim_end()
            .to_string();
        self.maybe_send_query(true);
    }

    /// Export: format, columns, then a save dialog; the engine streams the file.
    fn export_popover(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        use crate::export::Format;
        if !self.export_open {
            return;
        }
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let w = 400.0;
        let at = egui::pos2(screen.left() + 12.0, screen.bottom() - STATUS_H - 230.0);
        let shown: Vec<usize> =
            crate::grid::visible_order(&self.grid.order, &snap.columns, &self.grid.hidden);
        let mut go = false;
        let mut close = false;
        egui::Area::new(egui::Id::new("export-popover"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.label(RichText::new(format!("Export the {} rows this search keeps", group(snap.matched))).font(fonts::strong(13.0)));
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("As").color(pal.text2));
                            ui.selectable_value(&mut self.export_fmt, Format::Csv, "CSV (spreadsheets)");
                            ui.selectable_value(&mut self.export_fmt, Format::Xlsx, "Excel workbook (smaller file)");
                            ui.selectable_value(&mut self.export_fmt, Format::Jsonl, "JSON Lines");
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Columns").color(pal.text2));
                            ui.selectable_value(&mut self.export_visible_only, false, format!("all {}", snap.columns.len()));
                            ui.selectable_value(&mut self.export_visible_only, true, format!("the {} shown in the grid", shown.len()));
                        });
                        if self.export_fmt == Format::Xlsx {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Every value is written as text, so nothing is changed or run as a formula. A sheet holds 1,048,576 rows: more go on to further sheets. A value longer than a cell holds is kept whole on a sheet named Long values.")
                                        .font(theme::small_font())
                                        .color(pal.text2),
                                )
                                .wrap(),
                            );
                        }
                        if self.export_fmt == Format::Csv {
                            ui.label(
                                RichText::new("Cells that a spreadsheet would run as a formula (starting with = + - @) get a leading ' so they open as text.")
                                    .font(theme::small_font())
                                    .color(pal.text2),
                            );
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if primary_button(ui, pal, "Choose where to save").clicked() {
                                go = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if go {
            let name = snap
                .source
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "rows".into());
            let ext = self.export_fmt.ext();
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Export rows")
                .set_file_name(crate::export::suggest_name(&name, self.export_fmt))
                .add_filter(ext.to_uppercase(), &[ext])
                .save_file()
            {
                let cols = if self.export_visible_only {
                    shown
                } else {
                    (0..snap.columns.len()).collect()
                };
                self.export_dismissed = false;
                crate::export::CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
                self.bridge.send(Cmd::Export {
                    path,
                    format: self.export_fmt,
                    cols,
                });
            }
            self.export_open = false;
        }
        if close
            || (ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx))
        {
            self.export_open = false;
        }
    }

    /// Every keyboard shortcut, on "?".
    /// The help sheet (the ? button, or the ? key): keys, then the filter words.
    fn keys_overlay(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if !self.keys_open {
            return;
        }
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let keys = HELP_KEYS;
        let w = 620.0f32.min(screen.width() - 32.0);
        let top = (screen.top() + 60.0).min(screen.center().y);
        let at = egui::pos2(screen.center().x - w / 2.0, top);
        // never taller than the window: the list scrolls, the title stays
        let body_h = (screen.bottom() - top - 96.0).max(160.0);
        let mut close = false;
        egui::Area::new(egui::Id::new("keys-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}  Help", ph::QUESTION))
                                    .font(fonts::strong(15.0)),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc or ?)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .id_salt("help-body")
                            .max_height(body_h)
                            .min_scrolled_height(body_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("{}  Keys", ph::KEYBOARD))
                                        .font(fonts::strong(13.0)),
                                );
                                egui::Grid::new("keys-grid")
                                    .num_columns(2)
                                    .spacing([24.0, 6.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (k, what) in keys {
                                            ui.label(
                                                RichText::new(*k)
                                                    .font(theme::data_font())
                                                    .color(pal.accent_ink),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(*what).color(pal.text),
                                                )
                                                .wrap(),
                                            );
                                            ui.end_row();
                                        }
                                    });
                                ui.add_space(12.0);
                                crate::grid::filter_help(ui, pal);
                            });
                    });
            });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.keys_open = false;
        }
    }

    /// The Summarise sheet: group, count, click a group to drill down.
    fn summary_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(mut st) = self.summary.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let w = (screen.width() - 360.0).clamp(560.0, 820.0);
        let at = egui::pos2(
            (screen.right() - w - 24.0).max(screen.left() + 8.0),
            screen.top() + TOP_H + 4.0,
        );
        let mut act = crate::summary::SumAction::None;
        egui::Area::new(egui::Id::new("summary-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        act = crate::summary::show(ui, pal, &mut st, snap, &self.bridge);
                    });
            });
        let esc =
            ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx);
        match act {
            crate::summary::SumAction::Run(spec) => {
                crate::summary::CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
                self.bridge.send(Cmd::Summarise(spec));
                self.summary = Some(st);
            }
            crate::summary::SumAction::Drill(filters, range) => {
                self.push_history();
                for (c, t) in filters {
                    // the group's own rows: an inverted or switched-off chip is reset
                    let f = self.filters.entry(c).or_default();
                    f.text = t;
                    f.values = None;
                    f.invert = false;
                    f.enabled = true;
                }
                if let Some((a, b)) = range {
                    // never wider than the search's own time range
                    let search = leading_range(&self.query).map(|(x, y, _)| (x, y));
                    let (a, b) = crate::summary::bucket_range(a, b - a, search);
                    self.set_range(a, b);
                } else {
                    self.maybe_send_query(true);
                }
            }
            crate::summary::SumAction::Close => {}
            crate::summary::SumAction::None if !esc => self.summary = Some(st),
            crate::summary::SumAction::None => {}
        }
    }

    /// The stress script (LOGSEEK_STRESS): acts like a fast user, one step per
    /// frame, and records what each step costs. Settings are restored after.
    fn stress_drive(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let Some(mut st) = self.stress.take() else {
            return;
        };
        ctx.request_repaint();
        if st.phase == 0 && st.frame_in_phase == 0 {
            // in front, so Windows paces it like a window someone is using
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let f = st.frame_in_phase;
        let now = Instant::now();
        let answered = snap.phase.settled()
            && snap.query == self.sent_query
            && snap.query == self.effective_query()
            && !snap.busy
            && snap
                .page
                .as_ref()
                .is_some_and(|p| p.generation == snap.generation);
        let vis = self.grid.last_visible.max(1);
        let max_top = snap.matched.saturating_sub(vis);
        let col: Option<(usize, String)> = snap
            .columns
            .iter()
            .enumerate()
            .find(|(i, c)| !c.starts_with('_') && snap.time_col != Some(*i))
            .map(|(i, c)| (i, c.clone()));
        let word = crate::dev::var("LOGSEEK_STRESS_WORD").unwrap_or_else(|_| "the".into());
        let wait = |st: &mut crate::stress::Stress, done: bool| -> bool {
            if done {
                st.answered();
                true
            } else if f > 3000 {
                st.time_out();
                true
            } else {
                false
            }
        };
        match st.phase {
            0 => {
                if answered {
                    self.stress_prefs = Some(self.prefs.clone());
                    st.report.rows = snap.total_rows;
                    st.next();
                }
            }
            1 | 4 => {
                self.grid.top = if max_top == 0 {
                    0
                } else {
                    (self.grid.top + vis) % (max_top + 1)
                };
                if f >= if st.phase == 1 { 240 } else { 120 } {
                    self.grid.top = 0;
                    st.next();
                }
            }
            2 => {
                self.grid.top = if f % 2 == 0 { 0 } else { max_top };
                if f >= 60 {
                    self.grid.top = 0;
                    st.next();
                }
            }
            3 => {
                let n = (f / 3) as usize;
                if n <= word.len() {
                    self.query = word[..n].to_string();
                    self.edited_at = Some(now);
                    if n == word.len() && st.waiting_since.is_none() {
                        st.waiting_since = Some(now);
                    }
                } else if wait(&mut st, answered) {
                    st.next();
                }
            }
            5 => {
                if f == 0 {
                    if let Some((_, c)) = &col {
                        self.filters.entry(c.clone()).or_default().text = "e".into();
                        self.maybe_send_query(true);
                    }
                    st.waiting_since = Some(now);
                } else if wait(&mut st, answered) {
                    st.next();
                }
            }
            6 => {
                if f == 0 {
                    if let Some((ci, c)) = &col {
                        let query = crate::filters::compose(&self.query, &self.filters, Some(c));
                        self.bridge.send(Cmd::Values {
                            col: *ci,
                            query: query.clone(),
                            n: 500,
                            rare: false,
                            alpha: false,
                        });
                        self.values = Some(ValuesPopup {
                            col: *ci,
                            name: c.clone(),
                            at: egui::pos2(500.0, 200.0),
                            search: String::new(),
                            sel: Default::default(),
                            ready: false,
                            query,
                            sort: ValueSort::Count,
                            preview_sent: String::new(),
                            cond_tab: false,
                            conds: Vec::new(),
                            cal_row: None,
                            cal: Default::default(),
                            except: true,
                            cache: Default::default(),
                            cache_search: String::new(),
                            asked: Default::default(),
                            total: None,
                            pending: None,
                        });
                    }
                } else if f >= 90 {
                    self.values = None;
                    st.next();
                }
            }
            7 => {
                if f == 0 {
                    self.summary = col.as_ref().map(|(_, c)| {
                        crate::summary::SummaryState::new(Some(c.clone()), &snap.columns)
                    });
                    st.waiting_since = Some(now);
                } else {
                    let done = snap.summary.as_ref().is_some_and(|r| {
                        r.generation == snap.generation
                            && r.spec
                                .as_ref()
                                .is_some_and(|s| s.by.first() == col.as_ref().map(|c| &c.1))
                    });
                    if wait(&mut st, done || col.is_none()) {
                        self.summary = None;
                        st.next();
                    }
                }
            }
            8 => {
                if f == 0 && snap.time_col.is_some() {
                    let rest = leading_range(&self.query)
                        .map(|(_, _, r)| r.to_string())
                        .unwrap_or_else(|| self.query.clone());
                    self.cal_query = crate::filters::compose(rest.trim(), &self.filters, None);
                    self.cal = Default::default();
                    self.bridge.send(Cmd::Calendar(self.cal_query.clone()));
                    self.cal_open = true;
                } else if f >= 60 {
                    self.cal_open = false;
                    st.next();
                }
            }
            9 => {
                if f == 0 {
                    // rules that test every cell on screen: the cost of colouring,
                    // made from plain characters, not from any list of known names
                    use crate::colors::{ColorRule, RuleOp};
                    self.prefs.colors = [
                        (RuleOp::Contains, "e"),
                        (RuleOp::ContainsAny, "a, o, 1"),
                        (RuleOp::StartsWith, "s"),
                        (RuleOp::Gt, "100"),
                        (RuleOp::Regex, "[0-9]{3}"),
                        (RuleOp::IsAny, "0, 1, true"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (op, v))| ColorRule {
                        enabled: true,
                        name: String::new(),
                        column: None,
                        op,
                        value: v.to_string(),
                        tint: i,
                        cells: i % 2 == 1,
                    })
                    .collect();
                }
                self.grid.top = if max_top == 0 {
                    0
                } else {
                    (self.grid.top + vis) % (max_top + 1)
                };
                if f >= 120 {
                    self.grid.top = 0;
                    st.next();
                }
            }
            10 => {
                if f == 0 {
                    self.addcol = Some(crate::addcol::AddColState::new(
                        col.as_ref().map(|c| c.1.clone()),
                        &snap.columns,
                    ));
                } else if f >= 60 {
                    self.addcol = None;
                    st.next();
                }
            }
            11 => {
                if f == 0 {
                    self.query.clear();
                    self.filters.clear();
                    self.maybe_send_query(true);
                    st.waiting_since = Some(now);
                } else if wait(&mut st, answered) {
                    st.next();
                }
            }
            12 => {
                // a second file in its own tab (LOGSEEK_STRESS_FILE2), when one is given
                match crate::dev::var("LOGSEEK_STRESS_FILE2").ok().filter(|p| !p.is_empty()) {
                    None => st.next(),
                    Some(p) => {
                        let there = snap
                            .source
                            .as_ref()
                            .is_some_and(|s| s.path == std::path::Path::new(&p));
                        if f == 0 {
                            self.open(PathBuf::from(&p));
                            st.waiting_since = Some(now);
                        } else if wait(&mut st, answered && there) {
                            self.stress_switches = 0;
                            st.next();
                        }
                    }
                }
            }
            13 => {
                // back and forth between the two files' tabs, twelve times:
                // each switch is timed until the other file's rows are shown
                let other = self
                    .tabs
                    .list
                    .iter()
                    .map(|t| t.id)
                    .find(|id| Some(*id) != self.tabs.current);
                let Some(other) = other else {
                    st.next();
                    self.stress = Some(st);
                    return;
                };
                let shown = self
                    .tabs
                    .current
                    .and_then(|c| self.tabs.path(c))
                    .zip(snap.source.as_ref())
                    .is_some_and(|(p, s)| s.path == std::path::Path::new(p));
                match st.waiting_since {
                    None if self.stress_switches >= 12 => st.next(),
                    None => {
                        self.switch_tab(other);
                        self.stress_switches += 1;
                        st.waiting_since = Some(now);
                    }
                    Some(since) if answered && shown && f > 1 => {
                        st.report.switch_ms.push(since.elapsed().as_secs_f64() * 1000.0);
                        st.answered();
                    }
                    Some(since) if since.elapsed().as_secs_f64() > 60.0 => {
                        st.time_out();
                        st.next();
                    }
                    Some(_) => {}
                }
            }
            14 => {
                // nothing happens for ten seconds: memory should come back down
                // (LOGSEEK_STRESS_REST=<seconds> waits longer, past the point
                // where memory grown for heavy work is given back)
                let rest = crate::dev::var("LOGSEEK_STRESS_REST")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(10.0);
                ctx.request_repaint_after(Duration::from_millis(250));
                if st.phase_secs() >= rest {
                    st.note_rest();
                    st.next();
                }
            }
            _ => {}
        }
        if st.done() {
            if let Some(p) = self.stress_prefs.take() {
                self.prefs = p;
            }
            st.report.file = snap
                .source
                .as_ref()
                .map(|s| s.path.display().to_string())
                .unwrap_or_default();
            st.report.budget_mb = snap.budget_cap >> 20;
            st.report.index_mb = snap.index_bytes as f64 / (1u64 << 20) as f64;
            st.finish();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.stress = Some(st);
    }

    /// "Grouped by" above the grid: the levels as chips, the sort, and ways out.
    /// A column name being dragged can be dropped on the group line (or,
    /// when nothing is grouped yet, on a place that appears above the grid)
    /// to group the rows by that column.
    fn group_drop(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(c) = self.grid.dragging_col() else {
            return;
        };
        let Some(name) = snap.columns.get(c).cloned() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let avail = ui.available_rect_before_wrap();
        // grouped: the line itself; not grouped: a place over the strip above the grid
        let zone = self.group_rect.unwrap_or_else(|| {
            egui::Rect::from_center_size(
                egui::pos2(avail.center().x, avail.top() - 20.0),
                egui::vec2(avail.width().min(520.0), 34.0),
            )
        });
        let over = ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| zone.expand(4.0).contains(p));
        let already = self
            .grid
            .grouping
            .as_ref()
            .is_some_and(|g| g.by.contains(&name));
        let text = if already {
            format!("{}  Already grouped by {name}", ph::TREE_VIEW)
        } else if self.grid.grouping.is_some() {
            format!("{}  Drop here to group by {name} as well", ph::TREE_VIEW)
        } else {
            format!("{}  Drop here to group the rows by {name}", ph::TREE_VIEW)
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("group-drop"),
        ));
        painter.rect_filled(
            zone,
            CornerRadius::same(6),
            if over { pal.selection } else { pal.surface },
        );
        painter.rect_stroke(
            zone,
            CornerRadius::same(6),
            Stroke::new(if over { 2.0 } else { 1.0 }, pal.accent_ink),
            StrokeKind::Inside,
        );
        painter.text(
            zone.center(),
            Align2::CENTER_CENTER,
            text,
            theme::ui_font(),
            pal.text,
        );
        if over && ctx.input(|i| i.pointer.any_released()) {
            // the drop is taken here: the column itself does not move
            self.grid.cancel_col_drag();
            let (mut by, sort) = self
                .grid
                .grouping
                .as_ref()
                .map(|g| (g.by.clone(), g.sort))
                .unwrap_or_default();
            if crate::groups::add_level(&mut by, &name) {
                self.grid.grouping = Some(crate::groups::Grouping::new(by, sort));
                self.grid.ftree = None;
                self.grid.top = 0;
                self.grid.sel = None;
            }
        }
    }

    fn group_line(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        use crate::bridge::GroupSort;
        let Some(g) = self.grid.grouping.as_ref() else {
            return;
        };
        let mut by = g.by.clone();
        let mut sort = g.sort;
        let mut collapse = false;
        let mut ungroup = false;
        let mut chips: Vec<egui::Rect> = Vec::new();
        let mut moved: Option<(usize, usize)> = None;
        let mut dragging = self.group_drag;
        let line = Frame::new()
            .fill(pal.surface)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{}  Grouped by", ph::TREE_VIEW)).color(pal.text2),
                    )
                    .on_hover_text("Drag a column name here to group by it too; drag these to change their order");
                    let n = by.len();
                    let mut action: Option<(usize, i32)> = None;
                    for (i, name) in by.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new(ph::CARET_RIGHT).color(pal.text2));
                        }
                        let out = crate::groups::chip(ui, pal, name, i, n);
                        match out.act {
                            crate::groups::ChipAct::Earlier => action = Some((i, -1)),
                            crate::groups::ChipAct::Later => action = Some((i, 1)),
                            crate::groups::ChipAct::Remove => action = Some((i, 0)),
                            crate::groups::ChipAct::None => {}
                        }
                        // the chip can be dragged by its name to another place in the order
                        chips.push(out.rect);
                        let d = out.handle;
                        if d.drag_started() {
                            dragging = Some(i);
                        }
                        if d.hovered() && dragging.is_none() && n > 1 {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                    }
                    if let Some(from) = dragging {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        let spans: Vec<(f32, f32)> =
                            chips.iter().map(|r| (r.left(), r.right())).collect();
                        if let Some(p) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                            let to = crate::groups::drop_place(&spans, p.x, from);
                            // a mark where it would land
                            let others: Vec<&egui::Rect> = chips
                                .iter()
                                .enumerate()
                                .filter(|(k, _)| *k != from)
                                .map(|(_, r)| r)
                                .collect();
                            let x = match others.get(to) {
                                Some(r) => r.left() - 4.0,
                                None => others.last().map_or(p.x, |r| r.right() + 4.0),
                            };
                            if let Some(r) = chips.first() {
                                ui.painter().line_segment(
                                    [egui::pos2(x, r.top() - 2.0), egui::pos2(x, r.bottom() + 2.0)],
                                    Stroke::new(2.0, pal.accent_ink),
                                );
                            }
                            if ui.ctx().input(|i| i.pointer.any_released()) {
                                moved = Some((from, to));
                                dragging = None;
                            }
                        } else {
                            dragging = None;
                        }
                    }
                    match action {
                        Some((i, 0)) => {
                            by.remove(i);
                        }
                        Some((i, d)) => {
                            let j = (i as i32 + d) as usize;
                            by.swap(i, j);
                        }
                        None => {}
                    }
                    ui.add_space(12.0);
                    let label = |s: GroupSort| match s {
                        GroupSort::MostRows => "Most rows first",
                        GroupSort::FewestRows => "Fewest rows first (rare)",
                        GroupSort::FirstSeen => "First seen first",
                        GroupSort::LastSeen => "Last seen first",
                        GroupSort::Name => "By name, A to Z",
                    };
                    egui::ComboBox::from_id_salt("group-sort")
                        .selected_text(format!("Sort: {}", label(sort)))
                        .show_ui(ui, |ui| {
                            for s in [
                                GroupSort::MostRows,
                                GroupSort::FewestRows,
                                GroupSort::FirstSeen,
                                GroupSort::LastSeen,
                                GroupSort::Name,
                            ] {
                                ui.selectable_value(&mut sort, s, label(s));
                            }
                        });
                    if ui.button("Collapse all").clicked() {
                        collapse = true;
                    }
                    if ui.button("Ungroup").clicked() {
                        ungroup = true;
                    }
                });
            });
        self.group_rect = Some(line.response.rect);
        self.group_drag = dragging;
        if let Some((from, to)) = moved {
            crate::groups::move_to(&mut by, from, to);
        }
        let Some(g) = self.grid.grouping.as_mut() else {
            return;
        };
        if ungroup || by.is_empty() {
            self.grid.grouping = None;
            self.grid.top = 0;
            self.grid.sel = None;
        } else if by != g.by || sort != g.sort {
            self.grid.grouping = Some(crate::groups::Grouping::new(by, sort));
            self.grid.top = 0;
            self.grid.sel = None;
        } else if collapse {
            g.tree.open.clear();
            self.grid.top = 0;
            self.grid.sel = None;
        }
    }

    fn new_follow(&self, snap: &Snapshot) -> crate::follow::FollowState {
        let ns = snap.columns.len() - snap.virtual_cols.len();
        // start from the row the user is on; else tagged rows; else the result
        let tag = (0..crate::tags::TAGS.len())
            .find(|&i| !crate::tags::rows_with(&self.tags, 1 << i).is_empty());
        let start = crate::follow::first_start(self.grid.sel.is_some(), tag);
        let mut st = crate::follow::FollowState::new(&snap.columns[..ns], start, &snap.links);
        // nothing suggested by the file: rows that share a value in the column
        // the user is on (or the first that is not the time), never the time
        // matched with itself
        if st.suggested.is_none() {
            let pick = self
                .grid
                .focus_col
                .filter(|c| *c < ns && Some(*c) != snap.time_col)
                .or_else(|| (0..ns).find(|c| Some(*c) != snap.time_col));
            if let Some(c) = pick {
                st.from = snap.columns[c].clone();
                st.to = st.from.clone();
            }
        }
        st
    }

    /// The Follow sheet.
    fn follow_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        // dev switch: LOGSEEK_SHOT_FOLLOW=1 opens Follow from the current result and runs it
        // dev switch: LOGSEEK_SHOT_CELLFOLLOW="column|column to look in[|tree]" follows
        // the selected row's value as the cell menu does
        if let Ok(spec) = crate::dev::var("LOGSEEK_SHOT_CELLFOLLOW") {
            if snap.phase.settled() && ui.ctx().input(|i| i.time) > 3.0 {
                if let (Some(row), Some(p)) = (self.selected_row_id(snap), snap.page.as_ref()) {
                    std::env::remove_var("LOGSEEK_SHOT_CELLFOLLOW");
                    let f: Vec<&str> = spec.split('|').collect();
                    let col = f.first().copied().unwrap_or_default().to_string();
                    let value = snap
                        .columns
                        .iter()
                        .position(|n| *n == col)
                        .and_then(|c| {
                            let k = p.rows.iter().position(|r| *r == row)?;
                            p.cells.get(k)?.get(c)?.clone()
                        })
                        .unwrap_or_default();
                    self.follow_from_cell(
                        snap,
                        row,
                        col,
                        value,
                        f.get(1).copied().unwrap_or_default().to_string(),
                        f.get(2) == Some(&"tree"),
                    );
                }
            }
        }
        if crate::dev::var_os("LOGSEEK_SHOT_FOLLOW").is_some()
            && snap.phase.settled()
            && snap.page.is_some()
            && ui.ctx().input(|i| i.time) > 3.0
        {
            std::env::remove_var("LOGSEEK_SHOT_FOLLOW");
            let mut st = self.new_follow(snap);
            st.start = crate::follow::StartKind::Result;
            if let Some(spec) = st.spec() {
                st.run = 1;
                self.bridge.send(Cmd::Follow {
                    id: 1,
                    start: crate::bridge::FollowStart::Result,
                    spec,
                });
            }
            self.follow = Some(st);
        }
        if std::mem::take(&mut self.following_note) {
            let now = ui.input(|i| i.time);
            self.flash = Some(("Following: the linked rows are being found.".to_string(), now, false));
        }
        if let Some((st, words)) = self.follow_quiet.take() {
            match snap.follow.as_ref().filter(|o| o.id == st.run) {
                None => self.follow_quiet = Some((st, words)),
                Some(out) => {
                    let mut rows: Vec<u64> = out.start_rows.as_ref().clone();
                    rows.extend(out.levels.iter().flatten().map(|(r, _)| *r));
                    rows.sort_unstable();
                    rows.dedup();
                    let found = rows.len().saturating_sub(out.start_rows.len());
                    if let Some(e) = &out.error {
                        let now = ui.input(|i| i.time);
                        self.flash = Some((format!("Follow could not be done: {e}"), now, true));
                    } else if found == 0 {
                        // nothing links to it: said, and the sheet is opened so
                        // another column can be chosen
                        let now = ui.input(|i| i.time);
                        self.flash = Some((
                            format!("No other row has that value in {}. Choose another column in the Follow sheet.", st.to),
                            now,
                            false,
                        ));
                        self.follow = Some(st);
                    } else {
                        self.push_history();
                        self.query.clear();
                        self.filters.clear();
                        self.maybe_send_query(true);
                        self.tag_filter = None;
                        self.grid.reset();
                        self.follow_showing = true;
                        self.shown_as = Some(words);
                        self.why = None;
                        self.showing_list = false;
                        self.grid.grouping = None;
                        self.grid.ftree = Some(crate::followtree::FollowTree::new_timed(
                            out.id,
                            &out.start_rows,
                            &out.levels,
                            &out.times,
                        ));
                        self.bridge.send(Cmd::Restrict(Some(std::sync::Arc::new(rows))));
                        // kept, closed: "Change" opens it as it was run
                        self.follow_last = Some(st);
                    }
                }
            }
        }
        let Some(mut st) = self.follow.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let at = egui::pos2(
            (screen.right() - 640.0).max(screen.left() + 8.0),
            screen.top() + TOP_H + 4.0,
        );
        let selected = self.selected_row_id(snap);
        let mut act = crate::follow::FollowAction::None;
        egui::Area::new(egui::Id::new("follow-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        act = crate::follow::show(
                            ui,
                            pal,
                            &mut st,
                            snap,
                            &self.tags,
                            selected,
                            self.follow_showing,
                            &mut self.prefs.follow_recipes,
                        );
                    });
            });
        // dev switch: LOGSEEK_SHOT_FOLLOW_SHOW=1 presses "Show these rows" once the answer is in
        if crate::dev::var_os("LOGSEEK_SHOT_FOLLOW_SHOW").is_some() {
            if let Some(out) = snap
                .follow
                .as_ref()
                .filter(|o| o.id == st.run && st.run > 0)
            {
                std::env::remove_var("LOGSEEK_SHOT_FOLLOW_SHOW");
                // the filter switch re-adds its filter whenever it is missing
                std::env::remove_var("LOGSEEK_SHOT_FILTER");
                let mut rows: Vec<u64> = out.start_rows.as_ref().clone();
                rows.extend(out.levels.iter().flatten().map(|(r, _)| *r));
                rows.sort_unstable();
                rows.dedup();
                act = crate::follow::FollowAction::ShowRows(std::sync::Arc::new(rows));
            }
        }
        let esc =
            ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx);
        let mut keep = !esc;
        if matches!(act, crate::follow::FollowAction::ShowRows(_)) {
            // the tree is the answer: get the sheet out of its way
            keep = false;
        }
        match act {
            crate::follow::FollowAction::Run(start, spec) => {
                self.bridge.send(Cmd::Follow {
                    id: st.run,
                    start,
                    spec,
                });
            }
            crate::follow::FollowAction::ShowRows(rows) => {
                // the found rows rarely match the search that found the start
                self.push_history();
                self.query.clear();
                self.filters.clear();
                self.maybe_send_query(true);
                self.tag_filter = None;
                self.grid.reset();
                self.follow_showing = true;
                self.shown_as = None;
                self.why = None;
                self.showing_list = false;
                // the tree: every starting row with what links to it beneath
                if let Some(out) = snap.follow.as_ref().filter(|o| o.id == st.run) {
                    self.grid.grouping = None;
                    self.grid.ftree = Some(crate::followtree::FollowTree::new_timed(
                        out.id,
                        &out.start_rows,
                        &out.levels,
                        &out.times,
                    ));
                }
                self.bridge.send(Cmd::Restrict(Some(rows)));
                self.hide_sheet = true;
            }
            crate::follow::FollowAction::ShowAll => {
                self.grid.ftree = None;
                self.grid.reset();
                self.follow_showing = false;
                self.shown_as = None;
                self.why = None;
                self.bridge.send(Cmd::Restrict(None));
            }
            crate::follow::FollowAction::TagAll(rows, i) => {
                for r in rows {
                    *self.tags.entry(r).or_default() |= 1 << i;
                }
                self.bridge
                    .send(Cmd::SaveTags(std::sync::Arc::new(self.tags.clone())));
            }
            crate::follow::FollowAction::Close => keep = false,
            crate::follow::FollowAction::None => {}
        }
        if keep {
            self.follow = Some(st);
        }
    }

    /// The tab strip: click a tab to show it, x to close it, + for another
    /// tab on this file. Each tab keeps its own search, filters and sort.
    fn tab_strip(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        // dev switch: LOGSEEK_SHOT_CLEAR_AT=<seconds> clears the search at that time, as
        // removing every filter does (to check that the selected row is kept)
        if let Ok(at) = crate::dev::var("LOGSEEK_SHOT_CLEAR_AT") {
            let t = ui.ctx().input(|i| i.time);
            if at.parse::<f64>().is_ok_and(|a| t > a) && self.grid.sel.is_some() {
                std::env::remove_var("LOGSEEK_SHOT_CLEAR_AT");
                self.query.clear();
                self.filters.clear();
                self.maybe_send_query(true);
            }
        }
        // dev switch: LOGSEEK_SHOT_OPEN="file;file;…" opens each as a tab, one a second
        if let Ok(list) = crate::dev::var("LOGSEEK_SHOT_OPEN") {
            let t = ui.ctx().input(|i| i.time);
            let files: Vec<&str> = list.split(';').filter(|f| !f.is_empty()).collect();
            let opened = self.tabs.list.len().saturating_sub(1);
            if opened >= files.len() {
                std::env::remove_var("LOGSEEK_SHOT_OPEN");
            } else if snap.phase.settled() && t > 2.0 + opened as f64 * 0.9 {
                self.start_open(PathBuf::from(files[opened]), Default::default());
            }
        }
        // dev switch: LOGSEEK_SHOT_TABS="other file": search, a second tab on this file with
        // another search, the other file, then back to the first tab
        if let Ok(other) = crate::dev::var("LOGSEEK_SHOT_TABS") {
            let t = ui.ctx().input(|i| i.time);
            let n = self.tabs.list.len();
            if n == 1 && t > 2.0 && self.query.is_empty() {
                self.query = "sshd".into();
                self.maybe_send_query(true);
            } else if n == 1 && t > 3.5 {
                self.new_tab();
                self.query = "bash".into();
                self.maybe_send_query(true);
            } else if n == 2 && t > 5.0 {
                self.start_open(PathBuf::from(&other), Default::default());
            } else if n == 3 && t > 7.5 {
                std::env::remove_var("LOGSEEK_SHOT_TABS");
                if let Some(first) = self.tabs.list.first().map(|t| t.id) {
                    self.switch_tab(first);
                }
            }
        }
        if self.tabs.list.len() < 2 {
            return;
        }
        let mut show: Option<u64> = None;
        let mut close: Option<u64> = None;
        let mut add = false;
        egui::Panel::top("file-tabs")
            .min_size(30.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(pal.bg)
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().line_segment(
                    [full.left_bottom(), full.right_bottom()],
                    Stroke::new(1.0, pal.hairline),
                );
                // The list of open files comes first and is always there: with
                // a hundred files open it finds one by part of its name. The
                // tabs beside it take up to two lines and scroll beyond that,
                // so they never take the rows' room.
                const TABS_MAX_H: f32 = 62.0;
                let now_on = self.tabs.current;
                let moved_key = egui::Id::new("tab-shown");
                let moved = ui.data(|d| d.get_temp::<Option<u64>>(moved_key)) != Some(now_on);
                ui.data_mut(|d| d.insert_temp(moved_key, now_on));
                ui.horizontal_top(|ui| {
                    let list_id = egui::Id::new("open-files");
                    let opened = egui::ComboBox::from_id_salt(list_id)
                        .selected_text(format!(
                            "{}  {}",
                            ph::FILES,
                            crate::keep::plural(self.tabs.list.len(), "tab", "tabs")
                        ))
                        .height(420.0)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show_ui(ui, |ui| {
                            ui.set_min_width(520.0);
                            let typed = crate::colpick::find_box_saying(
                                ui,
                                list_id,
                                format!(
                                    "Find an open file ({} open)",
                                    group(self.tabs.list.len() as u64)
                                ),
                            );
                            let full: Vec<String> = self
                                .tabs
                                .list
                                .iter()
                                .map(|t| crate::fileset::short_name(std::path::Path::new(&t.path)))
                                .collect();
                            let found = crate::tabs::find_tabs(&full, &typed);
                            if found.is_empty() {
                                ui.label(RichText::new("No open file has that in its name").color(pal.muted));
                            }
                            for (k, name) in found {
                                let Some(tab) = self.tabs.list.get(k) else { continue };
                                let here = self.tabs.current == Some(tab.id);
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new(ph::X).color(pal.text2))
                                                .frame(false)
                                                .min_size(egui::vec2(24.0, 24.0)),
                                        )
                                        .on_hover_text("Close this tab. The file and its index stay")
                                        .clicked()
                                    {
                                        close = Some(tab.id);
                                    }
                                    // the whole name, wrapped: this is where it is read in full
                                    if ui
                                        .add(
                                            egui::Button::selectable(
                                                here,
                                                RichText::new(name.as_str()).color(if here {
                                                    pal.accent_ink
                                                } else {
                                                    pal.text
                                                }),
                                            )
                                            .wrap_mode(egui::TextWrapMode::Wrap),
                                        )
                                        .clicked()
                                    {
                                        if !here {
                                            show = Some(tab.id);
                                        }
                                        ui.close();
                                    }
                                });
                            }
                        });
                    if opened.inner.is_none() {
                        crate::colpick::closed(ui, list_id);
                    }
                    opened
                        .response
                        .on_hover_text("Every open file, with a box to find one by part of its name");
                egui::ScrollArea::vertical()
                    .id_salt("tabs-scroll")
                    .max_height(TABS_MAX_H)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                    let full = ui.max_rect();
                    ui.set_max_width(full.width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for tab in &self.tabs.list {
                            let on = self.tabs.current == Some(tab.id);
                            // what the tab is looking at: its search, in words
                            let v = if on {
                                Some(self.view_now())
                            } else {
                                self.views.get(&tab.id).cloned()
                            };
                            let looking = v
                                .map(|v| crate::filters::compose(&v.query, &v.filters, None))
                                .filter(|q| !q.trim().is_empty());
                            let tip = match looking {
                                Some(q) => format!("{}\nShowing: {q}", tab.path),
                                None => format!("{}\nShowing every row", tab.path),
                            };
                            // a tab is one piece: when it does not fit on the
                            // line, the whole tab starts the next line (its name
                            // is never broken in two)
                            let tab_text = format!("{}  {}", ph::FILE_TEXT, self.tabs.label(tab.id));
                            let tab_w = ui
                                .painter()
                                .layout_no_wrap(tab_text.clone(), theme::ui_font(), pal.text)
                                .size()
                                .x
                                + 2.0 * ui.spacing().button_padding.x
                                + 24.0
                                + 8.0;
                            // measured against the strip's own right edge
                            if ui.cursor().left() + tab_w > full.right() - 8.0
                                && ui.cursor().left() > full.left() + 16.0
                            {
                                ui.end_row();
                            }
                            // one outline holds the tab's name and its own close
                            // button, so it is plain which tab the cross closes
                            let framed = Frame::new()
                                .fill(if on { pal.surface } else { pal.bg })
                                .stroke(if on {
                                    Stroke::new(1.0, pal.accent_ink)
                                } else {
                                    Stroke::new(1.0, pal.hairline)
                                })
                                .corner_radius(CornerRadius::same(4))
                                .inner_margin(Margin::symmetric(2, 0))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    let b = ui.add(
                                        egui::Button::new(
                                            RichText::new(tab_text.as_str())
                                                .color(if on { pal.accent_ink } else { pal.text2 }),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .frame(false)
                                        .min_size(egui::vec2(0.0, 24.0)),
                                    );
                                    if b.on_hover_text(tip).clicked() && !on {
                                        show = Some(tab.id);
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(ph::X).color(pal.text2),
                                            )
                                            .frame(false)
                                            .min_size(egui::vec2(24.0, 24.0)),
                                        )
                                        .on_hover_text(
                                            "Close this tab  (Ctrl+W). The file and its index stay",
                                        )
                                        .clicked()
                                    {
                                        close = Some(tab.id);
                                    }
                                });
                            // the tab just gone to is brought into view
                            if on && moved {
                                framed.response.scroll_to_me(Some(Align::Center));
                            }
                            ui.add_space(4.0);
                        }
                        if ui
                            .add(egui::Button::new(ph::PLUS).min_size(egui::vec2(24.0, 24.0)))
                            .on_hover_text(
                                "Another tab on this file, starting from this view  (Ctrl+T)",
                            )
                            .clicked()
                        {
                            add = true;
                        }
                    });
                });
                });
            });
        let _ = snap;
        if add {
            self.new_tab();
        }
        if let Some(id) = close {
            self.close_tab(id);
        }
        if let Some(id) = show {
            self.switch_tab(id);
        }
    }

    /// The query workspace: an editor that says whether the query can run,
    /// everything run before, and the saved ones.
    fn work_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        // dev switch: LOGSEEK_SHOT_WORK="query text" opens the workspace on it (never saved)
        if let Ok(q) = crate::dev::var("LOGSEEK_SHOT_WORK") {
            if snap.page.is_some() {
                std::env::remove_var("LOGSEEK_SHOT_WORK");
                self.no_save = true;
                self.work = Some(crate::workspace::WorkState {
                    text: q.replace("\\n", "\n"),
                    ..Default::default()
                });
            }
        }
        let Some(mut w) = self.work.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let mut run: Option<String> = None;
        let mut load: Option<String> = None;
        let mut apply_saved: Option<usize> = None;
        let mut forget_saved: Option<usize> = None;
        let mut forget_ran: Option<usize> = None;
        let (mut close, mut clear_log, mut save) = (false, false, false);
        let mut open_help = false;
        let problem = crate::workspace::problem(&w.text);
        let line = crate::workspace::one_line(&w.text);
        egui::Area::new(egui::Id::new("work-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                (screen.right() - 780.0).max(screen.left() + 8.0),
                screen.top() + TOP_H + 4.0,
            ))
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width((screen.width() - 40.0).min(740.0));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Query workspace").font(fonts::strong(14.0)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                                    close = true;
                                }
                                let help = icon_button(ui, pal, ph::QUESTION, "Help: query words and keys  (?)");
                                if help.clicked() {
                                    open_help = true;
                                }
                                help.on_hover_ui(|ui| crate::grid::filter_help(ui, pal));
                            });
                        });
                        ui.label(
                            RichText::new("Write a query over as many lines as you like. It is checked as you type and runs on the file that is open.")
                                .color(pal.text2),
                        );
                        let ed = ui.add(
                            egui::TextEdit::multiline(&mut w.text)
                                .font(theme::data_font())
                                .desired_rows(5)
                                .desired_width(f32::INFINITY)
                                .hint_text(crate::workspace::example(&snap.columns, snap.time_col)),
                        );
                        if std::mem::take(&mut w.focus) {
                            ed.request_focus();
                        }
                        // what would run, or why it cannot
                        match &problem {
                            Some(e) => {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("{}  This cannot run yet: {e}", ph::WARNING))
                                            .color(pal.danger),
                                    )
                                    .wrap(),
                                );
                            }
                            None if line.is_empty() => {
                                ui.label(RichText::new("An empty query shows every row.").color(pal.text2));
                            }
                            None => {
                                ui.label(
                                    RichText::new(format!("{}  Ready to run", ph::CHECK_CIRCLE))
                                        .color(pal.accent_ink),
                                );
                            }
                        }
                        ui.horizontal_wrapped(|ui| {
                            let can = problem.is_none() && snap.source.is_some();
                            let b = ui
                                .add_enabled_ui(can, |ui| {
                                    primary_button(ui, pal, &format!("{}  Run", ph::PLAY))
                                })
                                .inner
                                .on_hover_text("Run it on the open file  (Ctrl+Enter)")
                                .on_disabled_hover_text("Open a file and fix the query first");
                            let key = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                            if can && (b.clicked() || key) {
                                run = Some(line.clone());
                            }
                            if ui.button(format!("{}  Copy", ph::COPY)).clicked() {
                                crate::clip::copy(ui.ctx(), line.clone(), "the query");
                            }
                            ui.add_space(12.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut w.save_name)
                                    .hint_text("Name to save it as")
                                    .desired_width(180.0),
                            );
                            if ui
                                .add_enabled(
                                    !w.save_name.trim().is_empty() && !line.is_empty() && problem.is_none(),
                                    egui::Button::new("Save"),
                                )
                                .on_disabled_hover_text("Type a name, and a query that can run")
                                .clicked()
                            {
                                save = true;
                            }
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut w.tab,
                                0,
                                format!("Run before ({})", group(self.prefs.query_log.len() as u64)),
                            );
                            ui.selectable_value(
                                &mut w.tab,
                                1,
                                format!("Saved ({})", group(self.prefs.presets.len() as u64)),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut w.find)
                                    .hint_text(format!("{}  Find", ph::MAGNIFYING_GLASS))
                                    .desired_width(200.0),
                            );
                            if w.tab == 0
                                && !self.prefs.query_log.is_empty()
                                && ui.button("Clear this list").clicked()
                            {
                                clear_log = true;
                            }
                        });
                        let max_h = (screen.height() - 430.0).max(140.0);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        egui::ScrollArea::vertical()
                            .max_height(max_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                if w.tab == 0 {
                                    let list = crate::workspace::find(&self.prefs.query_log, &w.find);
                                    if list.is_empty() {
                                        ui.label(
                                            RichText::new("Queries you run appear here, newest first.")
                                                .color(pal.text2),
                                        );
                                    }
                                    for i in list {
                                        let r = &self.prefs.query_log[i];
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add(egui::Button::new(ph::X).frame(false).min_size(egui::vec2(24.0, 24.0)))
                                                .on_hover_text("Forget this one")
                                                .clicked()
                                            {
                                                forget_ran = Some(i);
                                            }
                                            let q = ui
                                                .add(
                                                    egui::Label::new(
                                                        RichText::new(&r.query).font(theme::data_font()).color(pal.text),
                                                    )
                                                    .wrap()
                                                    .sense(Sense::click()),
                                                )
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .on_hover_text("Click to put it in the editor; double-click to run it");
                                            if q.double_clicked() {
                                                run = Some(r.query.clone());
                                            } else if q.clicked() {
                                                load = Some(r.query.clone());
                                            }
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} of {} rows  ·  {}  ·  {} UTC",
                                                    group(r.rows),
                                                    group(r.of),
                                                    r.file,
                                                    logseek_core::types::fmt_time(r.at as i64 * 1_000_000)
                                                ))
                                                .font(theme::small_font())
                                                .color(pal.text2),
                                            );
                                        });
                                    }
                                } else {
                                    let needle = w.find.to_lowercase();
                                    let mut any = false;
                                    for (i, p) in self.prefs.presets.iter().enumerate() {
                                        if !needle.is_empty()
                                            && !format!("{} {}", p.name, p.query).to_lowercase().contains(&needle)
                                        {
                                            continue;
                                        }
                                        any = true;
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add(egui::Button::new(ph::X).frame(false).min_size(egui::vec2(24.0, 24.0)))
                                                .on_hover_text("Forget this saved one")
                                                .clicked()
                                            {
                                                forget_saved = Some(i);
                                            }
                                            if ui
                                                .link(RichText::new(&p.name).strong())
                                                .on_hover_text("Apply it to the open file")
                                                .clicked()
                                            {
                                                apply_saved = Some(i);
                                            }
                                            let what = if p.columns.is_empty() {
                                                p.query.clone()
                                            } else {
                                                format!(
                                                    "{}  with {} column {}",
                                                    p.query,
                                                    p.columns.len(),
                                                    if p.columns.len() == 1 { "filter" } else { "filters" }
                                                )
                                            };
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(what).font(theme::data_font()).color(pal.text2),
                                                )
                                                .wrap(),
                                            );
                                        });
                                    }
                                    if !any {
                                        ui.label(
                                            RichText::new("Nothing saved yet. Name a query above and press Save.")
                                                .color(pal.text2),
                                        );
                                    }
                                }
                            });
                    });
            });
        if let Some(q) = load {
            w.text = q;
            w.focus = true;
        }
        if save {
            let name = w.save_name.trim().to_string();
            self.prefs.presets.retain(|p| p.name != name);
            self.prefs.presets.push(Preset {
                name,
                query: line.clone(),
                columns: Vec::new(),
                except: Vec::new(),
            });
            self.prefs
                .presets
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            w.save_name.clear();
            w.tab = 1;
        }
        if clear_log {
            self.prefs.query_log.clear();
        }
        if let Some(i) = forget_ran {
            self.prefs.query_log.remove(i);
        }
        if let Some(i) = forget_saved {
            self.prefs.presets.remove(i);
        }
        if let Some(i) = apply_saved {
            if let Some(p) = self.prefs.presets.get(i).cloned() {
                self.apply_preset(&p, snap);
                w.text = self.effective_query();
            }
        }
        if let Some(q) = run {
            // the whole query lives in the search box; column filters are cleared (Undo restores)
            self.push_history();
            self.query = q.clone();
            self.filters.clear();
            self.grid.top = 0;
            self.grid.sel = None;
            self.maybe_send_query(true);
            w.text = q;
        }
        let esc =
            ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx);
        if open_help {
            self.keys_open = true;
        }
        if !(close || esc) {
            self.work = Some(w);
        }
    }

    /// The case panel: the files of this investigation, one click apart.
    fn case_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        // dev switch: LOGSEEK_SHOT_CASE="folder" lists that folder as a case (never saved)
        if let Ok(dir) = crate::dev::var("LOGSEEK_SHOT_CASE") {
            std::env::remove_var("LOGSEEK_SHOT_CASE");
            self.no_save = true;
            let (found, skipped) = crate::cases::scan_folder(std::path::Path::new(&dir));
            let mut c = crate::cases::Case {
                name: "Sample case".into(),
                files: Vec::new(),
            };
            c.add(found);
            self.case_note = Some(format!(
                "{} listed.",
                crate::keep::plural(c.files.len(), "file", "files")
            ));
            self.case_skipped = skipped;
            self.prefs.cases = vec![c];
            self.prefs.case = 0;
            self.case_open = true;
        }
        if !self.case_open {
            return;
        }
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        if self.prefs.cases.is_empty() {
            self.prefs.cases.push(crate::cases::Case {
                name: "My case".into(),
                files: Vec::new(),
            });
        }
        self.prefs.case = self.prefs.case.min(self.prefs.cases.len() - 1);
        let current = snap
            .source
            .as_ref()
            .map(|s| s.path.to_string_lossy().into_owned());
        let mut open_path: Option<String> = None;
        let mut open_set: Option<Vec<PathBuf>> = None;
        let mut remove: Option<usize> = None;
        let (mut add_files, mut add_folder, mut pick, mut close, mut new_case) =
            (false, false, false, false, false);
        let (mut add_current, mut add_open) = (false, false);
        let open_paths: Vec<String> = self.tabs.list.iter().map(|t| t.path.clone()).collect();
        let area = egui::Area::new(egui::Id::new("case-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.left() + 8.0, screen.top() + TOP_H + 4.0))
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width((screen.width() - 40.0).min(760.0));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Case").font(fonts::strong(14.0)));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.prefs.cases[self.prefs.case].name)
                                    .desired_width(200.0),
                            )
                            .on_hover_text("This case's name: type to rename it");
                            let names: Vec<String> =
                                self.prefs.cases.iter().map(|c| c.name.clone()).collect();
                            if names.len() > 1 {
                                // another case: only offered when there is one
                                egui::ComboBox::from_id_salt("case-pick")
                                    .selected_text(format!("Switch case  ({} cases)", names.len()))
                                    .show_ui(ui, |ui| {
                                        for (i, n) in names.iter().enumerate() {
                                            ui.selectable_value(&mut self.prefs.case, i, n);
                                        }
                                    });
                            }
                            if ui
                                .button(format!("{}  New case", ph::PLUS))
                                .on_hover_text("Start another, empty case; this one is kept")
                                .clicked()
                            {
                                new_case = true;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.label(
                            RichText::new("The files of one investigation, a click apart. A file joins a case only when you add it here; opening a file does not add it. Files are only listed: never copied, moved or changed.")
                                .color(pal.text2),
                        );
                        // what is open, against this case: said, with one click to add it
                        if let Some(cur) = current.as_ref() {
                            let files = &self.prefs.cases[self.prefs.case].files;
                            let missing = open_paths.iter().filter(|p| !files.contains(*p)).count();
                            let cur_in = files.contains(cur);
                            ui.horizontal_wrapped(|ui| {
                                if cur_in && missing == 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  {} in this case.",
                                            ph::CHECK,
                                            if open_paths.len() > 1 {
                                                "Every open file is"
                                            } else {
                                                "The file on screen is"
                                            }
                                        ))
                                        .color(pal.text),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new(if cur_in {
                                            format!(
                                                "The file on screen is in this case; {} not.",
                                                if missing == 1 {
                                                    "1 other open file is".to_string()
                                                } else {
                                                    format!("{} other open files are", group(missing as u64))
                                                }
                                            )
                                        } else {
                                            "The file on screen is not in this case.".to_string()
                                        })
                                        .color(pal.text),
                                    );
                                    if !cur_in && primary_button(ui, pal, "Add the file on screen").clicked() {
                                        add_current = true;
                                    }
                                    if open_paths.len() > 1
                                        && missing > 0
                                        && ui
                                            .button(format!(
                                                "{}  Add all {} open files",
                                                ph::FILES,
                                                group(open_paths.len() as u64)
                                            ))
                                            .clicked()
                                    {
                                        add_open = true;
                                    }
                                }
                            });
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.button(format!("{}  Add files", ph::FILE_PLUS)).clicked() {
                                add_files = true;
                            }
                            if ui
                                .button(format!("{}  Add a folder", ph::FOLDER_PLUS))
                                .on_hover_text("Every CSV, JSON and text log in the folder, at every level")
                                .clicked()
                            {
                                add_folder = true;
                            }
                            if ui
                                .button(format!("{}  Open a file without adding it", ph::FOLDER_OPEN))
                                .clicked()
                            {
                                pick = true;
                            }
                            if current.is_some()
                                && ui
                                    .button(format!("{}  Start this file from scratch", ph::ARROW_COUNTER_CLOCKWISE))
                                    .on_hover_text("Choose what to clear: the search and view, tags, made columns. The file itself is never changed.")
                                    .clicked()
                            {
                                self.scratch = Some([true, false, false, false]);
                                close = true;
                            }
                        });
                        if let Some(n) = &self.case_note {
                            ui.add(egui::Label::new(RichText::new(n).color(pal.text2)).wrap());
                        }
                        if let Some(line) =
                            crate::cases::skipped_line(&self.case_skipped, self.case_skipped_all)
                        {
                            ui.horizontal_wrapped(|ui| {
                                ui.add(egui::Label::new(RichText::new(line).color(pal.text2)).wrap());
                                if self.case_skipped.len() > 3
                                    && ui
                                        .link(if self.case_skipped_all {
                                            "Show fewer"
                                        } else {
                                            "Show every kind"
                                        })
                                        .clicked()
                                {
                                    self.case_skipped_all = !self.case_skipped_all;
                                }
                            });
                        }
                        ui.separator();
                        let case = &self.prefs.cases[self.prefs.case];
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.case_find)
                                    .hint_text(format!("{}  Find a file (several words narrow it)", ph::MAGNIFYING_GLASS))
                                    .desired_width(300.0),
                            );
                            let shown = case.find(&self.case_find).len();
                            ui.label(
                                RichText::new(if shown == case.files.len() {
                                    crate::keep::plural(shown, "file", "files")
                                } else {
                                    format!("{} of {} files", group(shown as u64), group(case.files.len() as u64))
                                })
                                .color(pal.text2),
                            );
                            // the files listed (all, or those the words narrow it to) as one view
                            if shown >= 2
                                && ui
                                    .button(format!("{}  Open these {} as one view", ph::STACK, group(shown as u64)))
                                    .on_hover_text("One set of rows from every file listed, each row saying which file it came from (the _file column). Sort by time for one timeline. Nothing is copied.")
                                    .clicked()
                            {
                                open_set = Some(
                                    case.find(&self.case_find)
                                        .into_iter()
                                        .filter_map(|i| case.files.get(i).map(PathBuf::from))
                                        .collect(),
                                );
                            }
                        });
                        if case.files.is_empty() {
                            ui.label(RichText::new("No files yet. Add files or a folder to begin.").color(pal.text2));
                            return;
                        }
                        let list = case.find(&self.case_find);
                        let row_h = 24.0;
                        let max_h = (screen.height() - 330.0).max(160.0);
                        // a dense list: little air between files
                        ui.spacing_mut().item_spacing.y = 2.0;
                        egui::ScrollArea::both()
                            .max_height(max_h)
                            .min_scrolled_height(max_h)
                            .auto_shrink([false, true])
                            .show_rows(ui, row_h, list.len(), |ui, range| {
                                for k in range {
                                    let i = list[k];
                                    let path = &case.files[i];
                                    let p = std::path::Path::new(path);
                                    let name = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    // a file read out of a zip or workbook is named by what it
                                    // is inside it, and placed where that zip or workbook is
                                    let (name, folder): (String, String) = ui.data_mut(|d| {
                                        d.get_temp_mut_or_insert_with(
                                            egui::Id::new(("case-where", path)),
                                            || match crate::book::origin(p) {
                                                Some((book, inner)) => {
                                                    (inner, format!("inside {}", book.display()))
                                                }
                                                None => match crate::book::inner_name(p) {
                                                    Some(inner) => {
                                                        let said = crate::book::where_from(p);
                                                        (inner, said)
                                                    }
                                                    None => (
                                                        name.clone(),
                                                        p.parent()
                                                            .map(|d| d.to_string_lossy().into_owned())
                                                            .unwrap_or_default(),
                                                    ),
                                                },
                                            },
                                        )
                                        .clone()
                                    });
                                    let (bytes, indexed) =
                                        *self.case_info.entry(path.clone()).or_insert_with(|| {
                                            let bytes = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                                            let dir = crate::bridge::store_dir_for(p);
                                            (bytes, dir.join(logseek_core::store::manifest::MANIFEST).exists())
                                        });
                                    let is_current = current.as_deref() == Some(path.as_str());
                                    ui.horizontal(|ui| {
                                        ui.set_min_height(row_h);
                                        if ui
                                            .add(egui::Button::new(ph::X).frame(false).min_size(egui::vec2(24.0, 24.0)))
                                            .on_hover_text("Take it off this case's list (the file itself stays)")
                                            .clicked()
                                        {
                                            remove = Some(i);
                                        }
                                        let label = RichText::new(format!("{}  {name}", ph::FILE_TEXT))
                                            .color(if is_current { pal.accent_ink } else { pal.text })
                                            .strong();
                                        if ui
                                            .add(egui::Label::new(label).extend().sense(Sense::click()))
                                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                                            .on_hover_text("Open this file")
                                            .clicked()
                                        {
                                            open_path = Some(path.clone());
                                        }
                                        let state = if is_current {
                                            "open now"
                                        } else if indexed {
                                            "indexed"
                                        } else {
                                            "not opened yet"
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(format!("{}  ·  {state}  ·  {folder}", human_bytes(bytes)))
                                                    .font(theme::small_font())
                                                    .color(pal.text2),
                                            )
                                            // a long folder wraps: it is never cut off
                                            .wrap(),
                                        );
                                    });
                                }
                            });
                    });
            });
        if new_case {
            let n = self.prefs.cases.len() + 1;
            self.prefs.cases.push(crate::cases::Case {
                name: format!("Case {n}"),
                files: Vec::new(),
            });
            self.prefs.case = self.prefs.cases.len() - 1;
            self.case_note = None;
            self.case_skipped.clear();
        }
        if add_current || add_open {
            let paths: Vec<PathBuf> = if add_open {
                open_paths.iter().map(PathBuf::from).collect()
            } else {
                current.iter().map(PathBuf::from).collect()
            };
            let added = self.prefs.cases[self.prefs.case].add(paths);
            self.case_note = Some(format!(
                "{} added to this case.",
                crate::keep::plural(added, "file", "files")
            ));
        }
        if add_files {
            if let Some(files) = rfd::FileDialog::new()
                .set_title("Add files to this case")
                .add_filter("Logs (CSV, JSON, text)", &crate::cases::KINDS)
                .add_filter("All files", &["*"])
                .pick_files()
            {
                self.prefs.cases[self.prefs.case].add(files);
                self.case_note = None;
            }
        }
        if add_folder {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Add a folder to this case")
                .pick_folder()
            {
                let (found, skipped) = crate::cases::scan_folder(&dir);
                let added = self.prefs.cases[self.prefs.case].add(found);
                self.case_note = Some(format!(
                    "{} files added from {}.",
                    group(added as u64),
                    dir.display()
                ));
                self.case_skipped = skipped;
                self.case_skipped_all = false;
            }
        }
        if let Some(i) = remove {
            let f = self.prefs.cases[self.prefs.case].files.remove(i);
            self.case_info.remove(&f);
        }
        if pick {
            self.case_open = false;
            self.pick_file();
        }
        if let Some(files) = open_set {
            self.case_open = false;
            self.open_files(files);
        }
        if let Some(p) = open_path {
            self.case_open = false;
            self.case_info.remove(&p);
            self.open(PathBuf::from(p));
        }
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && !egui::Popup::is_any_open(&ctx)
            && ctx
                .pointer_interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p));
        if close
            || clicked_outside
            || (ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx))
        {
            self.case_open = false;
        }
    }

    /// The Find bar: jump from row to row that contains the text; nothing is filtered.
    fn find_bar(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(mut f) = self.find.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let mut step: Option<bool> = None;
        let mut close = false;
        let mut goto: Option<usize> = None;
        egui::Area::new(egui::Id::new("find-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                (screen.right() - 560.0).max(screen.left() + 8.0),
                screen.top() + TOP_H + 4.0,
            ))
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut f.text)
                                    .hint_text(format!(
                                        "{}  Find in the rows shown",
                                        ph::MAGNIFYING_GLASS
                                    ))
                                    .desired_width(220.0),
                            );
                            if std::mem::take(&mut f.focus) {
                                r.request_focus();
                            }
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                step = Some(!ui.input(|i| i.modifiers.shift));
                                f.focus = true;
                            }
                            if icon_button(ui, pal, ph::ARROW_UP, "Previous match  (Shift+Enter)")
                                .clicked()
                            {
                                step = Some(false);
                            }
                            if icon_button(ui, pal, ph::ARROW_DOWN, "Next match  (Enter)").clicked()
                            {
                                step = Some(true);
                            }
                            let answer = snap
                                .find
                                .as_ref()
                                .filter(|o| o.id == f.id && f.id > 0 && o.text == f.sent);
                            match answer {
                                Some(o) if o.error.is_some() => {
                                    ui.label(
                                        RichText::new(format!(
                                            "Cannot look for that: {}",
                                            o.error.as_deref().unwrap_or_default()
                                        ))
                                        .color(pal.danger),
                                    );
                                }
                                Some(o) => {
                                    ui.label(
                                        RichText::new(find_line(o.total, o.index)).color(pal.text2),
                                    );
                                }
                                None if f.id > 0 && !f.applied => {
                                    ui.label(RichText::new("Looking").color(pal.text2));
                                }
                                None => {}
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        // column names that contain it: click to go there
                        let needle = f.text.trim().to_lowercase();
                        if !needle.is_empty() {
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
                                        .font(theme::small_font())
                                        .color(pal.text2),
                                    );
                                    for c in cols {
                                        if ui
                                            .link(
                                                RichText::new(&snap.columns[c])
                                                    .font(theme::small_font()),
                                            )
                                            .on_hover_text("Go to this column")
                                            .clicked()
                                        {
                                            goto = Some(c);
                                        }
                                    }
                                });
                            }
                        }
                    });
            });
        if let Some(c) = goto {
            if let Some(n) = snap.columns.get(c) {
                self.grid.hidden.remove(n);
            }
            self.grid.goto = Some(c);
        }
        if let Some(forward) = step.filter(|_| !f.text.trim().is_empty()) {
            // Find walks the plain list: leave groups and trees
            self.grid.grouping = None;
            self.grid.ftree = None;
            let same = f.sent == f.text;
            f.sent = f.text.clone();
            f.id += 1;
            f.applied = false;
            let from_pos = self.grid.sel.filter(|_| same);
            let after_row = if same {
                self.selected_row_id(snap)
            } else {
                None
            };
            self.bridge.send(Cmd::Find {
                id: f.id,
                text: f.sent.clone(),
                after_row,
                from_pos,
                forward,
            });
        }
        // the answer: select the row found and bring it into view
        if !f.applied {
            if let Some(o) = snap.find.as_ref().filter(|o| o.id == f.id) {
                f.applied = true;
                if let Some(pos) = o.pos {
                    self.grid.sel = Some(pos);
                    self.grid.top = pos.saturating_sub(4);
                    f.pending_col = Some(pos);
                }
            }
        }
        // once that row's cells are here, scroll sideways to the cell that has it
        if let Some(pos) = f.pending_col {
            if let Some(p) = snap
                .page
                .as_ref()
                .filter(|p| p.generation == snap.generation)
            {
                if let Some(row) = pos
                    .checked_sub(p.start)
                    .and_then(|k| p.cells.get(k as usize))
                {
                    let needle = f.sent.trim().to_lowercase();
                    let order = crate::grid::visible_order(
                        &self.grid.order,
                        &snap.columns,
                        &self.grid.hidden,
                    );
                    if let Some(&c) = order.iter().find(|&&c| {
                        row.get(c)
                            .and_then(|v| v.as_ref())
                            .is_some_and(|v| v.to_lowercase().contains(&needle))
                    }) {
                        self.grid.goto = Some(c);
                    }
                    f.pending_col = None;
                }
            }
        }
        let esc =
            ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx);
        if !(close || esc) {
            self.find = Some(f);
        }
    }

    /// The "Add a column" sheet.
    fn addcol_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        let Some(mut st) = self.addcol.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let at = egui::pos2(
            (screen.right() - 640.0).max(screen.left() + 8.0),
            screen.top() + TOP_H + 4.0,
        );
        let mut act = crate::addcol::AddAction::None;
        egui::Area::new(egui::Id::new("addcol-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        act = crate::addcol::show(ui, pal, &mut st, snap);
                    });
            });
        // dev switch: LOGSEEK_SHOT_ADDCOL_ADD=1 presses "Add column" once (end-to-end check)
        if crate::dev::var_os("LOGSEEK_SHOT_ADDCOL_ADD").is_some() {
            if let Some(r) = st.recipe() {
                std::env::remove_var("LOGSEEK_SHOT_ADDCOL_ADD");
                act = crate::addcol::AddAction::Add {
                    name: r.default_name(),
                    formula: crate::addcol::engine_formula(&r, snap),
                };
            }
        }
        let esc =
            ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx);
        match act {
            crate::addcol::AddAction::Add { name, formula } => {
                self.show_made = Some(name.clone());
                self.bridge.send(Cmd::AddColumns(vec![(name, formula)]));
            }
            crate::addcol::AddAction::Close => {}
            crate::addcol::AddAction::None if !esc => self.addcol = Some(st),
            crate::addcol::AddAction::None => {}
        }
    }

    /// The colour rules sheet: ordered rules, first match colours the row.
    fn colors_sheet(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        use crate::colors::{self as cr, ColorRule, RuleOp};
        if !self.colors_open {
            return;
        }
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let w = 600.0_f32.min(screen.width() - 24.0);
        let at = egui::pos2(screen.right() - w - 24.0, screen.top() + TOP_H + 4.0);
        let mut rules = self.prefs.colors.clone();
        // how many rows of this view each rule matches: counted by the engine,
        // one rule at a time, remembered by query
        let search = self.query.clone();
        let count_qs: Vec<Option<String>> = rules
            .iter()
            .map(|r| cr::count_query(r, &search, &self.filters))
            .collect();
        if let Some((asked, when)) = self.rule_count_asked.clone() {
            if let Some((_, n)) = snap.preview.as_ref().filter(|(q, _)| *q == asked) {
                self.rule_counts.insert(asked, *n);
                self.rule_count_asked = None;
            } else if when.elapsed() > Duration::from_millis(1500) {
                // another count went first and this one was dropped: ask again
                self.rule_count_asked = None;
            }
        }
        if self.rule_count_asked.is_none() {
            if let Some(q) = count_qs
                .iter()
                .flatten()
                .find(|q| !self.rule_counts.contains_key(*q))
            {
                self.bridge.send(Cmd::Count(q.clone()));
                self.rule_count_asked = Some((q.clone(), Instant::now()));
            }
        }
        if self.rule_count_asked.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let (mut remove, mut up, mut down, mut show) = (None, None, None, None);
        let mut close = false;
        egui::Area::new(egui::Id::new("colors-sheet"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Colour rules").font(fonts::strong(14.0)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, ph::X, "Close  (Esc)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.label(
                            RichText::new("Rows that match a rule get its tint and a marker in the left margin. The first matching rule wins.")
                                .font(theme::small_font())
                                .color(pal.text2),
                        );
                        ui.add_space(6.0);
                        let n = rules.len();
                        egui::ScrollArea::vertical().max_height((screen.height() - 260.0).max(200.0)).show(ui, |ui| {
                            for (i, r) in rules.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut r.enabled, "").on_hover_text("Use this rule");
                                    // the tint, by name as well as colour
                                    let swatch = cr::tint_color(r.tint, pal.dark);
                                    ui.menu_button(swatch_label(swatch, cr::TINTS[r.tint % 8], pal.text), |ui| {
                                        for (t, name) in cr::TINTS.iter().enumerate() {
                                            let c = cr::tint_color(t, pal.dark);
                                            if ui.button(swatch_label(c, name, pal.text)).clicked() {
                                                r.tint = t;
                                                ui.close();
                                            }
                                        }
                                    });
                                    ui.add(egui::TextEdit::singleline(&mut r.name).hint_text("what it means").desired_width(170.0));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if icon_button(ui, pal, ph::TRASH, "Delete this rule").clicked() {
                                            remove = Some(i);
                                        }
                                        if i + 1 < n && icon_button(ui, pal, ph::ARROW_DOWN, "Move down").clicked() {
                                            down = Some(i);
                                        }
                                        if i > 0 && icon_button(ui, pal, ph::ARROW_UP, "Move up (checked first)").clicked() {
                                            up = Some(i);
                                        }
                                        let only = cr::show_only(r);
                                        let tip = if only.is_some() {
                                            "Filter the grid to the rows this rule matches"
                                        } else {
                                            "Pick one column to filter by this rule"
                                        };
                                        if ui.add_enabled(only.is_some(), egui::Button::new("Show only")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                                            show = only;
                                        }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.add_space(28.0);
                                    crate::colpick::combo_or_none(
                                        ui,
                                        ("color-col", i),
                                        &mut r.column,
                                        &snap.columns,
                                        150.0,
                                        "any column",
                                    );
                                    egui::ComboBox::from_id_salt(("color-op", i))
                                        .selected_text(cr::op_label(r.op))
                                        .width(190.0)
                                        .show_ui(ui, |ui| {
                                            for op in cr::OPS {
                                                ui.selectable_value(&mut r.op, op, cr::op_label(op));
                                            }
                                        });
                                    let hint = match r.op {
                                        RuleOp::ContainsAny | RuleOp::IsAny => "words, separated by commas",
                                        RuleOp::Gt | RuleOp::Lt => "a number",
                                        RuleOp::Regex => "a regular expression",
                                        _ => "text",
                                    };
                                    ui.add(egui::TextEdit::singleline(&mut r.value).hint_text(hint).desired_width(f32::INFINITY));
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(28.0);
                                    ui.selectable_value(&mut r.cells, false, "Colour the whole row")
                                        .on_hover_text("The row gets the tint and a mark in the left margin");
                                    ui.selectable_value(&mut r.cells, true, "Colour just the cell")
                                        .on_hover_text("Only the matching cells get the tint, with a mark in their corner");
                                    let words = match count_qs.get(i).and_then(|q| q.as_ref()) {
                                        None if r.value.trim().is_empty() => "Give it a value to see what it matches".to_string(),
                                        None if !r.enabled => "Switched off".to_string(),
                                        None => String::new(),
                                        Some(q) => match self.rule_counts.get(q) {
                                            Some(0) => "Matches no row in this view".to_string(),
                                            Some(1) => "Matches 1 row in this view".to_string(),
                                            Some(n) => format!("Matches {} rows in this view", group(*n)),
                                            None => "Counting the rows it matches".to_string(),
                                        },
                                    };
                                    if !words.is_empty() {
                                        ui.label(RichText::new(words).font(theme::small_font()).color(pal.text2));
                                    }
                                });
                                ui.separator();
                            }
                            if rules.is_empty() {
                                ui.label(RichText::new("No rules yet. Right-click a cell and choose \"Colour rows where\", or add one here.").color(pal.muted));
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button(format!("{}  Add a rule", ph::PLUS)).clicked() {
                                rules.push(ColorRule {
                                    enabled: true,
                                    name: String::new(),
                                    column: None,
                                    op: RuleOp::Contains,
                                    value: String::new(),
                                    tint: rules.len() % 8,
                                    cells: false,
                                });
                            }
                        });
                    });
            });
        if let Some(i) = remove {
            rules.remove(i);
        }
        if let Some(i) = up {
            rules.swap(i, i - 1);
        }
        if let Some(i) = down {
            rules.swap(i, i + 1);
        }
        self.prefs.colors = rules;
        match show {
            Some(cr::ShowOnly::Column(c, t)) => {
                self.filters.entry(c).or_default().text = t;
                self.maybe_send_query(true);
            }
            Some(cr::ShowOnly::Global(q)) => {
                self.query = if self.query.trim().is_empty() {
                    q
                } else {
                    format!("{} ({q})", self.query.trim())
                };
                self.maybe_send_query(true);
            }
            None => {}
        }
        if close
            || (ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx))
        {
            self.colors_open = false;
        }
    }

    /// The calendar opened from the timeline: rows per day, pick days or hours.
    fn calendar_popup(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot) {
        if !self.cal_open {
            return;
        }
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        // beside the spine's calendar button, kept on the screen
        let at = ctx
            .data(|d| d.get_temp::<egui::Pos2>(egui::Id::new("calendar-anchor")))
            .map(|p| egui::pos2(p.x + 8.0, p.y))
            .unwrap_or(egui::pos2(
                screen.left() + crate::spine::SPINE_W + 8.0,
                screen.top() + TOP_H,
            ));
        let at = egui::pos2(
            at.x.min(screen.right() - 478.0).max(screen.left() + 8.0),
            at.y,
        );
        let cal = snap
            .calendar
            .as_deref()
            .filter(|c| c.query == self.cal_query);
        let mut act = crate::calendar::CalAction::None;
        egui::Area::new(egui::Id::new("calendar-popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style())
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.control_border))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        act = crate::calendar::show(ui, pal, cal, &mut self.cal);
                    });
            });
        match act {
            crate::calendar::CalAction::Use { a, b, .. } => {
                self.cal_open = false;
                self.set_range(a, b);
            }
            crate::calendar::CalAction::Cancel => self.cal_open = false,
            crate::calendar::CalAction::None => {}
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cal_open = false;
        }
    }

    fn request_record(&mut self, snap: &Snapshot, idx: u64) {
        if let Some(p) = snap
            .page
            .as_ref()
            .filter(|p| p.generation == snap.generation)
        {
            if let Some(&row) = idx
                .checked_sub(p.start)
                .and_then(|k| p.rows.get(k as usize))
            {
                self.bridge.send(Cmd::Record(row));
            }
        }
    }

    /// The start screen: open a file on the left; on the right, the files
    /// opened before and the cases, each list whole (it scrolls, nothing is cut).
    fn start_screen(&mut self, ui: &mut egui::Ui, pal: &Palette, _snap: &Snapshot) {
        let full = ui.max_rect();
        let w = full.width().min(1120.0) - 48.0;
        let left = full.center().x - w * 0.5;
        let top = full.top() + (full.height() * 0.06).max(18.0);
        let p = ui.painter().clone();
        p.text(
            egui::pos2(left, top),
            Align2::LEFT_TOP,
            "Open a file",
            fonts::strong(22.0),
            pal.text,
        );
        let intro = p.layout(
            "A CSV, JSON or text log, an Excel workbook, a Windows event log, or a .gz or .zip of them, of any size. The file is only read, never changed.".to_string(),
            theme::ui_font(),
            pal.text2,
            w,
        );
        let intro_h = intro.size().y;
        p.galley(egui::pos2(left, top + 34.0), intro, pal.text2);
        let body_top = top + 34.0 + intro_h + 18.0;
        let foot_y = full.bottom() - 30.0;
        // drop zone
        let zone_w = (w * 0.46).max(340.0);
        let zone_h = (foot_y - 110.0 - body_top).clamp(170.0, 250.0);
        let zone = egui::Rect::from_min_size(egui::pos2(left, body_top), egui::vec2(zone_w, zone_h));
        let hovering_file = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let zresp = ui.interact(zone, ui.id().with("drop"), Sense::click());
        let edge = if hovering_file || zresp.hovered() {
            pal.accent_ink
        } else {
            pal.control_border
        };
        p.rect_filled(zone, CornerRadius::same(6), pal.surface);
        let pts = |a: egui::Pos2, b: egui::Pos2| {
            egui::Shape::dashed_line(&[a, b], Stroke::new(1.2, edge), 6.0, 5.0)
        };
        let z = zone.shrink(1.0);
        for s in [
            pts(z.left_top(), z.right_top()),
            pts(z.right_top(), z.right_bottom()),
            pts(z.right_bottom(), z.left_bottom()),
            pts(z.left_bottom(), z.left_top()),
        ] {
            p.extend(s);
        }
        p.text(
            zone.center() - egui::vec2(0.0, 50.0),
            Align2::CENTER_CENTER,
            ph::FILE_ARROW_DOWN,
            FontId::new(36.0, FontFamily::Proportional),
            pal.text2,
        );
        p.text(
            zone.center() - egui::vec2(0.0, 8.0),
            Align2::CENTER_CENTER,
            "Drop files here",
            fonts::strong(16.0),
            pal.text,
        );
        p.text(
            zone.center() + egui::vec2(0.0, 14.0),
            Align2::CENTER_CENTER,
            "one file, or several to see them as one view",
            theme::small_font(),
            pal.text2,
        );
        let btn = egui::Rect::from_center_size(
            zone.center() + egui::vec2(0.0, 54.0),
            egui::vec2(150.0, 32.0),
        );
        let open_clicked = ui
            .put(btn, |ui: &mut egui::Ui| {
                primary_button(ui, pal, "Open file   Ctrl+O")
            })
            .clicked();
        if open_clicked || zresp.clicked() {
            self.pick_file();
        }
        // one more way in, said in plain words under the drop zone
        let more = egui::Rect::from_min_size(
            egui::pos2(zone.left(), zone.bottom() + 12.0),
            egui::vec2(zone_w, 40.0),
        );
        let mut several = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(more), |ui| {
            several = ui
                .button(format!("{}  Open several files as one view", ph::STACK))
                .on_hover_text("Pick two or more files in the dialog: one set of rows, each row saying which file it came from")
                .clicked();
        });
        if several {
            self.pick_file();
        }

        // ── right: Recent, then Cases; each list is whole and scrolls ──
        let rx = left + zone_w + 36.0;
        let col = egui::Rect::from_min_max(
            egui::pos2(rx, body_top),
            egui::pos2(left + w, foot_y - 18.0),
        );
        let recent = self.prefs.recent.clone();
        let cases: Vec<(usize, String, usize)> = self
            .prefs
            .cases
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.name.clone(), c.files.len()))
            .collect();
        let mut chosen: Option<PathBuf> = None;
        let mut open_case: Option<usize> = None;
        let mut new_case = false;
        // a row of a list: lines that wrap whole, a hover fill behind them
        let row = |ui: &mut egui::Ui,
                   id: egui::Id,
                   icon: &str,
                   title: &str,
                   meta: &str,
                   meta_col: egui::Color32|
         -> bool {
            let bg = ui.painter().add(egui::Shape::Noop);
            let inner = ui
                .horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(icon)
                            .font(FontId::new(18.0, FontFamily::Proportional))
                            .color(pal.text2),
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let room = ui.available_width() - 8.0;
                        let wrap = |t: &str, font: FontId, c: egui::Color32| {
                            let mut job =
                                egui::text::LayoutJob::simple(t.to_string(), font, c, room);
                            job.wrap.break_anywhere = true;
                            job
                        };
                        ui.add_space(5.0);
                        ui.label(wrap(title, fonts::strong(13.5), pal.text));
                        ui.label(wrap(meta, theme::small_font(), meta_col));
                        ui.add_space(5.0);
                    });
                })
                .response;
            let rect = inner.rect.with_max_x(ui.max_rect().right());
            let resp = ui.interact(rect, id, Sense::click());
            if resp.hovered() {
                ui.painter().set(
                    bg,
                    egui::Shape::rect_filled(rect, CornerRadius::same(4), pal.surface2),
                );
            }
            ui.painter().line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                Stroke::new(1.0, pal.hairline),
            );
            resp.clicked()
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            // Recent takes what it needs, up to half the column when there are cases to show
            let half = (col.height() - 120.0).max(120.0) * if cases.is_empty() { 0.72 } else { 0.5 };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Recent").font(fonts::strong(15.0)));
                if recent.len() > 1 {
                    ui.label(
                        RichText::new(format!("{} files", recent.len())).color(pal.text2),
                    );
                }
            });
            ui.add_space(4.0);
            if recent.is_empty() {
                ui.label(RichText::new("Files you open appear here.").color(pal.text2));
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("start-recent")
                    .max_height(half)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for r in &recent {
                            let missing = !std::path::Path::new(&r.path).exists();
                            // where the user's file is; never the app's working copy
                            let place: String = ui.data_mut(|d| {
                                d.get_temp_mut_or_insert_with(
                                    egui::Id::new(("recent-where", &r.path)),
                                    || crate::book::where_from(std::path::Path::new(&r.path)),
                                )
                                .clone()
                            });
                            let meta = if missing {
                                format!("Not found · {place}")
                            } else {
                                format!("{} · {} · {place}", human_bytes(r.bytes), ago(r.opened))
                            };
                            let hit = row(
                                ui,
                                egui::Id::new(("recent", &r.path)),
                                ph::FILE_TEXT,
                                &r.name,
                                &meta,
                                if missing { pal.warning } else { pal.text2 },
                            );
                            if hit && !missing {
                                chosen = Some(PathBuf::from(&r.path));
                            }
                        }
                    });
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cases").font(fonts::strong(15.0)));
                if cases.len() > 1 {
                    ui.label(RichText::new(format!("{} cases", cases.len())).color(pal.text2));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(format!("{}  New case", ph::PLUS))
                        .on_hover_text("Name a case, then add its files or a whole folder")
                        .clicked()
                    {
                        new_case = true;
                    }
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new("A case keeps the files of one investigation together, a click apart. Files are only listed, never copied or changed.")
                        .color(pal.text2),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            if cases.is_empty() {
                ui.label(RichText::new("No cases yet.").color(pal.text2));
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("start-cases")
                    .max_height(ui.available_height().max(60.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, name, n) in &cases {
                            let meta = match n {
                                0 => "no files yet".to_string(),
                                1 => "1 file".to_string(),
                                n => format!("{} files", group(*n as u64)),
                            };
                            if row(
                                ui,
                                egui::Id::new(("case", i)),
                                ph::FOLDERS,
                                name,
                                &meta,
                                pal.text2,
                            ) {
                                open_case = Some(*i);
                            }
                        }
                    });
            }
        });
        if let Some(c) = chosen {
            self.open(c);
        }
        if let Some(i) = open_case {
            self.prefs.case = i;
            self.case_open = true;
        }
        if new_case {
            let n = self.prefs.cases.len() + 1;
            self.prefs.cases.push(crate::cases::Case {
                name: format!("Case {n}"),
                files: Vec::new(),
            });
            self.prefs.case = self.prefs.cases.len() - 1;
            self.case_note = None;
            self.case_skipped.clear();
            self.case_open = true;
        }
        // footer: where the indexes go
        p.line_segment(
            [egui::pos2(left, foot_y - 10.0), egui::pos2(left + w, foot_y - 10.0)],
            Stroke::new(1.0, pal.hairline),
        );
        // the folder in use is named, and can be changed right here
        let place = match &self.prefs.index_home {
            Some(d) => d.display().to_string(),
            None => "%LOCALAPPDATA%\\LogSeek\\cases".to_string(),
        };
        let foot = egui::Rect::from_min_max(
            egui::pos2(left, foot_y - 5.0),
            egui::pos2(left + w, full.bottom()),
        );
        let (mut choose, mut usual) = (false, false);
        ui.scope_builder(egui::UiBuilder::new().max_rect(foot), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Indexes are kept in {place}, apart from your files."
                        ))
                        .font(theme::small_font())
                        .color(pal.text2),
                    )
                    .wrap(),
                );
                choose = ui
                    .button(format!("{}  Keep indexes in another folder", ph::FOLDER_OPEN))
                    .on_hover_text("An index can be a fifth of its file's size or more. Choose a folder on a drive with room; files indexed before keep theirs where they are.")
                    .clicked();
                usual = self.prefs.index_home.is_some()
                    && ui.button("Use the usual folder again").clicked();
            });
        });
        let now = ui.input(|i| i.time);
        if choose {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Keep new indexes in this folder")
                .pick_folder()
            {
                let msg = if crate::bridge::index_home_usable(&dir) {
                    self.prefs.index_home = Some(dir.clone());
                    crate::bridge::set_index_home(Some(dir.clone()));
                    (format!("New indexes are kept in {}. Files indexed before keep theirs where they are.", dir.display()), false)
                } else {
                    (format!("{} cannot hold indexes (nothing can be written there); nothing changed.", dir.display()), true)
                };
                self.flash = Some((msg.0, now, msg.1));
            }
        }
        if usual {
            self.prefs.index_home = None;
            crate::bridge::set_index_home(None);
            self.flash = Some(("New indexes are kept in the app's own folder again.".to_string(), now, false));
        }
    }
}

/// The Fields and Tags drawer opens by itself only where the window is wide
/// enough to keep room for the rows beside it (a 13-inch screen keeps it closed).
pub(crate) fn drawer_open_by_default(width: f32) -> bool {
    width >= 1500.0
}

/// Why `new` cannot be the new name of made column `old` (among all `columns`).
pub(crate) fn rename_problem(new: &str, old: &str, columns: &[String]) -> Option<String> {
    let new = new.trim();
    if new.is_empty() {
        return Some("a name is needed".into());
    }
    columns
        .iter()
        .any(|c| c != old && c.eq_ignore_ascii_case(new))
        .then(|| format!("a column named {new} already exists"))
}

/// The window title: the open file first, then the app (one title, not two).
/// An analysis sheet that was closed, kept whole: "Back to …" in the status
/// bar puts it back as it was left (its column, its choices, its place).
pub(crate) enum ClosedSheet {
    List(crate::listwin::ListWin),
    Rhythm(crate::rhythmwin::RhythmWin),
    Pair(crate::pairwin::PairWin),
    Pattern(crate::patternwin::PatternWin),
    Strange(crate::strangewin::StrangeWin),
    Shape(crate::shapewin::ShapeWin),
    Explain(crate::explainwin::ExplainWin),
}

impl ClosedSheet {
    /// What the sheet is called, with the column it was about.
    pub(crate) fn name(&self) -> String {
        match self {
            ClosedSheet::List(_) => "Search a list".to_string(),
            ClosedSheet::Rhythm(_) => "Timeline signals".to_string(),
            ClosedSheet::Pair(w) => format!("Rare pairs for {}", w.a),
            ClosedSheet::Pattern(w) => format!("Patterns in {}", w.col),
            ClosedSheet::Strange(w) => format!("Strange text in {}", w.col),
            ClosedSheet::Shape(w) => format!("Shapes in {}", w.col),
            ClosedSheet::Explain(_) => "What sets these rows apart?".to_string(),
        }
    }
}

/// Where the list is scrolled to so a row that was kept in view sits a third
/// of the way down: the rows before it show above, the rows after it below.
pub(crate) fn top_for_kept_row(pos: u64, visible: u64) -> u64 {
    pos.saturating_sub(visible / 3)
}

/// What to try when a search finds no rows, said for the kind of search it
/// was: plain words, or terms that name a column.
fn no_rows_advice(query: &str) -> &'static str {
    let names_a_column = query.contains(['=', '<', '>', ':', '|']);
    if names_a_column {
        "No row matches all of this search. To see the values a column really holds, open the column's menu and choose Values and filter, or look in the Fields drawer."
    } else {
        "Plain words are looked for in every field and in the original text. Try fewer words or a shorter part of a word."
    }
}

pub(crate) fn window_title(file: Option<&str>) -> String {
    match file {
        Some(f) => format!("{f} — LogSeek Studio"),
        None => "LogSeek Studio".into(),
    }
}

/// What the rows a list search or a Follow found are called, on their chip.
fn shown_words(shown_as: Option<&str>, list: bool) -> String {
    match shown_as {
        Some(s) => s.to_string(),
        None if list => "rows with a keyword of the list".to_string(),
        None => "followed rows".to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rows_a_list_search_found_are_named_on_their_chip() {
        assert_eq!(
            super::shown_words(None, true),
            "rows with a keyword of the list"
        );
        assert_eq!(super::shown_words(None, false), "followed rows");
        assert_eq!(
            super::shown_words(Some("unusual rows"), true),
            "unusual rows"
        );
    }

    /// The time rail is open until folded, and a fold is remembered (settings
    /// saved before the fold existed open with the rail shown).
    #[test]
    fn the_time_rail_starts_open_and_a_fold_is_remembered() {
        let old: super::Prefs = serde_json::from_str(r#"{"dark":false}"#).unwrap();
        assert!(!old.spine_folded);
        let mut p = super::Prefs::default();
        p.spine_folded = true;
        let back: super::Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(back.spine_folded);
        // the rail's width either way; folded still leaves a way back
        assert_eq!(crate::spine::rail_width(false), crate::spine::SPINE_W);
        assert!(crate::spine::rail_width(true) > 0.0);
        assert!(crate::spine::rail_width(true) < crate::spine::SPINE_W / 3.0);
    }

    /// Screen readers (AccessKit) hear an icon button's name, not just "button".
    #[test]
    fn icon_buttons_tell_screen_readers_their_name() {
        let ctx = eframe::egui::Context::default();
        ctx.enable_accesskit();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            super::icon_button(ui, &crate::theme::DARK, "x", "Close the sheet");
        });
        out.textures_delta.clear();
        let upd = out
            .platform_output
            .accesskit_update
            .expect("an accessibility tree");
        let labels: Vec<String> = upd
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(str::to_string))
            .collect();
        // let go of the context first: a failing assert must not panic twice
        drop(ctx);
        assert!(labels.iter().any(|l| l == "Close the sheet"), "{labels:?}");
    }

    #[test]
    fn drawer_closed_by_default_on_13_inch() {
        // until the user opens or closes it: open where the screen has room
        assert!(!super::drawer_open_by_default(1366.0));
        assert!(!super::drawer_open_by_default(1440.0));
        assert!(super::drawer_open_by_default(1920.0));
        // a choice made sticks, and an older saved setting still reads
        let p: super::Prefs = serde_json::from_str(r#"{"fields_open": true}"#).unwrap();
        assert_eq!(p.fields_open, Some(true));
        let p: super::Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p.fields_open, None);
    }
    #[test]
    fn a_new_name_for_a_made_column_is_checked_while_typed() {
        let cols: Vec<String> = ["Time", "Size", "size_kb", "size_mb"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(super::rename_problem("kb", "size_kb", &cols), None);
        // the same name in another case is fine (it is still this column)
        assert_eq!(super::rename_problem("Size_KB", "size_kb", &cols), None);
        assert_eq!(
            super::rename_problem("SIZE", "size_kb", &cols).as_deref(),
            Some("a column named SIZE already exists")
        );
        assert_eq!(
            super::rename_problem("size_mb", "size_kb", &cols).as_deref(),
            Some("a column named size_mb already exists")
        );
        assert_eq!(
            super::rename_problem("  ", "size_kb", &cols).as_deref(),
            Some("a name is needed")
        );
    }
    #[test]
    fn made_column_changes_name_what_was_added_and_removed() {
        let v = |n: &[&str]| -> Vec<(String, String)> {
            n.iter().map(|x| (x.to_string(), "f".to_string())).collect()
        };
        assert_eq!(
            super::column_changes(&v(&["a"]), &v(&["a", "is_tor"])),
            (vec!["is_tor".to_string()], vec![])
        );
        assert_eq!(
            super::column_changes(&v(&["a", "b"]), &v(&["b"])),
            (vec![], vec!["a".to_string()])
        );
        assert_eq!(
            super::column_changes(&v(&["a"]), &v(&["a"])),
            (vec![], vec![])
        );
    }

    #[test]
    fn the_find_line_counts_in_words() {
        assert_eq!(super::find_line(0, None), "No row contains it");
        assert_eq!(super::find_line(1, Some(1)), "1 row contains it");
        assert_eq!(
            super::find_line(1204, Some(3)),
            "3 of 1,204 rows that contain it"
        );
        assert_eq!(super::find_line(1204, None), "1,204 rows contain it");
    }

    #[test]
    fn a_closed_sheet_is_named_with_its_column() {
        let p = ClosedSheet::Pattern(crate::patternwin::PatternWin::new("CommandLine".into()));
        assert_eq!(p.name(), "Patterns in CommandLine");
    }

    #[test]
    fn a_kept_row_sits_a_third_of_the_way_down() {
        // 30 rows in view: ten rows of what came before show above it
        assert_eq!(top_for_kept_row(500, 30), 490);
        // near the start there is nothing above to show
        assert_eq!(top_for_kept_row(4, 30), 0);
    }

    #[test]
    fn window_title_names_the_file_once() {
        assert_eq!(
            super::window_title(Some("web-10m.csv")),
            "web-10m.csv — LogSeek Studio"
        );
        assert_eq!(super::window_title(None), "LogSeek Studio");
    }
    #[test]
    fn a_field_inside_a_column_is_named_column_dot_field() {
        let cols = |n: &[&str]| -> Vec<String> { n.iter().map(|s| s.to_string()).collect() };
        // a field inside a column: column.field, so two JSON columns never share a name
        assert_eq!(
            field_column_name("user", "request", &cols(&["time"]), false),
            "request.user"
        );
        assert_eq!(
            field_column_name("user", "response", &cols(&["time", "request.user"]), false),
            "response.user"
        );
        // inside _raw (the whole line): the field's own name
        assert_eq!(
            field_column_name("Image", "_raw", &cols(&["time"]), false),
            "Image"
        );
        // already there (ignoring case, as the engine matches names): numbered
        assert_eq!(
            field_column_name("user", "response", &cols(&["Response.User"]), false),
            "response.user (2)"
        );
        assert_eq!(
            field_column_name("Image", "_raw", &cols(&["image"]), false),
            "Image (2)"
        );
    }

    #[test]
    fn made_column_names_never_clash_ignoring_case() {
        assert_eq!(unique_name("User", &["user".to_string()]), "User (2)");
        assert_eq!(unique_name("Image", &["host".to_string()]), "Image");
    }

    use super::*;

    #[test]
    fn dragged_range_round_trips_through_the_query_text() {
        let a = 1_472_063_259_000_000; // 2016-08-24T18:27:39Z
        let q = format!("@{}..{} host=host-a01", iso(a), iso(a + 3_600_000_000));
        assert_eq!(
            q,
            "@2016-08-24T18:27:39Z..2016-08-24T19:27:39Z host=host-a01"
        );
        let (x, y, rest) = leading_range(&q).unwrap();
        assert_eq!(
            (x, y, rest.trim()),
            (a, a + 3_600_000_000, "host=host-a01")
        );
        assert!(
            logseek_core::query::parse(&q).is_ok(),
            "the engine accepts what the bar writes"
        );
        assert!(leading_range("host=x").is_none());
    }
}
