//! Investigator's console: runs a full query in the app's query language including its pipes.
//! `cargo run --release --example hunt -- <store_dir> "<lsql>" [cols] [limit]`
//! - no pipes: matching rows sorted by the time column (ascending), `cols` printed
//! - `| top N col` / `| stats count[,dc(x)] by a,b`: grouped counts
//! - `| sort -col` / `| fields a,b` / `| head N` honoured for row output
//! - query `schema` prints columns with types and fill counts
use logseek_core::budget::Budget;
use logseek_core::query::agg::{run_stats, top, top_k_sorted};
use logseek_core::query::ast::{Expr, Pipe};
use logseek_core::query::exec::{filter, page, resolve_column, ChunkCache, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::sync::atomic::AtomicBool;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = Store::open(std::path::Path::new(&a[1])).unwrap();
    let cache = ChunkCache::new(Budget::new(256 << 20));
    let no = AtomicBool::new(false);
    if a[2] == "schema" {
        for (i, c) in st.schema().columns.iter().enumerate() {
            println!("{i:>3} {:<45} {:?} filled={}", c.name, c.ty, c.count);
        }
        println!(
            "time_col={:?} rows={}",
            st.schema().time_col,
            st.row_count()
        );
        return;
    }
    if a[2] == "row" {
        // every non-empty cell of the given row ids
        for r in a[3].split(',') {
            let r: u64 = r.trim().parse().unwrap();
            println!("--- row {r}");
            for (i, c) in st.schema().columns.iter().enumerate() {
                if let Some(v) = st.cell_text(r, i).unwrap().filter(|v| !v.is_empty()) {
                    println!("  {:<40} {}", c.name, v.replace('\n', "\\n"));
                }
            }
        }
        return;
    }
    let mut q = match parse(&a[2]) {
        Ok(q) => q,
        Err(e) => {
            println!("parse error at {:?}: {}", e.span, e.msg);
            return;
        }
    };
    // fold `| where` pipes into the filter
    let mut rest = Vec::new();
    for p in std::mem::take(&mut q.pipes) {
        match p {
            Pipe::Where(e) => {
                q.filter = Some(match q.filter.take() {
                    Some(f) => Expr::And(Box::new(f), Box::new(e)),
                    None => e,
                })
            }
            o => rest.push(o),
        }
    }
    let t = std::time::Instant::now();
    let m: MatchSet = if q.filter.is_none() && q.time.is_none() {
        MatchSet::all(&st)
    } else {
        match filter(&st, &q, &cache, &no) {
            Ok(m) => m,
            Err(e) => {
                println!("error: {e}");
                return;
            }
        }
    };
    println!("{} matches in {:.3}s", m.count(), t.elapsed().as_secs_f64());
    let mut cols: Vec<String> = a
        .get(3)
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| {
            [
                "Event Time",
                "Action Type",
                "File Name",
                "Process Command Line",
            ]
            .map(String::from)
            .to_vec()
        });
    let mut limit: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(50);
    let tc = st
        .schema()
        .time_col
        .map(|i| st.schema().columns[i].name.clone());
    let mut sort: Vec<(String, bool)> = tc.iter().map(|c| (c.clone(), false)).collect();
    for p in rest {
        match p {
            Pipe::Top { n, col } => {
                let r = top(&st, &m, n, &col, &cache, &no).unwrap();
                for (k, v) in r.iter() {
                    println!("{:>9}  {}", v[0], k.join(" | "));
                }
                return;
            }
            Pipe::Stats { aggs, by } => {
                let r = run_stats(&st, &m, &aggs, &by, &cache, &no).unwrap();
                let mut v: Vec<_> = r.iter().collect();
                v.sort_by(|x, y| y.1[0].partial_cmp(&x.1[0]).unwrap());
                println!("{} groups  ({})", v.len(), r.columns.join(", "));
                for (k, vals) in v.into_iter().take(limit.max(200)) {
                    let vs: Vec<String> = vals.iter().map(|x| format!("{x}")).collect();
                    println!("{:>9}  {}", vs.join(" / "), k.join(" | "));
                }
                return;
            }
            Pipe::Sort(s) => sort = s,
            Pipe::Fields(f) => cols = f,
            Pipe::Head(n) => limit = n,
            Pipe::Timechart { .. } => println!("(timechart not supported here)"),
            Pipe::Where(_) => {}
        }
    }
    let ids: Vec<usize> = cols
        .iter()
        .map(|c| resolve_column(&st, c).unwrap())
        .collect();
    let rows = top_k_sorted(&st, &m, &sort, limit, &cache, &no).unwrap();
    let sub = m.restrict(&st, &rows);
    let p = page(&st, &sub, 0, rows.len(), &ids, &cache).unwrap();
    // restrict returns store order; re-order to the sorted order
    let pos: std::collections::HashMap<u64, usize> =
        p.rows.iter().enumerate().map(|(i, r)| (*r, i)).collect();
    for r in rows {
        let i = pos[&r];
        let cells: Vec<String> = p.cells[i]
            .iter()
            .map(|c| c.clone().unwrap_or_default().replace('\n', "\\n"))
            .collect();
        println!("[{r}] {}", cells.join(" ¦ "));
    }
}
