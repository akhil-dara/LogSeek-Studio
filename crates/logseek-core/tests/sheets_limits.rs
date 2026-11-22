//! Workbooks whose own numbers cannot be trusted: a row or width no workbook
//! can hold, an empty stretch of rows between two cells, a wrong first column.
//! None of them may run the machine out of space or memory, hang past Stop, or
//! lose a cell.

use logseek_core::ingest::sheets::sheet_to_csv;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;

/// The free-space switch is read from the environment: one test at a time.
static ENV: Mutex<()> = Mutex::new(());

/// A one-sheet workbook with the given `<dimension>` and inline-string cells.
fn book(path: &Path, dimension: &str, cells: &[(&str, &str)]) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, body: &str| {
        z.start_file(name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    };
    put(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
    );
    put(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    put(
        "xl/workbook.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
    );
    put(
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
    );
    let mut rows = String::new();
    let mut cur = String::new();
    let mut open: Option<String> = None;
    for (r, v) in cells {
        let digits: String = r.chars().filter(|c| c.is_ascii_digit()).collect();
        if open.as_deref() != Some(digits.as_str()) {
            if open.is_some() {
                rows.push_str(&format!("{cur}</row>"));
            }
            cur = format!(r#"<row r="{digits}">"#);
            open = Some(digits);
        }
        cur.push_str(&format!(
            r#"<c r="{r}" t="inlineStr"><is><t>{v}</t></is></c>"#
        ));
    }
    if open.is_some() {
        rows.push_str(&format!("{cur}</row>"));
    }
    put(
        "xl/worksheets/sheet1.xml",
        &format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="{dimension}"/><sheetData>{rows}</sheetData></worksheet>"#
        ),
    );
    z.finish().unwrap();
}

fn convert(
    p: &Path,
    out: &Path,
    cancel: bool,
) -> Result<logseek_core::ingest::sheets::SheetOut, String> {
    sheet_to_csv(p, "S", out, &AtomicBool::new(cancel), &AtomicU64::new(0))
}

fn nothing_left(out: &Path) {
    assert!(!out.exists(), "a finished copy was left");
    assert!(
        !out.with_extension("part").exists(),
        "a working copy was left"
    );
}

#[test]
fn stop_is_heard_inside_a_long_run_of_empty_rows() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("gap.xlsx");
    // 1,000,000 empty rows between two cells, a width of 16,384 columns
    book(&p, "A1:XFD1", &[("A1", "a"), ("A1000000", "b")]);
    let out = d.path().join("gap");
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "10000000");
    let t = std::time::Instant::now();
    let r = convert(&p, &out, true);
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    assert_eq!(r, Err("stopped".into()));
    assert!(t.elapsed().as_secs() < 20, "Stop was not heard in time");
    nothing_left(&out);
}

#[test]
fn a_gap_that_would_not_fit_the_drive_is_refused_before_it_is_written() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("gap.xlsx");
    book(&p, "A1:XFD1", &[("A1", "a"), ("A1000000", "b")]);
    let out = d.path().join("gap");
    // 1 GiB free: the gap alone would write about 16 GB
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "1024");
    let r = convert(&p, &out, false);
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    let e = r.expect_err("must refuse");
    assert!(e.starts_with("not enough room to convert this sheet"), "{e}");
    nothing_left(&out);
}

#[test]
fn a_small_gap_with_room_is_kept_as_empty_rows() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("ok.xlsx");
    book(&p, "A1:B1", &[("A1", "a"), ("B1", "b"), ("A5", "c")]);
    let out = d.path().join("ok");
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "100000");
    let r = convert(&p, &out, false);
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    r.unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "a,b\n,\n,\n,\nc,\n");
}

#[test]
fn a_row_or_width_no_workbook_has_is_refused_with_a_message() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    // two cells about four billion rows apart
    let p = d.path().join("far.xlsx");
    book(&p, "A1:A1", &[("A1", "a"), ("A4000000000", "b")]);
    let out = d.path().join("far");
    let e = convert(&p, &out, false).expect_err("must refuse");
    assert!(!e.is_empty() && e != "stopped", "{e}");
    nothing_left(&out);

    // a row just past the last row of a workbook
    let p = d.path().join("past.xlsx");
    book(&p, "A1:A1", &[("A1", "a"), ("A1048577", "b")]);
    let e = convert(&p, &out, false).expect_err("must refuse");
    assert!(e.contains("beyond what a workbook can hold"), "{e}");
    nothing_left(&out);

    // a declared width past the last column
    let p = d.path().join("wide.xlsx");
    book(&p, "A1:XFE1", &[("A1", "a")]);
    let e = convert(&p, &out, false).expect_err("must refuse");
    assert!(e.contains("beyond what a workbook can hold"), "{e}");
    nothing_left(&out);

    // the last row and column a workbook has are fine
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "10000000");
    let p = d.path().join("edge.xlsx");
    book(&p, "A1:XFD2", &[("A1", "a"), ("XFD2", "z")]);
    let r = convert(&p, &out, false);
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    r.unwrap();
}

#[test]
fn a_cell_with_a_huge_column_is_refused_not_allocated() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("col.xlsx");
    book(&p, "A1:A1", &[("A1", "a"), ("ZZZZZZ1", "b")]);
    let out = d.path().join("col");
    let r = std::panic::catch_unwind(|| convert(&p, &out, false));
    let e = r.expect("must not panic").expect_err("must refuse");
    assert!(!e.is_empty() && e != "stopped", "{e}");
    nothing_left(&out);
}

#[test]
fn cells_left_of_the_declared_first_column_keep_their_own_columns() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("left.xlsx");
    book(
        &p,
        "C1:C2",
        &[
            ("A1", "a"),
            ("B1", "b"),
            ("C1", "c"),
            ("A2", "1"),
            ("B2", "2"),
            ("C2", "3"),
        ],
    );
    let out = d.path().join("left");
    let r = convert(&p, &out, false).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "a,b,c\n1,2,3\n");
    assert_eq!(r.cols, 3);

    // an honest offset is still trimmed: no empty columns on the left
    let p = d.path().join("offset.xlsx");
    book(&p, "C1:D2", &[("C1", "c"), ("D1", "d"), ("C2", "1"), ("D2", "2")]);
    let out = d.path().join("offset");
    convert(&p, &out, false).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "c,d\n1,2\n");
}
