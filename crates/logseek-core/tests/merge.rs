//! Several files as one index: columns matched by name, a `_file` column
//! naming each row's file, raw lines and searches read from each row's own
//! file, nothing copied.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest_many, IngestOptions, JobState, Keep};
use logseek_core::query::exec::{filter, page, ChunkCache, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn ingest(files: &[PathBuf], dir: &Path, opts: IngestOptions) -> Store {
    let job = start_ingest_many(files, dir, Budget::new(128 << 20), opts);
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    Store::open(dir).unwrap()
}

fn col(st: &Store, name: &str) -> usize {
    st.schema()
        .index_of(name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

/// Every row's cells for these columns, in row order.
fn cells(st: &Store, names: &[&str]) -> Vec<Vec<Option<String>>> {
    let cols: Vec<usize> = names.iter().map(|n| col(st, n)).collect();
    let all = MatchSet::all(st);
    page(
        st,
        &all,
        0,
        st.row_count() as usize,
        &cols,
        &ChunkCache::new(Budget::new(32 << 20)),
    )
    .unwrap()
    .cells
}

#[test]
fn two_csvs_with_different_columns_become_one_set_of_rows() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("hostA.csv");
    let b = d.path().join("hostB.csv");
    let mut ta = String::from("time,host,msg\n");
    for i in 0..3000 {
        ta.push_str(&format!(
            "2026-11-12 10:{:02}:{:02},web1,alpha {i}\n",
            i / 60 % 60,
            i % 60
        ));
    }
    // the same names in another order, one more column, one fewer
    let mut tb = String::from("user,time,host\n");
    for i in 0..2000 {
        tb.push_str(&format!(
            "bob{},2026-11-12 11:{:02}:{:02},db2\n",
            i % 3,
            i / 60 % 60,
            i % 60
        ));
    }
    std::fs::write(&a, &ta).unwrap();
    std::fs::write(&b, &tb).unwrap();
    let st = ingest(
        &[a.clone(), b.clone()],
        &d.path().join("ab.lsidx"),
        IngestOptions {
            chunk_bytes: 32 * 1024,
            ..Default::default()
        },
    );
    assert_eq!(st.row_count(), 5000);
    assert_eq!(st.manifest().parts.len(), 2);
    let rows = cells(&st, &["_file", "host", "user", "msg"]);
    // the first file's rows, then the second's, each saying where it came from
    assert_eq!(rows[0][0].as_deref(), Some("hostA.csv"));
    assert_eq!(rows[0][1].as_deref(), Some("web1"));
    assert_eq!(rows[0][2], None, "hostA has no user column");
    assert_eq!(rows[4999][0].as_deref(), Some("hostB.csv"));
    assert_eq!(rows[4999][1].as_deref(), Some("db2"));
    assert_eq!(rows[4999][3], None, "hostB has no msg column");
    // one time column across both files
    let tc = st.schema().time_col.expect("a time column");
    assert_eq!(st.schema().columns[tc].name, "time");
    // a row's raw line is read from its own file
    assert_eq!(
        st.raw_line(0).unwrap(),
        ta.lines().nth(1).unwrap().as_bytes()
    );
    assert_eq!(
        st.raw_line(4999).unwrap(),
        tb.lines().last().unwrap().as_bytes()
    );
    // searches reach every file, words and times alike
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    assert_eq!(
        filter(&st, &parse("db2").unwrap(), &c, &no)
            .unwrap()
            .count(),
        2000
    );
    // contains: "alpha 7" is also in "alpha 70" … "alpha 799"
    assert_eq!(
        filter(&st, &parse("\"alpha 7\"").unwrap(), &c, &no)
            .unwrap()
            .count(),
        111
    );
    assert_eq!(
        filter(&st, &parse("\"alpha 2999\"").unwrap(), &c, &no)
            .unwrap()
            .count(),
        1
    );
    // a time from the second file, found in its own file's bytes
    assert_eq!(
        filter(&st, &parse("\"11:30:15\"").unwrap(), &c, &no)
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        filter(
            &st,
            &parse("_file=\"hostB.csv\" user=bob1").unwrap(),
            &c,
            &no
        )
        .unwrap()
        .count(),
        667
    );
    // changing any one file makes the index out of date
    assert!(!st.is_stale());
    std::fs::write(&b, format!("{tb}bob9,2026-11-12 12:00:00,db2\n")).unwrap();
    assert!(st.is_stale());
}

#[test]
fn rotated_text_logs_line_up_by_place_and_keep_their_file() {
    let d = tempfile::tempdir().unwrap();
    let line = |ip: &str, t: &str, code: u32| {
        format!("{ip} - - [{t} +0000] \"GET /x HTTP/1.1\" {code} 512 \"-\" \"agent\"\n")
    };
    let mut x = String::new();
    let mut y = String::new();
    for i in 0..400 {
        x.push_str(&line(
            "10.0.0.1",
            &format!("12/Nov/2026:09:{:02}:{:02}", i / 60, i % 60),
            200,
        ));
        y.push_str(&line(
            "10.0.0.2",
            &format!("12/Nov/2026:10:{:02}:{:02}", i / 60, i % 60),
            404,
        ));
    }
    let a = d.path().join("access.log.1");
    let b = d.path().join("access.log");
    std::fs::write(&a, &x).unwrap();
    std::fs::write(&b, &y).unwrap();
    let st = ingest(
        &[a, b],
        &d.path().join("logs.lsidx"),
        IngestOptions::default(),
    );
    assert_eq!(st.row_count(), 800);
    let rows = cells(&st, &["_file", "column_1", "column_6"]);
    assert_eq!(
        rows[0],
        vec![
            Some("access.log.1".into()),
            Some("10.0.0.1".into()),
            Some("200".into())
        ]
    );
    assert_eq!(
        rows[799],
        vec![
            Some("access.log".into()),
            Some("10.0.0.2".into()),
            Some("404".into())
        ]
    );
    assert!(st.schema().time_col.is_some());
}

#[test]
fn files_with_the_same_name_in_different_folders_say_which_one() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("srv1")).unwrap();
    std::fs::create_dir_all(d.path().join("srv2")).unwrap();
    let a = d.path().join("srv1").join("app.csv");
    let b = d.path().join("srv2").join("app.csv");
    std::fs::write(&a, "k,v\n1,a\n").unwrap();
    std::fs::write(&b, "k,v\n2,b\n").unwrap();
    let st = ingest(
        &[a, b],
        &d.path().join("app.lsidx"),
        IngestOptions::default(),
    );
    let rows = cells(&st, &["_file"]);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        rows[0][0].as_deref(),
        Some(format!("srv1{sep}app.csv").as_str())
    );
    assert_eq!(
        rows[1][0].as_deref(),
        Some(format!("srv2{sep}app.csv").as_str())
    );
}

