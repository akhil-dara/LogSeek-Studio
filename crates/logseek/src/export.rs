//! Export the rows a search keeps: CSV (spreadsheet-safe), JSON Lines or an
//! Excel workbook, streamed page by page on the engine thread.

use std::sync::atomic::AtomicBool;

/// Set from the UI to stop an export between pages.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Jsonl,
    /// An Excel workbook: every value as text, far smaller on disk than CSV.
    Xlsx,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Jsonl => "jsonl",
            Format::Xlsx => "xlsx",
        }
    }
}

/// What one workbook sheet and one of its cells can hold (Excel's own limits).
pub const XLSX_ROWS: u32 = 1_048_576;
pub const XLSX_COLS: usize = 16_384;
pub const XLSX_CELL: usize = 32_767;

/// A value in pieces that each fit a workbook cell, cut between characters;
/// joined again they are the value, whole.
pub fn cell_pieces(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut n) = (0, 0);
    for (i, _) in v.char_indices() {
        if n == XLSX_CELL {
            out.push(&v[start..i]);
            start = i;
            n = 0;
        }
        n += 1;
    }
    out.push(&v[start..]);
    out
}

/// A value as the text of a workbook cell: the three characters XML keeps
/// for itself are named, and what XML cannot carry (control characters) is
/// written the way workbooks write it, `_xHHHH_`. A value that already holds
/// such a sequence gets its underscore written out, so it comes back as it was.
pub fn xml_text(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = String::with_capacity(v.len() + 8);
    for (i, ch) in v.char_indices() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' => out.push(ch),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {
                out.push_str(&format!("_x{:04X}_", c as u32));
            }
            '_' if b.len() >= i + 7
                && b[i + 1] == b'x'
                && b[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit)
                && b[i + 6] == b'_' =>
            {
                out.push_str("_x005F_");
            }
            c => out.push(c),
        }
    }
    out
}

/// A column's letters in a cell reference: 0 is A, 25 is Z, 26 is AA.
pub fn col_letters(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

const SHEET_HEAD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    "<sheetViews><sheetView workbookViewId=\"0\">",
    "<pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/>",
    "</sheetView></sheetViews><sheetData>"
);
const SHEET_TAIL: &str = "</sheetData></worksheet>";

/// One row of a sheet as XML: every cell a text cell, so nothing in it is
/// ever read as a number, a date or a formula.
fn row_xml(row: u32, cells: &[(usize, &str)]) -> String {
    let mut x = format!("<row r=\"{row}\">");
    for (c, v) in cells {
        x.push_str(&format!(
            "<c r=\"{}{row}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            col_letters(*c),
            xml_text(v)
        ));
    }
    x.push_str("</row>");
    x
}

/// Rows written to a workbook as they come, straight into the file: memory
/// stays flat whatever the size. More rows than a sheet holds go on to
/// further sheets, each with the column names; a value longer than a cell
/// holds is written whole, in pieces, on a sheet named "Long values", and
/// its own cell says where. Nothing is cut.
pub struct XlsxOut {
    /// None once the workbook is closed.
    zip: Option<zip::ZipWriter<std::io::BufWriter<std::fs::File>>>,
    names: Vec<String>,
    /// Names of the sheets written so far, in order ("Rows", "Rows 2").
    sheets: Vec<String>,
    /// Next row (1-based) of the sheet being filled.
    next: u32,
    /// Rows exported so far.
    row: u64,
    /// Long values wait in files beside the workbook until the rows are in
    /// (a workbook file is written one sheet at a time): (file, rows in it).
    long: Vec<(std::path::PathBuf, std::io::BufWriter<std::fs::File>, u32)>,
    base: std::path::PathBuf,
}

fn opts() -> zip::write::SimpleFileOptions {
    // large-file records from the start: an export past 4 GB still opens
    zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true)
}

