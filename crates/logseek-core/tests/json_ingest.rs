//! JSON ingest: records, nested fields and arrays.
use logseek_core::ingest::batch::RecordBatch;
use logseek_core::ingest::json::{
    parse_json_records, ArrayElementScanner, JsonOptions, SchemaBuilder,
};
use logseek_core::ingest::report::ImportReport;

fn run(data: &[u8], opts: &JsonOptions) -> (SchemaBuilder, Vec<RecordBatch>, ImportReport) {
    let mut schema = SchemaBuilder::default();
    let mut rep = ImportReport::default();
    let b = parse_json_records(data, 0, &mut schema, opts, &mut rep);
    (schema, b, rep)
}

fn get<'a>(schema: &SchemaBuilder, b: &'a RecordBatch, row: usize, name: &str) -> Option<&'a str> {
    schema.index_of(name).and_then(|c| b.field(row, c))
}

#[test]
fn nested_flattened_dotted() {
    let (s, b, _) = run(
        br#"{"a":{"b":1,"c":[1,2],"d":{"e":"x"}},"n":null,"t":true}"#,
        &JsonOptions::default(),
    );
    assert_eq!(get(&s, &b[0], 0, "a.b"), Some("1"));
    assert_eq!(get(&s, &b[0], 0, "a.c"), Some("[1,2]"));
    assert_eq!(get(&s, &b[0], 0, "a.d.e"), Some("x"));
    assert_eq!(get(&s, &b[0], 0, "n"), None);
    assert_eq!(get(&s, &b[0], 0, "t"), Some("true"));
}

#[test]
fn numbers_are_kept_byte_exact() {
    let (s, b, _) = run(
        br#"{"x":1.50,"y":12345678901234567890,"z":1e5}"#,
        &JsonOptions::default(),
    );
    assert_eq!(get(&s, &b[0], 0, "x"), Some("1.50"));
    assert_eq!(get(&s, &b[0], 0, "y"), Some("12345678901234567890"));
    assert_eq!(get(&s, &b[0], 0, "z"), Some("1e5"));
}

#[test]
fn literal_dotted_keys_preserved() {
    let (s, b, _) = run(
        br#"{"event.type[0]":"start","process.entity_id":"7"}"#,
        &JsonOptions::default(),
    );
    assert_eq!(get(&s, &b[0], 0, "event.type[0]"), Some("start"));
    assert_eq!(get(&s, &b[0], 0, "process.entity_id"), Some("7"));
}

#[test]
fn schema_drift_adds_columns() {
    let data = b"{\"x\":1}\n{\"x\":2,\"y\":\"new\"}\n{\"z\":3}\n";
    let (s, b, rep) = run(data, &JsonOptions::default());
    assert_eq!(s.names(), &["x", "y", "z"]);
    assert_eq!(get(&s, &b[0], 0, "y"), None);
    assert_eq!(get(&s, &b[0], 1, "y"), Some("new"));
    assert_eq!(get(&s, &b[0], 2, "z"), Some("3"));
    assert_eq!(get(&s, &b[0], 2, "x"), None);
    assert_eq!(rep.rows, 3);
}

#[test]
fn array_with_one_object_per_line() {
    let data = b"[{\"a\":1},\n{\"a\":2},\n{\"a\":3}]";
    let (s, b, _) = run(data, &JsonOptions::default());
    assert_eq!(b[0].rows(), 3);
    assert_eq!(get(&s, &b[0], 2, "a"), Some("3"));
    assert_eq!(b[0].offset(1), 10);
}

#[test]
fn wrapper_records_array_is_unwrapped() {
    let data = br#"{"Records":[{"eventName":"GetObject","requestParameters":{"bucketName":"b"}},{"eventName":"ListBuckets","requestParameters":null}]}"#;
    let opts = JsonOptions {
        record_path: Some("Records".into()),
        ..Default::default()
    };
    let (s, b, _) = run(data, &opts);
    assert_eq!(b[0].rows(), 2);
    assert_eq!(get(&s, &b[0], 0, "requestParameters.bucketName"), Some("b"));
    assert_eq!(get(&s, &b[0], 1, "eventName"), Some("ListBuckets"));
}

