//! `cargo run --release --example unusual -- <store_dir> ["<query>"]`
//! Times "Unusual rows" on a real index and prints the most unusual rows' reasons.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{filter, page, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::query::unusual::{unusual_by, Weigh};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let no = AtomicBool::new(false);
    let what = if a.get(2).map(String::as_str) == Some("together") {
        Weigh::Together
    } else {
        Weigh::All
    };
    let rows = match a.get(2).filter(|q| q.as_str() != "together") {
        Some(q) => filter(&st, &parse(q).unwrap(), &cache, &no).unwrap(),
        None => MatchSet::all(&st),
    };
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let cols: Vec<usize> = (0..names.len()).collect();
    let peak = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (p2, s2) = (peak.clone(), stop.clone());
    let w = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            p2.fetch_max(process_memory().private, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    let t = std::time::Instant::now();
    let u = unusual_by(
        &st,
        &rows,
        &cols,
        None,
        what,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    let weighed = cols.iter().filter(|&&c| u.model.weight(c) > 0.0).count();
    println!(
        "{:.1} s; {} rows; usual {:.1}, line {:.1}; {} rows at or above; {} columns weighed, {} left out",
        t.elapsed().as_secs_f64(),
        rows.count(),
        u.usual,
        u.line,
        u.rows.len(),
        weighed,
        u.model.left_out().len()
    );
    let jc: Vec<&str> = u
        .model
        .joint_columns()
        .iter()
        .map(|&c| names[c].as_str())
        .collect();
    println!("numbers read together: {} columns: {:?}", jc.len(), jc);
    let all = MatchSet::all(&st);
    for &r in u.rows.iter().take(8) {
        let p = page(&st, &all, r, 1, &cols, &cache).unwrap();
        let why = u.model.why(&p.cells[0]);
        let s: Vec<String> = why
            .iter()
            .take(3)
            .map(|x| {
                let v: String = x
                    .value
                    .clone()
                    .unwrap_or_else(|| "(empty)".into())
                    .chars()
                    .take(50)
                    .collect();
                format!(
                    "{} = {} ({} of {}, {:.1})",
                    names[x.col], v, x.rows, x.of, x.bits
                )
            })
            .collect();
        println!("  row {r}: {}", s.join("  |  "));
    }
    stop.store(true, Ordering::Relaxed);
    w.join().unwrap();
    println!(
        "peak RAM {:.0} MB",
        peak.load(Ordering::Relaxed) as f64 / 1048576.0
    );
}
