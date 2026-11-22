//! What the Check before opening screen would show: `cargo run --release --example profile -- <file>`
use logseek_core::ingest::csv::{read_header, sniff, CsvChunkParser};
use logseek_core::ingest::json::ArrayElementScanner;
use logseek_core::ingest::report::ImportReport;
use logseek_core::ingest::sample::{sample_file, SampleOptions};
use logseek_core::schema::{
    detect_json_columns, suggest_record_path, suggest_type_key, SchemaNode,
};
use serde_json::Value;
use std::io::Read;

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let t = std::time::Instant::now();
    let (bytes, label) = if path.ends_with(".gz") {
        let mut d = flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap());
        let mut v = Vec::new();
        d.by_ref().take(64 << 20).read_to_end(&mut v).unwrap();
        (v, "gz head (≤64 MB decoded)")
    } else {
        let s = sample_file(std::path::Path::new(&path), &SampleOptions::default()).unwrap();
        let mut v = s.head.clone();
        for (_, b) in &s.slices {
            v.extend_from_slice(b);
        }
        (
            v,
            if s.slices.is_empty() {
                "whole file"
            } else {
                "head + 64 slices"
            },
        )
    };
    println!(
        "file: {path}\nsample: {label}, {:.1} MB",
        bytes.len() as f64 / 1e6
    );
    let first = bytes
        .iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace())
        .unwrap_or(b' ');
    if first == b'{' || first == b'[' {
        let mut docs: Vec<Value> = Vec::new();
        let rp = if first == b'{' {
            suggest_record_path(&bytes)
        } else {
            None
        };
        let ndjson_ok = first == b'{' && rp.is_none();
        if ndjson_ok {
            println!("layout: NDJSON (one record per line)");
            for line in bytes.split(|&b| b == b'\n').take(20_000) {
                if let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) {
                    docs.push(v);
                }
            }
        } else {
            match &rp {
                Some(k) => {
                    println!("suggest: array `{k}` holds the records — use each element as a row")
                }
                None => println!("layout: top-level array"),
            }
            let mut sc = ArrayElementScanner::new(rp.clone());
            let mut buf = Vec::new();
            sc.feed(
                &bytes,
                |el| {
                    if docs.len() < 20_000 {
                        if let Ok(v) = serde_json::from_slice::<Value>(el) {
                            docs.push(v);
                        }
                    }
                },
                &mut buf,
            );
        }
        let mut n = SchemaNode::default();
        for d in &docs {
            n.add(d);
        }
        let paths = n.paths();
        println!(
            "records sampled: {}   distinct paths: {}",
            docs.len(),
            paths.len()
        );
        let dotted = paths
            .iter()
            .filter(|p| p.depth == 0 && p.path.contains('.'))
            .count();
        if dotted > 10 {
            println!(
                "suggest: {dotted} top-level keys look like flattened paths — show them as a tree"
            );
        }
        let nested = paths.iter().filter(|p| p.depth > 0).count();
        let arrays = paths
            .iter()
            .filter(|p| p.types.has(logseek_core::schema::T_ARRAY))
            .count();
        println!("nested paths: {nested}   array-valued paths: {arrays}");
        let mut by_presence: Vec<_> = paths.iter().filter(|p| p.depth == 0).collect();
        by_presence.sort_by(|a, b| b.presence.partial_cmp(&a.presence).unwrap());
        println!("top-level fields by presence:");
        for p in by_presence.iter().take(10) {
            let hints: Vec<_> = p.hints.iter().map(|h| h.label()).collect();
            println!(
                "  {:>5.1}%  {:<40} {:<14} {}",
                p.presence * 100.0,
                trunc(&p.path, 40),
                p.types.labels().join("|"),
                hints.join(",")
            );
        }
        let rare = paths
            .iter()
            .filter(|p| p.depth == 0 && p.presence < 0.05)
            .count();
        println!("top-level fields present in <5% of records: {rare}");
        match suggest_type_key(&docs) {
            Some(s) => {
                println!(
                    "suggest: group by `{}` ({} types, separation {:.2}); top:",
                    s.key,
                    s.types.len(),
                    s.score
                );
                for (v, c, k) in s.types.iter().take(5) {
                    println!("    {v:<36} {c:>6} records  {k:>4} fields");
                }
            }
            None => println!("no event-type field found"),
        }
    } else {
        let opts = sniff(&bytes);
        let header = read_header(&bytes, &opts);
        let mut rep = ImportReport::default();
        let p = CsvChunkParser::new(opts, header.len());
        let batches = p.parse(
            &bytes,
            0,
            opts.has_header,
            Some(header.as_slice()),
            &mut rep,
        );
        println!(
            "layout: delimited  delimiter={:?} header={} columns={} encoding={}",
            opts.delimiter as char,
            opts.has_header,
            header.len(),
            opts.encoding.label()
        );
        println!("sampled rows: {}  ragged: {}  overflow fields: {}  bad quotes: {}  repeated headers: {}  decode fallbacks: {}",
            rep.rows, rep.ragged_rows, rep.overflow_fields, rep.bad_quote_spans.len(), rep.header_repeats, rep.decode_errors);
        if let Some(b) = batches.first() {
            let j = detect_json_columns(b);
            if !j.is_empty() {
                let names: Vec<_> = j.iter().map(|&c| header[c].as_str()).collect();
                println!("suggest: JSON columns {names:?} — expand their paths");
            }
        }
        println!("first columns: {:?}", &header[..header.len().min(8)]);
    }
    println!("profiled in {:.2} s", t.elapsed().as_secs_f64());
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
