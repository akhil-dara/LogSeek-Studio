//! Timeline signals, read from the file's own times (nothing known in
//! advance):
//!
//! - silent gaps: stretches with no rows far longer than the file's usual
//!   pauses (the rows are counted per second, or per a coarser step when the
//!   span is very long; a pause is the distance between two seconds that have
//!   rows);
//! - steps: lasting changes in the rate of rows, at least twice or half the
//!   rate before, found by splitting the timeline where the rate shifts most
//!   and again within each part (the silent gaps are left out, so a silence
//!   is reported as a gap, not as two steps);
//! - regular repeats: for a column, each value's rows in time order; a value
//!   whose gaps are nearly all alike comes like clockwork. The measure looks at
//!   the middle of the gaps (quartiles and the median distance from the
//!   median), so a missed beat or some jitter does not hide a heartbeat.

use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::{Error, Result};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SEC: i64 = 1_000_000;
/// The finest count of rows over time: per second, unless the span would
/// need more counts than this (then coarser).
const FINE: i64 = 4_000_000;
/// The rate series steps are read from: about this many steps of time.
const COARSE: i64 = 600;
/// A step must last this many steps of the coarse series on each side.
const MIN_SIDE: usize = 6;
/// A step changes the rate by at least this much (or its inverse).
const MIN_RATIO: f64 = 2.0;

/// A stretch with no rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    /// The last row before it and the first row after it (µs).
    pub from: i64,
    pub to: i64,
}

/// A lasting change in the rate of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// When it happened (µs, the start of the step of time it begins in).
    pub at: i64,
    /// Rows per second before and after (each over its own part).
    pub before: f64,
    pub after: f64,
}

pub struct Signals {
    /// Longest first.
    pub gaps: Vec<Gap>,
    /// The usual pause between rows (µs), and the shortest pause that counts
    /// as a gap.
    pub usual_pause: i64,
    pub gap_line: i64,
    /// In time order.
    pub steps: Vec<Step>,
    /// The width of the steps of time the rate is read in (µs).
    pub step_width: i64,
    /// The first and last row (µs).
    pub first: i64,
    pub last: i64,
}

/// Each row's time (µs) in block `si` for the rows of `m`.
fn times(
    st: &Store,
    cache: &ChunkCache,
    si: usize,
    m: &SegMatch,
    mut f: impl FnMut(usize, i64),
) -> Result<()> {
    let Some(tc) = st.schema().time_col else {
        return Err(Error::Invalid("this source has no time column".into()));
    };
    let unit = st.schema().time_unit_us.unwrap_or(1);
    let blk = cache.block(st, si)?;
    let Some(c) = blk.column(tc as u32)? else {
        return Ok(());
    };
    each_in(m, |r| {
        if let Some(t) = c.int(r) {
            f(r, t.saturating_mul(unit));
        }
    });
    Ok(())
}