#[test]
fn columns_read_back_from_the_source_come_from_each_rows_own_file() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("one.csv");
    let b = d.path().join("two.csv");
    let mut ta = String::from("time,msg\n");
    let mut tb = String::from("time,msg\n");
    for i in 0..1500 {
        ta.push_str(&format!(
            "2026-11-12 08:{:02}:{:02},first {i}\n",
            i / 60 % 60,
            i % 60
        ));
        tb.push_str(&format!(
            "2026-11-12 09:{:02}:{:02},second {i}\n",
            i / 60 % 60,
            i % 60
        ));
    }
    std::fs::write(&a, &ta).unwrap();
    std::fs::write(&b, &tb).unwrap();
    // only the time kept in the index: msg and _file are read from the files
    let st = ingest(
        &[a, b],
        &d.path().join("ab.lsidx"),
        IngestOptions {
            chunk_bytes: 16 * 1024,
            keep: Keep::Only(vec!["time".into()]),
            ..Default::default()
        },
    );
    assert!(st.source_columns().contains(&col(&st, "msg")));
    let rows = cells(&st, &["msg", "_file"]);
    assert_eq!(rows[0][0].as_deref(), Some("first 0"));
    assert_eq!(rows[2999][0].as_deref(), Some("second 1499"));
    assert_eq!(rows[2999][1].as_deref(), Some("two.csv"));
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    assert_eq!(
        filter(&st, &parse("\"second 14\"").unwrap(), &c, &no)
            .unwrap()
            .count(),
        111
    );
}