impl XlsxOut {
    /// `path` is the file to write (the caller renames it when it is whole).
    pub fn new(names: &[String], path: &std::path::Path) -> Result<XlsxOut, String> {
        if names.len() > XLSX_COLS {
            return Err(format!(
                "a workbook sheet holds {} columns and this export has {}: export fewer columns, or as CSV",
                crate::moment::group(XLSX_COLS as u64),
                crate::moment::group(names.len() as u64)
            ));
        }
        let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut out = XlsxOut {
            zip: Some(zip::ZipWriter::new(std::io::BufWriter::with_capacity(1 << 20, f))),
            names: names.to_vec(),
            sheets: Vec::new(),
            next: 1,
            row: 0,
            long: Vec::new(),
            base: path.to_path_buf(),
        };
        out.next_sheet()?;
        Ok(out)
    }

    fn zip(&mut self) -> Result<&mut zip::ZipWriter<std::io::BufWriter<std::fs::File>>, String> {
        self.zip.as_mut().ok_or_else(|| "the workbook is already closed".to_string())
    }

    fn put(&mut self, text: &str) -> Result<(), String> {
        use std::io::Write;
        self.zip()?.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }

    fn next_sheet(&mut self) -> Result<(), String> {
        if !self.sheets.is_empty() {
            self.put(SHEET_TAIL)?;
        }
        let n = self.sheets.len() + 1;
        self.sheets.push(if n == 1 {
            "Rows".to_string()
        } else {
            format!("Rows {n}")
        });
        self.zip()?
            .start_file(format!("xl/worksheets/sheet{n}.xml"), opts())
            .map_err(|e| e.to_string())?;
        self.put(SHEET_HEAD)?;
        let names = self.names.clone();
        let head: Vec<(usize, &str)> = names.iter().enumerate().map(|(c, n)| (c, n.as_str())).collect();
        self.put(&row_xml(1, &head))?;
        self.next = 2;
        Ok(())
    }

    /// Put one long value aside; returns (its sheet's name, its row there).
    fn long_row(&mut self, col: &str, pieces: &[&str]) -> Result<(String, u32), String> {
        use std::io::Write;
        if self.long.last().is_none_or(|l| l.2 >= XLSX_ROWS) {
            let mut p = self.base.clone().into_os_string();
            p.push(format!(".long{}", self.long.len() + 1));
            let p = std::path::PathBuf::from(p);
            let f = std::fs::File::create(&p).map_err(|e| e.to_string())?;
            let mut w = std::io::BufWriter::new(f);
            let head = [
                (0, "row of the export"),
                (1, "column"),
                (2, "the value, in pieces to join"),
            ];
            w.write_all(row_xml(1, &head).as_bytes()).map_err(|e| e.to_string())?;
            self.long.push((p, w, 1));
        }
        let k = self.long.len();
        let exported = self.row.to_string();
        let Some(l) = self.long.last_mut() else {
            return Err("no room for a long value".into());
        };
        l.2 += 1;
        let mut cells: Vec<(usize, &str)> = vec![(0, exported.as_str()), (1, col)];
        cells.extend(pieces.iter().enumerate().map(|(i, p)| (i + 2, *p)));
        l.1.write_all(row_xml(l.2, &cells).as_bytes()).map_err(|e| e.to_string())?;
        let name = if k == 1 {
            "Long values".to_string()
        } else {
            format!("Long values {k}")
        };
        Ok((name, l.2))
    }

    /// One exported row.
    pub fn push(&mut self, cells: &[Option<String>]) -> Result<(), String> {
        if self.next > XLSX_ROWS {
            self.next_sheet()?;
        }
        self.row += 1;
        let mut notes: Vec<(usize, String)> = Vec::new();
        for (c, v) in cells.iter().enumerate() {
            let Some(v) = v else { continue };
            // most values fit; only a longer one is counted by characters
            if v.len() <= XLSX_CELL || v.chars().count() <= XLSX_CELL {
                continue;
            }
            let pieces = cell_pieces(v);
            if pieces.len() + 2 > XLSX_COLS {
                return Err(format!(
                    "a value of {} characters is too long for a workbook: export as CSV",
                    crate::moment::group(v.chars().count() as u64)
                ));
            }
            let name = self.names.get(c).cloned().unwrap_or_default();
            let (sheet, at) = self.long_row(&name, &pieces)?;
            notes.push((
                c,
                format!(
                    "({} characters, more than a workbook cell holds: the whole value is on sheet \"{sheet}\", row {at})",
                    crate::moment::group(v.chars().count() as u64)
                ),
            ));
        }
        let row: Vec<(usize, &str)> = cells
            .iter()
            .enumerate()
            .filter_map(|(c, v)| {
                let v = v.as_deref().filter(|v| !v.is_empty())?;
                Some(match notes.iter().find(|n| n.0 == c) {
                    Some(n) => (c, n.1.as_str()),
                    None => (c, v),
                })
            })
            .collect();
        let x = row_xml(self.next, &row);
        self.put(&x)?;
        self.next += 1;
        Ok(())
    }