/// The first and last time of the rows of `m`.
fn span(
    st: &Store,
    m: &MatchSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Option<(i64, i64)>> {
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || (i64::MAX, i64::MIN),
        |(lo, hi), si| {
            times(st, cache, si, &m.segs[si], |_, t| {
                *lo = (*lo).min(t);
                *hi = (*hi).max(t);
            })
        },
    )?;
    let (lo, hi) = parts
        .into_iter()
        .fold((i64::MAX, i64::MIN), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    Ok((lo <= hi).then_some((lo, hi)))
}

/// Rows per step of `width` µs from `lo`.
fn counts(
    st: &Store,
    m: &MatchSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
    lo: i64,
    width: i64,
    n: usize,
) -> Result<Vec<u32>> {
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || vec![0u32; n],
        |c, si| {
            times(st, cache, si, &m.segs[si], |_, t| {
                let i = (((t - lo) / width) as usize).min(n - 1);
                c[i] = c[i].saturating_add(1);
            })?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut out = vec![0u32; n];
    for p in parts {
        for (a, b) in out.iter_mut().zip(p) {
            *a = a.saturating_add(b);
        }
    }
    Ok(out)
}

fn median(v: &mut [i64]) -> i64 {
    if v.is_empty() {
        return 0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable(k).1
}

/// Silent gaps and steps in the rate of the rows of `m`.
pub fn timeline_signals(
    st: &Store,
    m: &MatchSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Signals> {
    let Some((lo, hi)) = span(st, m, cache, cancel)? else {
        return Ok(Signals {
            gaps: Vec::new(),
            usual_pause: 0,
            gap_line: 0,
            steps: Vec::new(),
            step_width: 0,
            first: 0,
            last: 0,
        });
    };
    // the span of the file's times, measured so that it cannot wrap: an absurd
    // time in one row (a value no clock ever showed) is said plainly
    let Some(span_us) = hi.checked_sub(lo).filter(|&d| d >= 0) else {
        return Err(Error::Invalid(
            "the times in this source span more than can be measured (one row holds a time              far outside any real date), so the timeline cannot be drawn; choose another time              column or fix that row"
                .into(),
        ));
    };
    // 1. per second (or coarser for very long spans): where the pauses are
    let width = SEC.max(span_us / FINE + 1);
    let n = (span_us / width + 1) as usize;
    let fine = counts(st, m, cache, cancel, done, lo, width, n)?;
    let filled: Vec<usize> = (0..n).filter(|&i| fine[i] > 0).collect();
    let pauses: Vec<i64> = filled.windows(2).map(|w| (w[1] - w[0]) as i64).collect();
    let usual = median(&mut pauses.clone()).max(1);
    let madm = {
        let mut d: Vec<i64> = pauses.iter().map(|p| (p - usual).abs()).collect();
        median(&mut d)
    };
    // the beat: the pause a moment of the file's ordinary activity falls in
    // (pauses weighed by their length, the far longer ones left out). Rows that
    // come in bursts have a usual pause of a second inside each burst; the
    // time between the bursts is their beat, not a silence.
    let beat = {
        let mut p: Vec<i64> = pauses
            .iter()
            .copied()
            .filter(|&p| p < usual * 100)
            .collect();
        p.sort_unstable();
        let half = p.iter().sum::<i64>() / 2;
        let mut run = 0;
        p.into_iter()
            .find(|&x| {
                run += x;
                run > half
            })
            .unwrap_or(usual)
    };
    // a gap: far longer than the usual pause and the beat, on the file's own terms
    let line = (usual + 6 * madm.max(1))
        .max(usual * 10)
        .max(beat * 3)
        .max(2);
    // the exact times either side of each gap: the last row of the second
    // before, the first row of the second after
    let mut raw: Vec<(usize, usize)> = filled
        .windows(2)
        .filter(|w| (w[1] - w[0]) as i64 >= line)
        .map(|w| (w[0], w[1]))
        .collect();
    raw.sort_by_key(|g| std::cmp::Reverse(g.1 - g.0));
    let edges: Vec<(i64, i64)> = raw
        .iter()
        .map(|&(a, b)| (lo + a as i64 * width, lo + b as i64 * width))
        .collect();
    let mut gaps: Vec<Gap> = Vec::with_capacity(edges.len());
    if !edges.is_empty() {
        // the exact last row of the second before each gap, and the first
        // row of the second after it
        let parts = over_blocks(
            st,
            cache,
            |si| m.segs[si].count() > 0,
            cancel,
            || vec![(i64::MIN, i64::MAX); edges.len()],
            |e, si| {
                times(st, cache, si, &m.segs[si], |_, t| {
                    for (k, &(a, b)) in edges.iter().enumerate() {
                        if t >= a && t < a.saturating_add(width) {
                            e[k].0 = e[k].0.max(t);
                        }
                        if t >= b && t < b.saturating_add(width) {
                            e[k].1 = e[k].1.min(t);
                        }
                    }
                })
            },
        )?;
        let mut exact = vec![(i64::MIN, i64::MAX); edges.len()];
        for p in parts {
            for (x, (a, b)) in exact.iter_mut().zip(p) {
                x.0 = x.0.max(a);
                x.1 = x.1.min(b);
            }
        }
        for (&(a, b), (ea, eb)) in edges.iter().zip(exact) {
            gaps.push(Gap {
                from: if ea == i64::MIN { a } else { ea },
                to: if eb == i64::MAX { b } else { eb },
            });
        }
        gaps.sort_by_key(|g| std::cmp::Reverse(g.to - g.from));
    }
    // 2. steps in the rate, the gaps left out
    let sw = (span_us / COARSE + 1).max(width);
    let ns = (span_us / sw + 1) as usize;
    let mut series: Vec<(i64, f64)> = Vec::with_capacity(ns);
    for k in 0..ns {
        let a = lo.saturating_add((k as i64).saturating_mul(sw));
        let b = a.saturating_add(sw);
        // a step of time inside a gap is not part of the rate
        if gaps.iter().any(|g| a >= g.from && b <= g.to) {
            continue;
        }
        let i0 = ((a - lo) / width) as usize;
        let i1 = (((b - lo) / width) as usize).min(n);
        let rows: u64 = fine[i0.min(n)..i1].iter().map(|&c| c as u64).sum();
        // the part of the step of time not in a gap
        let mut live = sw;
        for g in &gaps {
            let o = (b.min(g.to) - a.max(g.from)).max(0);
            live -= o;
        }
        // mostly silent: the few seconds left say nothing about the rate (a
        // burst between two gaps would read as a flood)
        if live * 2 >= sw {
            series.push((a, rows as f64 * SEC as f64 / live as f64));
        }
    }
    let mut steps: Vec<Step> = Vec::new();
    split(&series, 0, series.len(), &mut steps);
    steps.sort_by_key(|s| s.at);
    Ok(Signals {
        gaps,
        usual_pause: usual * width,
        gap_line: line * width,
        steps,
        step_width: sw,
        first: lo,
        last: hi,
    })
}

/// Split `s[a..b]` where the rate shifts most, if the two sides differ by at
/// least `MIN_RATIO` and each lasts `MIN_SIDE` steps; then each side again.
fn split(s: &[(i64, f64)], a: usize, b: usize, out: &mut Vec<Step>) {
    if b - a < 2 * MIN_SIDE {
        return;
    }
    let total: f64 = s[a..b].iter().map(|p| p.1).sum();
    let mut left = 0.0;
    let mut best: Option<(usize, f64)> = None;
    for k in a + 1..b {
        left += s[k - 1].1;
        let (nl, nr) = ((k - a) as f64, (b - k) as f64);
        if (k - a) < MIN_SIDE || (b - k) < MIN_SIDE {
            continue;
        }
        let (ml, mr) = (left / nl, (total - left) / nr);
        // the shift that explains most: the difference of means, weighed by
        // how much of the part lies each side
        let d = (ml - mr).abs() * (nl * nr / (nl + nr)).sqrt();
        if best.is_none_or(|(_, x)| d > x) {
            best = Some((k, d));
        }
    }
    let Some((k, _)) = best else { return };
    let before = s[a..k].iter().map(|p| p.1).sum::<f64>() / (k - a) as f64;
    let after = s[k..b].iter().map(|p| p.1).sum::<f64>() / (b - k) as f64;
    let (hi, lo) = (before.max(after), before.min(after));
    if lo > 0.0 && hi / lo >= MIN_RATIO {
        out.push(Step {
            at: s[k].0,
            before,
            after,
        });
        split(s, a, k, out);
        split(s, k, b, out);
    }
}

/// A value of a column and how regularly its rows come.
#[derive(Clone, Debug, PartialEq)]
pub struct Rhythm {
    pub value: Option<String>,
    pub rows: u64,
    /// The usual time between its rows (µs), and how far the gaps usually
    /// stray from it (µs).
    pub every: i64,
    pub give_or_take: i64,
    pub first: i64,
    pub last: i64,
    /// 0 (no rhythm) to 1 (exact clockwork).
    pub score: f64,
}

pub struct Regular {
    /// Most like clockwork first.
    pub values: Vec<Rhythm>,
    /// Values with too few rows to judge (fewer than 10 gaps).
    pub too_few: usize,
}

/// For each value of `col` on the rows of `m`: how regularly its rows come.
pub fn regular(
    st: &Store,
    m: &MatchSet,
    col: usize,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Regular> {
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        HashMap::<Option<String>, Vec<i64>>::new,
        |acc, si| {
            let sm = &m.segs[si];
            let rows = st.segments()[si].meta().rows as usize;
            let mut t = vec![i64::MIN; rows];
            times(st, cache, si, sm, |r, x| t[r] = x)?;
            let mut vals: Vec<Option<String>> = vec![None; rows];
            if col >= st.schema().len() {
                let mut segs = vec![SegMatch::None; st.segments().len()];
                segs[si] = sm.clone();
                let one = MatchSet::from_segs(segs);
                let base = st.segments()[si].meta().row_start;
                let mut start = 0;
                while start < one.count() {
                    let p = super::exec::page_with(st, &one, start, 20_000, &[col], fs, cache)?;
                    if p.rows.is_empty() {
                        break;
                    }
                    start += p.rows.len() as u64;
                    for (g, cells) in p.rows.iter().zip(p.cells) {
                        vals[(g - base) as usize] = cells.into_iter().next().flatten();
                    }
                }
            } else {
                let blk = cache.block(st, si)?;
                if let Some(c) = blk.column(col as u32)? {
                    each_in(sm, |r| {
                        vals[r] = c.text(r).map(|s| s.into_owned()).filter(|s| !s.is_empty())
                    });
                }
            }
            each_in(sm, |r| {
                if t[r] != i64::MIN {
                    acc.entry(vals[r].take()).or_default().push(t[r]);
                }
            });
            done.fetch_add(sm.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut all: HashMap<Option<String>, Vec<i64>> = HashMap::new();
    for p in parts {
        for (k, v) in p {
            all.entry(k).or_default().extend(v);
        }
    }
    let mut values = Vec::new();
    let mut too_few = 0;
    for (value, mut ts) in all {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        ts.sort_unstable();
        let gaps: Vec<i64> = ts.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
        if gaps.len() < 10 {
            too_few += 1;
            continue;
        }
        let mut g = gaps.clone();
        g.sort_unstable();
        let q = |p: f64| g[((g.len() - 1) as f64 * p).round() as usize];
        let (q1, q2, q3) = (q(0.25), q(0.5), q(0.75));
        // how alike the middle of the gaps is on either side of the median
        let skew = if q3 > q1 {
            1.0 - ((q1 as f64 + q3 as f64 - 2.0 * q2 as f64) / (q3 as f64 - q1 as f64)).abs()
        } else {
            1.0
        };
        let mut dev: Vec<i64> = gaps.iter().map(|x| x.saturating_sub(q2).saturating_abs()).collect();
        let madm = median(&mut dev);
        let tight = if q2 > 0 {
            (1.0 - madm as f64 / q2 as f64).max(0.0)
        } else {
            0.0
        };
        values.push(Rhythm {
            value,
            rows: ts.len() as u64,
            every: q2,
            give_or_take: madm,
            first: ts[0],
            last: *ts.last().unwrap(),
            // tightness first: a rhythm that strays a third of its beat is loose,
            // however even the straying
            score: tight * (0.5 + 0.5 * skew),
        });
    }
    values.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.rows.cmp(&a.rows))
            .then_with(|| a.value.cmp(&b.value))
    });
    Ok(Regular { values, too_few })
}
