//! Column type inference from samples (95 % rule; failures stay as text).

use super::batch::RecordBatch;
use crate::time::{infer_date_order, parse_time_with, DateOrder};
use crate::types::ColType;

const SAMPLE_ROWS: usize = 2_000;
const RULE: f64 = 0.95;

/// Integer without leading zeros (so codes like `007` stay text) that fits i64.
pub fn parse_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let digits = if b.first() == Some(&b'-') { &b[1..] } else { b };
    if digits.is_empty() || digits.len() > 19 || !digits.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    s.parse().ok()
}

pub fn parse_float(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_digit() || b[0] == b'-' || b[0] == b'.') {
        return None;
    }
    let ip = if b[0] == b'-' { &b[1..] } else { b };
    if ip.len() > 1 && ip[0] == b'0' && ip[1].is_ascii_digit() {
        return None;
    }
    s.parse::<f64>().ok().filter(|f| f.is_finite())
}

pub fn parse_bool(s: &str) -> Option<bool> {
    if s.eq_ignore_ascii_case("true") {
        Some(true)
    } else if s.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// The date order a time column should be parsed with.
pub fn column_date_order(batch: &RecordBatch, col: usize) -> DateOrder {
    let s: Vec<&str> = (0..batch.rows().min(SAMPLE_ROWS))
        .filter_map(|r| batch.field(r, col))
        .collect();
    infer_date_order(&s)
}

pub fn infer_column(values: &[&str]) -> ColType {
    let vals: Vec<&str> = values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect();
    if vals.is_empty() {
        return ColType::Text;
    }
    let n = vals.len() as f64;
    let order = infer_date_order(&vals);
    let (mut ints, mut floats, mut bools, mut times) = (0usize, 0usize, 0usize, 0usize);
    for v in &vals {
        if parse_int(v).is_some() {
            ints += 1;
        } else if parse_float(v).is_some() {
            floats += 1;
        } else if parse_bool(v).is_some() {
            bools += 1;
        } else if parse_time_with(v, order).is_some() {
            times += 1;
        }
    }
    let f = |c: usize| c as f64 >= n * RULE;
    if f(ints) {
        ColType::Int
    } else if f(ints + floats) && floats > 0 {
        ColType::Float
    } else if f(bools) {
        ColType::Bool
    } else if f(times) {
        ColType::Time
    } else {
        ColType::Text
    }
}

pub fn infer_types(batch: &RecordBatch) -> Vec<ColType> {
    let rows = batch.rows().min(SAMPLE_ROWS);
    (0..batch.ncols())
        .map(|c| {
            let v: Vec<&str> = (0..rows).filter_map(|r| batch.field(r, c)).collect();
            infer_column(&v)
        })
        .collect()
}

/// For whole numbers whose min/max are plausible epochs (1990-2100), the
/// µs-per-unit (s, ms or µs). Seconds also fit byte counts and ids, so the
/// import never takes a seconds column on its own: it offers it.
pub fn epoch_unit_of(min: i64, max: i64) -> Option<i64> {
    let ok = |s: i64| (631_152_000..4_102_444_800).contains(&s);
    for unit in [1_000_000i64, 1_000, 1] {
        let div = 1_000_000 / unit;
        if ok(min / div) && ok(max / div) {
            return Some(unit);
        }
    }
    None
}

/// The column to use as the time axis: a Time column (never chosen by its name).
pub fn pick_time_column(names: &[String], types: &[ColType]) -> Option<usize> {
    pick_time_column_with(names, types, &[])
}

/// As [`pick_time_column`], also weighing how many rows have each column:
/// well-populated (≥ half of the best-populated candidate) beats sparse, then
/// more rows, then shallower paths, then the first in the file. Names never count.
pub fn pick_time_column_with(names: &[String], types: &[ColType], counts: &[u64]) -> Option<usize> {
    let cands: Vec<usize> = (0..names.len())
        .filter(|&i| types.get(i) == Some(&ColType::Time))
        .collect();
    let count = |i: usize| counts.get(i).copied().unwrap_or(0);
    let best = cands.iter().map(|&i| count(i)).max()?;
    cands.into_iter().min_by_key(|&i| {
        let sparse = count(i) * 2 < best;
        let depth = names[i].matches('.').count();
        (sparse, std::cmp::Reverse(count(i)), depth, i)
    })
}
