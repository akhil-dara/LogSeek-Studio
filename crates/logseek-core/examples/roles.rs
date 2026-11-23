//! `cargo run --release --example roles -- <store_dir>`: every column's role,
//! fill, distinct values in the sample, middle length and score, most telling first.
use logseek_core::budget::Budget;
use logseek_core::query::exec::ChunkCache;
use logseek_core::query::roles::{profile, smart_columns};
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let c = ChunkCache::new(Budget::new(256 << 20));
    let t = std::time::Instant::now();
    let mut p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    println!(
        "{} columns profiled in {:.2} s",
        p.len(),
        t.elapsed().as_secs_f64()
    );
    let smart = smart_columns(&p, 8);
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    println!(
        "smart: {:?}",
        smart.iter().map(|&i| names[i].as_str()).collect::<Vec<_>>()
    );
    p.sort_by(|a, b| b.score.total_cmp(&a.score));
    for x in p.iter().filter(|x| x.filled * 20 >= x.rows).take(40) {
        println!(
            "{:<32} {:?} fill {:>5.1}%  distinct {}/{}  len {}  score {:.3}  same_as {:?}",
            names[x.col],
            x.role,
            100.0 * x.filled as f64 / x.rows.max(1) as f64,
            x.distinct_sampled,
            x.sampled,
            x.median_len,
            x.score,
            x.same_as.map(|i| names[i].as_str())
        );
    }
}
