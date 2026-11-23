//! What each column is, from its own numbers: how full it is, how many
//! different values it has, how long they are. No names and no lists of known
//! kinds: a column that holds one value on every filled row is a constant, one
//! with a different long value on nearly every row is an identifier, one filled
//! on few rows of a wide file is sparse, and the rest are informative, ranked by
//! how much a reader gets from them at a glance.
//!
//! Distinct values and lengths come from a sample spread over the file (fast on
//! hundreds of columns); fill is exact (counted at import), and a constant is
//! checked on every row before it is called one.

use super::exec::ChunkCache;
use crate::error::{Error, Result};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a column is, for showing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The time column the file was opened with.
    Anchor,
    /// No value on any row.
    Empty,
    /// One value, on at least 95% of the rows (and on every row that has one).
    Constant,
    /// Filled on under 5% of the rows, in a file of more than 12 columns.
    Sparse,
    /// A different, long value on nearly every filled row (ids, hashes).
    Id,
    /// Everything else: what a reader scans.
    Informative,
}

#[derive(Clone, Debug)]
pub struct ColumnProfile {
    pub col: usize,
    pub role: Role,
    /// Rows with a value, of all rows (exact).
    pub filled: u64,
    pub rows: u64,
    /// Different values among the sampled filled rows, and how many were sampled.
    pub distinct_sampled: u64,
    pub sampled: u64,
    /// The middle length of the sampled values, in characters.
    pub median_len: usize,
    /// For a constant: its one value.
    pub constant: Option<String>,
    /// How telling the column is at a glance (informative columns only, else 0).
    pub score: f64,
    /// An informative column that says the same as a more telling one (its
    /// values map one to one on the same rows): that column.
    pub same_as: Option<usize>,
    /// The three values most often seen in the sample, with their sampled rows
    /// (most first): a glance at what the column holds.
    pub top: Vec<(String, u64)>,
}

