//! Opening a workbook (Excel, OpenDocument): which sheets it has, and
//! reading the ones picked. Each sheet opens as its own tab. The reading
//! happens on its own thread, so the window never waits.

use crate::moment::group;
use crate::theme::Palette;
use crossbeam_channel::{unbounded, Receiver};
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use logseek_core::ingest::evtx;
use logseek_core::ingest::jsondoc;
use logseek_core::ingest::packed;
use logseek_core::ingest::sheets::{self, SheetInfo, SheetOut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// A sheet read and ready to open: where its rows are, its name, what was found.
pub struct Ready {
    pub path: PathBuf,
    pub sheet: String,
    pub out: SheetOut,
    /// Event log: events that could not be read (damaged parts of the log).
    pub damaged: u64,
}

/// A file read through here before it opens: a workbook or a Windows event log.
pub fn handles(p: &Path) -> bool {
    sheets::is_workbook(p) || evtx::is_evtx(p) || packed::is_packed(p) || is_json_doc(p)
}

/// A JSON file bigger than this is read the usual way (as rows), not looked
/// through for parts first: looking means reading it once from end to end.
const DOC_LOOK_MAX: u64 = 256 << 20;

/// A JSON file that is one object holding several lists, opened part by
/// part. The answer is kept for the file as it is now (its size and time of
/// last change), so asking again costs nothing.
pub fn is_json_doc(p: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static SEEN: OnceLock<Mutex<HashMap<Key, bool>>> = OnceLock::new();
    let json = p
        .extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("json"));
    if !json {
        return false;
    }
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if meta.len() > DOC_LOOK_MAX {
        return false;
    }
    let key = (p.to_path_buf(), meta.len(), meta.modified().ok());
    let seen = SEEN.get_or_init(Default::default);
    if let Some(known) = seen.lock().ok().and_then(|m| m.get(&key).copied()) {
        return known;
    }
    let is = jsondoc::is_doc(p);
    if let Ok(mut m) = seen.lock() {
        m.insert(key, is);
    }
    is
}

/// "1.9 GB", "12 MB", "640 KB".
pub fn size_words(b: u64) -> String {
    const K: f64 = 1024.0;
    let f = b as f64;
    if f >= K * K * K {
        format!("{:.1} GB", f / (K * K * K))
    } else if f >= K * K {
        format!("{:.0} MB", f / (K * K))
    } else {
        format!("{:.0} KB", (f / K).max(1.0))
    }
}

/// What unpacking needs against what the disk has: `Err` says so in words.
pub fn room_for(need: u64, free: Option<u64>) -> Result<(), String> {
    match free {
        Some(f) if need > f => Err(format!(
            "unpacking needs {} of free disk space and only {} is free",
            size_words(need),
            size_words(f)
        )),
        _ => Ok(()),
    }
}

/// "1,204 damaged events could not be read and were left out".
pub fn damaged_line(n: u64) -> Option<String> {
    (n > 0).then(|| {
        format!(
            "{} damaged {} could not be read and {} left out",
            group(n),
            if n == 1 { "event" } else { "events" },
            if n == 1 { "was" } else { "were" }
        )
    })
}

enum Msg {
    Sheets(Result<Vec<SheetInfo>, String>),
    Started(String),
    Sheet(Result<Ready, String>),
    Finished,
}

enum Phase {
    Listing,
    Pick(Vec<SheetInfo>),
    Reading { total: usize, now: String },
    Failed(String),
}

pub enum BookAction {
    None,
    Close,
    Open(Vec<Ready>),
}

pub struct Book {
    pub path: PathBuf,
    phase: Phase,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    ready: Vec<Ready>,
    /// Open the one sheet with this name without asking (dev switch, or one sheet only).
    auto: Option<String>,
}

/// A sheet name as part of a file name.
pub fn safe_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = t.trim().trim_end_matches('.').to_string();
    if t.is_empty() {
        "sheet".into()
    } else {
        t
    }
}

