//! Excel and OpenDocument workbooks. A workbook is a set of sheets; each
//! sheet is turned into rows (a CSV file) that the usual import reads, so
//! search, filters, sorting and everything else work on a sheet as on any
//! other file.
//!
//! Formula cells: a workbook stores, beside each formula, the result the
//! spreadsheet program calculated last. That saved result is what is read;
//! nothing is recalculated here.

use super::packed::{PartGuard, Room};
use calamine::{Data, DataRef, ExcelDateTime, Reader};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The most rows and columns a workbook can hold (Excel's own limits). A cell
/// or a declared width beyond them is not a spreadsheet's, and is refused.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;

/// Most cells an .xlsb sheet may spread over (the rows between its first and
/// last cell times the columns) before it is read whole. Past this the sheet
/// is refused with a message rather than running the machine out of memory.
const DENSE_CELLS_MAX: u64 = 20_000_000;

/// Stands for "a cell lies left of the declared first column": the sheet is
/// then read again from column A, so no cell shares a column with another.
const LEFT_OF_DECLARED: &str = "\u{1}left of the declared first column";

fn beyond_workbook(row: u32, col: u32) -> String {
    format!(
        "this sheet has a cell at row {} and column {}, beyond what a workbook can hold ({} rows, {} columns); the file is damaged or was not made by a spreadsheet program",
        u64::from(row) + 1,
        u64::from(col) + 1,
        MAX_ROWS,
        MAX_COLS
    )
}

fn stopped_or(e: String) -> std::io::Error {
    std::io::Error::other(e)
}

/// File kinds read as workbooks.
pub const KINDS: [&str; 5] = ["xlsx", "xlsm", "xlsb", "xls", "ods"];

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_workbook(p: &Path) -> bool {
    KINDS.contains(&ext_of(p).as_str())
}

#[derive(Clone, Debug)]
pub struct SheetInfo {
    pub name: String,
    /// Hidden in the spreadsheet program (still readable here).
    pub hidden: bool,
    /// Rows and columns the sheet says it uses, when it says.
    pub rows: Option<u64>,
    pub cols: Option<u64>,
    /// A file inside a packed file: its size once unpacked, when known.
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetOut {
    /// Rows written after the first (the column names).
    pub rows: u64,
    pub cols: u64,
    /// Cells that hold a formula, and those among them with no saved result.
    pub formulas: u64,
    pub formulas_without_result: u64,
    /// Rows above the column names that only title the sheet (a heading, labels
    /// over groups of columns): their words, one line per row. Not rows of data.
    pub title: Vec<String>,
}

/// Every sheet of the workbook, in its own order.
pub fn sheets(path: &Path) -> Result<Vec<SheetInfo>, String> {
    let streaming = matches!(ext_of(path).as_str(), "xlsx" | "xlsm");
    if streaming {
        let mut wb: calamine::Xlsx<_> =
            calamine::open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
        let meta = wb.sheets_metadata().to_vec();
        let mut out = Vec::with_capacity(meta.len());
        for s in meta {
            // the size comes from the sheet's own note of its used range
            let dims = wb
                .worksheet_cells_reader(&s.name)
                .ok()
                .map(|r| r.dimensions());
            let (rows, cols) = match dims {
                Some(d) if d.end >= d.start && d.end != (0, 0) => (
                    Some((d.end.0 - d.start.0) as u64 + 1),
                    Some((d.end.1 - d.start.1) as u64 + 1),
                ),
                _ => (None, None),
            };
            out.push(SheetInfo {
                name: s.name,
                hidden: s.visible != calamine::SheetVisible::Visible,
                rows,
                cols,
                bytes: None,
            });
        }
        return Ok(out);
    }
    let wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    Ok(wb
        .sheets_metadata()
        .iter()
        .map(|s| SheetInfo {
            name: s.name.clone(),
            hidden: s.visible != calamine::SheetVisible::Visible,
            rows: None,
            cols: None,
            bytes: None,
        })
        .collect())
}

fn date_text(d: &ExcelDateTime) -> String {
    if d.is_duration() {
        // a length of time: hours can pass 24
        let secs = (d.as_f64() * 86_400.0).round() as i64;
        let (neg, s) = (secs < 0, secs.abs());
        return format!(
            "{}{}:{:02}:{:02}",
            if neg { "-" } else { "" },
            s / 3600,
            s / 60 % 60,
            s % 60
        );
    }
    let (y, mo, da, h, mi, s, ms) = d.to_ymd_hms_milli();
    let time = if ms == 0 {
        format!("{h:02}:{mi:02}:{s:02}")
    } else {
        format!("{h:02}:{mi:02}:{s:02}.{ms:03}")
    };
    if d.as_f64() < 1.0 {
        // a time of day with no date
        time
    } else if (h, mi, s, ms) == (0, 0, 0, 0) {
        format!("{y:04}-{mo:02}-{da:02}")
    } else {
        format!("{y:04}-{mo:02}-{da:02} {time}")
    }
}

fn float_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn ref_text(v: &DataRef) -> String {
    match v {
        DataRef::Int(i) => i.to_string(),
        DataRef::Float(f) => float_text(*f),
        DataRef::String(s) => s.clone(),
        DataRef::SharedString(s) => (*s).to_string(),
        DataRef::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        DataRef::DateTime(d) => date_text(d),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
        DataRef::Error(e) => e.to_string(),
        DataRef::Empty => String::new(),
    }
}

fn data_text(v: &Data) -> String {
    match v {
        Data::Int(i) => i.to_string(),
        Data::Float(f) => float_text(*f),
        Data::String(s) => s.clone(),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(d) => date_text(d),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => e.to_string(),
        Data::Empty => String::new(),
    }
}

/// Rows going out as CSV, one at a time: empty rows between two rows with
/// data are kept, so a row's number here is its number in the sheet.
struct Rows<W: Write> {
    w: W,
    width: usize,
    /// Row being filled, and its number in the sheet.
    cur: Vec<String>,
    cur_row: Option<u32>,
    first_row: Option<u32>,
    written: u64,
    /// The first rows, held until it is clear which of them names the columns.
    held: Option<Vec<Vec<String>>>,
    /// The rows above the names that only title the sheet.
    title: Vec<String>,
    /// Watches the free space of the drive being written to.
    room: Option<Room>,
}

/// How many rows are held to find the row that names the columns.
const HOLD: usize = 8;

fn numeric(s: &str) -> bool {
    s.trim().parse::<f64>().is_ok()
}

/// How many leading rows only title the sheet: each holds at most half as many
/// words as the row under them, and that row reads as column names (two or more
/// names, none a number, no two alike). Nothing is known about the sheet's maker.
fn title_rows(held: &[Vec<String>]) -> usize {
    let filled = |r: &Vec<String>| r.iter().filter(|c| !c.is_empty()).count();
    for k in 1..held.len().min(HOLD - 1) {
        let names = &held[k];
        let n = filled(names);
        if n < 2 || held[..k].iter().any(|r| filled(r) * 2 > n) {
            continue;
        }
        let words: Vec<&String> = names.iter().filter(|c| !c.is_empty()).collect();
        let distinct: std::collections::HashSet<&String> = words.iter().copied().collect();
        if words.iter().any(|c| numeric(c)) || distinct.len() != words.len() {
            continue;
        }
        // the row under the names, when there is one, is not itself all names:
        // a sheet of words only has no title band to find
        return k;
    }
    0
}

impl<W: Write> Rows<W> {
    /// Decide on the held rows: drop a title band above the names, write the rest.
    fn finish(&mut self) -> std::io::Result<()> {
        let Some(held) = self.held.take() else {
            return Ok(());
        };
        let k = title_rows(&held);
        for r in &held[..k] {
            let words: Vec<&str> = r
                .iter()
                .map(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .collect();
            if !words.is_empty() {
                self.title.push(words.join(" · "));
            }
        }
        for r in &held[k..] {
            self.emit(r)?;
        }
        Ok(())
    }

    fn field(w: &mut W, v: &str) -> std::io::Result<()> {
        let quote = v.contains([',', '"', '\n', '\r']) || v.starts_with(' ') || v.ends_with(' ');
        if quote {
            w.write_all(b"\"")?;
            w.write_all(v.replace('"', "\"\"").as_bytes())?;
            w.write_all(b"\"")
        } else {
            w.write_all(v.as_bytes())
        }
    }

    fn line(&mut self, cells: &[String]) -> std::io::Result<()> {
        if let Some(h) = self.held.as_mut() {
            h.push(cells.to_vec());
            if h.len() >= HOLD {
                self.finish()?;
            }
            return Ok(());
        }
        self.emit(cells)
    }

    fn emit(&mut self, cells: &[String]) -> std::io::Result<()> {
        // the first row names the columns: no later row is narrower than it,
        // whatever width the sheet claims (a narrower row holding a line break
        // would be read as a broken one)
        if self.written == 0 {
            self.width = self.width.max(cells.len());
        }
        if let Some(room) = self.room.as_mut() {
            let text: usize = cells.iter().map(String::len).sum();
            room.wrote((text + self.width.max(cells.len()) + 1) as u64)
                .map_err(stopped_or)?;
        }
        if self.width.max(cells.len()) <= 1 && cells.first().is_none_or(|c| c.is_empty()) {
            // an empty row of a one-column sheet: still a row, not a blank line
            self.written += 1;
            return self.w.write_all(b"\"\"\n");
        }
        for i in 0..self.width.max(cells.len()).max(1) {
            if i > 0 {
                self.w.write_all(b",")?;
            }
            if let Some(v) = cells.get(i) {
                Self::field(&mut self.w, v)?;
            }
        }
        self.written += 1;
        self.w.write_all(b"\n")
    }

    fn flush_row(&mut self, done: &AtomicU64) -> std::io::Result<()> {
        if self.cur_row.is_some() {
            let cells = std::mem::take(&mut self.cur);
            self.line(&cells)?;
            done.store(self.written.saturating_sub(1), Ordering::Relaxed);
        }
        Ok(())
    }

    /// The bytes one empty row takes at the width now in force.
    fn empty_row_bytes(&self) -> u64 {
        if self.width <= 1 {
            3
        } else {
            self.width as u64
        }
    }

    fn cell(
        &mut self,
        row: u32,
        col: usize,
        v: String,
        done: &AtomicU64,
        cancel: &AtomicBool,
    ) -> std::io::Result<()> {
        if v.is_empty() {
            return Ok(());
        }
        // a position no workbook has: refused before anything is sized by it
        if row >= MAX_ROWS || col >= MAX_COLS as usize {
            return Err(stopped_or(beyond_workbook(
                row,
                col.min(u32::MAX as usize) as u32,
            )));
        }
        if self.cur_row != Some(row) {
            self.flush_row(done)?;
            // the empty rows in between
            if let Some(prev) = self.cur_row {
                let gap = u64::from(row.saturating_sub(prev + 1));
                if gap > 0 {
                    // what is about to be written must fit the drive, with its margin
                    if let Some(room) = self.room.as_ref() {
                        room.need(gap.saturating_mul(self.empty_row_bytes()))
                            .map_err(stopped_or)?;
                    }
                }
                for k in 0..gap {
                    if k % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                        return Err(stopped_or("stopped".into()));
                    }
                    self.line(&[])?;
                }
            }
            self.cur_row = Some(row);
            self.first_row.get_or_insert(row);
        }
        if self.cur.len() <= col {
            self.cur.resize(col + 1, String::new());
        }
        self.cur[col] = v;
        Ok(())
    }
}

/// Write one sheet to `out` as CSV: the first row that has anything in it
/// leads (the column names), every later row follows. `done` counts the
/// rows written so far; `cancel` stops it (the unfinished file is removed).
pub fn sheet_to_csv(
    path: &Path,
    sheet: &str,
    out: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<SheetOut, String> {
    let tmp = out.with_extension("part");
    // gone on every way out, a panic included; kept only once renamed into place
    let mut guard = PartGuard::new(&tmp);
    let r = match write_sheet(path, sheet, &tmp, cancel, done, true) {
        // a cell left of the declared first column: read again from column A
        Err(e) if e == LEFT_OF_DECLARED => {
            done.store(0, Ordering::Relaxed);
            write_sheet(path, sheet, &tmp, cancel, done, false)?
        }
        r => r?,
    };
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    guard.disarm();
    Ok(r)
}

/// One reading of the sheet. `trust_left`: take the first column the sheet
/// declares as the left edge (the usual case, leaving no empty columns on the
/// left); when a cell turns up further left, that is reported and the sheet
/// is read again with column A as the edge.
fn write_sheet(
    path: &Path,
    sheet: &str,
    tmp: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
    trust_left: bool,
) -> Result<SheetOut, String> {
    let io = |e: std::io::Error| e.to_string();
    let f = std::fs::File::create(tmp).map_err(io)?;
    let mut rows = Rows {
        w: BufWriter::with_capacity(1 << 20, f),
        width: 0,
        cur: Vec::new(),
        cur_row: None,
        first_row: None,
        written: 0,
        held: Some(Vec::new()),
        title: Vec::new(),
        room: Some(Room::new(tmp, "convert this sheet")),
    };
    let mut r = SheetOut::default();
    let mut max_col = 0usize;
    if matches!(ext_of(path).as_str(), "xlsx" | "xlsm") {
        // cell by cell, never the whole sheet in memory
        let mut wb: calamine::Xlsx<_> =
            calamine::open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
        let mut cells = wb
            .worksheet_cells_reader(sheet)
            .map_err(|e| e.to_string())?;
        let d = cells.dimensions();
        // the declared first column is a claim, not a fact: it is only used
        // while no cell contradicts it
        let left = if trust_left { d.start.1 } else { 0 };
        // a width no workbook has is not drawn: it would be written on every row
        if d.end.1 >= MAX_COLS {
            return Err(format!(
                "this sheet declares {} columns, beyond what a workbook can hold ({}); the file is damaged or was not made by a spreadsheet program",
                u64::from(d.end.1) + 1,
                MAX_COLS
            ));
        }
        rows.width = (d.end.1.saturating_sub(left) + 1) as usize;
        let mut n = 0u32;
        while let Some(c) = cells.next_cell_with_formula().map_err(|e| e.to_string())? {
            n = n.wrapping_add(1);
            if n % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            let v = ref_text(&c.value);
            if c.formula.as_ref().is_some_and(|f| !f.is_empty()) {
                r.formulas += 1;
                if v.is_empty() {
                    r.formulas_without_result += 1;
                }
            }
            if !v.is_empty() {
                if c.pos.0 >= MAX_ROWS || c.pos.1 >= MAX_COLS {
                    return Err(beyond_workbook(c.pos.0, c.pos.1));
                }
                if c.pos.1 < left {
                    return Err(LEFT_OF_DECLARED.into());
                }
            }
            let col = (c.pos.1 - left.min(c.pos.1)) as usize;
            if !v.is_empty() {
                max_col = max_col.max(col + 1);
            }
            rows.cell(c.pos.0, col, v, done, cancel).map_err(io)?;
        }
    } else {
        // the older kinds are read whole (an .xls sheet holds 65,536 rows at most)
        if ext_of(path) == "xlsb" {
            check_xlsb_extent(path, sheet, cancel)?;
        }
        let mut wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
        let range = wb.worksheet_range(sheet).map_err(|e| e.to_string())?;
        let (top, left) = range.start().unwrap_or((0, 0));
        rows.width = range.width();
        for (i, row) in range.rows().enumerate() {
            if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            for (j, v) in row.iter().enumerate() {
                let v = data_text(v);
                if !v.is_empty() {
                    max_col = max_col.max(j + 1);
                }
                rows.cell(top + i as u32, j, v, done, cancel).map_err(io)?;
            }
        }
        if let Ok(fr) = wb.worksheet_formula(sheet) {
            let (ft, fl) = fr.start().unwrap_or((0, 0));
            for (i, j, f) in fr.used_cells() {
                if f.is_empty() {
                    continue;
                }
                r.formulas += 1;
                let at = (ft + i as u32, fl + j as u32);
                if range.get_value(at).is_none_or(|v| data_text(v).is_empty()) {
                    r.formulas_without_result += 1;
                }
            }
        }
        let _ = left;
    }
    rows.flush_row(done).map_err(io)?;
    rows.finish().map_err(io)?;
    rows.w.flush().map_err(io)?;
    r.title = std::mem::take(&mut rows.title);
    r.rows = rows.written.saturating_sub(1);
    r.cols = max_col.max(rows.width.min(max_col.max(1))) as u64;
    done.store(r.rows, Ordering::Relaxed);
    Ok(r)
}

/// Before an .xlsb sheet is read whole (as a block of rows by columns), look
/// at where its cells lie: a cell in a far corner would make that block
/// enormous. The sheet is refused, with a message, when the block is absurd.
fn check_xlsb_extent(path: &Path, sheet: &str, cancel: &AtomicBool) -> Result<(), String> {
    let mut wb: calamine::Xlsb<_> =
        calamine::open_workbook(path).map_err(|e: calamine::XlsbError| e.to_string())?;
    let mut cells = wb.worksheet_cells_reader(sheet).map_err(|e| e.to_string())?;
    let (mut r0, mut r1, mut c0, mut c1) = (u32::MAX, 0u32, u32::MAX, 0u32);
    let mut n = 0u32;
    while let Some(c) = cells.next_cell().map_err(|e| e.to_string())? {
        n = n.wrapping_add(1);
        if n % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let (row, col) = c.get_position();
        if matches!(c.get_value(), DataRef::Empty) {
            continue;
        }
        if row >= MAX_ROWS || col >= MAX_COLS {
            return Err(beyond_workbook(row, col));
        }
        (r0, r1) = (r0.min(row), r1.max(row));
        (c0, c1) = (c0.min(col), c1.max(col));
    }
    if r0 > r1 {
        return Ok(());
    }
    let cells = (u64::from(r1 - r0) + 1) * (u64::from(c1 - c0) + 1);
    if cells > DENSE_CELLS_MAX {
        return Err(format!(
            "this sheet spreads over {cells} cells (rows {} to {}, columns {} to {}), more than can be read at once ({DENSE_CELLS_MAX}); save it as .xlsx and open that",
            r0 + 1,
            r1 + 1,
            c0 + 1,
            c1 + 1
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(width: usize) -> Rows<Vec<u8>> {
        Rows {
            w: Vec::new(),
            width,
            cur: Vec::new(),
            cur_row: None,
            first_row: None,
            written: 0,
            held: Some(Vec::new()),
            title: Vec::new(),
            room: None,
        }
    }

    #[test]
    fn no_row_is_narrower_than_the_first_even_when_the_sheet_claims_less() {
        let done = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let mut r = rows(2);
        for (c, v) in ["a", "b", "c", "d"].iter().enumerate() {
            r.cell(0, c, v.to_string(), &done, &stop).unwrap();
        }
        r.cell(1, 0, "two\nlines".into(), &done, &stop).unwrap();
        r.flush_row(&done).unwrap();
        r.finish().unwrap();
        assert_eq!(
            String::from_utf8(r.w).unwrap(),
            "a,b,c,d\n\"two\nlines\",,,\n"
        );
    }

    fn feed(r: &mut Rows<Vec<u8>>, sheet: &[&[&str]]) {
        let done = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        for (i, row) in sheet.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                r.cell(i as u32, c, v.to_string(), &done, &stop).unwrap();
            }
        }
        r.flush_row(&done).unwrap();
        r.finish().unwrap();
    }

    #[test]
    fn a_title_above_the_column_names_is_not_taken_for_them() {
        let mut r = rows(5);
        feed(
            &mut r,
            &[
                &["Report - Timeline", "", "", "Visit specific"],
                &["Type", "Time", "URL", "Visit ID", "Count"],
                &["url", "2026-11-01 10:00:00", "https://a.example/", "7", "2"],
            ],
        );
        assert_eq!(
            String::from_utf8(r.w).unwrap(),
            "Type,Time,URL,Visit ID,Count\nurl,2026-11-01 10:00:00,https://a.example/,7,2\n"
        );
        assert_eq!(
            r.title,
            vec!["Report - Timeline · Visit specific".to_string()]
        );
    }

    #[test]
    fn a_sheet_that_starts_with_its_column_names_is_left_as_it_is() {
        let mut r = rows(3);
        feed(&mut r, &[&["a", "b", "c"], &["1", "2", "3"]]);
        assert_eq!(String::from_utf8(r.w).unwrap(), "a,b,c\n1,2,3\n");
        assert!(r.title.is_empty());
        // a first row with blanks followed by numbers: that row names the columns
        let mut r = rows(3);
        feed(
            &mut r,
            &[&["time", "", ""], &["5", "6", "7"], &["8", "9", "10"]],
        );
        assert_eq!(String::from_utf8(r.w).unwrap(), "time,,\n5,6,7\n8,9,10\n");
        assert!(r.title.is_empty());
    }

    #[test]
    fn an_empty_row_of_a_one_column_sheet_is_still_a_row() {
        let done = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let mut r = rows(1);
        r.cell(0, 0, "note".into(), &done, &stop).unwrap();
        r.cell(2, 0, "x".into(), &done, &stop).unwrap();
        r.flush_row(&done).unwrap();
        r.finish().unwrap();
        assert_eq!(String::from_utf8(r.w).unwrap(), "note\n\"\"\nx\n");
    }
}
