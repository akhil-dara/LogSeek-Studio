//! Timeline signals, read from the file's own times: silent gaps far longer
//! than its usual pauses, lasting steps in the rate of rows, and values of a
//! column whose rows come like clockwork.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::rhythm::{regular, timeline_signals};
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

const S: i64 = 1_000_000; // a second, in microseconds

/// Six hours from 00:00. Hours 0-3: a row every 4 s; hours 3-6: every 1 s
/// (a step up, 4 times). 05:00:00 to 05:39:59: nothing (a silent gap).
/// All the while host "beacon-7" every 60 s give or take 3 s.
fn make() -> (tempfile::TempDir, Store) {
    let mut rows: Vec<(i64, String)> = Vec::new();
    let mut lcg: u64 = 12345;
    let mut rnd = || {
        lcg = lcg
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (lcg >> 33) as i64
    };
    for t in 0..6 * 3600i64 {
        let busy = t >= 3 * 3600;
        let silent = (5 * 3600..5 * 3600 + 40 * 60).contains(&t);
        if silent {
            continue;
        }
        if busy || t % 4 == 0 {
            rows.push((t, format!("host-{}", rnd() % 40)));
        }
    }
    let mut t = 30i64;
    while t < 6 * 3600 {
        if !(5 * 3600..5 * 3600 + 40 * 60).contains(&t) {
            rows.push((t, "beacon-7".to_string()));
        }
        t += 60 + (rnd() % 7) - 3;
    }
    rows.sort();
    let mut text = String::from("Time,Host\n");
    for (t, h) in rows {
        writeln!(
            text,
            "2026-11-12 {:02}:{:02}:{:02},{h}",
            t / 3600,
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
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    let st = Store::open(&dir).unwrap();
    assert!(st.schema().time_col.is_some());
    (d, st)
}

fn at(h: i64, m: i64, s: i64) -> i64 {
    // 2026-11-12 00:00:00 UTC
    1_794_441_600 * S + (h * 3600 + m * 60 + s) * S
}

#[test]
fn the_silent_gap_and_the_step_are_found_on_the_file_s_own_terms() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let s = timeline_signals(
        &st,
        &MatchSet::all(&st),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    // the longest silence first: from the last row before 05:00 to the first after 05:40
    let g = &s.gaps[0];
    assert_eq!((g.from, g.to), (at(4, 59, 59), at(5, 40, 0)));
    // nothing else is anywhere near as long
    assert!(
        s.gaps.iter().skip(1).all(|x| x.to - x.from < 60 * S),
        "{:?}",
        &s.gaps[..s.gaps.len().min(5)]
    );
    assert!(s.usual_pause <= 4 * S);
    // the step up at 03:00, about four times the rate
    let up: Vec<_> = s.steps.iter().filter(|x| x.after > x.before).collect();
    assert!(!up.is_empty(), "{:?}", s.steps);
    let first = up[0];
    assert!(
        (first.at - at(3, 0, 0)).abs() <= 2 * s.step_width,
        "{} vs {}",
        first.at,
        at(3, 0, 0)
    );
    let ratio = first.after / first.before;
    assert!((3.0..=5.0).contains(&ratio), "{ratio}");
    // the silence itself is not mistaken for a lasting change of rate
    assert!(s.steps.iter().all(|x| x.after > 0.0 && x.before > 0.0));
}

#[test]
fn a_value_whose_rows_come_like_clockwork_comes_first() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let r = regular(
        &st,
        &MatchSet::all(&st),
        st.schema().index_of("Host").unwrap(),
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    let b = &r.values[0];
    assert_eq!(b.value.as_deref(), Some("beacon-7"));
    assert!((59 * S..=61 * S).contains(&b.every), "{}", b.every);
    assert!(b.give_or_take <= 3 * S, "{}", b.give_or_take);
    // the busy, irregular hosts score well below it
    assert!(
        r.values[1].score + 0.2 < b.score,
        "{} vs {}",
        r.values[1].score,
        b.score
    );
    // every value with enough rows to judge is listed; fewer are counted apart
    assert_eq!(r.values.len() + r.too_few, 41);
}

/// A store of one row per given second (seconds from 2026-11-01 00:00:00; under 29 days).
fn store_of(secs: &[i64]) -> (tempfile::TempDir, Store) {
    let mut text = String::from("Time,N\n");
    for (i, t) in secs.iter().enumerate() {
        writeln!(
            text,
            "2026-11-{:02} {:02}:{:02}:{:02},{i}",
            1 + t / 86_400,
            t / 3600 % 24,
            t / 60 % 60,
            t % 60
        )
        .unwrap();
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(256 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    let st = Store::open(&dir).unwrap();
    assert!(st.schema().time_col.is_some());
    (d, st)
}

#[test]
fn bursts_on_a_beat_are_not_silences() {
    // two hours of bursts: three rows in three seconds, every 23 seconds;
    // once, half an hour with nothing
    let mut secs = Vec::new();
    let mut t = 0i64;
    while t < 2 * 3600 {
        if !(3600..5400).contains(&t) {
            secs.extend([t, t + 1, t + 2]);
        }
        t += 23;
    }
    let (_d, st) = store_of(&secs);
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let s = timeline_signals(
        &st,
        &MatchSet::all(&st),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!(
        s.gaps.len(),
        1,
        "gaps: {:?}",
        s.gaps
            .iter()
            .map(|g| (g.to - g.from) / S)
            .collect::<Vec<_>>()
    );
    let g = &s.gaps[0];
    assert!((g.to - g.from) / S >= 1800, "{}", (g.to - g.from) / S);
}

#[test]
fn a_burst_between_two_silences_is_no_step() {
    // a row every 10 s for five days, a day and a half of nothing, 200 rows in
    // four seconds, a day and a half of nothing, then a row every 10 s again
    let day = 86_400i64;
    let mut secs: Vec<i64> = (0..5 * day).step_by(10).collect();
    let mid = 5 * day + day * 3 / 2;
    secs.extend((0..200).map(|i| mid + i % 4));
    secs.sort();
    secs.extend((8 * day..13 * day).step_by(10));
    let (_d, st) = store_of(&secs);
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let s = timeline_signals(
        &st,
        &MatchSet::all(&st),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!(s.gaps.len(), 2);
    assert!(
        s.steps.is_empty(),
        "steps: {:?}",
        s.steps
            .iter()
            .map(|x| (x.before, x.after))
            .collect::<Vec<_>>()
    );
}
