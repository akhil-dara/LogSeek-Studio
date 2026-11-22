//! Text logs with no header: the parts of each line (words, "quoted" and
//! [bracketed] groups, comma chains) worked out from the file's own lines,
//! never from a list of known formats; what does not fit is kept, never dropped.

use logseek_core::budget::Budget;
use logseek_core::ingest::detect::detect_spec;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::ingest::report::ImportReport;
use logseek_core::ingest::textlayout::{infer, parse_chunk, PartKind, REST};
use logseek_core::store::{SourceSpec, Store};

const ACCESS: &str = concat!(
    "203.0.113.5 - - [12/Nov/2026:10:00:01 +0000] \"GET /index.html HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko)\"\n",
    "198.51.100.7 - alice [12/Nov/2026:10:00:03 +0000] \"POST /login HTTP/1.1\" 302 0 \"https://example.com/\" \"curl/8.4.0\"\n",
    "203.0.113.5 - - [12/Nov/2026:10:00:09 +0000] \"GET /admin HTTP/1.1\" 403 199 \"-\" \"Mozilla/5.0 (X11; Linux x86_64)\"\n",
);

#[test]
fn an_access_log_with_no_header_reads_as_its_parts() {
    let head = ACCESS.repeat(20);
    let lay = infer(head.as_bytes()).expect("a layout");
    let kinds: Vec<PartKind> = lay.parts.iter().map(|p| p.kind).collect();
    use PartKind::*;
    assert_eq!(
        kinds,
        [Word, Word, Word, Bracketed, Quoted, Word, Word, Quoted, Quoted]
    );
    // named by place, then the column that keeps whatever does not fit
    assert_eq!(lay.names.len(), 10);
    assert_eq!(lay.names[0], "column_1");
    assert_eq!(lay.names[9], REST);
    // the file is read this way, not as a CSV split at the semicolons
    match detect_spec(head.as_bytes()) {
        SourceSpec::Text { parts, header } => {
            assert_eq!(parts, lay.parts);
            assert_eq!(header, lay.names);
        }
        other => panic!("read as {other:?}"),
    }
}

#[test]
fn each_line_becomes_its_values_unquoted_and_nothing_is_dropped() {
    let head = ACCESS.repeat(20);
    let lay = infer(head.as_bytes()).unwrap();
    let mut rep = ImportReport::default();
    // one line in another shape, and a comment
    let chunk = format!("{ACCESS}# a note\nsomething else entirely\n");
    let b = parse_chunk(chunk.as_bytes(), 0, &lay.parts, lay.names.len(), &mut rep);
    let rows: usize = b.iter().map(|x| x.rows()).sum();
    assert_eq!(
        rows, 4,
        "three log lines and the odd one; the comment is not a row"
    );
    let first = &b[0];
    assert_eq!(first.field(0, 0), Some("203.0.113.5"));
    assert_eq!(first.field(0, 3), Some("12/Nov/2026:10:00:01 +0000"));
    assert_eq!(first.field(0, 4), Some("GET /index.html HTTP/1.1"));
    assert_eq!(first.field(0, 5), Some("200"));
    assert_eq!(first.field(1, 2), Some("alice"));
    assert_eq!(first.field(0, 9), None, "nothing left over");
    // the odd line is kept whole in the last column, and counted
    assert_eq!(first.field(3, 9), Some("something else entirely"));
    assert_eq!(rep.ragged_rows, 1);
    // each row starts where its line starts in the file
    assert_eq!(first.offset(1), ACCESS.find("198.51").unwrap() as u64);
}

