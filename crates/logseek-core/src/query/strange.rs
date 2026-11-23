//! Strange text for a column: which values are written unlike the rest of
//! the column. The column itself is the measure: one read learns how often
//! each byte follows each other byte in its values (a 256 x 256 table, about
//! half a megabyte, nothing known in advance), and each value is then scored
//! by how unusual its byte pairs are there, in bits per pair. A column of
//! paths, of SQL, of names in any language or of commands each sets its own
//! "usual"; encoded blobs, garbled text, broken lines and obfuscated commands
//! stand out against it.

use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::Result;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Buckets of the spread of scores: tenths of a bit, up to 16 bits.
pub const BUCKETS: usize = 161;

/// How often each byte follows each other byte in a column.
pub struct Model {
    pairs: Vec<u64>,
    from: Vec<u64>,
}

impl Model {
    fn new() -> Model {
        Model {
            pairs: vec![0; 256 * 256],
            from: vec![0; 256],
        }
    }
    fn learn(&mut self, b: &[u8]) {
        for w in b.windows(2) {
            self.pairs[(w[0] as usize) << 8 | w[1] as usize] += 1;
            self.from[w[0] as usize] += 1;
        }
    }
    fn add(&mut self, o: &Model) {
        for (a, b) in self.pairs.iter_mut().zip(&o.pairs) {
            *a += b;
        }
        for (a, b) in self.from.iter_mut().zip(&o.from) {
            *a += b;
        }
    }
    /// Bits per byte pair: how unusual the value's pairs are in this column
    /// (every pair counted once more, so a pair never seen is rare, not
    /// impossible). `None` for a value of fewer than two bytes.
    pub fn score(&self, b: &[u8]) -> Option<f64> {
        if b.len() < 2 {
            return None;
        }
        let mut bits = 0.0;
        for w in b.windows(2) {
            let n = self.pairs[(w[0] as usize) << 8 | w[1] as usize] as f64 + 1.0;
            let d = self.from[w[0] as usize] as f64 + 256.0;
            bits -= (n / d).log2();
        }
        Some(bits / (b.len() - 1) as f64)
    }
}

fn bucket(score: f64) -> usize {
    ((score * 10.0).round() as usize).min(BUCKETS - 1)
}

/// A value stranger than the line asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct StrangeValue {
    pub text: String,
    /// Bits per byte pair.
    pub score: f64,
    pub rows: u64,
}

pub struct Strange {
    /// Rows by score (tenths of a bit); values too short to score are apart.
    pub spread: Vec<u64>,
    pub unscored: u64,
    /// The middle score of the column (half its scored rows are below it).
    pub usual: f64,
    /// Every value scoring at least the line asked for, strangest first.
    pub values: Vec<StrangeValue>,
    /// The rows of those values (ids, in file order).
    pub rows: Vec<u64>,
    /// The line used.
    pub line: f64,
}

/// Each row's text in `col` for the rows of `m` in block `si`, as bytes.
fn each_text(
    st: &Store,
    cache: &ChunkCache,
    fs: &FormulaSet,
    si: usize,
    m: &SegMatch,
    col: usize,
    mut f: impl FnMut(usize, &[u8]),
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
                if let Some(v) = cells.first().and_then(|c| c.as_deref()) {
                    f((g - base) as usize, v.as_bytes());
                }
            }
        }
        return Ok(());
    }
    let blk = cache.block(st, si)?;
    let Some(c) = blk.column(col as u32)? else {
        return Ok(());
    };
    each_in(m, |r| {
        if let Some(b) = c.text_bytes(r) {
            f(r, b);
        } else if let Some(t) = c.text(r) {
            f(r, t.as_bytes());
        }
    });
    Ok(())
}

/// Learn the column's text on the rows of `m`, score every value, and return
/// the spread of scores with every value scoring at least `line` bits per pair
/// (`None`: the line above which the strangest thousandth of the scored rows
/// lie). Counts are exact; `done` counts rows read (three reads).
#[allow(clippy::too_many_arguments)]
pub fn strange_text(
    st: &Store,
    m: &MatchSet,
    col: usize,
    line: Option<f64>,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Strange> {
    let some = |si: usize| m.segs[si].count() > 0;
    // 1. learn
    let parts = over_blocks(st, cache, some, cancel, Model::new, |model, si| {
        each_text(st, cache, fs, si, &m.segs[si], col, |_, b| model.learn(b))?;
        done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
        Ok(())
    })?;
    let mut model = Model::new();
    for p in &parts {
        model.add(p);
    }
    // 2. the spread of scores
    let parts = over_blocks(
        st,
        cache,
        some,
        cancel,
        || (vec![0u64; BUCKETS], 0u64),
        |(spread, unscored), si| {
            each_text(st, cache, fs, si, &m.segs[si], col, |_, b| {
                match model.score(b) {
                    Some(s) => spread[bucket(s)] += 1,
                    None => *unscored += 1,
                }
            })?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut spread = vec![0u64; BUCKETS];
    let mut unscored = 0;
    for (s, u) in parts {
        for (a, b) in spread.iter_mut().zip(s) {
            *a += b;
        }
        unscored += u;
    }
    let scored: u64 = spread.iter().sum();
    let at_share = |share: f64| -> f64 {
        // the score below which `share` of the scored rows lie
        let want = (scored as f64 * share).ceil() as u64;
        let mut acc = 0;
        for (i, n) in spread.iter().enumerate() {
            acc += n;
            if acc >= want.max(1) {
                return i as f64 / 10.0;
            }
        }
        (BUCKETS - 1) as f64 / 10.0
    };
    let usual = at_share(0.5);
    let line = line.unwrap_or_else(|| at_share(0.999) + 0.05);
    // 3. every value at or above the line, with its rows
    let parts = over_blocks(
        st,
        cache,
        some,
        cancel,
        || (HashMap::<Vec<u8>, (f64, u64)>::new(), Vec::<u64>::new()),
        |(vals, rows), si| {
            let base = st.segments()[si].meta().row_start;
            each_text(st, cache, fs, si, &m.segs[si], col, |r, b| {
                if let Some(s) = model.score(b).filter(|&s| s >= line) {
                    vals.entry(b.to_vec()).or_insert((s, 0)).1 += 1;
                    rows.push(base + r as u64);
                }
            })?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut all: HashMap<Vec<u8>, (f64, u64)> = HashMap::new();
    let mut rows = Vec::new();
    for (v, r) in parts {
        for (k, (s, n)) in v {
            all.entry(k).or_insert((s, 0)).1 += n;
        }
        rows.extend(r);
    }
    rows.sort_unstable();
    let mut values: Vec<StrangeValue> = all
        .into_iter()
        .map(|(k, (score, n))| StrangeValue {
            text: String::from_utf8_lossy(&k).into_owned(),
            score,
            rows: n,
        })
        .collect();
    values.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.text.cmp(&b.text)));
    Ok(Strange {
        spread,
        unscored,
        usual,
        values,
        rows,
        line,
    })
}

/// The share of scored rows whose score is below `score`, from a spread.
pub fn share_below(spread: &[u64], score: f64) -> f64 {
    let total: u64 = spread.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let below: u64 = spread[..bucket(score).min(spread.len())].iter().sum();
    below as f64 / total as f64
}
