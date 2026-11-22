//! `cargo run --release --example query -- <store_dir> "<lsql>" ...`
use logseek_core::budget::Budget;
use logseek_core::query::exec::{filter, page, ChunkCache, PROF};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::AtomicBool;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let cache = ChunkCache::new(Budget::new(256 << 20));
    println!(
        "store: {} rows, {} segments",
        st.row_count(),
        st.segments().len()
    );
    for q in &a[2..] {
        for pass in ["cold", "warm"] {
            for p in PROF.iter() {
                p.store(0, std::sync::atomic::Ordering::Relaxed);
            }
            let t = std::time::Instant::now();
            let m = match filter(&st, &parse(q).unwrap(), &cache, &AtomicBool::new(false)) {
                Ok(m) => m,
                Err(e) => {
                    println!("{q}: {e}");
                    break;
                }
            };
            let tf = t.elapsed().as_secs_f64();
            let p = page(&st, &m, 0, 40, &[0, 1, 2], &cache).unwrap();
            println!("{pass:4} {:>9.3}s  {:>10} matches  pruned {}/{}  first page {} rows  RAM {:.0} MB  | {q}",
                tf, m.count(), m.stats.segments_pruned, m.stats.segments_total, p.rows.len(), process_memory().private as f64 / 1048576.0);
            let s: Vec<String> = PROF
                .iter()
                .map(|p| {
                    format!(
                        "{:.2}",
                        p.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
                    )
                })
                .collect();
            println!(
                "      thread-seconds: raw-read {} raw-search {} decode {} match {}",
                s[0], s[1], s[2], s[3]
            );
        }
    }
}
