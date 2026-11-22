//! Runs a made column over an index and prints timing and a sample.
use logseek_core::budget::Budget;
use logseek_core::query::exec::{filter_with, page_with, ChunkCache, FormulaSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let mut fs = FormulaSet::default();
    let idx = fs
        .add(&st, &a[2], &a[3])
        .unwrap_or_else(|e| panic!("{e:?}"));
    let c = ChunkCache::new(Budget::new(256 << 20));
    let t = std::time::Instant::now();
    let m = filter_with(
        &st,
        &parse(&a[4]).unwrap(),
        &fs,
        &c,
        &AtomicBool::new(false),
    )
    .unwrap();
    let p = page_with(&st, &m, 0, 3, &[idx], &fs, &c).unwrap();
    println!(
        "{:.2}s  {} matches  sample {:?}  | {} = {}  where {}",
        t.elapsed().as_secs_f64(),
        m.count(),
        p.cells.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        a[2],
        a[3],
        a[4]
    );
}