#[test]
fn a_forwarded_chain_and_text_glued_to_a_quote_are_kept_apart() {
    let line = "192.0.2.85, 198.51.100.135, 203.0.113.10:47350 - - [29/Oct/2026:23:59:59 +0000] \"GET / HTTP/1.1\" 200 55875 \"-\" \"Uptime(https://x)\"respTime=48541;\n";
    let head = line.repeat(30);
    let lay = infer(head.as_bytes()).unwrap();
    assert_eq!(lay.parts[0].kind, PartKind::Chain);
    let last = lay.parts.last().unwrap();
    assert!(last.glued, "the timing sits right after the quote");
    let mut rep = ImportReport::default();
    let b = parse_chunk(line.as_bytes(), 0, &lay.parts, lay.names.len(), &mut rep);
    assert_eq!(
        b[0].field(0, 0),
        Some("192.0.2.85, 198.51.100.135, 203.0.113.10:47350")
    );
    assert_eq!(b[0].field(0, lay.parts.len() - 1), Some("respTime=48541;"));
    assert_eq!(rep.ragged_rows, 0);
}

#[test]
fn lines_of_free_text_keep_the_message_whole() {
    let mut head = String::new();
    let msgs = [
        "user alice signed in",
        "disk almost full on volume two",
        "retrying",
        "connection from 10.0.0.4 closed by peer",
    ];
    let levels = ["INFO", "WARN", "ERROR"];
    for i in 0..200 {
        head.push_str(&format!(
            "2026-11-12 10:{:02}:{:02} {} {}\n",
            (i / 60) % 60,
            i % 60,
            levels[i % 3],
            msgs[i % 4]
        ));
    }
    let lay = infer(head.as_bytes()).unwrap();
    // date, time and level are columns; the message, of any length, is the rest
    assert_eq!(lay.parts.len(), 3);
    let mut rep = ImportReport::default();
    let b = parse_chunk(head.as_bytes(), 0, &lay.parts, lay.names.len(), &mut rep);
    assert_eq!(b[0].field(1, 2), Some("WARN"));
    assert_eq!(b[0].field(1, 3), Some("disk almost full on volume two"));
    assert_eq!(rep.ragged_rows, 0);
}

#[test]
fn a_fields_line_names_the_columns() {
    let mut head = String::from(
        "#Software: a web server\n#Fields: date time c-ip cs-method cs-uri-stem sc-status\n",
    );
    for i in 0..50 {
        head.push_str(&format!(
            "2026-11-12 10:00:{:02} 10.0.0.{} GET /p{} 200\n",
            i % 60,
            i % 9,
            i % 7
        ));
    }
    let lay = infer(head.as_bytes()).unwrap();
    assert_eq!(
        lay.names,
        [
            "date",
            "time",
            "c-ip",
            "cs-method",
            "cs-uri-stem",
            "sc-status",
            REST
        ]
    );
}

#[test]
fn a_csv_with_quoted_text_full_of_spaces_stays_a_csv() {
    let mut head = String::from("Host,User,Cmd\n");
    for i in 0..300 {
        head.push_str(&format!(
            "WS-{:02},u{},\"svchost.exe -k netsvcs {}\"\n",
            i % 20,
            i % 4,
            i % 97
        ));
    }
    assert!(matches!(
        detect_spec(head.as_bytes()),
        SourceSpec::Csv { .. }
    ));
}

#[test]
fn a_real_csv_stays_a_csv() {
    let mut head = String::from("time,host,status,path\n");
    for i in 0..50 {
        head.push_str(&format!(
            "2026-11-12 10:00:{:02},h{},200,/a b/{}\n",
            i % 60,
            i % 3,
            i
        ));
    }
    assert!(matches!(
        detect_spec(head.as_bytes()),
        SourceSpec::Csv { .. }
    ));
}

#[test]
fn numbered_columns_take_the_names_the_user_gives_before_opening() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("access.log");
    std::fs::write(&src, ACCESS.repeat(20)).unwrap();
    let dir = d.path().join("a.lsidx");
    let mut names = vec![String::new(); 10];
    names[0] = "client".into();
    names[3] = "when".into();
    names[5] = "status".into();
    // a name already used elsewhere is numbered, never clashing
    names[6] = "client".into();
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(64 << 20),
        IngestOptions {
            names,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = Store::open(&dir).unwrap();
    let cols: Vec<&str> = st
        .schema()
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        &cols[..7],
        &["client", "column_2", "column_3", "when", "column_5", "status", "client_2"]
    );
    // the time is found under its new name
    assert_eq!(st.schema().time_col.map(|c| cols[c]), Some("when"));
}
