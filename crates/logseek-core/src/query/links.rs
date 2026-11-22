//! Columns that point at each other, from their values alone: column B links
//! to column A when most of B's different values are also A's values (on
//! other rows: a parent id pointing at an id, a session pointing at the row
//! that opened it). Never from what the columns are called. A column that
//! only repeats another on the same row is a copy, not a link.

use super::exec::ChunkCache;
use crate::error::{Error, Result};
use crate::store::Store;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// `from`'s values are found among `to`'s: `share` of its different values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Link {
    pub from: usize,
    pub to: usize,
    pub share: f64,
}

/// Rows read, spread over the file's blocks.
const SAMPLE: usize = 5_000;
/// A column needs this many different values to point anywhere (a handful
/// of small numbers is shared by chance).
const MIN_DISTINCT: usize = 20;
/// The share of a column's values found in the other for a link.
const MIN_SHARE: f64 = 0.3;
/// In the sample, a pair sharing this much is measured over many more rows:
/// a pointer's target row is often outside a sample (a parent started long
/// before), so the sample only finds the pairs worth measuring.
const MAYBE_SHARE: f64 = 0.01;
/// Values two columns must share in the sample to be measured at all.
const MAYBE_HITS: usize = 3;
/// Rows the pairs found are measured over, from the start of the file (a
/// pointer mostly points at rows not far before it).
const MEASURE_ROWS: usize = 1_000_000;

fn fnv(b: &[u8]) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h as u32) | 1
}

