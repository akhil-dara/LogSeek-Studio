//! Following links between rows: from a row to the rows that refer to it.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, MatchSet};
use logseek_core::query::follow::{follow, FollowSpec, Window};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::AtomicBool;

// row: time, pid, ppid, name
const ROWS: &[(&str, &str, &str, &str)] = &[
    ("10:00:00", "100", "1", "sshd"),    // 0
    ("10:00:05", "200", "100", "bash"),  // 1  child of 0
    ("10:00:06", "300", "200", "curl"),  // 2  grandchild
    ("10:30:00", "400", "100", "late"),  // 3  child of 0, 30 minutes later
    ("10:00:07", "500", "999", "other"), // 4  unrelated
    ("10:00:08", "600", "", "noparent"), // 5  no parent value
    ("10:00:09", "700", "800", "loopa"), // 6  700 <- 800
    ("10:00:10", "800", "700", "loopb"), // 7  800 <- 700 (a loop)
];

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("p.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "time,pid,ppid,name").unwrap();
    for (t, p, pp, n) in ROWS {
        writeln!(f, "2026-11-12 {t},{p},{pp},{n}").unwrap();
    }
    drop(f);
    let dir = d.path().join("p.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (d, Store::open(&dir).unwrap())
}

fn run(
    st: &Store,
    start: &[u64],
    from: &str,
    to: &str,
    window: Window,
    repeat: bool,
) -> Vec<Vec<(u64, u64)>> {
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let s = MatchSet::all(st).restrict(st, start);
    let spec = FollowSpec {
        from_col: from.into(),
        to_col: to.into(),
        also_to: Vec::new(),
        window,
        repeat,
    };
    follow(st, &s, &spec, &cache, &AtomicBool::new(false))
        .unwrap()
        .levels
}

#[test]
fn one_step_finds_the_rows_whose_column_equals_the_starting_rows_column() {
    let (_d, st) = store();
    // children of sshd: rows whose ppid equals its pid
    let l = run(&st, &[0], "pid", "ppid", Window::Any, false);
    assert_eq!(l, vec![vec![(1, 0), (3, 0)]]);
}

#[test]
fn keep_following_builds_the_whole_tree_and_stops_on_loops() {
    let (_d, st) = store();
    let l = run(&st, &[0], "pid", "ppid", Window::Any, true);
    assert_eq!(l, vec![vec![(1, 0), (3, 0)], vec![(2, 1)]]);
    // a loop in the data ends: 700 -> 800 -> (700 is where we started)
    let l = run(&st, &[6], "pid", "ppid", Window::Any, true);
    assert_eq!(l, vec![vec![(7, 6)]]);
}

#[test]
fn swapping_the_columns_walks_up_to_the_parents() {
    let (_d, st) = store();
    let l = run(&st, &[2], "ppid", "pid", Window::Any, true);
    assert_eq!(l, vec![vec![(1, 2)], vec![(0, 1)]]);
}

#[test]
fn a_time_window_keeps_only_rows_near_their_starting_row() {
    let (_d, st) = store();
    let min = 60 * 1_000_000;
    let l = run(&st, &[0], "pid", "ppid", Window::After(5 * min), false);
    assert_eq!(l, vec![vec![(1, 0)]]);
    // "before" a row finds nothing later than it
    let l = run(&st, &[0], "pid", "ppid", Window::Before(5 * min), false);
    assert!(l.is_empty());
}

#[test]
fn empty_values_never_link_and_several_starting_rows_work_together() {
    let (_d, st) = store();
    // row 5 has no parent value: it is nobody's child; nothing has an empty pid
    let l = run(&st, &[5], "ppid", "pid", Window::Any, true);
    assert!(l.is_empty());
    let l = run(&st, &[0, 6], "pid", "ppid", Window::Any, false);
    assert_eq!(l, vec![vec![(1, 0), (3, 0), (7, 6)]]);
}

#[test]
fn a_link_on_either_of_two_columns_finds_children_and_what_each_process_did() {
    // Children point back with ppid, other events with ctx
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("cs.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "time,event,pid,ppid,ctx").unwrap();
    writeln!(f, "2026-11-12 10:00:00,ProcessRollup2,100,1,").unwrap(); // 0 bash
    writeln!(f, "2026-11-12 10:00:01,ProcessRollup2,200,100,").unwrap(); // 1 child
    writeln!(f, "2026-11-12 10:00:02,NetworkConnectIP4,,,100").unwrap(); // 2 bash's connection
    writeln!(f, "2026-11-12 10:00:03,DnsRequest,,,200").unwrap(); // 3 the child's lookup
    writeln!(f, "2026-11-12 10:00:04,DnsRequest,,,999").unwrap(); // 4 someone else's
    drop(f);
    let dir = d.path().join("cs.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = Store::open(&dir).unwrap();
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let s = MatchSet::all(&st).restrict(&st, &[0]);
    let spec = FollowSpec {
        from_col: "pid".into(),
        to_col: "ppid".into(),
        also_to: vec!["ctx".into()],
        window: Window::Any,
        repeat: true,
    };
    let r = follow(&st, &s, &spec, &cache, &AtomicBool::new(false)).unwrap();
    assert_eq!(r.levels, vec![vec![(1, 0), (2, 0)], vec![(3, 1)]]);
}

#[test]
fn stepping_through_a_set_of_rows_and_finding_their_places() {
    use logseek_core::query::exec::{next_row, rank_of};
    let (_d, st) = store();
    let all = MatchSet::all(&st);
    let some = all.restrict(&st, &[1, 4, 6]);
    // forward, from nothing, and wrapping round at the end
    assert_eq!(next_row(&st, &some, None, true), Some(1));
    assert_eq!(next_row(&st, &some, Some(1), true), Some(4));
    assert_eq!(next_row(&st, &some, Some(6), true), Some(1));
    // backward, wrapping round at the start
    assert_eq!(next_row(&st, &some, Some(4), false), Some(1));
    assert_eq!(next_row(&st, &some, Some(1), false), Some(6));
    assert_eq!(next_row(&st, &some, None, false), Some(6));
    // an empty set has no next row
    assert_eq!(next_row(&st, &all.restrict(&st, &[]), None, true), None);
    // a row's place: how many rows of the set come before it
    assert_eq!(rank_of(&st, &some, 4), 1);
    assert_eq!(rank_of(&st, &all, 6), 6);
    assert_eq!(rank_of(&st, &some, 0), 0);
}
