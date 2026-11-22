//! `cargo run --release --example formula_bench -- <store_dir> [low_col] [high_col]`
//! Made columns against the targets: a six-deep made column over a column with
//! few values and one with many, filtered cold (fresh cache) and warm, and pages
//! of 60 rows x 5 made columns.
use logseek_core::budget::{Budget, Machine};
use logseek_core::query::exec::{filter_with, page_with, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn deep(col: &str) -> String {
    format!(r#"upper(trim(replace(lower(concat(" ", {col}, "-x ")), "-x", ""))) & "!""#)
}

fn mb() -> f64 {
    process_memory().private as f64 / 1048576.0
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let low = a.get(2).map(String::as_str).unwrap_or("host");
    let high = a.get(3).map(String::as_str).unwrap_or("src_ip");
    let m = Machine::detect();
    let fresh = || ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let no = AtomicBool::new(false);
    println!(
        "{} rows in {} blocks; {} cores",
        st.row_count(),
        st.segments().len(),
        m.cores
    );
    let mut fs = FormulaSet::default();
    fs.add(&st, "low6", &deep(low)).unwrap();
    fs.add(&st, "high6", &deep(high)).unwrap();
    fs.add(&st, "u", "lower(user)").unwrap();
    fs.add(&st, "plen", "len(path)").unwrap();
    fs.add(&st, "kb", "bytes / 1024").unwrap();
    let ns = st.schema().len();
    // values that exist: the made columns on a row in the middle
    let mid = page_with(
        &st,
        &MatchSet::all(&st),
        st.row_count() / 2,
        1,
        &[ns, ns + 1],
        &fs,
        &fresh(),
    )
    .unwrap();
    let low_v = mid.cells[0][0].clone().unwrap();
    let high_v = mid.cells[0][1].clone().unwrap();
    for (k, (name, v, target_cold)) in [("low6", &low_v, 1.5), ("high6", &high_v, 4.0)]
        .into_iter()
        .enumerate()
    {
        let c = fresh();
        let q = parse(&format!("{name}=\"{v}\"")).unwrap();
        let before = fs.items[k].1.evals();
        let t = Instant::now();
        let r = filter_with(&st, &q, &fs, &c, &no).unwrap();
        let cold = t.elapsed().as_secs_f64();
        let evals = fs.items[k].1.evals() - before;
        // warm: the same question again (and twice more, as the cache settles)
        let mut warms = Vec::new();
        for _ in 0..3 {
            let t = Instant::now();
            let r2 = filter_with(&st, &q, &fs, &c, &no).unwrap();
            warms.push(t.elapsed().as_secs_f64());
            assert_eq!(r.count(), r2.count());
        }
        let warm = warms[0];
        println!(
            "  {name} warm runs: {}; cache limit {:.0} MB",
            warms
                .iter()
                .map(|w| format!("{w:.3} s"))
                .collect::<Vec<_>>()
                .join(", "),
            c.budget().cap() as f64 / 1048576.0
        );
        println!(
            "{name}: {} rows; cold {cold:.2} s (target {target_cold} s), warm {warm:.3} s (target 0.3 s); {evals} evaluations; RAM {:.0} MB",
            r.count(),
            mb()
        );
    }
    // pages of 60 rows x 5 made columns, warm, spread over the file
    let c = fresh();
    let all = MatchSet::all(&st);
    let cols: Vec<usize> = (ns..ns + 5).collect();
    let starts: Vec<u64> = (0..50).map(|i| st.row_count() / 50 * i).collect();
    for &s in &starts {
        page_with(&st, &all, s, 60, &cols, &fs, &c).unwrap();
    }
    let t = Instant::now();
    for &s in &starts {
        page_with(&st, &all, s, 60, &cols, &fs, &c).unwrap();
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / starts.len() as f64;
    println!(
        "page of 60 rows x 5 made columns: {per:.2} ms (target < 2 ms); RAM {:.0} MB",
        mb()
    );
    drop(c);
    println!("after the cache lets go: RAM {:.0} MB", mb());
}
