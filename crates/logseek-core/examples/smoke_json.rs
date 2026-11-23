//! cargo run --release -p logseek-core --example smoke_json -- <file.json>
use logseek_core::ingest::json::{parse_json_records, JsonOptions, SchemaBuilder};
use logseek_core::ingest::report::ImportReport;
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let data = std::fs::read(&path).unwrap();
    let t = std::time::Instant::now();
    let mut schema = SchemaBuilder::default();
    let mut rep = ImportReport::default();
    let batches = parse_json_records(&data, 0, &mut schema, &JsonOptions::default(), &mut rep);
    let secs = t.elapsed().as_secs_f64();
    let rows: usize = batches.iter().map(|b| b.rows()).sum();
    println!(
        "rows={rows} cols={} batches={} invalid={} secs={secs:.2} MB/s={:.0}",
        schema.len(),
        batches.len(),
        rep.invalid_json,
        data.len() as f64 / 1e6 / secs
    );
    let c = schema.index_of("#event_simpleName").unwrap();
    println!(
        "row0 event={:?} offset={}",
        batches[0].field(0, c),
        batches[0].offset(0)
    );
}