/// Every link between the store's columns, most telling first.
pub fn linked_columns(st: &Store, cache: &ChunkCache, cancel: &AtomicBool) -> Result<Vec<Link>> {
    let ncols = st.schema().columns.len();
    let segs = st.segments();
    if ncols < 2 || segs.is_empty() {
        return Ok(Vec::new());
    }
    // blocks evenly spread over the file until the sample is full
    let total: usize = segs.iter().map(|s| s.meta().rows as usize).sum();
    let step = if total <= SAMPLE {
        1
    } else {
        let avg = (total / segs.len()).max(1);
        (segs.len() / (SAMPLE / avg).max(1)).max(1)
    };
    let mut plan: Vec<(usize, usize)> = Vec::new();
    let mut budget = SAMPLE;
    for si in (0..segs.len()).step_by(step) {
        if budget == 0 {
            break;
        }
        let n = (segs[si].meta().rows as usize).min(budget);
        budget -= n;
        plan.push((si, n));
    }
    let rows: usize = plan.iter().map(|p| p.1).sum();
    // each sampled row's value per column, as a fingerprint (0: empty)
    let mut prints: Vec<Vec<u32>> = vec![vec![0; rows]; ncols];
    let mut base = 0;
    for &(si, n) in &plan {
        for (c, col_prints) in prints.iter_mut().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let Some(col) = cache.get(st, si, c as u32)? else {
                continue;
            };
            for r in 0..n {
                if let Some(t) = col.text(r) {
                    if !t.is_empty() {
                        col_prints[base + r] = fnv(t.as_bytes());
                    }
                }
            }
        }
        base += n;
    }
    let sets: Vec<HashSet<u32>> = prints
        .iter()
        .map(|p| p.iter().copied().filter(|&h| h != 0).collect())
        .collect();
    let cand: Vec<usize> = (0..ncols)
        .filter(|&c| sets[c].len() >= MIN_DISTINCT)
        .collect();
    // which candidate columns hold each value
    let mut holders: HashMap<u32, Vec<usize>> = HashMap::new();
    for &c in &cand {
        for &h in &sets[c] {
            holders.entry(h).or_default().push(c);
        }
    }
    // a copy: filled on mostly the same rows, and equal on nearly all of them
    // (two columns equal now and then are not copies: a process that is its
    // own context, say)
    let copy = |a: usize, b: usize| {
        let (mut both, mut same, mut either) = (0usize, 0usize, 0usize);
        for (x, y) in prints[a].iter().zip(&prints[b]) {
            if *x != 0 || *y != 0 {
                either += 1;
            }
            if *x != 0 && *y != 0 {
                both += 1;
                same += usize::from(x == y);
            }
        }
        both > 0 && same * 100 >= both * 95 && both * 10 >= either * 9
    };
    // a pointer sits beside what it points at: both filled on the same rows
    // at least half the time. The same field written under two names by
    // different kinds of rows (rarely both on one row) is no pointer.
    let beside = |a: usize, b: usize| {
        let (mut both, mut na, mut nb) = (0usize, 0usize, 0usize);
        for (x, y) in prints[a].iter().zip(&prints[b]) {
            na += usize::from(*x != 0);
            nb += usize::from(*y != 0);
            both += usize::from(*x != 0 && *y != 0);
        }
        both * 2 >= na.min(nb).max(1)
    };
    let mut found: HashMap<(usize, usize), f64> = HashMap::new();
    // a column that repeats an earlier one row for row says nothing more:
    // it is left out, so each link is named once
    let mut copies: HashSet<usize> = HashSet::new();
    for &b in &cand {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let mut hits: HashMap<usize, usize> = HashMap::new();
        for h in &sets[b] {
            for &a in holders.get(h).map(Vec::as_slice).unwrap_or(&[]) {
                if a != b {
                    *hits.entry(a).or_insert(0) += 1;
                }
            }
        }
        for (a, n) in hits {
            let share = n as f64 / sets[b].len() as f64;
            if n < MAYBE_HITS || share < MAYBE_SHARE {
                continue;
            }
            if copy(a, b) {
                copies.insert(a.max(b));
            } else if beside(a, b) {
                found.insert((b, a), share);
            }
        }
    }
    // measure the pairs found over many more rows: every value of the
    // columns involved, from the start of the file
    let involved: Vec<usize> = {
        let mut v: Vec<usize> = found.keys().flat_map(|&(b, a)| [b, a]).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let mut wide: HashMap<usize, HashSet<u32>> =
        involved.iter().map(|&c| (c, HashSet::new())).collect();
    let mut read = 0usize;
    for (si, seg) in segs.iter().enumerate() {
        if read >= MEASURE_ROWS || involved.is_empty() {
            break;
        }
        let n = (seg.meta().rows as usize).min(MEASURE_ROWS - read);
        for &c in &involved {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let Some(col) = cache.get(st, si, c as u32)? else {
                continue;
            };
            let set = wide.entry(c).or_default();
            for r in 0..n {
                if let Some(t) = col.text(r) {
                    if !t.is_empty() {
                        set.insert(fnv(t.as_bytes()));
                    }
                }
            }
        }
        read += n;
    }
    let found: HashMap<(usize, usize), f64> = found
        .keys()
        .filter_map(|&(b, a)| {
            let (sb, sa) = (wide.get(&b)?, wide.get(&a)?);
            let hit = sb.iter().filter(|h| sa.contains(h)).count();
            let share = hit as f64 / sb.len().max(1) as f64;
            (hit >= MIN_DISTINCT && share >= MIN_SHARE).then_some(((b, a), share))
        })
        .collect();
    // of two columns found in each other, the one found more fully points
    let mut links: Vec<Link> = found
        .iter()
        .filter(|(&(b, a), &s)| {
            found
                .get(&(a, b))
                .is_none_or(|&back| s > back || (s == back && b > a))
        })
        .map(|(&(from, to), &share)| Link { from, to, share })
        .filter(|l| !copies.contains(&l.from) && !copies.contains(&l.to))
        .collect();
    // the links that carry the most rows first: how fully a pointer's values
    // are found, times the rows that have one (exact counts from the import)
    let filled = |c: usize| st.schema().columns[c].count as f64;
    links.sort_by(|x, y| {
        (y.share * filled(y.from))
            .total_cmp(&(x.share * filled(x.from)))
            .then(sets[y.to].len().cmp(&sets[x.to].len()))
            .then(x.from.cmp(&y.from))
    });
    Ok(links)
}
