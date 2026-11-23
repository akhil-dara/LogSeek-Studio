//! Times the aggregation functions (top, stats, timechart) on an index.
use logseek_core::budget::Budget;
use logseek_core::query::agg::{run_stats, timechart, top, top_k_sorted};
use logseek_core::query::ast::{Agg, AggFn};
use logseek_core::query::exec::{ChunkCache, MatchSet};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::AtomicBool;
fn main() {
    let st = Store::open(std::path::Path::new(&std::env::args().nth(1).unwrap())).unwrap();
    let c = ChunkCache::new(Budget::new(256 << 20));
    let no = AtomicBool::new(false);
    let all = MatchSet::all(&st);
    let cnt = Agg {
        func: AggFn::Count,
        col: None,
    };
    let t = std::time::Instant::now();
    let r = top(&st, &all, 5, "Label", &c, &no).unwrap();
    println!(
        "top Label              {:.2}s  {:?}",
        t.elapsed().as_secs_f64(),
        r.iter()
            .map(|g| (g.0[0].clone(), g.1[0] as u64))
            .collect::<Vec<_>>()
    );
    let t = std::time::Instant::now();
    let r = run_stats(
        &st,
        &all,
        &[
            cnt.clone(),
            Agg {
                func: AggFn::Avg,
                col: Some("Flow Duration".into()),
            },
        ],
        &["Protocol".into()],
        &c,
        &no,
    )
    .unwrap();
    println!(
        "stats count,avg by Protocol {:.2}s  {} groups",
        t.elapsed().as_secs_f64(),
        r.len()
    );
    let t = std::time::Instant::now();
    let r = run_stats(
        &st,
        &all,
        &[Agg {
            func: AggFn::Dc,
            col: Some("Src IP".into()),
        }],
        &[],
        &c,
        &no,
    )
    .unwrap();
    println!(
        "dc(Src IP)             {:.2}s  {}",
        t.elapsed().as_secs_f64(),
        r.get(0).unwrap().1[0]
    );
    let t = std::time::Instant::now();
    let r = run_stats(&st, &all, &[cnt.clone()], &["Flow ID".into()], &c, &no).unwrap();
    println!(
        "count by Flow ID       {:.2}s  {} groups  passes {}  spilled {}  RAM {:.0} MB",
        t.elapsed().as_secs_f64(),
        r.len(),
        r.passes,
        r.spilled(),
        process_memory().private as f64 / 1048576.0
    );
    let t = std::time::Instant::now();
    let s = timechart(&st, &all, 3_600_000_000, &cnt, None, &c, &no).unwrap();
    println!(
        "timechart 1h           {:.2}s  {} buckets",
        t.elapsed().as_secs_f64(),
        s.points.len()
    );
    let t = std::time::Instant::now();
    let rows = top_k_sorted(&st, &all, &[("Flow Byts/s".into(), true)], 100, &c, &no).unwrap();
    println!(
        "sort -Flow Byts/s top100 {:.2}s  first row {}",
        t.elapsed().as_secs_f64(),
        rows[0]
    );
}
