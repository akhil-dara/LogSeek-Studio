//! `cargo run --release --example pairs -- <store_dir> <column a> <column b>`
//! Times "Rare pairs" on a real index and prints the top of both lists.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{resolve_column, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::pairs::rare_pairs;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let (ca, cb) = (
        resolve_column(&st, &a[2]).unwrap(),
        resolve_column(&st, &a[3]).unwrap(),
    );
    let t = std::time::Instant::now();
    let p = rare_pairs(
        &st,
        &MatchSet::all(&st),
        ca,
        cb,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "{:.1} s; {} pairs; {} values of {}; RAM {:.0} MB",
        t.elapsed().as_secs_f64(),
        p.unusual.len(),
        p.a_values,
        a[2],
        process_memory().private as f64 / 1048576.0
    );
    let cut = |s: &Option<String>| -> String {
        s.clone()
            .unwrap_or_else(|| "(empty)".into())
            .chars()
            .take(60)
            .collect()
    };
    for l in p
        .unusual
        .iter()
        .filter(|l| l.a.is_some() && l.b.is_some())
        .take(6)
    {
        println!(
            "  {} with {}: {} of {} rows",
            cut(&l.a),
            cut(&l.b),
            l.rows,
            l.a_rows
        );
    }
    for l in p.peers.iter().filter(|l| l.b.is_some()).take(6) {
        println!(
            "  {}: seen with {} of {}; {} rows",
            cut(&l.b),
            l.peers,
            p.a_values,
            l.rows
        );
    }
}
