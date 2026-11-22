//! Never-miss audit: `cargo run --release --example verify -- <store> [samples]`
//! prints, for evenly spaced rows, every displayed cell and the row's raw source
//! record, as JSON lines, so an independent parser can compare them.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = logseek_core::store::Store::open(std::path::Path::new(&a[1])).unwrap();
    let n: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let rows = st.row_count();
    let ncols = st.schema().len();
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    println!("{}", serde_json::json!({"columns": names}));
    for k in 0..n.min(rows) {
        let r = k * rows / n.min(rows);
        let cells: Vec<Option<String>> = (0..ncols).map(|c| st.cell_text(r, c).unwrap()).collect();
        let raw = st.raw_line(r).unwrap();
        println!(
            "{}",
            serde_json::json!({"row": r, "cells": cells, "raw": String::from_utf8_lossy(&raw)})
        );
    }
}
