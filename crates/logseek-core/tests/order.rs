//! Whether a file's rows run in time order (oldest first or newest first, a
//! few rows a little out of place allowed), so a gap between two rows next to
//! each other means a real silence, and a time is a place in the rows.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::ChunkCache;
use logseek_core::query::order::{judge, time_order, Direction};
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::AtomicBool;

const S: i64 = 1_000_000;

#[test]
fn rows_in_order_either_way_and_a_little_jitter_allowed() {
    let up: Vec<i64> = (0..1000).map(|i| i * S).collect();
    let o = judge(up.iter().copied());
    assert_eq!(o.direction, Direction::OldestFirst);
    assert_eq!(o.slack_us, 0);
    let down: Vec<i64> = up.iter().rev().copied().collect();
    assert_eq!(judge(down.into_iter()).direction, Direction::NewestFirst);
    // threads writing a few seconds out of turn: still in order, with that slack
    let mut jitter = up.clone();
    jitter.swap(100, 109);
    let o = judge(jitter.into_iter());
    assert_eq!(o.direction, Direction::OldestFirst);
    assert_eq!(o.slack_us, 9 * S);
}

#[test]
fn rows_jumping_hours_back_and_forth_are_not_in_order() {
    // an export in no time order: 22:11, 21:55, 23:54, 17:31, ...
    let h = 3600 * S;
    let t = [
        22 * h + 11 * 60 * S,
        21 * h + 55 * 60 * S,
        23 * h + 54 * 60 * S,
        17 * h + 31 * 60 * S,
    ];
    let rows: Vec<i64> = (0..400).map(|i| t[i % 4] + (i as i64 / 4) * S).collect();
    assert_eq!(judge(rows.into_iter()).direction, Direction::None);
}

#[test]
fn the_check_reads_the_file_and_stops_early_when_it_can() {
    let d = tempfile::tempdir().unwrap();
    let mut sorted = String::from("time,v\n");
    let mut shuffled = String::from("time,v\n");
    for i in 0..20_000u32 {
        writeln!(
            sorted,
            "2026-11-12 {:02}:{:02}:{:02},{i}",
            i / 3600 % 24,
            i / 60 % 60,
            i % 60
        )
        .unwrap();
        // the same times, the hours dealt out of turn
        let k = (i * 7919) % 20_000;
        writeln!(
            shuffled,
            "2026-11-12 {:02}:{:02}:{:02},{i}",
            k / 3600 % 24,
            k / 60 % 60,
            k % 60
        )
        .unwrap();
    }
    let open = |name: &str, text: &str| {
        let src = d.path().join(format!("{name}.csv"));
        std::fs::write(&src, text).unwrap();
        let dir = d.path().join(format!("{name}.lsidx"));
        let job = start_ingest(
            &src,
            &dir,
            Budget::new(64 << 20),
            IngestOptions {
                chunk_bytes: 64 * 1024,
                ..Default::default()
            },
        );
        while matches!(job.state(), JobState::Running) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Store::open(&dir).unwrap()
    };
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let a = open("sorted", &sorted);
    assert!(a.segments().len() > 1, "several blocks");
    let o = time_order(&a, &cache, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(o.direction, Direction::OldestFirst);
    let b = open("shuffled", &shuffled);
    let o = time_order(&b, &cache, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(o.direction, Direction::None);
}
