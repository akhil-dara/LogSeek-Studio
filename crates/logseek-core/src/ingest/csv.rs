//! Delimited-text reading: sniffing, headers, and a chunk parser that never
//! loses a byte (ragged rows → `_overflow`, unbalanced quotes → bounded and
//! re-read literally, bad UTF-8 → Windows-1252, repeated headers counted).

use super::batch::RecordBatch;
use super::encoding::{detect_encoding, to_text, Encoding};
use super::pipeline::RESERVED_COLUMNS;
use super::report::ImportReport;
use csv_core::{ReadRecordResult, Reader, ReaderBuilder};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CsvOptions {
    pub delimiter: u8,
    pub quote: u8,
    pub has_header: bool,
    pub encoding: Encoding,
}

impl Default for CsvOptions {
    fn default() -> Self {
        CsvOptions {
            delimiter: b',',
            quote: b'"',
            has_header: true,
            encoding: Encoding::Utf8,
        }
    }
}

const CANDIDATES: [u8; 4] = [b',', b'\t', b';', b'|'];
const SNIFF_BYTES: usize = 64 * 1024;
const SNIFF_RECORDS: usize = 200;

fn reader(delim: u8, quote: u8) -> Reader {
    ReaderBuilder::new().delimiter(delim).quote(quote).build()
}

fn strip_bom(b: &[u8]) -> &[u8] {
    &b[detect_encoding(b).bom_len().min(b.len())..]
}

/// Parse records from `data` (assumed to start at a record boundary), calling
/// `f(raw_start, raw_end, fields)` until it returns false or input ends.
fn each_record(
    data: &[u8],
    delim: u8,
    quote: u8,
    mut f: impl FnMut(usize, usize, &[&[u8]]) -> bool,
) {
    let mut rdr = reader(delim, quote);
    let mut out = vec![0u8; 4096];
    let mut ends = vec![0usize; 64];
    let mut pos = 0;
    loop {
        let start = skip_newlines(data, pos);
        pos = start;
        let (mut o, mut e) = (0, 0);
        loop {
            let (res, nin, nout, nend) =
                rdr.read_record(&data[pos..], &mut out[o..], &mut ends[e..]);
            pos += nin;
            o += nout;
            e += nend;
            match res {
                ReadRecordResult::InputEmpty => {}
                ReadRecordResult::OutputFull => out.resize(out.len() * 2, 0),
                ReadRecordResult::OutputEndsFull => ends.resize(ends.len() * 2, 0),
                ReadRecordResult::Record => break,
                ReadRecordResult::End => return,
            }
        }
        let mut fields: Vec<&[u8]> = Vec::with_capacity(e);
        let mut prev = 0;
        for &end in &ends[..e] {
            fields.push(&out[prev..end]);
            prev = end;
        }
        if !f(start, pos, &fields) {
            return;
        }
    }
}

#[inline]
fn skip_newlines(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() && (data[pos] == b'\n' || data[pos] == b'\r') {
        pos += 1;
    }
    pos
}

fn sample_lines(sample: &[u8]) -> &[u8] {
    let s = strip_bom(sample);
    let s = &s[..s.len().min(SNIFF_BYTES)];
    if s.len() == SNIFF_BYTES {
        match s.iter().rposition(|&b| b == b'\n') {
            Some(i) => &s[..=i],
            None => s,
        }
    } else {
        s
    }
}

fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

