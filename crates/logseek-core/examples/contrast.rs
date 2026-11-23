//! `cargo run --release --example contrast -- <store_dir> "<rows of interest>" ["<compare with>"]`
//! Times "what sets these rows apart" on a real index and prints the answer.
use logseek_core::budget::Budget;
use logseek_core::query::contrast::{contrast, ContrastOpts};
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    // as the app does: starts small, may grow with what the machine has free
    let m = logseek_core::budget::Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    println!(
        "machine: {} cores, {:.1} GB free; may grow to {:.1} GB; {} blocks at once",
        m.cores,
        m.available_ram as f64 / 1e9,
        m.elastic_ceiling() as f64 / 1e9,
        logseek_core::query::exec::threads_for(&st, &cache)
    );
    let no = AtomicBool::new(false);
    let here = filter(&st, &parse(&a[2]).unwrap(), &cache, &no).unwrap();
    let rest = match a.get(3) {
        Some(q) => filter(&st, &parse(q).unwrap(), &cache, &no)
            .unwrap()
            .minus(&here),
        None => MatchSet::all(&st).minus(&here),
    };
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let cols: Vec<usize> = (0..names.len())
        .filter(|&c| Some(c) != st.schema().time_col)
        .collect();
    println!(
        "{} rows, {} columns; here {} rows, rest {} rows; RAM {:.0} MB",
        st.row_count(),
        cols.len(),
        here.count(),
        rest.count(),
        process_memory().private as f64 / 1048576.0
    );
    let done = AtomicU64::new(0);
    let peak = std::sync::Arc::new(AtomicU64::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let (p2, s2) = (peak.clone(), stop.clone());
    let watcher = std::thread::spawn(move || {
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            p2.fetch_max(
                process_memory().private,
                std::sync::atomic::Ordering::Relaxed,
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    let t = std::time::Instant::now();
    let c = contrast(
        &st,
        &here,
        &rest,
        &cols,
        &FormulaSet::default(),
        &cache,
        &ContrastOpts::default(),
        &no,
        &done,
    )
    .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
    println!(
        "{:.1} s, {} rows read, {} values counted exactly, peak RAM {:.0} MB, {} findings",
        t.elapsed().as_secs_f64(),
        done.load(std::sync::atomic::Ordering::Relaxed),
        c.candidates,
        peak.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1048576.0,
        c.findings.len()
    );
    for f in c.findings.iter().take(25) {
        let w: Vec<String> = f
            .conds
            .iter()
            .map(|k| match &k.value {
                Some(v) => format!("{} is {}", names[k.col], v),
                None => format!("{} is empty", names[k.col]),
            })
            .collect();
        println!(
            "{:>8.1}x  here {} of {} ({:.1}%)  rest {} of {} ({:.2}%)  {}",
            f.ratio(),
            f.here,
            f.here_total,
            f.share_here() * 100.0,
            f.rest,
            f.rest_total,
            f.share_rest() * 100.0,
            w.join(" and ")
        );
        for s in f.same.iter().filter(|s| !s.is_empty()) {
            println!("            same rows as one of them: {} more", s.len());
        }
        if let Some((_, alts)) = &f.instead {
            println!(
                "            same counts with {} others in place of one",
                alts.len()
            );
        }
    }
}