    /// Close the workbook: the long values, then the parts that name the sheets.
    pub fn finish(mut self) -> Result<(), String> {
        use std::io::Write;
        self.put(SHEET_TAIL)?;
        let long = std::mem::take(&mut self.long);
        for (k, (path, w, _)) in long.into_iter().enumerate() {
            let f = w.into_inner().map_err(|e| e.to_string())?;
            drop(f);
            let n = self.sheets.len() + 1;
            self.sheets.push(if k == 0 {
                "Long values".to_string()
            } else {
                format!("Long values {}", k + 1)
            });
            self.zip()?
                .start_file(format!("xl/worksheets/sheet{n}.xml"), opts())
                .map_err(|e| e.to_string())?;
            self.put(SHEET_HEAD)?;
            let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let copied = std::io::copy(&mut f, self.zip()?).map_err(|e| e.to_string());
            drop(f);
            let _ = std::fs::remove_file(&path);
            copied?;
            self.put(SHEET_TAIL)?;
        }
        let n = self.sheets.len();
        let mut types = String::from(concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
            "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
            "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
            "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>"
        ));
        let mut book = String::from(concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" ",
            "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>"
        ));
        let mut rels = String::from(concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
        ));
        for i in 1..=n {
            types.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"));
            book.push_str(&format!(
                "<sheet name=\"{}\" sheetId=\"{i}\" r:id=\"rId{i}\"/>",
                xml_text(&self.sheets[i - 1])
            ));
            rels.push_str(&format!("<Relationship Id=\"rId{i}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{i}.xml\"/>"));
        }
        types.push_str("</Types>");
        book.push_str("</sheets></workbook>");
        rels.push_str("</Relationships>");
        let root = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
            "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>",
            "</Relationships>"
        );
        for (name, text) in [
            ("[Content_Types].xml", types.as_str()),
            ("_rels/.rels", root),
            ("xl/workbook.xml", book.as_str()),
            ("xl/_rels/workbook.xml.rels", rels.as_str()),
        ] {
            self.zip()?.start_file(name, opts()).map_err(|e| e.to_string())?;
            self.put(text)?;
        }
        let zip = self.zip.take().ok_or("the workbook is already closed")?;
        let mut w = zip.finish().map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())
    }
}

impl Drop for XlsxOut {
    /// Stopped or failed part-way: the long values put aside go too.
    fn drop(&mut self) {
        for (p, w, _) in self.long.drain(..) {
            drop(w);
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A cell a spreadsheet would run as a formula gets a leading ' so it is
/// shown as text. Plain numbers (-5, +3.25) are left alone.
pub fn safe_cell(v: &str) -> String {
    let risky = v.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if risky && v.trim().parse::<f64>().is_err() {
        format!("'{v}")
    } else {
        v.to_string()
    }
}

/// One CSV record (RFC 4180 quoting), formula cells neutralised, CRLF ended.
pub fn csv_line(fields: &[Option<String>]) -> String {
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let Some(v) = f else { continue };
        let v = safe_cell(v);
        if v.contains([',', '"', '\n', '\r']) || v.starts_with(' ') || v.ends_with(' ') {
            out.push('"');
            out.push_str(&v.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&v);
        }
    }
    out.push_str("\r\n");
    out
}

/// One JSON object per line, keys in column order; no value → null.
pub fn jsonl_line(names: &[String], fields: &[Option<String>]) -> String {
    let mut out = String::from("{");
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(n).unwrap_or_default());
        out.push(':');
        match fields.get(i).and_then(|v| v.as_deref()) {
            Some(v) => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
            None => out.push_str("null"),
        }
    }
    out.push_str("}\n");
    out
}

