//! `cargo run --release --example colprof -- <store_dir> <column> ["<query>"]`
//! Times "Shapes in this column" and "Strange text in this column" on a real index.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{filter, resolve_column, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::query::shape::{column_shapes, shape_words, Shape};
use logseek_core::query::strange::strange_text;
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
    let rows = match a.get(3) {
        Some(q) => filter(&st, &parse(q).unwrap(), &cache, &no).unwrap(),
        None => MatchSet::all(&st),
    };
    let col = resolve_column(&st, &a[2]).unwrap();
    let peak = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (p2, s2) = (peak.clone(), stop.clone());
    let w = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            p2.fetch_max(process_memory().private, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    println!(
        "{} rows of {}; column {}",
        rows.count(),
        st.row_count(),
        a[2]
    );
    let t = std::time::Instant::now();
    let (shapes, empty) = column_shapes(
        &st,
        &rows,
        col,
        Shape::AnyLength,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "shapes: {:.1} s, {} shapes, {} empty",
        t.elapsed().as_secs_f64(),
        shapes.len(),
        empty
    );
    for s in shapes.iter().rev().take(5) {
        println!(
            "  {:>8} rows  {}",
            s.rows,
            shape_words(&s.pattern).unwrap_or_else(|| "(plain text)".into())
        );
    }
    let t = std::time::Instant::now();
    let s = strange_text(
        &st,
        &rows,
        col,
        None,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "strange: {:.1} s, usual {:.1} bits, line {:.1}, {} values in {} rows",
        t.elapsed().as_secs_f64(),
        s.usual,
        s.line,
        s.values.len(),
        s.rows.len()
    );
    for v in s.values.iter().take(8) {
        let short: String = v.text.chars().take(140).collect();
        println!("  {:.1} bits  {} rows  {short}", v.score, v.rows);
    }
    let t = std::time::Instant::now();
    let p = logseek_core::query::patterns::patterns(
        &st,
        &rows,
        col,
        &Default::default(),
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "patterns: {:.1} s, {} patterns, {} empty",
        t.elapsed().as_secs_f64(),
        p.patterns.len(),
        p.empty
    );
    for x in p
        .patterns
        .iter()
        .take(6)
        .chain(p.patterns.iter().rev().take(4))
    {
        let w: String = logseek_core::query::patterns::pattern_words(x)
            .chars()
            .take(150)
            .collect();
        println!("  {:>8} rows  {w}", x.rows);
    }
    stop.store(true, Ordering::Relaxed);
    w.join().unwrap();
    println!(
        "peak RAM {:.0} MB",
        peak.load(Ordering::Relaxed) as f64 / 1048576.0
    );
}
