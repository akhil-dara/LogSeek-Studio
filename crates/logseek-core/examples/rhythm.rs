//! `cargo run --release --example rhythm -- <store_dir> [<column for regular repeats>...]`
//! Times timeline signals on a real index and prints what they found.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{resolve_column, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::rhythm::{regular, timeline_signals};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn when(us: i64) -> String {
    logseek_core::types::fmt_time(us)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let all = MatchSet::all(&st);
    let no = AtomicBool::new(false);
    let t = std::time::Instant::now();
    let s = timeline_signals(&st, &all, &cache, &no, &AtomicU64::new(0)).unwrap();
    println!(
        "signals: {:.1} s; {} to {}; usual pause {} s, gap line {} s; {} gaps, {} steps; RAM {:.0} MB",
        t.elapsed().as_secs_f64(),
        when(s.first),
        when(s.last),
        s.usual_pause / 1_000_000,
        s.gap_line / 1_000_000,
        s.gaps.len(),
        s.steps.len(),
        process_memory().private as f64 / 1048576.0
    );
    for g in s.gaps.iter().take(6) {
        println!(
            "  gap {} min: {} to {}",
            (g.to - g.from) / 60_000_000,
            when(g.from),
            when(g.to)
        );
    }
    for x in s.steps.iter().take(8) {
        println!(
            "  step at {}: {:.2} -> {:.2} rows/s",
            when(x.at),
            x.before,
            x.after
        );
    }
    for c in &a[2..] {
        let t = std::time::Instant::now();
        let r = regular(
            &st,
            &all,
            resolve_column(&st, c).unwrap(),
            &FormulaSet::default(),
            &cache,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap();
        println!(
            "regular {c}: {:.1} s; {} values, {} too few",
            t.elapsed().as_secs_f64(),
            r.values.len(),
            r.too_few
        );
        for v in r.values.iter().take(6) {
            let name: String = v
                .value
                .clone()
                .unwrap_or_else(|| "(empty)".into())
                .chars()
                .take(70)
                .collect();
            println!(
                "  {:.2}  every {} s +/- {} s  {} rows  {name}",
                v.score,
                v.every / 1_000_000,
                v.give_or_take / 1_000_000,
                v.rows
            );
        }
    }
}
