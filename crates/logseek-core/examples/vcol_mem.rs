//! Memory of a filter on formula (virtual) columns under a fixed budget.
//! `cargo run --release --example vcol_mem -- <store_dir> <columns.json> <budget_mib> "<query>"`
use logseek_core::budget::Budget;
use logseek_core::query::exec::{filter_with, ChunkCache, FormulaSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::AtomicBool;

fn mib(b: u64) -> u64 {
    b >> 20
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let cols: Vec<(String, String)> =
        serde_json::from_str(&std::fs::read_to_string(&a[2]).unwrap()).unwrap();
    let budget_mib: u64 = a[3].parse().unwrap();
    let mut fs = FormulaSet::default();
    for (n, src) in &cols {
        fs.add(&st, n, src).unwrap();
    }
    let cache = ChunkCache::new(Budget::new(budget_mib << 20));
    let m0 = process_memory();
    println!(
        "rows {} segments {} · before: private {} MiB, ws {} MiB",
        st.row_count(),
        st.segments().len(),
        mib(m0.private),
        mib(m0.working_set)
    );
    let t = std::time::Instant::now();
    let m = filter_with(
        &st,
        &parse(&a[4]).unwrap(),
        &fs,
        &cache,
        &AtomicBool::new(false),
    )
    .unwrap();
    let m1 = process_memory();
    println!(
        "{} → {} rows in {:.2} s · after: private {} MiB, ws {} MiB, peak ws {} MiB",
        a[4],
        m.count(),
        t.elapsed().as_secs_f64(),
        mib(m1.private),
        mib(m1.working_set),
        mib(m1.peak_working_set)
    );
}
