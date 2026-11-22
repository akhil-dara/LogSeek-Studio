//! CSV ingest: parsing records, quoting and header handling.
use logseek_core::ingest::batch::RecordBatch;
use logseek_core::ingest::csv::{
    read_header, read_header_raw, sniff, CsvChunkParser, CsvOptions,
};
use logseek_core::ingest::encoding::{decode_cp1252, detect_encoding, Encoding};
use logseek_core::ingest::report::ImportReport;

fn parse_all(data: &[u8]) -> (Vec<String>, Vec<RecordBatch>, ImportReport) {
    let opts = sniff(data);
    let header = read_header(data, &opts);
    let mut rep = ImportReport::default();
    let p = CsvChunkParser::new(opts, header.len());
    let batches = p.parse(data, 0, opts.has_header, Some(header.as_slice()), &mut rep);
    (header, batches, rep)
}

fn rows(batches: &[RecordBatch]) -> Vec<Vec<Option<String>>> {
    let mut out = vec![];
    for b in batches {
        for r in 0..b.rows() {
            let mut row: Vec<Option<String>> = (0..b.ncols())
                .map(|c| b.field(r, c).map(str::to_string))
                .collect();
            row.push(b.overflow(r).map(str::to_string));
            out.push(row);
        }
    }
    out
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn edge_csv_keeps_everything() {
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/edge.csv"
    ))
    .unwrap();
    let (header, b, rep) = parse_all(&data);
    assert_eq!(header, vec!["a", "b", "c"]);
    let r = rows(&b);
    // the unclosed-quote line is split by delimiter, quote kept literally
    assert_eq!(r[0], vec![s("\"unclosed quote"), s("2"), s("3"), None]);
    assert_eq!(r[1], vec![s("4"), s("5"), None, None]); // short row padded
    assert_eq!(r[2], vec![s("6"), s("7"), s("8"), s("9")]); // long row: overflow kept
    assert_eq!(r[3], vec![None, None, None, None]); // empty fields
    assert_eq!(r[4], vec![s("multi\nline"), s("x"), s("y"), None]); // legit multi-line field
                                                                    // the fixture itself holds double-encoded UTF-8 ("cafÃ©"); it must come back byte-exact
    assert_eq!(
        r[5],
        vec![s("caf\u{C3}\u{A9}"), s("na\u{C3}\u{AF}ve"), s("ok"), None]
    );
    assert_eq!(rep.decode_errors, 0);
    assert_eq!(r.len(), 6);
    assert_eq!(rep.ragged_rows, 2);
    assert_eq!(rep.overflow_fields, 1);
    assert_eq!(rep.bad_quote_spans.len(), 1);
    assert_eq!(rep.bad_quote_spans[0].0, 6); // starts at the quote on line 2
}

#[test]
fn unclosed_quote_is_bounded() {
    let mut data = b"h1,h2\n\"broken,1\n".to_vec();
    for i in 0..20_000 {
        data.extend_from_slice(format!("v{i},{i}\n").as_bytes());
    }
    let (_, b, rep) = parse_all(&data);
    let r = rows(&b);
    assert_eq!(rep.bad_quote_spans.len(), 1);
    assert_eq!(r.len(), 20_001);
    assert_eq!(r[20_000], vec![s("v19999"), s("19999"), None]);
}

#[test]
fn byte_offsets_point_at_row_starts() {
    let data = b"a,b\n1,2\n\"x\ny\",3\n4,5";
    let (_, b, _) = parse_all(data);
    assert_eq!(b[0].offset(0), 4);
    assert_eq!(b[0].offset(1), 8);
    assert_eq!(b[0].offset(2), 16);
    assert_eq!(b[0].end_offset(), data.len() as u64);
}

#[test]
fn repeated_header_rows_skipped_and_counted() {
    let data = b"Dst Port,Protocol\n80,6\nDst Port,Protocol\n443,6\n";
    let (_, b, rep) = parse_all(data);
    assert_eq!(rows(&b).len(), 2);
    assert_eq!(rep.header_repeats, 1);
}