/// Rows read for distinct values and lengths, spread over the file's blocks.
const SAMPLE: usize = 5_000;

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn fnv(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The role and score of a column from its numbers (pure, so it can be tested).
pub fn role_of(
    filled: u64,
    rows: u64,
    distinct_sampled: u64,
    sampled: u64,
    median_len: usize,
    top_share: f64,
    constant: bool,
    anchor: bool,
    wide: bool,
) -> (Role, f64) {
    if anchor {
        return (Role::Anchor, 0.0);
    }
    if filled == 0 {
        return (Role::Empty, 0.0);
    }
    let fill = filled as f64 / rows.max(1) as f64;
    // the same value on (nearly) every row; one value on a few rows is a
    // rare flag, worth seeing, not a constant
    if constant && fill >= 0.95 {
        return (Role::Constant, 0.0);
    }
    if wide && fill < 0.05 {
        return (Role::Sparse, 0.0);
    }
    let unique = distinct_sampled as f64 / sampled.max(1) as f64;
    if unique > 0.8 && median_len >= 10 && sampled >= 20 {
        return (Role::Id, 0.0);
    }
    let len_penalty = 1.0 + (median_len as f64 - 32.0).max(0.0) / 32.0;
    // variety: a column whose one value covers nearly every row says little
    let variety = (1.0 - top_share).clamp(0.0, 1.0);
    // scannable: few values recur; a value shared by only a couple of rows
    // (process numbers, times to the millisecond) is read, not scanned
    let recur = (1.0 - unique).clamp(0.0, 1.0).powi(2);
    (Role::Informative, fill * variety * recur / len_penalty)
}

/// Every stored column's profile.
pub fn profile(st: &Store, cache: &ChunkCache, cancel: &AtomicBool) -> Result<Vec<ColumnProfile>> {
    let schema = st.schema();
    let ncols = schema.columns.len();
    let rows = st.row_count();
    let wide = ncols > 12;
    let segs = st.segments();
    // blocks evenly spread over the file, until the sample is full
    let per_block = |si: usize| segs[si].meta().rows as usize;
    let total: usize = (0..segs.len()).map(per_block).sum();
    let step = if total <= SAMPLE || segs.is_empty() {
        1
    } else {
        let avg = (total / segs.len()).max(1);
        (segs.len() / (SAMPLE / avg).max(1)).max(1)
    };
    // the blocks read and how many rows of each, until the sample is full
    let mut plan: Vec<(usize, usize)> = Vec::new();
    let mut budget = SAMPLE;
    for si in (0..segs.len()).step_by(step) {
        if budget == 0 {
            break;
        }
        let n = per_block(si).min(budget);
        budget -= n;
        plan.push((si, n));
    }
    let sample_rows: usize = plan.iter().map(|p| p.1).sum();
    // each sampled row's value, as a fingerprint (0: empty), to find columns
    // that say the same thing row by row
    let mut prints: Vec<Vec<u32>> = vec![vec![0; sample_rows]; ncols];
    let mut seen: Vec<HashMap<u64, u32>> = vec![HashMap::new(); ncols];
    let mut lens: Vec<Vec<usize>> = vec![Vec::new(); ncols];
    let mut sampled = vec![0u64; ncols];
    let mut first: Vec<Option<String>> = vec![None; ncols];
    let mut one_value = vec![true; ncols];
    let mut base = 0usize;
    for &(si, n) in &plan {
        for c in 0..ncols {
            check(cancel)?;
            let Some(col) = cache.get(st, si, c as u32)? else {
                continue;
            };
            for r in 0..n {
                let Some(t) = col.text(r) else { continue };
                if t.is_empty() {
                    continue;
                }
                sampled[c] += 1;
                let h = fnv(t.as_bytes());
                *seen[c].entry(h).or_insert(0) += 1;
                prints[c][base + r] = (h as u32) | 1;
                lens[c].push(t.chars().count());
                match &first[c] {
                    None => first[c] = Some(t.into_owned()),
                    Some(f) if *f != t => one_value[c] = false,
                    _ => {}
                }
            }
        }
        base += n;
    }
    // each column's three most often seen values: their fingerprints, then
    // their text from a second look at the same sampled rows (only those)
    let mut top_keys: Vec<Vec<(u64, u32)>> = seen
        .iter()
        .map(|m| {
            let mut v: Vec<(u64, u32)> = m.iter().map(|(&h, &n)| (h, n)).collect();
            v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v.truncate(3);
            v
        })
        .collect();
    let mut top_text: Vec<Vec<Option<String>>> =
        top_keys.iter().map(|v| vec![None; v.len()]).collect();
    for &(si, n) in &plan {
        for c in 0..ncols {
            if top_text[c].iter().all(Option::is_some) {
                continue;
            }
            check(cancel)?;
            let Some(col) = cache.get(st, si, c as u32)? else {
                continue;
            };
            for r in 0..n {
                let Some(t) = col.text(r) else { continue };
                if t.is_empty() {
                    continue;
                }
                let h = fnv(t.as_bytes());
                if let Some(k) = top_keys[c].iter().position(|x| x.0 == h) {
                    if top_text[c][k].is_none() {
                        top_text[c][k] = Some(t.into_owned());
                    }
                }
            }
        }
    }
    let tops: Vec<Vec<(String, u64)>> = (0..ncols)
        .map(|c| {
            let mut v: Vec<(String, u64)> = std::mem::take(&mut top_keys[c])
                .into_iter()
                .zip(std::mem::take(&mut top_text[c]))
                .filter_map(|((_, n), t)| Some((t?, n as u64)))
                .collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v
        })
        .collect();
    // a constant in the sample is checked on every row before it is called one
    for c in 0..ncols {
        if !(one_value[c] && first[c].is_some()) || schema.columns[c].count == 0 {
            continue;
        }
        let want = first[c].clone().unwrap_or_default();
        'blocks: for si in 0..segs.len() {
            check(cancel)?;
            let Some(col) = cache.get(st, si, c as u32)? else {
                continue;
            };
            for r in 0..per_block(si) {
                if let Some(t) = col.text(r) {
                    if !t.is_empty() && t != want.as_str() {
                        one_value[c] = false;
                        break 'blocks;
                    }
                }
            }
        }
    }
    let mut out: Vec<ColumnProfile> = (0..ncols)
        .map(|c| {
            let mut l = std::mem::take(&mut lens[c]);
            l.sort_unstable();
            let median_len = l.get(l.len() / 2).copied().unwrap_or(0);
            let filled = schema.columns[c].count;
            let constant = one_value[c] && first[c].is_some() && filled > 0;
            let top = seen[c].values().copied().max().unwrap_or(0) as f64;
            let top_share = top / sampled[c].max(1) as f64;
            let (role, score) = role_of(
                filled,
                rows,
                seen[c].len() as u64,
                sampled[c],
                median_len,
                top_share,
                constant,
                schema.time_col == Some(c),
                wide,
            );
            ColumnProfile {
                col: c,
                role,
                filled,
                rows,
                distinct_sampled: seen[c].len() as u64,
                sampled: sampled[c],
                median_len,
                constant: if role == Role::Constant {
                    first[c].clone()
                } else {
                    None
                },
                score,
                same_as: None,
                top: tops[c].clone(),
            }
        })
        .collect();
    // informative columns that say the same thing row by row: the more telling
    // one keeps its place, the other points to it (only columns with as many
    // different values can map one to one, so few pairs are compared)
    let mut info: Vec<usize> = (0..ncols)
        .filter(|&c| out[c].role == Role::Informative)
        .collect();
    info.sort_by(|&a, &b| out[b].score.total_cmp(&out[a].score).then(a.cmp(&b)));
    for (k, &a) in info.iter().enumerate() {
        if out[a].same_as.is_some() {
            continue;
        }
        for &b in &info[k + 1..] {
            check(cancel)?;
            if out[b].same_as.is_none()
                && out[b].distinct_sampled == out[a].distinct_sampled
                && one_to_one(&prints[a], &prints[b])
            {
                out[b].same_as = Some(a);
            }
        }
    }
    Ok(out)
}

/// Two columns' sampled values map one to one, on the same rows (allowing
/// one row in a hundred to differ).
fn one_to_one(a: &[u32], b: &[u32]) -> bool {
    let mut ab: HashMap<u32, u32> = HashMap::new();
    let mut ba: HashMap<u32, u32> = HashMap::new();
    let (mut both, mut off) = (0usize, 0usize);
    for (&x, &y) in a.iter().zip(b) {
        match (x, y) {
            (0, 0) => {}
            (0, _) | (_, 0) => off += 1,
            _ => {
                both += 1;
                if *ab.entry(x).or_insert(y) != y || *ba.entry(y).or_insert(x) != x {
                    off += 1;
                }
            }
        }
    }
    both > 0 && off * 100 <= both
}

/// The columns a smart view shows, in order: the anchor, then up to `keep`
/// informative columns, most telling first.
pub fn smart_columns(profiles: &[ColumnProfile], keep: usize) -> Vec<usize> {
    let mut out: Vec<usize> = profiles
        .iter()
        .filter(|p| p.role == Role::Anchor)
        .map(|p| p.col)
        .collect();
    let mut info: Vec<&ColumnProfile> = profiles
        .iter()
        .filter(|p| p.role == Role::Informative && p.same_as.is_none())
        .collect();
    info.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.col.cmp(&b.col)));
    out.extend(info.iter().take(keep).map(|p| p.col));
    out
}
