//! Cost of one page (block of matches) at several places of a result.
//! `cargo run --release --example page_cost -- <store_dir> "<query>" <rows>`
use logseek_core::budget::Budget;
use logseek_core::query::exec::{filter, page, ChunkCache, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let rows: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(340);
    let cache = ChunkCache::new(Budget::new(150 << 20));
    let m = if a[2].is_empty() {
        MatchSet::all(&st)
    } else {
        filter(&st, &parse(&a[2]).unwrap(), &cache, &AtomicBool::new(false)).unwrap()
    };
    let cols: Vec<usize> = (0..st.schema().len()).collect();
    println!("matches {}", m.count());
    let mut at = 0u64;
    let step = (rows as u64).max(1);
    for i in 0..12 {
        let t = Instant::now();
        let p = page(&st, &m, at, rows, &cols, &cache).unwrap();
        println!(
            "page {i} at {at}: {} rows in {:.1} ms",
            p.rows.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );
        at += step;
    }
}