#[test]
fn batches_are_row_bounded() {
    let mut data = b"n\n".to_vec();
    for i in 0..150_000 {
        data.extend_from_slice(format!("{i}\n").as_bytes());
    }
    let (_, b, _) = parse_all(&data);
    assert!(b.iter().all(|x| x.rows() <= 65_536));
    assert_eq!(b.iter().map(|x| x.rows()).sum::<usize>(), 150_000);
}

#[test]
fn sniff_semicolon_tab_pipe() {
    assert_eq!(sniff(b"a;b;c\n1;2;3\n4;5;6\n").delimiter, b';');
    assert_eq!(sniff(b"a\tb\tc\n1\t2,5\t3\n4\t5\t6\n").delimiter, b'\t');
    assert_eq!(sniff(b"a|b\n1|2\n").delimiter, b'|');
    assert_eq!(sniff(b"a,b,c\n\"1;2\",2,3\n").delimiter, b',');
}

#[test]
fn header_detection() {
    assert!(sniff(b"time,host,bytes\n2026-01-01,ws1,10\n").has_header);
    assert!(!sniff(b"1,2,3\n4,5,6\n").has_header);
}

#[test]
fn header_names_cleaned() {
    let opts = CsvOptions {
        delimiter: b',',
        quote: b'"',
        has_header: true,
        encoding: Encoding::Utf8,
    };
    assert_eq!(
        read_header(b" Time ,,host,host\n", &opts),
        vec!["Time", "column_2", "host", "host_2"]
    );
}

#[test]
fn utf8_bom_and_utf16_detected() {
    assert_eq!(detect_encoding(b"\xEF\xBB\xBFa,b\n"), Encoding::Utf8Bom);
    assert_eq!(detect_encoding(b"\xFF\xFEa\x00,\x00"), Encoding::Utf16Le);
    assert_eq!(detect_encoding(b"\xFE\xFF\x00a\x00,"), Encoding::Utf16Be);
    assert_eq!(detect_encoding(b"plain,ascii\n"), Encoding::Utf8);
}

#[test]
fn bom_is_not_part_of_first_header() {
    let (h, _, _) = parse_all(b"\xEF\xBB\xBFtime,user\n1,a\n");
    assert_eq!(h[0], "time");
}

#[test]
fn invalid_utf8_field_falls_back_to_cp1252_and_is_counted() {
    let (_, b, rep) = parse_all(b"name,city\nJos\xE9,M\x80nchen\n");
    let r = rows(&b);
    assert_eq!(r[0][0], s("José"));
    assert_eq!(r[0][1], s("M€nchen"));
    assert_eq!(rep.decode_errors, 2);
    assert_eq!(decode_cp1252(b"\x93hi\x94"), "\u{201C}hi\u{201D}");
}

#[test]
fn crlf_line_endings() {
    let (_, b, _) = parse_all(b"a,b\r\n1,2\r\n3,4\r\n");
    assert_eq!(
        rows(&b),
        vec![vec![s("1"), s("2"), None], vec![s("3"), s("4"), None]]
    );
}

#[test]
fn chunk_starting_mid_file_uses_base_offset() {
    let opts = CsvOptions {
        delimiter: b',',
        quote: b'"',
        has_header: false,
        encoding: Encoding::Utf8,
    };
    let p = CsvChunkParser::new(opts, 2);
    let mut rep = ImportReport::default();
    let b = p.parse(b"7,8\n9,10\n", 1_000, false, None, &mut rep);
    assert_eq!(b[0].offset(0), 1_000);
    assert_eq!(b[0].offset(1), 1_004);
}

#[test]
fn a_header_named_like_the_apps_own_columns_is_renamed_and_the_rename_is_recorded() {
    let data = b"time,_file,msg,_overflow,_file_2
1,b.csv,hi,x,y
";
    let opts = sniff(data);
    // the names as written stay available, so the Check screen can list the change
    assert_eq!(
        read_header_raw(data, &opts),
        ["time", "_file", "msg", "_overflow", "_file_2"]
    );
    let header = read_header(data, &opts);
    assert_eq!(header, ["time", "_file_2", "msg", "_overflow_2", "_file_2_2"]);
    // the values are still there, under the new names
    let (h, b, rep) = parse_all(data);
    assert_eq!(h, header);
    assert!(rep.is_clean());
    assert_eq!(b[0].field(0, 1), Some("b.csv"));
    assert_eq!(b[0].field(0, 3), Some("x"));
}
