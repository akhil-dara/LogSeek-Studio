//! `cargo run --release --example newin -- <store_dir> [hours] [before|just]`
//! The last `hours` (default 24) of a file compared with everything before
//! (or the same length just before): what is new in it, and what grew most.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::ast::Query;
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet};
use logseek_core::query::rhythm::timeline_signals;
use logseek_core::store::Store;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let hours: i64 = a.get(2).and_then(|h| h.parse().ok()).unwrap_or(24);
    let just = a.get(3).map(String::as_str) == Some("just");
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let no = AtomicBool::new(false);
    let all = logseek_core::query::exec::MatchSet::all(&st);
    let s = timeline_signals(&st, &all, &cache, &no, &AtomicU64::new(0)).unwrap();
    let end = s.last + 1;
    let start = end - hours * 3_600_000_000;
    let base_from = if just {
        start - hours * 3_600_000_000
    } else {
        i64::MIN
    };
    let q = |t: (i64, i64)| Query {
        filter: None,
        time: Some(t),
        pipes: Vec::new(),
    };
    let here = filter(&st, &q((start, end)), &cache, &no).unwrap();
    let rest = filter(&st, &q((base_from, start)), &cache, &no).unwrap();
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let cols: Vec<usize> = (0..names.len())
        .filter(|&c| Some(c) != st.schema().time_col)
        .collect();
    let t = std::time::Instant::now();
    let nv = logseek_core::query::newin::new_values(
        &st,
        &here,
        &rest,
        &cols,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "last {hours} h: {} rows; compared with {}: {} rows; {:.1} s; {} columns with something new; RAM {:.0} MB",
        nv.here,
        if just { "the same length just before" } else { "everything before" },
        nv.before,
        t.elapsed().as_secs_f64(),
        nv.columns.len(),
        logseek_core::sysmem::process_memory().private as f64 / 1048576.0
    );
    for k in nv.columns.iter().take(12) {
        let vals: Vec<String> = k
            .values
            .iter()
            .take(4)
            .map(|v| {
                format!(
                    "{} ({})",
                    v.value.chars().take(60).collect::<String>(),
                    v.rows
                )
            })
            .collect();
        println!(
            "  before {} values in {} rows  {}: {} new in {} of {} rows: {}",
            k.before_values,
            k.before_filled,
            names[k.col],
            k.values.len(),
            k.new_rows,
            k.filled,
            vals.join(" | ")
        );
    }
}