/// Where a sheet's rows are kept: a folder per workbook as it is now (its
/// size and time of last change), so a changed workbook is read again.
pub fn rows_path(book: &Path, sheet: &str) -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let canon = std::fs::canonicalize(book).unwrap_or_else(|_| book.to_path_buf());
    let meta = std::fs::metadata(book).ok();
    let stamp = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    let len = meta.map_or(0, |m| m.len());
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon
        .to_string_lossy()
        .bytes()
        .chain(stamp.to_le_bytes())
        .chain(len.to_le_bytes())
        // how sheets are written: raise it when that changes, so old copies are not reused
        .chain(3u64.to_le_bytes())
    {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let name = book
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    base.join("LogSeek")
        .join("sheets")
        .join(format!("{h:016x}"))
        .join(format!("{name} · {}", safe_name(sheet)))
}

/// Kept in the folder of a book's rows: the book they were read from.
fn origin_note(rows: &Path) -> Option<PathBuf> {
    rows.parent().map(|d| d.join("from"))
}

fn write_origin(rows: &Path, book: &Path) {
    if let Some(note) = origin_note(rows) {
        let full = std::fs::canonicalize(book).unwrap_or_else(|_| book.to_path_buf());
        // the plain path, without the "\\?\" Windows puts before a long one
        let s = full.to_string_lossy();
        let _ = std::fs::write(note, s.strip_prefix(r"\\?\").unwrap_or(&s));
    }
}

/// Rows kept in the app's own working folder (read out of a book), told by
/// where they are: `LogSeek/sheets/<a folder per book>/<book · sheet>`.
fn is_working_copy(rows: &Path) -> bool {
    let up = |p: &Path, n: usize| p.ancestors().nth(n).and_then(|a| a.file_name().map(|f| f.to_owned()));
    up(rows, 2).is_some_and(|f| f == "sheets") && up(rows, 3).is_some_and(|f| f == "LogSeek")
}

/// The book a file of rows was read from, and the name inside it (a sheet,
/// or a file of a packed file). None for a file that is not such a copy.
pub fn origin(rows: &Path) -> Option<(PathBuf, String)> {
    let text = std::fs::read_to_string(origin_note(rows)?).ok()?;
    let book = PathBuf::from(text.trim());
    let name = rows.file_name()?.to_string_lossy().into_owned();
    let book_name = book.file_name()?.to_string_lossy().into_owned();
    let inner = name.strip_prefix(&format!("{book_name} · "))?.to_string();
    Some((book, inner))
}

/// The name inside its book of rows kept in the working folder ("access.jsonl"
/// of "logs.zip · access.jsonl"), told from the name alone.
pub fn inner_name(rows: &Path) -> Option<String> {
    if !is_working_copy(rows) {
        return None;
    }
    let name = rows.file_name()?.to_string_lossy().into_owned();
    name.split_once(" · ").map(|(_, inner)| inner.to_string())
}

/// Where a file is, in words for a list: its own path, or for rows read out
/// of a book "sheet inside the book's path". The app's working folder is
/// never shown: it is not where the user's file is.
pub fn where_from(path: &Path) -> String {
    if let Some((book, inner)) = origin(path) {
        return format!("{inner} inside {}", book.display());
    }
    if is_working_copy(path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some((book, inner)) = name.split_once(" · ") {
            return format!("{inner} inside {book}");
        }
    }
    path.display().to_string()
}

/// A book that can hold more than one thing to open (a zip, a workbook);
/// a gzip file and an event log hold one.
pub fn holds_many(book: &Path) -> bool {
    let ext = book
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    handles(book) && ext != "gz" && !evtx::is_evtx(book)
}

/// "1,204 rows · 12 columns", or what is known of it.
pub fn size_line(s: &SheetInfo) -> String {
    if let Some(b) = s.bytes {
        return size_words(b);
    }
    match (s.rows, s.cols) {
        (Some(r), None) => format!("{} {}", group(r), if r == 1 { "row" } else { "rows" }),
        (Some(r), Some(c)) => format!(
            "{} {} · {} {}",
            group(r),
            if r == 1 { "row" } else { "rows" },
            group(c),
            if c == 1 { "column" } else { "columns" }
        ),
        _ => String::new(),
    }
}

/// What to say about a sheet's formula cells once it is read.
pub fn formula_line(sheet: &str, o: &SheetOut) -> Option<String> {
    if o.formulas == 0 {
        return None;
    }
    let cells = |n: u64| if n == 1 { "cell" } else { "cells" };
    let mut s = format!(
        "{sheet}: {} formula {} shown as the results saved in the workbook",
        group(o.formulas),
        cells(o.formulas)
    );
    if o.formulas_without_result > 0 {
        s.push_str(&format!(
            "; {} of them {} no saved result and {} empty (open the workbook in Excel and save it to fill them)",
            group(o.formulas_without_result),
            if o.formulas_without_result == 1 { "has" } else { "have" },
            if o.formulas_without_result == 1 { "is" } else { "are" },
        ));
    }
    Some(s)
}

fn info_path(rows: &Path) -> PathBuf {
    let mut s = rows.as_os_str().to_os_string();
    s.push(".info");
    PathBuf::from(s)
}

/// What to say about rows above a sheet's column names that only title it.
pub fn title_line(sheet: &str, o: &SheetOut) -> Option<String> {
    if o.title.is_empty() {
        return None;
    }
    let quoted: Vec<String> = o.title.iter().map(|t| format!("\"{t}\"")).collect();
    let rows = if o.title.len() == 1 {
        "the row above the column names is".to_string()
    } else {
        format!("the {} rows above the column names are", o.title.len())
    };
    Some(format!(
        "{sheet}: {rows} a title, not data: {}",
        quoted.join(" / ")
    ))
}

/// For the check before opening a sheet's rows: its title, when it has one.
pub fn title_sentence(rows: &Path) -> Option<String> {
    let (o, _) = read_info(rows)?;
    if o.title.is_empty() {
        return None;
    }
    let quoted: Vec<String> = o.title.iter().map(|t| format!("\"{t}\"")).collect();
    Some(format!(
        "Above the column names the sheet has a title, not read as rows: {}",
        quoted.join(" / ")
    ))
}

/// How a sheet's rows are written; rows written another way are read again.
/// 2: a title band above the column names is kept apart from the rows.
const ROWS_FORMAT: u64 = 2;

/// Kept beside a sheet's rows: the counts and the format on the first line,
/// then the title rows.
fn write_info(rows: &Path, o: &SheetOut, damaged: u64) {
    let mut s = format!(
        "{} {} {} {} {damaged} {ROWS_FORMAT}",
        o.rows, o.cols, o.formulas, o.formulas_without_result
    );
    for t in &o.title {
        s.push('\n');
        s.push_str(&t.replace(['\r', '\n'], " "));
    }
    let _ = std::fs::write(info_path(rows), s);
}

fn read_info(rows: &Path) -> Option<(SheetOut, u64)> {
    let t = std::fs::read_to_string(info_path(rows)).ok()?;
    let mut lines = t.lines();
    let mut it = lines
        .next()?
        .split_whitespace()
        .map(|x| x.parse::<u64>().ok());
    let o = SheetOut {
        rows: it.next()??,
        cols: it.next()??,
        formulas: it.next()??,
        formulas_without_result: it.next()??,
        title: lines.map(str::to_string).collect(),
    };
    let damaged = it.next()??;
    (it.next()?? == ROWS_FORMAT).then_some((o, damaged))
}

/// Read one sheet (or the event log) into `out`.
fn convert(
    book: &Path,
    sheet: &str,
    out: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<(SheetOut, u64), String> {
    if packed::is_packed(book) {
        // room on the disk first, when the packed file says how big it gets
        let need = packed::entries(book)?
            .into_iter()
            .find(|e| e.name == sheet)
            .and_then(|e| e.bytes);
        if let Some(need) = need {
            room_for(need, out.parent().and_then(logseek_core::sysmem::disk_free))?;
        }
        packed::unpack(book, sheet, out, cancel, done)?;
        return Ok((SheetOut::default(), 0));
    }
    if is_json_doc(book) {
        let rows = jsondoc::write_part(book, sheet, out, cancel, done)?;
        let o = SheetOut {
            rows,
            ..Default::default()
        };
        return Ok((o, 0));
    }
    if evtx::is_evtx(book) {
        let e = evtx::evtx_to_jsonl(book, out, cancel, done)?;
        let o = SheetOut {
            rows: e.events,
            ..Default::default()
        };
        return Ok((o, e.damaged));
    }
    sheets::sheet_to_csv(book, sheet, out, cancel, done).map(|o| (o, 0))
}

impl Book {
    /// Start looking at a workbook. `only`: open that sheet without asking.
    pub fn start(path: PathBuf, only: Option<String>) -> Self {
        let (tx, rx) = unbounded();
        let p = path.clone();
        std::thread::spawn(move || {
            // an event log is one list of events
            let list = if packed::is_packed(&p) {
                packed::entries(&p).map(|l| {
                    l.into_iter()
                        .map(|e| SheetInfo {
                            name: e.name,
                            hidden: false,
                            rows: None,
                            cols: None,
                            bytes: e.bytes,
                        })
                        .collect()
                })
            } else if is_json_doc(&p) {
                jsondoc::parts(&p).map(|l| {
                    l.into_iter()
                        .map(|part| SheetInfo {
                            name: part.name,
                            hidden: false,
                            rows: Some(part.rows),
                            cols: None,
                            bytes: None,
                        })
                        .collect()
                })
            } else if evtx::is_evtx(&p) {
                Ok(vec![SheetInfo {
                    name: "events".into(),
                    hidden: false,
                    rows: None,
                    cols: None,
                    bytes: None,
                }])
            } else {
                sheets::sheets(&p)
            };
            let _ = tx.send(Msg::Sheets(list));
        });
        Self {
            path,
            phase: Phase::Listing,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            ready: Vec::new(),
            auto: only,
        }
    }

    fn read(&mut self, names: Vec<String>) {
        let (tx, rx) = unbounded();
        self.rx = rx;
        self.ready.clear();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.done = Arc::new(AtomicU64::new(0));
        self.phase = Phase::Reading {
            total: names.len(),
            now: names.first().cloned().unwrap_or_default(),
        };
        let (book, cancel, done) = (self.path.clone(), self.cancel.clone(), self.done.clone());
        std::thread::spawn(move || {
            for sheet in names {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                done.store(0, Ordering::Relaxed);
                let _ = tx.send(Msg::Started(sheet.clone()));
                let out = rows_path(&book, &sheet);
                if let Some(d) = out.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                // where these rows are from, for every list that names them
                write_origin(&out, &book);
                // read before and the workbook has not changed since: nothing to do
                let res = match read_info(&out).filter(|_| out.exists()) {
                    Some(o) => Ok(o),
                    None => {
                        if let Some(d) = out.parent() {
                            let _ = std::fs::create_dir_all(d);
                        }
                        convert(&book, &sheet, &out, &cancel, &done)
                            .inspect(|(o, bad)| write_info(&out, o, *bad))
                    }
                };
                let _ = tx.send(Msg::Sheet(
                    res.map(|(o, damaged)| Ready {
                        path: out,
                        sheet: sheet.clone(),
                        out: o,
                        damaged,
                    })
                    .map_err(|e| format!("{sheet}: {e}")),
                ));
            }
            let _ = tx.send(Msg::Finished);
        });
    }

    /// Read every sheet (when the list is on screen). Says whether it started.
    pub fn pick_all(&mut self) -> bool {
        let Phase::Pick(list) = &self.phase else {
            return false;
        };
        let names = list.iter().map(|s| s.name.clone()).collect();
        self.read(names);
        true
    }

    pub fn show(&mut self, ctx: &egui::Context, pal: &Palette) -> BookAction {
        let mut act = BookAction::None;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Sheets(Err(e)) => self.phase = Phase::Failed(e),
                Msg::Sheets(Ok(list)) => {
                    let one = match (&self.auto, list.len()) {
                        (Some(w), _) if w == "*" => None,
                        (Some(w), _) => list.iter().find(|s| &s.name == w).map(|s| s.name.clone()),
                        (None, 1) => Some(list[0].name.clone()),
                        _ => None,
                    };
                    match one {
                        Some(name) => self.read(vec![name]),
                        None if list.is_empty() => {
                            self.phase = Phase::Failed("this workbook has no sheets".into())
                        }
                        None => self.phase = Phase::Pick(list),
                    }
                }
                Msg::Started(s) => {
                    if let Phase::Reading { now, .. } = &mut self.phase {
                        *now = s;
                    }
                }
                Msg::Sheet(Ok(r)) => self.ready.push(r),
                Msg::Sheet(Err(e)) => {
                    if !self.cancel.load(Ordering::Relaxed) {
                        self.phase = Phase::Failed(e);
                    }
                }
                Msg::Finished => {
                    if matches!(self.phase, Phase::Reading { .. }) {
                        return if self.ready.is_empty() {
                            BookAction::Close
                        } else {
                            BookAction::Open(std::mem::take(&mut self.ready))
                        };
                    }
                }
            }
        }
        let screen = ctx.content_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("book-scrim"),
        ))
        .rect_filled(
            screen,
            0.0,
            egui::Color32::from_black_alpha(if pal.dark { 120 } else { 70 }),
        );
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let w = (screen.width() - 32.0).min(640.0);
        let mut read: Option<Vec<String>> = None;
        egui::Window::new("workbook-sheets")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .default_width(w)
            .max_height(screen.height() - 48.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(w);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(if evtx::is_evtx(&self.path) {
                            "Windows event log"
                        } else if packed::is_packed(&self.path) {
                            "Packed file"
                        } else if is_json_doc(&self.path) {
                            "JSON file with several lists"
                        } else {
                            "Workbook"
                        })
                        .font(crate::fonts::strong(16.0)),
                    );
                    ui.label(RichText::new(&name).color(pal.text2));
                });
                ui.add_space(6.0);
                match &self.phase {
                    Phase::Listing => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Looking at what is inside");
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    Phase::Failed(e) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{}  This file could not be read: {e}",
                                    ph::WARNING
                                ))
                                .color(pal.danger),
                            )
                            .wrap(),
                        );
                    }
                    Phase::Reading { total, now } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spinner();
                            let k = self.ready.len() + 1;
                            ui.label(if *total > 1 {
                                format!("Reading {k} of {total}: {now}")
                            } else {
                                format!("Reading {now}")
                            });
                        });
                        ui.label(
                            RichText::new(if packed::is_packed(&self.path) {
                                format!(
                                    "{} unpacked so far",
                                    size_words(self.done.load(Ordering::Relaxed))
                                )
                            } else {
                                format!("{} rows so far", group(self.done.load(Ordering::Relaxed)))
                            })
                            .color(pal.text2),
                        );
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    Phase::Pick(list) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(if packed::is_packed(&self.path) {
                                    format!(
                                        "It holds {} files. Each opens as its own tab; click one, or open them all.",
                                        group(list.len() as u64)
                                    )
                                } else if is_json_doc(&self.path) {
                                    format!(
                                        "This file is one object with {} parts in it, not a list of rows. Each part opens as its own tab; click one, or open them all.",
                                        group(list.len() as u64)
                                    )
                                } else {
                                    format!(
                                        "It has {} sheets. Each sheet opens as its own tab; click one, or open them all.",
                                        group(list.len() as u64)
                                    )
                                })
                                .color(pal.text),
                            )
                            .wrap(),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(if packed::is_packed(&self.path) {
                                    let need: u64 = list.iter().filter_map(|s| s.bytes).sum();
                                    let free = rows_path(&self.path, "x")
                                        .ancestors()
                                        .find(|p| p.exists())
                                        .and_then(logseek_core::sysmem::disk_free);
                                    format!(
                                        "A file is unpacked to a working copy before it is read: all of them need {} of disk space{}. The packed file is never changed.",
                                        size_words(need),
                                        free.map_or(String::new(), |f| format!(
                                            ", and {} is free",
                                            size_words(f)
                                        ))
                                    )
                                } else if is_json_doc(&self.path) {
                                    "A list gives one row for each item in it; an object gives one row. What is nested inside an item becomes columns, and a list inside an item stays in its cell, where it can be made into columns. The file is never changed.".to_string()
                                } else {
                                    "Formula cells show the result saved in the workbook. LogSeek does not recalculate them, and the workbook is never changed.".to_string()
                                })
                                .color(pal.text2),
                            )
                            .wrap(),
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height((screen.height() - 260.0).max(120.0))
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for s in list {
                                    ui.horizontal_wrapped(|ui| {
                                        if ui
                                            .link(
                                                RichText::new(format!("{}  {}", ph::TABLE, s.name))
                                                    .strong(),
                                            )
                                            .on_hover_text("Open this one as a tab")
                                            .clicked()
                                        {
                                            read = Some(vec![s.name.clone()]);
                                        }
                                        ui.label(RichText::new(size_line(s)).color(pal.text2));
                                        if s.hidden {
                                            ui.label(
                                                RichText::new("hidden in Excel")
                                                    .color(pal.accent_ink),
                                            );
                                        }
                                    });
                                }
                            });
                        ui.add_space(8.0);
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if let Phase::Pick(list) = &self.phase {
                        if ui
                            .button(format!(
                                "{}  Open all {}",
                                ph::TABLE,
                                group(list.len() as u64)
                            ))
                            .clicked()
                        {
                            read = Some(list.iter().map(|s| s.name.clone()).collect());
                        }
                    }
                    let word = if matches!(self.phase, Phase::Reading { .. }) {
                        "Stop"
                    } else {
                        "Close"
                    };
                    if ui.button(word).clicked()
                        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                        act = BookAction::Close;
                    }
                });
            });
        if let Some(names) = read {
            self.read(names);
        }
        act
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_names_become_file_names() {
        assert_eq!(safe_name("Q1/Q2: totals?"), "Q1_Q2_ totals_");
        assert_eq!(safe_name("  "), "sheet");
        assert_eq!(safe_name("Logons"), "Logons");
    }

    #[test]
    fn formula_cells_are_explained_only_when_there_are_some() {
        let mut o = SheetOut::default();
        assert_eq!(formula_line("Logons", &o), None);
        o.formulas = 1204;
        assert_eq!(
            formula_line("Logons", &o).unwrap(),
            "Logons: 1,204 formula cells shown as the results saved in the workbook"
        );
        o.formulas_without_result = 1;
        assert!(formula_line("Logons", &o)
            .unwrap()
            .ends_with("1 of them has no saved result and is empty (open the workbook in Excel and save it to fill them)"));
    }

    #[test]
    fn a_title_above_the_column_names_is_said_in_full() {
        let mut o = SheetOut::default();
        assert_eq!(title_line("Timeline", &o), None);
        o.title = vec!["Hindsight - Timeline · URL Visit Specific".into()];
        assert_eq!(
            title_line("Timeline", &o).unwrap(),
            "Timeline: the row above the column names is a title, not data: \"Hindsight - Timeline · URL Visit Specific\""
        );
        o.title.push("Prepared 2026-11-19".into());
        assert_eq!(
            title_line("Timeline", &o).unwrap(),
            "Timeline: the 2 rows above the column names are a title, not data: \"Hindsight - Timeline · URL Visit Specific\" / \"Prepared 2026-11-19\""
        );
    }

    #[test]
    fn the_check_before_opening_names_a_sheets_title() {
        let d = tempfile::tempdir().unwrap();
        let rows = d.path().join("Timeline.csv");
        assert_eq!(title_sentence(&rows), None);
        let o = SheetOut {
            rows: 3,
            cols: 2,
            title: vec!["Report - Timeline · Visit specific".into()],
            ..Default::default()
        };
        write_info(&rows, &o, 0);
        assert_eq!(
            title_sentence(&rows).unwrap(),
            "Above the column names the sheet has a title, not read as rows: \"Report - Timeline · Visit specific\""
        );
    }

    #[test]
    fn what_was_read_of_a_sheet_is_kept_with_its_title() {
        let d = tempfile::tempdir().unwrap();
        let rows = d.path().join("Timeline.csv");
        let o = SheetOut {
            rows: 31_730,
            cols: 34,
            formulas: 2,
            formulas_without_result: 1,
            title: vec!["Report · Visit specific".into(), "Second line".into()],
        };
        write_info(&rows, &o, 3);
        assert_eq!(read_info(&rows), Some((o, 3)));
        // written before titles were kept apart: read the sheet again
        std::fs::write(info_path(&rows), "31730 34 2 1 3").unwrap();
        assert_eq!(read_info(&rows), None);
    }

    #[test]
    fn rows_read_out_of_a_packed_file_say_where_that_file_is() {
        let d = tempfile::tempdir().unwrap();
        let folder = d.path().join("LogSeek").join("sheets").join("fe5e6bb6");
        std::fs::create_dir_all(&folder).unwrap();
        let rows = folder.join("logs.zip · access.jsonl");
        // nothing recorded yet: the names are said, the working folder is not
        assert_eq!(origin(&rows), None);
        assert_eq!(where_from(&rows), "access.jsonl inside logs.zip");
        let zip = d.path().join("logs.zip");
        std::fs::write(&zip, b"x").unwrap();
        write_origin(&rows, &zip);
        let (book, inner) = origin(&rows).unwrap();
        assert_eq!(book.file_name().unwrap(), "logs.zip");
        assert_eq!(inner, "access.jsonl");
        let said = where_from(&rows);
        assert!(said.starts_with("access.jsonl inside "), "{said}");
        assert!(said.ends_with("logs.zip"), "{said}");
        assert!(!said.contains("sheets") && !said.contains(r"\\?\"), "{said}");
    }

    #[test]
    fn a_working_copy_is_named_by_what_it_is_inside_its_book() {
        let rows = Path::new("x").join("LogSeek").join("sheets").join("ab12").join("logs.zip · access.jsonl");
        assert_eq!(inner_name(&rows).as_deref(), Some("access.jsonl"));
        assert_eq!(inner_name(Path::new("C:/data/logs.zip · access.jsonl")), None);
    }

    #[test]
    fn a_file_of_the_users_own_says_its_own_place() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("events.csv");
        assert_eq!(origin(&p), None);
        assert_eq!(where_from(&p), p.display().to_string());
    }

    #[test]
    fn a_json_file_of_several_lists_is_opened_part_by_part() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("resume.json");
        std::fs::write(
            &p,
            "{\n \"basics\": {\"name\": \"Mina\"},\n \"work\": [{\"company\": \"Northwind\"}, {\"company\": \"Contoso\"}]\n}",
        )
        .unwrap();
        assert!(is_json_doc(&p) && handles(&p) && holds_many(&p));
        // rows already (one record per line) are read the usual way
        let rows = d.path().join("rows.json");
        std::fs::write(&rows, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        assert!(!is_json_doc(&rows) && !handles(&rows));
        // one part read out of it
        let out = d.path().join("work.rows");
        let (o, _) = convert(&p, "work", &out, &AtomicBool::new(false), &AtomicU64::new(0)).unwrap();
        assert_eq!(o.rows, 2);
        assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 2);
    }

    #[test]
    fn a_zip_and_a_workbook_can_hold_more_to_open() {
        assert!(holds_many(Path::new("a.zip")) && holds_many(Path::new("b.XLSX")));
        assert!(!holds_many(Path::new("a.json.gz")));
        assert!(!holds_many(Path::new("a.evtx")));
        assert!(!holds_many(Path::new("a.csv")));
    }

    #[test]
    fn damaged_events_are_counted_in_words() {
        assert_eq!(damaged_line(0), None);
        assert_eq!(
            damaged_line(1).unwrap(),
            "1 damaged event could not be read and was left out"
        );
        assert_eq!(
            damaged_line(1204).unwrap(),
            "1,204 damaged events could not be read and were left out"
        );
        assert!(handles(Path::new("a.EVTX")) && handles(Path::new("b.xlsx")));
        assert!(!handles(Path::new("c.csv")));
    }

    #[test]
    fn sizes_and_disk_room_are_said_in_words() {
        assert_eq!(size_words(1_904_104_568), "1.8 GB");
        assert_eq!(size_words(12 << 20), "12 MB");
        assert_eq!(size_words(10), "1 KB");
        assert_eq!(room_for(5 << 30, Some(6 << 30)), Ok(()));
        assert_eq!(room_for(5 << 30, None), Ok(()));
        assert_eq!(
            room_for(5 << 30, Some(1 << 30)).unwrap_err(),
            "unpacking needs 5.0 GB of free disk space and only 1.0 GB is free"
        );
        assert!(handles(Path::new("x.json.gz")) && handles(Path::new("y.ZIP")));
    }

    #[test]
    fn a_sheet_says_its_size_when_known() {
        let mut s = SheetInfo {
            name: "a".into(),
            hidden: false,
            rows: Some(1204),
            cols: Some(1),
            bytes: None,
        };
        assert_eq!(size_line(&s), "1,204 rows · 1 column");
        s.rows = None;
        assert_eq!(size_line(&s), "");
    }

    #[test]
    fn a_changed_workbook_gets_a_new_place() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.xlsx");
        std::fs::write(&p, b"one").unwrap();
        let a = rows_path(&p, "S");
        std::fs::write(&p, b"longer now").unwrap();
        assert_ne!(a, rows_path(&p, "S"));
        assert!(a.ends_with("b.xlsx · S"));
    }
}