/// Guess delimiter, quote and header from the first bytes of a file.
pub fn sniff(sample: &[u8]) -> CsvOptions {
    let encoding = detect_encoding(sample);
    let lines = sample_lines(sample);
    let mut best = (0u64, b',');
    for &d in &CANDIDATES {
        let mut counts: Vec<usize> = Vec::new();
        each_record(lines, d, b'"', |_, _, f| {
            counts.push(f.len());
            counts.len() < SNIFF_RECORDS
        });
        if counts.is_empty() {
            continue;
        }
        let mut freq = std::collections::HashMap::new();
        for c in &counts {
            *freq.entry(*c).or_insert(0usize) += 1;
        }
        let (&mode, &n) = freq.iter().max_by_key(|(c, n)| (**n, **c)).unwrap();
        if mode < 2 {
            continue;
        }
        let score = (n as u64 * 1000 / counts.len() as u64) * 1000 + mode as u64;
        if score > best.0 {
            best = (score, d);
        }
    }
    let delimiter = best.1;
    let mut first: Vec<String> = Vec::new();
    each_record(lines, delimiter, b'"', |_, _, f| {
        first = f.iter().map(|b| to_text(b).0.into_owned()).collect();
        false
    });
    let has_header = !first.is_empty() && !first.iter().any(|s| looks_numeric(s));
    CsvOptions {
        delimiter,
        quote: b'"',
        has_header,
        encoding,
    }
}

/// The share of the first records that split into the most common number of
/// fields (two or more) with these options: 1.0 for a clean CSV, low for
/// lines that merely contain the delimiter here and there.
pub fn consistency(sample: &[u8], opts: &CsvOptions) -> f64 {
    let mut counts: Vec<usize> = Vec::new();
    each_record(
        sample_lines(sample),
        opts.delimiter,
        opts.quote,
        |_, _, f| {
            counts.push(f.len());
            counts.len() < SNIFF_RECORDS
        },
    );
    let mut freq = std::collections::HashMap::new();
    for c in &counts {
        *freq.entry(*c).or_insert(0usize) += 1;
    }
    match freq.iter().max_by_key(|(c, n)| (**n, **c)) {
        Some((&mode, &n)) if mode >= 2 => n as f64 / counts.len() as f64,
        _ => 0.0,
    }
}

/// Column names from the first record (or `column_N` when there is no header).
pub fn read_header(sample: &[u8], opts: &CsvOptions) -> Vec<String> {
    let lines = sample_lines(sample);
    let mut first: Vec<String> = Vec::new();
    each_record(lines, opts.delimiter, opts.quote, |_, _, f| {
        first = f.iter().map(|b| to_text(b).0.trim().to_string()).collect();
        false
    });
    if !opts.has_header {
        return (1..=first.len()).map(|i| format!("column_{i}")).collect();
    }
    dedupe_names(first)
}

/// The header row's names exactly as written (before repeats are numbered
/// and empty names filled in); empty when the file has no header row.
pub fn read_header_raw(sample: &[u8], opts: &CsvOptions) -> Vec<String> {
    if !opts.has_header {
        return Vec::new();
    }
    let mut first: Vec<String> = Vec::new();
    each_record(
        sample_lines(sample),
        opts.delimiter,
        opts.quote,
        |_, _, f| {
            first = f.iter().map(|b| to_text(b).0.trim().to_string()).collect();
            false
        },
    );
    first
}

/// Unique, non-empty names for a header row: repeats are numbered (`Time_2`),
/// empty names become `column_N`, and a name the app uses for its own columns
/// (`_file`, `_overflow`) is numbered too, so a field of the file never shares
/// a column with them.
pub fn dedupe_names(names: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = RESERVED_COLUMNS.iter().map(|s| s.to_string()).collect();
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let base = if n.is_empty() {
                format!("column_{}", i + 1)
            } else {
                n
            };
            let mut name = base.clone();
            let mut k = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{k}");
                k += 1;
            }
            name
        })
        .collect()
}

/// Parses one chunk of a delimited file into byte-bounded row batches.
pub struct CsvChunkParser {
    opts: CsvOptions,
    ncols: usize,
    /// A record longer than this is treated as an unbalanced quote.
    pub max_record_bytes: usize,
    /// A record spanning more lines than this is treated as an unbalanced quote.
    pub max_record_lines: usize,
}

impl CsvChunkParser {
    pub fn new(opts: CsvOptions, ncols: usize) -> Self {
        CsvChunkParser {
            opts,
            ncols,
            max_record_bytes: 1024 * 1024,
            max_record_lines: 10_000,
        }
    }

