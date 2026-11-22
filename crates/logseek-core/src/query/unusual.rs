//! Unusual rows: every row scored by how rare its values are in their own
//! columns, the file being its own measure (nothing known in advance).
//!
//! - A value of a text column scores by how few rows share it: -log2 of its
//!   share of the rows, in bits. An empty cell is a value like any other.
//! - A number scores by how far out in its column it lies: -log2 of twice
//!   the share of rows at or beyond it on its side (the "empirical tail"
//!   way, which needs no settings).
//! - A column weighs less the more of its values are all different, among
//!   the rows that have one (a column naming every row it is filled in
//!   weighs nothing), and a column of numbers that counts
//!   up row by row weighs nothing: neither says anything about a row.
//!
//! - Numbers that are each ordinary but unusual together (a hot bearing at
//!   idle load): the columns of numbers, each on its own robust scale
//!   (median, median distance), are mixed a few at a time in many fixed
//!   random ways, and each mix's spread over the rows is learned; a row whose
//!   strongest mix falls where few rows fall scores the more, beyond what a
//!   usual row scores in that mix. The columns whose mixes score higher than the mixes without
//!   them are named (the "lightweight on-line detector of anomalies" way).
//!
//! A row's score is the sum over its columns. Only the rows on screen are
//! explained, column by column, with exact counts ("User is mallory: 1 of
//! 20,000 rows").
//!
//! Counting every value of a column takes memory; a column with more
//! different values than a quarter of the rows weighs little and is left out
//! (and said so), as are the columns that do not fit in what the machine
//! has free, the ones with most different values first.