/// "events.csv" → "events (export).csv".
pub fn suggest_name(source: &str, f: Format) -> String {
    let stem = source
        .rsplit_once('.')
        .map(|(a, _)| a)
        .filter(|a| !a.is_empty())
        .unwrap_or(source);
    format!("{stem} (export).{}", f.ext())
}

/// Rows in the next page: about 32 MiB of text, between 50 and 10,000 rows,
/// so wide rows (CloudTrail, PowerShell script blocks) never fill memory.
pub fn next_page_len(last_bytes: u64, last_rows: u64) -> usize {
    if last_rows == 0 {
        return 10_000;
    }
    let avg = (last_bytes / last_rows).max(1);
    ((32u64 << 20) / avg).clamp(50, 10_000) as usize
}

/// Where an export stands, for the status bar.
#[derive(Clone, Debug, Default)]
pub struct ExportStatus {
    pub path: std::path::PathBuf,
    pub done: u64,
    pub total: u64,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_are_sized_by_bytes_not_rows() {
        assert_eq!(
            next_page_len(655_360, 10),
            512,
            "64 KiB rows: 32 MiB a page"
        );
        assert_eq!(
            next_page_len(1_000_000, 10_000),
            10_000,
            "small rows: the full page"
        );
        assert_eq!(next_page_len(0, 0), 10_000);
        assert!(
            next_page_len(100_000_000, 10) >= 50,
            "never stalls on huge rows"
        );
    }

