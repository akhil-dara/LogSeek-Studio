//! Excel workbooks: the sheets inside, and a sheet turned into rows.

use logseek_core::ingest::sheets::{is_workbook, sheet_to_csv, sheets};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};

/// A small workbook written by hand: two sheets (the second hidden), text,
/// numbers, a date, a formula with its saved result and one without.
fn workbook(path: &Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, body: &str| {
        z.start_file(name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    };
    put(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#,
    );
    put(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    put(
        "xl/workbook.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Logons" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#,
    );
    put(
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
    );
    put(
        "xl/styles.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy-mm-dd hh:mm:ss"/></numFmts><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="164" applyNumberFormat="1"/></cellXfs></styleSheet>"#,
    );
    put(
        "xl/sharedStrings.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="6" uniqueCount="6"><si><t>when</t></si><si><t>user</t></si><si><t>bytes</t></si><si><t>double</t></si><si><t>ann, "the admin"</t></si><si><t>bob</t></si></sst>"#,
    );
    // 45000.5 is 2023-03-15 12:00:00; row 3 is empty; D4's formula has no saved result
    put(
        "xl/worksheets/sheet1.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:D4"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c><c r="D1" t="s"><v>3</v></c></row><row r="2"><c r="A2" s="1"><v>45000.5</v></c><c r="B2" t="s"><v>4</v></c><c r="C2"><v>120</v></c><c r="D2"><f>C2*2</f><v>240</v></c></row><row r="4"><c r="A4" s="1"><v>45001</v></c><c r="B4" t="s"><v>5</v></c><c r="C4"><v>1.5</v></c><c r="D4"><f>C4*2</f></c></row></sheetData></worksheet>"#,
    );
    put(
        "xl/worksheets/sheet2.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:A1"/><sheetData><row r="1"><c r="A1" t="s"><v>1</v></c></row></sheetData></worksheet>"#,
    );
    z.finish().unwrap();
}

#[test]
fn a_workbook_lists_every_sheet_with_its_size_and_whether_excel_hides_it() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("book.xlsx");
    workbook(&p);
    assert!(is_workbook(&p));
    assert!(!is_workbook(Path::new("a.csv")));
    let s = sheets(&p).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].name.as_str(), s[0].hidden), ("Logons", false));
    assert_eq!((s[0].rows, s[0].cols), (Some(4), Some(4)));
    assert_eq!((s[1].name.as_str(), s[1].hidden), ("Notes", true));
}

#[test]
fn a_sheet_becomes_rows_with_dates_as_dates_and_formulas_as_their_saved_results() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("book.xlsx");
    workbook(&p);
    let out = d.path().join("Logons");
    let done = AtomicU64::new(0);
    let r = sheet_to_csv(&p, "Logons", &out, &AtomicBool::new(false), &done).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "when,user,bytes,double");
    // the date is written as a date, the text is quoted, the formula shows 240
    assert_eq!(
        lines[1],
        "2023-03-15 12:00:00,\"ann, \"\"the admin\"\"\",120,240"
    );
    // the empty row 3 stays a row, so row numbers match the sheet's
    assert_eq!(lines[2], ",,,");
    assert_eq!(lines[3], "2023-03-16,bob,1.5,");
    assert_eq!(lines.len(), 4);
    assert_eq!((r.rows, r.cols), (3, 4));
    assert_eq!((r.formulas, r.formulas_without_result), (2, 1));
    assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), 3);
}

#[test]
fn a_missing_sheet_or_a_file_that_is_not_a_workbook_says_so() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("book.xlsx");
    workbook(&p);
    let out = d.path().join("x");
    let stop = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    assert!(sheet_to_csv(&p, "Nope", &out, &stop, &done).is_err());
    let bad = d.path().join("bad.xlsx");
    std::fs::write(&bad, b"not a workbook").unwrap();
    assert!(sheets(&bad).is_err());
}