#[test]
fn invalid_line_recorded_and_kept_raw() {
    let data = b"{\"a\":1}\nnot json at all\n{\"a\":3}\n";
    let (s, b, rep) = run(data, &JsonOptions::default());
    assert_eq!(rep.invalid_json, 1);
    assert_eq!(b[0].rows(), 3);
    assert_eq!(get(&s, &b[0], 1, "_raw"), Some("not json at all"));
}

#[test]
fn sparse_wide_records_are_cheap() {
    // 2,000 rows, each with its own unique key: must not be quadratic.
    let mut data = Vec::new();
    for i in 0..2_000 {
        data.extend_from_slice(format!("{{\"k{i}\":{i}}}\n").as_bytes());
    }
    let t = std::time::Instant::now();
    let (s, b, _) = run(&data, &JsonOptions::default());
    assert!(t.elapsed().as_secs_f64() < 2.0);
    assert_eq!(s.names().len(), 2_000);
    assert_eq!(get(&s, &b[0], 1_999, "k1999"), Some("1999"));
}

#[test]
fn scanner_finds_elements_across_any_chunk_split() {
    let doc = br#"  [ {"a":"x,]"} , {"b":[1,{"c":"}"}]} ,3, "s\"]" ]  "#;
    let want: Vec<&[u8]> = vec![
        br#"{"a":"x,]"}"#,
        br#"{"b":[1,{"c":"}"}]}"#,
        b"3",
        br#""s\"]""#,
    ];
    for split in 1..doc.len() {
        let mut sc = ArrayElementScanner::new(None);
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut buf = Vec::new();
        for part in [&doc[..split], &doc[split..]] {
            sc.feed(part, |el| got.push(el.to_vec()), &mut buf);
        }
        let got: Vec<&[u8]> = got.iter().map(|v| v.as_slice()).collect();
        assert_eq!(got, want, "split at {split}");
    }
}

