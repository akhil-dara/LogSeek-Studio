//! `cargo run --release --example links -- <store_dir>`: which columns point at which.
use logseek_core::budget::Budget;
use logseek_core::query::exec::ChunkCache;
use logseek_core::query::links::linked_columns;
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;

fn main() {
    let dir = std::env::args().nth(1).expect("store dir");
    let st = Store::open(std::path::Path::new(&dir)).unwrap();
    let c = ChunkCache::new(Budget::new(128 << 20));
    let t = std::time::Instant::now();
    let links = linked_columns(&st, &c, &AtomicBool::new(false)).unwrap();
    println!(
        "{} links in {:.2} s",
        links.len(),
        t.elapsed().as_secs_f64()
    );
    let n = |i: usize| st.schema().columns[i].name.clone();
    for l in &links {
        println!("{:>5.1}%  {} -> {}", l.share * 100.0, n(l.from), n(l.to));
    }
}
