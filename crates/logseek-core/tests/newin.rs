//! New in a period: the values of each column seen in the period and never
//! before it, with how settled each column is (the share of the period's rows
//! whose value was seen before), steadiest columns first. Nothing about the
//! columns is known in advance.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::ast::Query;
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet};
use logseek_core::query::newin::new_values;
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

const S: i64 = 1_000_000;
const DAY0: i64 = 1_794_441_600; // 2026-11-12 00:00:00 UTC

/// Ten hours of rows with three apps, then an hour where one app is new;
/// an id that is different on every row; a host that never changes.
fn make() -> (tempfile::TempDir, Store) {
    let mut text = String::from("Time,App,Id,Host\n");
    for i in 0..11 * 360 {
        let t = i * 10; // a row every 10 s
        let hour = t / 3600;
        let app = if hour == 10 && i % 9 == 0 {
            "dropper.exe".to_string()
        } else {
            ["chrome.exe", "svchost.exe", "explorer.exe"][(i % 3) as usize].to_string()
        };
        writeln!(
            text,
            "2026-11-12 {:02}:{:02}:{:02},{app},id-{i},ws-01",
            hour,
            t / 60 % 60,
            t % 60
        )
        .unwrap();
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(256 << 20),
        IngestOptions {
            chunk_bytes: 16 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    (d, Store::open(&dir).unwrap())
}

fn rows(st: &Store, c: &ChunkCache, from: i64, to: i64) -> logseek_core::query::exec::MatchSet {
    let q = Query {
        filter: None,
        time: Some((from, to)),
        pipes: Vec::new(),
    };
    filter(st, &q, c, &AtomicBool::new(false)).unwrap()
}

#[test]
fn new_values_lead_with_the_columns_whose_values_came_back_before() {
    let (_d, st) = make();
    assert!(st.segments().len() > 2);
    let c = ChunkCache::new(Budget::new(64 << 20));
    let ten = (DAY0 + 10 * 3600) * S;
    let here = rows(&st, &c, ten, ten + 3600 * S);
    let before = rows(&st, &c, i64::MIN, ten);
    assert_eq!(here.count(), 360);
    // App, Id, Host (the time column is left to the caller)
    let out = new_values(
        &st,
        &here,
        &before,
        &[1, 2, 3],
        &FormulaSet::default(),
        &c,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!((out.here, out.before), (360, 3600));
    // only columns with something new; App (40 of 360 rows new) before Id (all new)
    let cols: Vec<usize> = out.columns.iter().map(|k| k.col).collect();
    assert_eq!(cols, [1, 2]);
    let app = &out.columns[0];
    assert_eq!((app.filled, app.new_rows), (360, 40));
    // before: three values in 3,600 rows (a column whose values come back); the id: one per row
    assert_eq!((app.before_values, app.before_filled), (3, 3600));
    assert_eq!(out.columns[1].before_filled, 3600);
    assert!(
        out.columns[1].before_values > 3000,
        "{}",
        out.columns[1].before_values
    );
    assert_eq!(app.values.len(), 1);
    assert_eq!(app.values[0].value, "dropper.exe");
    assert_eq!(app.values[0].rows, 40);
    let id = &out.columns[1];
    assert_eq!((id.filled, id.new_rows), (360, 360));
    assert_eq!(id.values.len(), 360);
    // nothing new in the hour before: a period compared with the same length just before it
    let nine = ten - 3600 * S;
    let quiet = new_values(
        &st,
        &rows(&st, &c, nine, ten),
        &rows(&st, &c, nine - 3600 * S, nine),
        &[1, 3],
        &FormulaSet::default(),
        &c,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert!(quiet.columns.is_empty());
}