#[test]
fn scanner_follows_record_path() {
    let doc = br#"{"meta":{"Records":[9]},"Records":[{"a":1},{"a":2}],"x":[0]}"#;
    let mut sc = ArrayElementScanner::new(Some("Records".into()));
    let mut got = Vec::new();
    let mut buf = Vec::new();
    sc.feed(
        doc,
        |el| got.push(String::from_utf8(el.to_vec()).unwrap()),
        &mut buf,
    );
    assert_eq!(got, vec![r#"{"a":1}"#, r#"{"a":2}"#]);
}

#[test]
fn stream_scanner_splits_back_to_back_objects_at_any_chunk_boundary() {
    let data =
        b"{\n  \"a\": \"}{\",\n  \"b\": [1, {\"c\": 2}]\n}\n{\"a\": \"x\\\"}\"}\n\n  {\n\"a\":3}\n";
    let want: Vec<&[u8]> = vec![
        b"{\n  \"a\": \"}{\",\n  \"b\": [1, {\"c\": 2}]\n}",
        b"{\"a\": \"x\\\"}\"}",
        b"{\n\"a\":3}",
    ];
    for split in 1..data.len() {
        let mut sc = ArrayElementScanner::stream();
        let mut buf = Vec::new();
        let mut got: Vec<(u64, Vec<u8>)> = Vec::new();
        for part in [&data[..split], &data[split..]] {
            sc.feed_with_offsets(part, &mut buf, |off, el| got.push((off, el.to_vec())));
        }
        let els: Vec<&[u8]> = got.iter().map(|(_, e)| e.as_slice()).collect();
        assert_eq!(els, want, "split at {split}");
        for (off, el) in &got {
            assert_eq!(
                &data[*off as usize..*off as usize + el.len()],
                el.as_slice()
            );
        }
    }
}

// ───────────── line-oriented sources never re-sniff a chunk as an array ─────────────

#[test]
fn a_line_oriented_chunk_that_starts_with_a_bracket_line_keeps_every_line() {
    let data = b"[0]\n{\"a\":1}\n{\"a\":2}\n";
    let opts = JsonOptions {
        line_oriented: true,
        ..Default::default()
    };
    let (s, b, rep) = run(data, &opts);
    assert_eq!(b.iter().map(|x| x.rows()).sum::<usize>(), 3);
    assert_eq!(rep.rows, 3);
    assert_eq!(rep.invalid_json, 0);
    assert_eq!(get(&s, &b[0], 0, "_value"), Some("[0]"));
    assert_eq!(get(&s, &b[0], 2, "a"), Some("2"));
    // offsets still point at each line
    assert_eq!(b[0].offset(1), 4);
}

// ───────────── what the record-path scanner does not read is counted ─────────────

fn feed_all(path: Option<&str>, doc: &[u8]) -> (ArrayElementScanner, Vec<String>) {
    let mut sc = ArrayElementScanner::new(path.map(String::from));
    let mut got = Vec::new();
    let mut buf = Vec::new();
    sc.feed(
        doc,
        |el| got.push(String::from_utf8(el.to_vec()).unwrap()),
        &mut buf,
    );
    (sc, got)
}

fn non_ws(s: &str) -> u64 {
    s.bytes().filter(|b| !b.is_ascii_whitespace()).count() as u64
}

#[test]
fn a_clean_wrapper_or_array_leaves_nothing_unread() {
    for (path, doc) in [
        (Some("Records"), "{\"Records\": [ {\"a\":1} ]}\n"),
        (Some("Records"), "{\n \"Records\": [ {\"a\":1} ]\n}\n"),
        (None, " [ {\"a\":1} ]\n\n"),
    ] {
        let (sc, got) = feed_all(path, doc.as_bytes());
        assert_eq!(got.len(), 1, "{doc}");
        assert!(sc.is_done());
        assert_eq!(sc.unread_after_array(), 0, "{doc}");
        assert_eq!(sc.unfinished(), None, "{doc}");
    }
}

#[test]
fn a_later_sibling_array_a_second_document_and_trailing_data_are_counted() {
    let tail = ",\n  \"Records\": [ {\"evil\": 1}, {\"evil\": 2} ]\n";
    let doc = format!("{{\n  \"decoy\": [ {{\"a\": 1}} ]{tail}}}\n");
    let (sc, got) = feed_all(Some("decoy"), doc.as_bytes());
    assert_eq!(got, vec![r#"{"a": 1}"#]);
    assert_eq!(sc.unread_after_array(), non_ws(tail));

    let second = "{\n \"Records\": [ {\"n\": 2} ]\n}\n";
    let doc = format!("{{\n \"Records\": [ {{\"n\": 1}} ]\n}}\n{second}");
    let (sc, got) = feed_all(Some("Records"), doc.as_bytes());
    assert_eq!(got.len(), 1);
    assert_eq!(sc.unread_after_array(), non_ws(second));

    let (sc, _) = feed_all(None, b"[ {\"a\":1} ] [ {\"a\":2} ]");
    assert_eq!(sc.unread_after_array(), non_ws("[ {\"a\":2} ]"));
}

#[test]
fn counting_the_unread_part_does_not_depend_on_where_the_feeds_split() {
    let doc = b"{\n \"decoy\": [ {\"a\":1} ], \"s\": \"x } ] y\",\n \"Records\": [ {\"b\":2} ]\n}\n";
    let (whole, _) = feed_all(Some("decoy"), doc);
    assert!(whole.unread_after_array() > 0);
    for split in 1..doc.len() {
        let mut sc = ArrayElementScanner::new(Some("decoy".into()));
        let mut buf = Vec::new();
        for part in [&doc[..split], &doc[split..]] {
            sc.feed(part, |_| {}, &mut buf);
        }
        assert_eq!(
            sc.unread_after_array(),
            whole.unread_after_array(),
            "split at {split}"
        );
    }
}

#[test]
fn an_element_left_open_at_the_end_is_reported() {
    let doc = b"{\n \"Records\": [ {\"a\":1}, {\"a\":2}";
    let (sc, got) = feed_all(Some("Records"), doc);
    assert_eq!(got.len(), 1);
    assert!(!sc.is_done());
    let (a, b) = sc.unfinished().expect("the open element is reported");
    assert_eq!(&doc[a as usize..b as usize], br#"{"a":2}"#);
    // a stream of values ending mid-value
    let mut sc = ArrayElementScanner::stream();
    let mut buf = Vec::new();
    sc.feed(b"{\"a\":1}\n{\"b\":", |_| {}, &mut buf);
    assert_eq!(sc.unfinished(), Some((8, 13)));
}

// ───────────── nesting depth ─────────────

fn on_small_stack<T: Send + 'static>(bytes: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(bytes)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn deeply_nested_objects_are_kept_raw_and_counted_never_a_crash() {
    let (invalid, raw_len, rows) = on_small_stack(1 << 20, || {
        let n = 200_000;
        let mut data = "{\"a\":".repeat(n).into_bytes();
        data.push(b'1');
        data.extend(std::iter::repeat(b'}').take(n));
        data.push(b'\n');
        data.extend_from_slice(b"{\"ok\":1}\n");
        let mut schema = SchemaBuilder::default();
        let mut rep = ImportReport::default();
        let b = parse_json_records(&data, 0, &mut schema, &JsonOptions::default(), &mut rep);
        let raw = schema.index_of("_raw").and_then(|c| b[0].field(0, c));
        (rep.invalid_json, raw.map(str::len), rep.rows)
    });
    assert_eq!(invalid, 1);
    assert_eq!(raw_len, Some(200_000 * 6 + 1));
    assert_eq!(rows, 2);
}

#[test]
fn deeply_nested_arrays_inside_a_value_are_kept_raw_and_counted() {
    let invalid = on_small_stack(1 << 20, || {
        let n = 100_000;
        let mut data = b"{\"a\":".to_vec();
        data.extend(std::iter::repeat(b'[').take(n));
        data.extend(std::iter::repeat(b']').take(n));
        data.extend_from_slice(b"}\n");
        let mut schema = SchemaBuilder::default();
        let mut rep = ImportReport::default();
        parse_json_records(&data, 0, &mut schema, &JsonOptions::default(), &mut rep);
        rep.invalid_json
    });
    assert_eq!(invalid, 1);
}

#[test]
fn ordinary_nesting_well_under_the_limit_still_flattens() {
    let n = 300;
    let mut data = "{\"a\":".repeat(n).into_bytes();
    data.push(b'1');
    data.extend(std::iter::repeat(b'}').take(n));
    let (s, b, rep) = run(&data, &JsonOptions::default());
    assert_eq!(rep.invalid_json, 0);
    let name = vec!["a"; n].join(".");
    assert_eq!(get(&s, &b[0], 0, &name), Some("1"));
}

// ───────────── one cell per row and column ─────────────

#[test]
fn a_repeated_key_keeps_the_last_value_and_never_shifts_other_rows() {
    let data = b"{\"u\":\"alice\",\"u\":\"mallory\"}\n{\"u\":\"bob\"}\n{\"u\":\"carol\"}\n{\"x\":1}\n";
    let (s, b, _) = run(data, &JsonOptions::default());
    assert_eq!(b[0].rows(), 4);
    assert_eq!(get(&s, &b[0], 0, "u"), Some("mallory"));
    assert_eq!(get(&s, &b[0], 1, "u"), Some("bob"));
    assert_eq!(get(&s, &b[0], 2, "u"), Some("carol"));
    assert_eq!(get(&s, &b[0], 3, "u"), None);
    for r in 0..4 {
        let cells = b[0].row_cells(r);
        let mut cols: Vec<usize> = cells.iter().map(|c| c.0).collect();
        cols.sort();
        cols.dedup();
        assert_eq!(cols.len(), cells.len(), "row {r} holds one cell per column");
    }
}

#[test]
fn a_dotted_key_and_a_nested_path_that_collide_hold_one_value() {
    let data = b"{\"a.b\":\"1\",\"a\":{\"b\":\"2\"}}\n{\"a.b\":\"3\"}\n";
    let (s, b, _) = run(data, &JsonOptions::default());
    assert_eq!(get(&s, &b[0], 0, "a.b"), Some("2"));
    assert_eq!(get(&s, &b[0], 1, "a.b"), Some("3"));
    assert_eq!(b[0].row_cells(0).len(), 1);
}

// ───────────── source fields never share an id with the app's own columns ─────────────

#[test]
fn source_fields_named_like_the_apps_own_columns_keep_their_values_under_another_name() {
    let data = b"{\"_file\":\"b.json\",\"_overflow\":\"x\",\"msg\":\"hi\"}\n";
    let (s, b, _) = run(data, &JsonOptions::default());
    assert!(!s.names().iter().any(|n| n == "_file" || n == "_overflow"));
    assert_eq!(get(&s, &b[0], 0, "_file"), Some("b.json"));
    assert_eq!(get(&s, &b[0], 0, "_overflow"), Some("x"));
    assert_eq!(get(&s, &b[0], 0, "msg"), Some("hi"));
    assert_eq!(s.names()[0], "_file_2");
}
