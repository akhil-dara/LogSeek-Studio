//! "Check before opening": the first rows of a file parsed exactly as the
//! import will read them, the column types, and every time-like field with
//! how full it is — so the user sees (and can change) what the timeline uses.

use super::detect::detect_spec;
use super::json::{parse_json_records, JsonOptions, SchemaBuilder};
use super::report::ImportReport;
use super::sample::{sample_file, SampleOptions};
use crate::ingest::csv::CsvChunkParser;
use crate::ingest::infer::{epoch_unit_of, infer_column, parse_int, pick_time_column_with};
use crate::store::SourceSpec;
use crate::time::{infer_date_order, parse_time_with};
use crate::types::ColType;
use std::path::Path;

/// A field the timeline could use.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeCandidate {
    /// Index into `Preview::columns`.
    pub col: usize,
    pub name: String,
    /// Share of the sampled rows with a readable time (0..=1).
    pub filled: f32,
    /// Earliest and latest time in the sample, epoch µs UTC.
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// For a number read as epoch time: µs per unit (1 000 000 = seconds).
    pub epoch_unit_us: Option<i64>,
    /// The values never go backwards in file order (how a log written in time
    /// order looks; byte counts and ids do not).
    pub in_order: bool,
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub spec: SourceSpec,
    pub file_len: u64,
    pub columns: Vec<String>,
    pub types: Vec<ColType>,
    /// The first rows (at most `max_rows`), one value per column.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows read from the start of the file to judge types and fill.
    pub sampled_rows: usize,
    pub time: Vec<TimeCandidate>,
    /// Index into `time` of the field the import would pick on its own.
    pub guess: Option<usize>,
    /// Columns whose name in the file was repeated or empty: (the name as
    /// written, the name it was given).
    pub renamed: Vec<(String, String)>,
}

/// Read the head of `path` (about 1 MB) and describe it.
pub fn preview(path: &Path, max_rows: usize) -> std::io::Result<Preview> {
    let s = sample_file(
        path,
        &SampleOptions {
            head_bytes: 1 << 20,
            slices: 0,
            slice_bytes: 0,
        },
    )?;
    let head = &s.head;
    let spec = detect_spec(head);
    let mut rep = ImportReport::default();
    let renamed: Vec<(String, String)> = match &spec {
        SourceSpec::Csv { opts, header } => crate::ingest::csv::read_header_raw(head, opts)
            .into_iter()
            .zip(header.iter())
            .filter(|(raw, given)| raw != *given)
            .map(|(raw, given)| (raw, given.clone()))
            .collect(),
        _ => Vec::new(),
    };
    let (columns, batches) = match &spec {
        SourceSpec::Csv { opts, header } => {
            let p = CsvChunkParser::new(*opts, header.len());
            let b = p.parse(head, 0, opts.has_header, Some(header.as_slice()), &mut rep);
            (header.clone(), b)
        }
        SourceSpec::Text { parts, header } => {
            let cut = head
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(head.len(), |i| i + 1);
            let b = crate::ingest::textlayout::parse_chunk(
                &head[..cut],
                0,
                parts,
                header.len(),
                &mut rep,
            );
            (header.clone(), b)
        }
        SourceSpec::Json {
            record_path, bulk, ..
        } => {
            let mut schema = SchemaBuilder::default();
            let o = JsonOptions {
                record_path: record_path.clone(),
                bulk: *bulk,
                ..JsonOptions::default()
            };
            let b = parse_json_records(head, 0, &mut schema, &o, &mut rep);
            (schema.names().to_vec(), b)
        }
    };
    let n = columns.len();
    // every sampled row as cells (JSON batches may know fewer columns than the end)
    let mut all: Vec<Vec<Option<&str>>> = Vec::new();
    for b in &batches {
        for r in 0..b.rows() {
            all.push(
                (0..n)
                    .map(|c| if c < b.ncols() { b.field(r, c) } else { None })
                    .collect(),
            );
        }
    }
    let rows = all
        .iter()
        .take(max_rows)
        .map(|r| r.iter().map(|v| v.map(str::to_string)).collect())
        .collect();
    let mut types = Vec::with_capacity(n);
    let mut counts = Vec::with_capacity(n);
    let mut time = Vec::new();
    for c in 0..n {
        let vals: Vec<&str> = all
            .iter()
            .filter_map(|r| r[c])
            .filter(|v| !v.is_empty())
            .collect();
        let ty = infer_column(&vals[..vals.len().min(2000)]);
        types.push(ty);
        counts.push(vals.len() as u64);
        let total = all.len().max(1) as f32;
        let span = |ts: &[i64]| (ts.iter().min().copied(), ts.iter().max().copied());
        if ty == ColType::Time {
            let order = infer_date_order(&vals[..vals.len().min(2000)]);
            let ts: Vec<i64> = vals
                .iter()
                .filter_map(|v| parse_time_with(v, order))
                .collect();
            let (first, last) = span(&ts);
            time.push(TimeCandidate {
                col: c,
                name: columns[c].clone(),
                filled: ts.len() as f32 / total,
                first,
                last,
                epoch_unit_us: None,
                in_order: ts.windows(2).all(|w| w[0] <= w[1]),
            });
        } else if ty == ColType::Int {
            let ints: Vec<i64> = vals.iter().filter_map(|v| parse_int(v)).collect();
            let (Some(lo), Some(hi)) = (ints.iter().min(), ints.iter().max()) else {
                continue;
            };
            // whole numbers that read as epoch times: offered, whatever the column is called
            if let Some(unit) = epoch_unit_of(*lo, *hi) {
                time.push(TimeCandidate {
                    col: c,
                    name: columns[c].clone(),
                    filled: ints.len() as f32 / total,
                    first: Some(lo * unit),
                    last: Some(hi * unit),
                    epoch_unit_us: Some(unit),
                    in_order: lo < hi && ints.windows(2).all(|w| w[0] <= w[1]),
                });
            }
        }
    }
    let guess = guess_time(&columns, &types, &counts, &time);
    Ok(Preview {
        spec,
        file_len: s.file_len,
        columns,
        types,
        rows,
        sampled_rows: all.len(),
        time,
        guess,
        renamed,
    })
}

/// The import's own rule: a time-typed field filled on at least half the
/// rows; else a well-filled number of epoch times, in ms or µs, or in seconds
/// when the values never go backwards in file order (seconds alone also fit
/// byte counts and ids: those are offered, never guessed); else the
/// time-typed field even if sparse. Names never count.
fn guess_time(
    names: &[String],
    types: &[ColType],
    counts: &[u64],
    time: &[TimeCandidate],
) -> Option<usize> {
    let at = |col: usize| time.iter().position(|t| t.col == col);
    let typed = pick_time_column_with(names, types, counts);
    if let Some(i) = typed.and_then(at).filter(|&i| time[i].filled >= 0.5) {
        return Some(i);
    }
    let epoch = time
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.epoch_unit_us.is_some_and(|u| u < 1_000_000 || t.in_order) && t.filled >= 0.5
        })
        .max_by(|a, b| a.1.filled.total_cmp(&b.1.filled).then(b.0.cmp(&a.0)))
        .map(|(i, _)| i);
    epoch.or_else(|| typed.and_then(at))
}