    /// `chunk` must start and end on record boundaries. `skip_first` drops the
    /// header record; `header` enables repeated-header detection.
    pub fn parse(
        &self,
        chunk: &[u8],
        base_offset: u64,
        skip_first: bool,
        header: Option<&[String]>,
        rep: &mut ImportReport,
    ) -> Vec<RecordBatch> {
        let start = if base_offset == 0 {
            detect_encoding(chunk).bom_len().min(chunk.len())
        } else {
            0
        };
        let mut out = Vec::new();
        let mut batch = RecordBatch::new(self.ncols);
        let mut skip = skip_first;
        let mut pos = start;
        let (d, q) = (self.opts.delimiter, self.opts.quote);
        while pos < chunk.len() {
            let mut resume: Option<usize> = None;
            let mut stopped_at = chunk.len();
            each_record(&chunk[pos..], d, q, |rs, re, fields| {
                let (rs, re) = (pos + rs, pos + re);
                let raw = trim_eol(&chunk[rs..re]);
                if raw.is_empty() {
                    return true;
                }
                let multi = memchr_nl(raw);
                let bad = raw.len() > self.max_record_bytes
                    || (multi
                        && (fields.len() != self.ncols || count_nl(raw) > self.max_record_lines));
                if bad {
                    let line_end = rs + raw.iter().position(|&b| b == b'\n').unwrap_or(raw.len());
                    let line = trim_eol(&chunk[rs..line_end]);
                    rep.bad_quote_spans
                        .push((base_offset + rs as u64, base_offset + line_end as u64));
                    let lf: Vec<&[u8]> = line.split(|&b| b == d).collect();
                    self.emit(&mut batch, &mut out, base_offset + rs as u64, &lf, rep);
                    resume = Some((line_end + 1).min(chunk.len()));
                    return false;
                }
                if skip {
                    skip = false;
                    return true;
                }
                if let Some(h) = header {
                    if fields.len() == h.len()
                        && fields
                            .iter()
                            .zip(h)
                            .all(|(f, n)| to_text(f).0.trim() == n.as_str())
                    {
                        rep.header_repeats += 1;
                        return true;
                    }
                }
                self.emit(&mut batch, &mut out, base_offset + rs as u64, fields, rep);
                stopped_at = re;
                true
            });
            match resume {
                Some(p) => {
                    skip = false;
                    pos = p
                }
                None => {
                    let _ = stopped_at;
                    break;
                }
            }
        }
        if batch.rows() > 0 {
            batch.set_end_offset(base_offset + chunk.len() as u64);
            out.push(batch);
        }
        out
    }

    fn emit(
        &self,
        batch: &mut RecordBatch,
        out: &mut Vec<RecordBatch>,
        offset: u64,
        fields: &[&[u8]],
        rep: &mut ImportReport,
    ) {
        if batch.is_full() {
            let full = std::mem::replace(batch, RecordBatch::new(self.ncols));
            let mut full = full;
            full.set_end_offset(offset);
            out.push(full);
        }
        batch.begin_row(offset);
        rep.rows += 1;
        if fields.len() != self.ncols {
            rep.ragged_rows += 1;
        }
        for (i, f) in fields.iter().take(self.ncols).enumerate() {
            let (t, fell_back) = to_text(f);
            if fell_back {
                rep.decode_errors += 1;
            }
            batch.set(i, &t);
        }
        if fields.len() > self.ncols {
            let extra = &fields[self.ncols..];
            rep.overflow_fields += extra.len() as u64;
            let mut s = String::new();
            for (k, f) in extra.iter().enumerate() {
                if k > 0 {
                    s.push(self.opts.delimiter as char);
                }
                let (t, fell_back) = to_text(f);
                if fell_back {
                    rep.decode_errors += 1;
                }
                s.push_str(&t);
            }
            batch.set_overflow(s);
        }
    }
}

#[inline]
fn trim_eol(b: &[u8]) -> &[u8] {
    let mut e = b.len();
    while e > 0 && (b[e - 1] == b'\n' || b[e - 1] == b'\r') {
        e -= 1;
    }
    &b[..e]
}
#[inline]
fn memchr_nl(b: &[u8]) -> bool {
    b.contains(&b'\n')
}
#[inline]
fn count_nl(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}
