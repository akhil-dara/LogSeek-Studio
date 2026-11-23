//! An absurd time in one row must not take the engine down: the span of the
//! file's times is measured with checked arithmetic.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, MatchSet};
use logseek_core::query::rhythm::timeline_signals;
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn make(extreme: i64) -> (tempfile::TempDir, Store) {
    let mut text = String::from("timestamp,msg\n");
    for i in 0..200_000i64 {
        writeln!(text, "{},m", 1_794_564_000_000i64 + i * 1000).unwrap();
    }
    // lands in a later chunk, after the column was taken as epoch milliseconds
    writeln!(text, "{extreme},x").unwrap();
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(256 << 20),
        IngestOptions {
            chunk_bytes: 4 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    let st = Store::open(&dir).unwrap();
    (d, st)
}

fn run(st: &Store) -> std::thread::Result<logseek_core::error::Result<()>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let cache = ChunkCache::new(Budget::new(64 << 20));
        timeline_signals(
            st,
            &MatchSet::all(st),
            &cache,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
        )
        .map(|_| ())
    }))
}

#[test]
fn a_span_wider_than_i64_does_not_panic() {
    let (_d, st) = make(-9_300_000_000_000_000);
    assert_eq!(st.schema().time_unit_us, Some(1000));
    let r = run(&st);
    assert!(r.is_ok(), "the engine thread panicked");
    // a plain, readable error is fine; so is a result
    if let Ok(Err(e)) = r {
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn a_huge_but_representable_span_still_answers() {
    // about 290 years of milliseconds: wide, but it fits
    let (_d, st) = make(-9_000_000_000_000);
    let r = run(&st);
    assert!(matches!(r, Ok(Ok(()))), "{r:?}");
}
