//! `cargo run --release --example plain -- <store_dir> "<plain words>"...`
//! Times reading a real index into words, then recasts each phrase.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet};
use logseek_core::query::parse;
use logseek_core::query::plain::{build_lexicon, recast};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::AtomicBool;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let no = AtomicBool::new(false);
    let t = std::time::Instant::now();
    let lex = build_lexicon(&st, &FormulaSet::default(), &cache, &no).unwrap();
    println!(
        "lexicon: {:.1} s, {} columns, {} values, RAM {:.0} MB",
        t.elapsed().as_secs_f64(),
        lex.cols.len(),
        lex.values(),
        process_memory().private as f64 / 1048576.0
    );
    for q in &a[2..] {
        let t = std::time::Instant::now();
        let r = recast(q, &lex);
        let took = t.elapsed().as_secs_f64() * 1000.0;
        let words: Vec<String> = r
            .parts
            .iter()
            .map(|p| {
                if p.others.is_empty() {
                    p.words.clone()
                } else {
                    format!("{} (or {} more)", p.words, p.others.len())
                }
            })
            .collect();
        let n = filter(&st, &parse(&r.query()).unwrap(), &cache, &no)
            .map(|m| m.count().to_string())
            .unwrap_or_else(|e| e.to_string());
        println!(
            "{q:?} -> {} [{:.2} ms] => {} rows",
            words.join(" · "),
            took,
            n
        );
        if let Some(s) = &r.summary {
            println!(
                "   summary: {} {:?} by {:?} per {:?}",
                s.measure, s.column, s.by, s.per
            );
        }
    }
}