use super::exec::{each_in, over_blocks, Block, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::Result;
use crate::store::Store;
use crate::types::ColType;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Buckets of the spread of scores: half bits, up to 4,000 bits (a row of a
/// file with hundreds of columns can add up to a lot).
pub const BUCKETS: usize = 8001;
const STEP: f64 = 0.5;

fn bucket(score: f64) -> usize {
    ((score / STEP).round() as usize).min(BUCKETS - 1)
}

/// What one column knows about its values.
enum ColModel {
    /// Counts of each value (`""` for empty).
    Text { counts: HashMap<String, u64> },
    /// The numbers in order with how many rows have each, and the empty rows.
    Num {
        values: Vec<(f64, u64)>,
        /// Rows at or below each value (running total).
        below: Vec<u64>,
        empty: u64,
        /// Counts of the text of values that are not numbers.
        other: HashMap<String, u64>,
    },
    /// Not weighed (too many different values, or left out for memory).
    Off,
}

/// The measure learned from the rows: per column, how its values spread.
pub struct Model {
    cols: Vec<usize>,
    models: Vec<ColModel>,
    weights: Vec<f64>,
    n: u64,
    joint: Option<Joint>,
}

/// One reason a row is unusual.
#[derive(Clone, Debug, PartialEq)]
pub struct Reason {
    /// The column (index into the store's columns).
    pub col: usize,
    /// The row's value (`None`: empty).
    pub value: Option<String>,
    /// For a text value: rows with this value; for a number: rows at or
    /// beyond it on its side.
    pub rows: u64,
    pub of: u64,
    /// The largest (or smallest) values of a column of numbers.
    pub largest: bool,
    pub smallest: bool,
    /// How much it adds to the row's score (bits, weighed).
    pub bits: f64,
    /// For numbers unusual together: the columns that make it so, most first
    /// (`col` is the first of them, `value` empty).
    pub together: Vec<usize>,
}

/// Bins of each mix's spread: 64 across eight scale units either side of the
/// middle, and one open bin each side beyond.
const BINS: usize = 66;
const REACH: f64 = 8.0;
/// How many mixes.
const MIXES: usize = 64;

/// One mix of a few columns of numbers and how its values spread.
struct Mix {
    /// Places in `Joint::cols`, and their weights.
    feat: Vec<usize>,
    w: Vec<f64>,
    hist: Vec<u64>,
    total: u64,
    /// The bits a usual row scores in this mix (the spread's entropy).
    usual: f64,
}

/// Numbers unusual together.
struct Joint {
    /// Places in the model's columns, each with its middle and scale.
    cols: Vec<usize>,
    mid: Vec<f64>,
    scale: Vec<f64>,
    mixes: Vec<Mix>,
}

fn bin(z: f64) -> usize {
    if z < -REACH {
        0
    } else if z >= REACH {
        BINS - 1
    } else {
        1 + (((z + REACH) / (2.0 * REACH)) * (BINS - 2) as f64) as usize
    }
    .min(BINS - 1)
}

impl Joint {
    /// The mixes chosen for `d` columns: fixed (the same every time), each
    /// of about the square root of `d` columns (at least two).
    fn mixes(d: usize) -> Vec<(Vec<usize>, Vec<f64>)> {
        let k = ((d as f64).sqrt().ceil() as usize).clamp(2, d);
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..MIXES)
            .map(|_| {
                let mut feat: Vec<usize> = Vec::with_capacity(k);
                while feat.len() < k {
                    let f = (next() % d as u64) as usize;
                    if !feat.contains(&f) {
                        feat.push(f);
                    }
                }
                feat.sort_unstable();
                // weights from the normal spread (two uniform draws)
                let w: Vec<f64> = feat
                    .iter()
                    .map(|_| {
                        let u1 = ((next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
                        let u2 = (next() >> 11) as f64 / (1u64 << 53) as f64;
                        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
                    })
                    .collect();
                let norm = w.iter().map(|x| x * x).sum::<f64>().sqrt().max(1e-12);
                (feat, w.iter().map(|x| x / norm).collect())
            })
            .collect()
    }

    /// The value of a mix for a row (`None` when one of its numbers is missing).
    fn value(&self, m: &Mix, x: &[f64]) -> Option<f64> {
        let mut z = 0.0;
        for (&f, &w) in m.feat.iter().zip(&m.w) {
            let v = x[f];
            if v.is_nan() {
                return None;
            }
            z += w * (v - self.mid[f]) / self.scale[f];
        }
        Some(z)
    }

    /// Bits the numbers say together beyond what they say each alone, and
    /// what each column adds. `alone`: each number's own rarity (bits). A mix
    /// counts by how far it goes past both a usual row in it and the rarest
    /// of its own numbers taken alone: a row at the far end of a line all
    /// rows follow is extreme, not unusual together.
    fn score(&self, x: &[f64], alone: &[f64]) -> (f64, Vec<f64>) {
        let d = self.cols.len();
        let mut s = Vec::with_capacity(self.mixes.len());
        for m in &self.mixes {
            if let Some(z) = self.value(m, x) {
                let p = (m.hist[bin(z)] as f64 + 1.0) / (m.total as f64 + BINS as f64);
                let own = m.feat.iter().map(|&f| alone[f]).fold(m.usual, f64::max);
                s.push((m, -p.log2() - own));
            }
        }
        if s.is_empty() {
            return (0.0, vec![0.0; d]);
        }
        // the strongest mix: one way of looking at the numbers where the row
        // lands where almost no row lands is the signal (an average over every
        // mix would hide it among the mixes that do not hold the right columns)
        let all = s.iter().map(|x| x.1).fold(0.0f64, f64::max);
        let mut by = vec![0.0; d];
        for (f, slot) in by.iter_mut().enumerate() {
            let (mut a, mut na, mut b, mut nb) = (0.0, 0, 0.0, 0);
            for (m, v) in &s {
                if m.feat.contains(&f) {
                    a += v;
                    na += 1;
                } else {
                    b += v;
                    nb += 1;
                }
            }
            if na > 0 && nb > 0 {
                *slot = a / na as f64 - b / nb as f64;
            }
        }
        (all.max(0.0), by)
    }
}

impl Model {
    /// A measure that knows nothing (every row scores nothing).
    pub fn empty() -> Model {
        Model {
            cols: Vec::new(),
            models: Vec::new(),
            weights: Vec::new(),
            n: 0,
            joint: None,
        }
    }

    /// How much a column weighs (0 to 1), by its place among the columns given.
    pub fn weight(&self, col: usize) -> f64 {
        self.cols
            .iter()
            .position(|&c| c == col)
            .map_or(0.0, |j| self.weights[j])
    }

    /// The columns that were not weighed, and why: (column, too many values).
    pub fn left_out(&self) -> Vec<usize> {
        self.cols
            .iter()
            .zip(&self.models)
            .filter(|(_, m)| matches!(m, ColModel::Off))
            .map(|(&c, _)| c)
            .collect()
    }

    fn reason(&self, j: usize, v: Option<&str>) -> Option<Reason> {
        let w = self.weights[j];
        if w <= 0.0 {
            return None;
        }
        let n = self.n.max(1);
        let v = v.filter(|s| !s.is_empty());
        let mk = |rows: u64, bits: f64, largest: bool, smallest: bool| Reason {
            col: self.cols[j],
            value: v.map(str::to_string),
            rows,
            of: n,
            largest,
            smallest,
            bits: bits * w,
            together: Vec::new(),
        };
        match &self.models[j] {
            ColModel::Off => None,
            ColModel::Text { counts } => {
                let c = counts.get(v.unwrap_or("")).copied().unwrap_or(1).max(1);
                Some(mk(c, -((c as f64) / n as f64).log2(), false, false))
            }
            ColModel::Num {
                values,
                below,
                empty,
                other,
            } => {
                let Some(s) = v else {
                    let c = (*empty).max(1);
                    return Some(mk(c, -((c as f64) / n as f64).log2(), false, false));
                };
                let Some(x) = crate::ingest::infer::parse_float(s) else {
                    let c = other.get(s).copied().unwrap_or(1).max(1);
                    return Some(mk(c, -((c as f64) / n as f64).log2(), false, false));
                };
                let numbers = *below.last().unwrap_or(&0);
                if numbers == 0 {
                    return None;
                }
                // rows at or below x, and at or above x
                let i = values.partition_point(|p| p.0 < x);
                let at_or_below = if i < values.len() && values[i].0 == x {
                    below[i]
                } else if i == 0 {
                    1
                } else {
                    below[i - 1] + 1
                };
                let before = if i == 0 { 0 } else { below[i - 1] };
                let at_or_above = numbers - before;
                let (tail, largest) = if at_or_above <= at_or_below {
                    (at_or_above.max(1), true)
                } else {
                    (at_or_below.max(1), false)
                };
                let share = (2.0 * tail as f64 / numbers as f64).min(1.0);
                let first = values.first().map(|p| p.0) == Some(x);
                let last = values.last().map(|p| p.0) == Some(x);
                Some(mk(tail, -share.log2(), largest && last, !largest && first))
            }
        }
    }

    /// The row's score: the sum of what each column adds.
    pub fn score(&self, cells: &[Option<String>]) -> f64 {
        let one: f64 = (0..self.cols.len())
            .filter_map(|j| self.reason(j, cells.get(j).and_then(|c| c.as_deref())))
            .map(|r| r.bits)
            .sum();
        one + self.together(cells).map_or(0.0, |r| r.bits)
    }

    /// The numbers of a row for the joint measure (missing: NaN).
    fn numbers(&self, cells: &[Option<String>]) -> Option<Vec<f64>> {
        let j = self.joint.as_ref()?;
        Some(
            j.cols
                .iter()
                .map(|&k| {
                    cells
                        .get(k)
                        .and_then(|c| c.as_deref())
                        .and_then(crate::ingest::infer::parse_float)
                        .unwrap_or(f64::NAN)
                })
                .collect(),
        )
    }

    /// The reason "these numbers are unusual together", when it adds anything.
    fn together(&self, cells: &[Option<String>]) -> Option<Reason> {
        let j = self.joint.as_ref()?;
        let x = self.numbers(cells)?;
        let alone: Vec<f64> = j
            .cols
            .iter()
            .map(|&k| {
                self.reason(k, cells.get(k).and_then(|c| c.as_deref()))
                    .map_or(0.0, |r| r.bits)
            })
            .collect();
        let (bits, by) = j.score(&x, &alone);
        if bits <= 0.0 {
            return None;
        }
        // the columns whose mixes score clearly higher than the mixes without them
        let top = by.iter().cloned().fold(0.0f64, f64::max);
        let mut named: Vec<(usize, f64)> = by
            .iter()
            .enumerate()
            .filter(|(_, &v)| v > 0.0 && v >= top / 2.0)
            .map(|(f, &v)| (self.cols[j.cols[f]], v))
            .collect();
        named.sort_by(|a, b| b.1.total_cmp(&a.1));
        let together: Vec<usize> = named.iter().map(|x| x.0).collect();
        // the numbers named, as the row has them ("Load is 500, Temp is 85"),
        // by the column's place among the columns given
        let said: Vec<String> = together
            .iter()
            .filter_map(|&c| {
                let k = self.cols.iter().position(|&x| x == c)?;
                cells
                    .get(k)
                    .and_then(|v| v.clone())
                    .map(|v| format!("{{{k}}} is {v}"))
            })
            .collect();
        Some(Reason {
            col: together.first().copied().unwrap_or(0),
            value: Some(said.join(", ")),
            rows: 0,
            of: self.n,
            largest: false,
            smallest: false,
            bits,
            together,
        })
    }

    /// Why a row is unusual: what each column adds, most first (only what
    /// adds anything). `cells` are the row's values for the columns given.
    pub fn why(&self, cells: &[Option<String>]) -> Vec<Reason> {
        let mut out: Vec<Reason> = (0..self.cols.len())
            .filter_map(|j| self.reason(j, cells.get(j).and_then(|c| c.as_deref())))
            .filter(|r| r.bits > 0.0)
            .collect();
        out.extend(self.together(cells));
        out.sort_by(|a, b| b.bits.total_cmp(&a.bits));
        out
    }

    /// The columns of numbers read together (store columns), most first.
    pub fn joint_columns(&self) -> Vec<usize> {
        self.joint
            .as_ref()
            .map(|j| j.cols.iter().map(|&k| self.cols[k]).collect())
            .unwrap_or_default()
    }

    /// The columns the model reads, in the order `score` and `why` expect.
    pub fn columns(&self) -> &[usize] {
        &self.cols
    }
}

pub struct Unusual {
    /// Rows by score (half bits).
    pub spread: Vec<u64>,
    /// The middle score (half the rows score less).
    pub usual: f64,
    /// The line used, and the rows at or above it (ids, file order).
    pub line: f64,
    pub rows: Vec<u64>,
    pub model: Arc<Model>,
}

/// Each row's text in `col` for the rows of `m` in block `si`.
fn texts(
    st: &Store,
    blk: &Block,
    fs: &FormulaSet,
    cache: &ChunkCache,
    si: usize,
    m: &SegMatch,
    col: usize,
    mut f: impl FnMut(usize, Option<&str>),
) -> Result<()> {
    if col >= st.schema().len() {
        let mut segs = vec![SegMatch::None; st.segments().len()];
        segs[si] = m.clone();
        let one = MatchSet::from_segs(segs);
        let base = st.segments()[si].meta().row_start;
        let mut start = 0;
        while start < one.count() {
            let p = super::exec::page_with(st, &one, start, 20_000, &[col], fs, cache)?;
            if p.rows.is_empty() {
                break;
            }
            start += p.rows.len() as u64;
            for (g, cells) in p.rows.iter().zip(&p.cells) {
                f(
                    (g - base) as usize,
                    cells.first().and_then(|c| c.as_deref()),
                );
            }
        }
        return Ok(());
    }
    let c = blk.column(col as u32)?;
    each_in(m, |r| match &c {
        None => f(r, None),
        Some(c) => f(r, c.text(r).as_deref()),
    });
    Ok(())
}

/// The numbers of the model's columns at places `which` for every row of a
/// block (row-major, `which.len()` per row; missing: NaN).
fn block_numbers(
    st: &Store,
    fs: &FormulaSet,
    cache: &ChunkCache,
    si: usize,
    m: &SegMatch,
    cols: &[usize],
    which: &[usize],
) -> Result<Vec<f64>> {
    let rows = st.segments()[si].meta().rows as usize;
    let d = which.len();
    let mut x = vec![f64::NAN; rows * d];
    let blk = cache.block(st, si)?;
    for (k, &j) in which.iter().enumerate() {
        texts(st, &blk, fs, cache, si, m, cols[j], |r, v| {
            if let Some(f) = v.and_then(crate::ingest::infer::parse_float) {
                x[r * d + k] = f;
            }
        })?;
    }
    Ok(x)
}

/// What a row is scored for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Weigh {
    /// Every reason: rare values, far-out numbers, numbers unusual together.
    #[default]
    All,
    /// Only numbers that are unusual together.
    Together,
}

/// Learn the rows of `m` for `cols`, score every row, and return the spread
/// with every row at or above `line` (`None`: the line above which the most
/// unusual thousandth lie). `done` counts rows read (two reads).
#[allow(clippy::too_many_arguments)]
pub fn unusual(
    st: &Store,
    m: &MatchSet,
    cols: &[usize],
    line: Option<f64>,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Unusual> {
    unusual_by(st, m, cols, line, Weigh::All, fs, cache, cancel, done)
}

/// `unusual`, scoring rows for `what`.
#[allow(clippy::too_many_arguments)]
pub fn unusual_by(
    st: &Store,
    m: &MatchSet,
    cols: &[usize],
    line: Option<f64>,
    what: Weigh,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Unusual> {
    let n = m.count();
    let ns = st.schema().len();
    let numeric =
        |c: usize| c < ns && matches!(st.schema().columns[c].ty, ColType::Int | ColType::Float);
    // the timeline says when, not what: it is not weighed
    let tcol = st.schema().time_col;
    // 1. count each column's values; a column passing a quarter of the rows
    // in different values is dropped as soon as it does
    let limit = (n / 4).max(16) as usize;
    let dropped: Vec<AtomicBool> = cols.iter().map(|_| AtomicBool::new(false)).collect();
    let seen: Vec<AtomicU64> = cols.iter().map(|_| AtomicU64::new(0)).collect();
    struct Part {
        counts: Vec<HashMap<String, u64>>,
        nums: Vec<HashMap<u64, u64>>,
        empty: Vec<u64>,
        /// Rows where a number was larger than the row before it, and pairs looked at.
        up: Vec<(u64, u64)>,
    }
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || Part {
            counts: vec![HashMap::new(); cols.len()],
            nums: vec![HashMap::new(); cols.len()],
            empty: vec![0; cols.len()],
            up: vec![(0, 0); cols.len()],
        },
        |p, si| {
            let sm = &m.segs[si];
            let blk = cache.block(st, si)?;
            for (j, &c) in cols.iter().enumerate() {
                if Some(c) == tcol || dropped[j].load(Ordering::Relaxed) {
                    continue;
                }
                let num = numeric(c);
                let mut last: Option<f64> = None;
                let mut new = 0u64;
                texts(st, &blk, fs, cache, si, sm, c, |_, v| {
                    let v = v.filter(|s| !s.is_empty());
                    match (num, v) {
                        (_, None) => p.empty[j] += 1,
                        (true, Some(s)) => match crate::ingest::infer::parse_float(s) {
                            Some(x) => {
                                let e = p.nums[j].entry(x.to_bits()).or_insert(0);
                                if *e == 0 {
                                    new += 1;
                                }
                                *e += 1;
                                if let Some(l) = last {
                                    p.up[j].1 += 1;
                                    if x > l {
                                        p.up[j].0 += 1;
                                    }
                                }
                                last = Some(x);
                            }
                            None => *p.counts[j].entry(s.to_string()).or_default() += 1,
                        },
                        (false, Some(s)) => {
                            let e = p.counts[j].entry(s.to_string()).or_insert(0);
                            if *e == 0 {
                                new += 1;
                            }
                            *e += 1;
                        }
                    }
                })?;
                // numbers are always kept (their spread is their measure)
                if !num && seen[j].fetch_add(new, Ordering::Relaxed) + new > limit as u64 {
                    dropped[j].store(true, Ordering::Relaxed);
                    p.counts[j] = HashMap::new();
                }
            }
            done.fetch_add(sm.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    // put the parts together
    let mut counts: Vec<HashMap<String, u64>> = vec![HashMap::new(); cols.len()];
    let mut nums: Vec<HashMap<u64, u64>> = vec![HashMap::new(); cols.len()];
    let mut empty = vec![0u64; cols.len()];
    let mut up = vec![(0u64, 0u64); cols.len()];
    for p in parts {
        for j in 0..cols.len() {
            if dropped[j].load(Ordering::Relaxed) {
                continue;
            }
            for (k, v) in p.counts[j].iter() {
                *counts[j].entry(k.clone()).or_default() += v;
            }
            for (k, v) in p.nums[j].iter() {
                *nums[j].entry(*k).or_default() += v;
            }
            empty[j] += p.empty[j];
            up[j].0 += p.up[j].0;
            up[j].1 += p.up[j].1;
        }
    }
    let mut models: Vec<ColModel> = Vec::with_capacity(cols.len());
    let mut weights: Vec<f64> = Vec::with_capacity(cols.len());
    for j in 0..cols.len() {
        let c = cols[j];
        if Some(c) == tcol || dropped[j].load(Ordering::Relaxed) {
            models.push(ColModel::Off);
            weights.push(0.0);
            continue;
        }
        if numeric(c) {
            let mut values: Vec<(f64, u64)> = nums[j]
                .iter()
                .map(|(b, k)| (f64::from_bits(*b), *k))
                .collect();
            values.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut acc = 0;
            let below: Vec<u64> = values
                .iter()
                .map(|p| {
                    acc += p.1;
                    acc
                })
                .collect();
            // counting up row by row (an id, a sequence): weighs nothing
            let (rises, pairs) = up[j];
            let rising = pairs > 16 && rises * 100 >= pairs * 95;
            weights.push(if rising { 0.0 } else { 1.0 });
            let mut other = std::mem::take(&mut counts[j]);
            other.shrink_to_fit();
            models.push(ColModel::Num {
                values,
                below,
                empty: empty[j],
                other,
            });
        } else {
            let mut t = std::mem::take(&mut counts[j]);
            // the more of its values are all different, the less it weighs,
            // among the rows that have a value (a column filled in few rows,
            // a different value in each, says nothing either)
            let filled = (n - empty[j]) as f64;
            let u = t.len() as f64;
            let w = if filled > 1.0 {
                (1.0 - (u - 1.0) / (filled - 1.0)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if empty[j] > 0 {
                t.insert(String::new(), empty[j]);
            }
            weights.push(w);
            models.push(ColModel::Text { counts: t });
        }
    }
    // numbers unusual together: the columns of numbers that weigh, each on
    // its own scale, mixed in fixed ways; each mix's spread learned in one read
    let mut jcols: Vec<usize> = Vec::new();
    let (mut mid, mut scale) = (Vec::new(), Vec::new());
    for (j, md) in models.iter().enumerate() {
        let ColModel::Num { values, below, .. } = md else {
            continue;
        };
        let total = *below.last().unwrap_or(&0);
        // read together only columns filled in at least half the rows (a mix
        // needs every one of its numbers; columns of one kind of row only
        // are read together once the rows shown are that kind)
        if weights[j] <= 0.0 || total < 16 || values.len() < 3 || total * 2 < n {
            continue;
        }
        let at = |share: f64| {
            let want = ((total as f64 * share).ceil() as u64).max(1);
            values[below.partition_point(|&b| b < want).min(values.len() - 1)].0
        };
        let m = at(0.5);
        let mut dev: Vec<(f64, u64)> = values.iter().map(|&(v, k)| ((v - m).abs(), k)).collect();
        dev.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut acc = 0;
        let mut mad = 0.0;
        for (d, k) in &dev {
            acc += k;
            if acc * 2 >= total {
                mad = *d;
                break;
            }
        }
        if mad <= 0.0 {
            // most rows share one value: use the spread of the rest
            mad = (at(0.9) - at(0.1)) / 2.56;
        }
        if mad > 0.0 && mad.is_finite() {
            jcols.push(j);
            mid.push(m);
            scale.push(mad * 1.4826);
        }
    }
    let mut joint = None;
    if jcols.len() >= 2 {
        let shape = Joint::mixes(jcols.len());
        let mut j = Joint {
            cols: jcols.clone(),
            mid,
            scale,
            mixes: shape
                .into_iter()
                .map(|(feat, w)| Mix {
                    feat,
                    w,
                    hist: vec![0; BINS],
                    total: 0,
                    usual: 0.0,
                })
                .collect(),
        };
        let parts = over_blocks(
            st,
            cache,
            |si| m.segs[si].count() > 0,
            cancel,
            || vec![vec![0u64; BINS]; MIXES],
            |h, si| {
                let x = block_numbers(st, fs, cache, si, &m.segs[si], cols, &jcols)?;
                let d = jcols.len();
                each_in(&m.segs[si], |r| {
                    let row = &x[r * d..(r + 1) * d];
                    for (k, mx) in j.mixes.iter().enumerate() {
                        if let Some(z) = j.value(mx, row) {
                            h[k][bin(z)] += 1;
                        }
                    }
                });
                done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
                Ok(())
            },
        )?;
        for p in parts {
            for (mx, h) in j.mixes.iter_mut().zip(p) {
                for (a, b) in mx.hist.iter_mut().zip(h) {
                    *a += b;
                }
            }
        }
        for mx in j.mixes.iter_mut() {
            mx.total = mx.hist.iter().sum();
            let t = mx.total as f64 + BINS as f64;
            mx.usual = mx
                .hist
                .iter()
                .map(|&c| {
                    let q = c as f64 / mx.total.max(1) as f64;
                    let p = (c as f64 + 1.0) / t;
                    -q * p.log2()
                })
                .sum();
        }
        joint = Some(j);
    }
    let model = Arc::new(Model {
        cols: cols.to_vec(),
        models,
        weights,
        n,
        joint,
    });
    // 2. every row scored: the spread, and the rows at or above the line
    let used: Vec<(usize, usize)> = cols
        .iter()
        .enumerate()
        .filter(|(j, _)| model.weights[*j] > 0.0)
        .map(|(j, &c)| (j, c))
        .collect();
    let score_block = |si: usize, f: &mut dyn FnMut(usize, f64)| -> Result<()> {
        let sm = &m.segs[si];
        let rows = st.segments()[si].meta().rows as usize;
        let mut score = vec![0.0f64; rows];
        let blk = cache.block(st, si)?;
        // each number's own rarity, kept for the joint measure
        let jd = model.joint.as_ref().map_or(0, |j| j.cols.len());
        let mut alone = vec![0.0f64; rows * jd];
        for &(j, c) in &used {
            let place = model
                .joint
                .as_ref()
                .and_then(|jt| jt.cols.iter().position(|&k| k == j));
            if what == Weigh::Together && place.is_none() {
                continue;
            }
            texts(st, &blk, fs, cache, si, sm, c, |r, v| {
                if let Some(x) = model.reason(j, v) {
                    if what == Weigh::All {
                        score[r] += x.bits;
                    }
                    if let Some(k) = place {
                        alone[r * jd + k] = x.bits;
                    }
                }
            })?;
        }
        if let Some(j) = model.joint.as_ref() {
            let x = block_numbers(st, fs, cache, si, sm, cols, &j.cols)?;
            let d = j.cols.len();
            each_in(sm, |r| {
                score[r] += j
                    .score(&x[r * d..(r + 1) * d], &alone[r * d..(r + 1) * d])
                    .0;
            });
        }
        each_in(sm, |r| f(r, score[r]));
        Ok(())
    };
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || vec![0u64; BUCKETS],
        |spread, si| {
            score_block(si, &mut |_, s| spread[bucket(s)] += 1)?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut spread = vec![0u64; BUCKETS];
    for p in parts {
        for (a, b) in spread.iter_mut().zip(p) {
            *a += b;
        }
    }
    let at_share = |share: f64| -> f64 {
        let want = ((n as f64 * share).ceil() as u64).max(1);
        let mut acc = 0;
        for (i, k) in spread.iter().enumerate() {
            acc += k;
            if acc >= want {
                return i as f64 * STEP;
            }
        }
        (BUCKETS - 1) as f64 * STEP
    };
    let usual = at_share(0.5);
    let line = line.unwrap_or_else(|| at_share(0.999) + STEP / 2.0);
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        Vec::<u64>::new,
        |ids, si| {
            let base = st.segments()[si].meta().row_start;
            score_block(si, &mut |r, s| {
                if s >= line {
                    ids.push(base + r as u64);
                }
            })?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut rows: Vec<u64> = parts.into_iter().flatten().collect();
    rows.sort_unstable();
    Ok(Unusual {
        spread,
        usual,
        line,
        rows,
        model,
    })
}
