//! Try Follow on a file: follow_try <file> <start query> <from col> <to col> [repeat]
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter_with, ChunkCache, FormulaSet};
use logseek_core::query::follow::{follow, FollowSpec, Window};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("x.lsidx");
    let job = start_ingest(
        a[0].as_ref(),
        &dir,
        Budget::new(512 << 20),
        IngestOptions::default(),
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let st = Store::open(&dir).unwrap();
    let cache = ChunkCache::new(Budget::new(256 << 20));
    let q = parse(&a[1]).unwrap();
    let start = filter_with(
        &st,
        &q,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
    )
    .unwrap();
    let spec = FollowSpec {
        from_col: a[2].clone(),
        to_col: a[3].clone(),
        also_to: a.get(5).map(|c| vec![c.clone()]).unwrap_or_default(),
        window: Window::Any,
        repeat: a.get(4).is_some(),
    };
    let t = std::time::Instant::now();
    let r = follow(&st, &start, &spec, &cache, &AtomicBool::new(false)).unwrap();
    println!(
        "{} rows in file, {} starting rows -> {} linked rows in {} steps, {:.0} ms",
        st.row_count(),
        start.count(),
        r.found(),
        r.levels.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    for (i, l) in r.levels.iter().enumerate() {
        println!("  step {}: {} rows", i + 1, l.len());
    }
}