    use super::*;
    use logseek_core::ingest::csv::{read_header, sniff, CsvChunkParser};
    use logseek_core::ingest::report::ImportReport;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn csv_escapes_quotes_commas_newlines() {
        let names = vec!["host".to_string(), "cmd".to_string(), "note".to_string()];
        let rows = vec![
            vec![s("we1"), s("a, b"), s("say \"hi\"")],
            vec![s("we2"), s("line one\nline two"), None],
            vec![s("we3"), s(""), s("plain")],
        ];
        let mut text = csv_line(&names.iter().map(|n| Some(n.clone())).collect::<Vec<_>>());
        for r in &rows {
            text.push_str(&csv_line(r));
        }
        let data = text.as_bytes();
        let opts = sniff(data);
        let header = read_header(data, &opts);
        assert_eq!(header, names);
        let mut rep = ImportReport::default();
        let batches = CsvChunkParser::new(opts, header.len()).parse(
            data,
            0,
            opts.has_header,
            Some(&header),
            &mut rep,
        );
        let back: Vec<Vec<Option<String>>> = batches
            .iter()
            .flat_map(|b| {
                (0..b.rows()).map(move |r| {
                    (0..b.ncols())
                        .map(|c| b.field(r, c).map(str::to_string))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        assert_eq!(back.len(), 3, "three rows come back");
        assert_eq!(back[0][1].as_deref(), Some("a, b"));
        assert_eq!(back[0][2].as_deref(), Some("say \"hi\""));
        assert_eq!(back[1][1].as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn jsonl_is_valid_json() {
        let names = vec!["host".to_string(), "cmd".to_string()];
        let line = jsonl_line(&names, &[s("we1 \"x\"\n"), None]);
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["host"], "we1 \"x\"\n");
        assert!(v["cmd"].is_null());
        // keys keep the column order
        assert!(line.find("host").unwrap() < line.find("cmd").unwrap());
    }

    #[test]
    fn formula_cells_are_neutralised() {
        for (cell, want) in [
            ("=cmd|' /C calc'!A0", "'=cmd|' /C calc'!A0"),
            ("+1+1", "'+1+1"),
            ("-x", "'-x"),
            ("@SUM(A1)", "'@SUM(A1)"),
            ("\t=1", "'\t=1"),
            ("-5", "-5"),
            ("+3.25", "+3.25"),
            ("plain", "plain"),
        ] {
            assert_eq!(safe_cell(cell), want, "{cell:?}");
        }
        assert!(csv_line(&[s("=1+1")]).starts_with("'=1+1"));
    }

    #[test]
    fn export_file_names_are_suggested() {
        assert_eq!(
            suggest_name("events.csv", Format::Csv),
            "events (export).csv"
        );
        assert_eq!(
            suggest_name("logs.jsonl", Format::Jsonl),
            "logs (export).jsonl"
        );
    }
}

#[cfg(test)]
mod xlsx_tests {
    use super::*;

    #[test]
    fn a_long_value_is_cut_into_cell_sized_pieces_that_join_back_whole() {
        assert_eq!(cell_pieces("short"), vec!["short"]);
        assert_eq!(cell_pieces(""), vec![""]);
        // two characters over the limit, the last ones not ASCII
        let v = format!("{}éß", "a".repeat(XLSX_CELL));
        let p = cell_pieces(&v);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].chars().count(), XLSX_CELL);
        assert_eq!(p[1], "éß");
        assert_eq!(p.concat(), v);
    }

    #[test]
    fn cell_text_survives_xml_and_workbook_escapes() {
        assert_eq!(xml_text("a<b & c>d"), "a&lt;b &amp; c&gt;d");
        // a control character is written the workbook way; a line break stays
        assert_eq!(xml_text("x\u{1}y\nz"), "x_x0001_y\nz");
        assert_eq!(xml_text("a\rb"), "a_x000D_b");
        // text that looks like such a sequence comes back as it was
        assert_eq!(xml_text("id_x0041_end"), "id_x005F_x0041_end");
        assert_eq!(xml_text("_x12_"), "_x12_");
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(16_383), "XFD");
    }

    #[test]
    fn rows_are_written_to_a_workbook_with_a_long_value_kept_whole() {
        let dir = std::env::temp_dir().join(format!("logseek-xlsx-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("out.xlsx");
        let names = vec!["user".to_string(), "note".to_string()];
        let mut out = XlsxOut::new(&names, &path).unwrap();
        out.push(&[Some("ann".into()), Some("=cmd|x".into())]).unwrap();
        out.push(&[Some("bob".into()), None]).unwrap();
        let long = "z".repeat(XLSX_CELL + 10);
        out.push(&[Some("cy".into()), Some(long.clone())]).unwrap();
        out.push(&[Some("a<b & _x0041_".into()), Some("tab\there".into())]).unwrap();
        out.finish().unwrap();
        // a copy to open in a spreadsheet program by hand, when asked for
        if let Some(keep) = std::env::var_os("LOGSEEK_KEEP_XLSX") {
            let _ = std::fs::copy(&path, keep);
        }
        // nothing is left beside the workbook
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["out.xlsx".to_string()]);
        // read it back as the app reads any workbook
        let sheets = logseek_core::ingest::sheets::sheets(&path).unwrap();
        let names: Vec<&str> = sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Rows", "Long values"]);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let done = std::sync::atomic::AtomicU64::new(0);
        let csv = dir.join("rows.csv");
        logseek_core::ingest::sheets::sheet_to_csv(&path, "Rows", &csv, &cancel, &done).unwrap();
        let text = std::fs::read_to_string(&csv).unwrap();
        assert!(text.starts_with("user,note"), "{text}");
        assert!(text.contains("ann,=cmd|x"), "a formula-like value stays text: {text}");
        assert!(text.contains("bob,"), "{text}");
        assert!(text.contains("the whole value is on sheet"), "{text}");
        assert!(text.contains("a<b & _x0041_,tab\there"), "marks and escapes come back: {text}");
        let csv2 = dir.join("long.csv");
        logseek_core::ingest::sheets::sheet_to_csv(&path, "Long values", &csv2, &cancel, &done)
            .unwrap();
        let text2 = std::fs::read_to_string(&csv2).unwrap();
        let joined: String = text2.lines().nth(1).unwrap().split(',').skip(2).collect();
        assert_eq!(joined, long, "the long value comes back whole");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
