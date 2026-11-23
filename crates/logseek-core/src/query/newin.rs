//! New in a period: for each column, the values seen in the rows of a period
//! and never in the rows it is compared with (everything before it, or the
//! same length just before), counted exactly, however rare.
//!
//! Whether a new value means anything depends on the column, and the column
//! says so itself: in a column that had few different values before (the apps
//! that run, the users who log on, a tactic, a level) a new one stands out; in
//! a column with a different value on nearly every row (an id, a thread, a
//! byte count) new is ordinary. Each column carries how many different values
//! it had before and in how many rows, and the columns with the fewest values
//! for their rows come first; how settled it is in the period is said too.
//! Nothing about the columns is known in advance.

use super::contrast::{View, NONE};
use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet};
use crate::error::{Error, Result};
use crate::store::Store;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A value of a column never seen before the period, and its rows in it.
#[derive(Clone, Debug, PartialEq)]
pub struct NewValue {
    pub value: String,
    pub rows: u64,
}

/// One column's new values.
#[derive(Clone, Debug)]
pub struct ColumnNew {
    pub col: usize,
    /// Rows of the period with a value in this column, and those among them
    /// whose value was never seen before.
    pub filled: u64,
    pub new_rows: u64,
    /// Most rows first.
    pub values: Vec<NewValue>,
    /// Before the period: different values (counted exactly up to 1,024,
    /// estimated beyond) and rows with a value.
    pub before_values: u64,
    pub before_filled: u64,
}

impl ColumnNew {
    /// The share of the period's rows (with a value) whose value was seen before.
    pub fn settled(&self) -> f64 {
        if self.filled == 0 {
            return 1.0;
        }
        1.0 - self.new_rows as f64 / self.filled as f64
    }
    /// Different values before for each row with a value before: near 0 for a
    /// column whose values keep coming back, near 1 for one that differs on
    /// every row (0 when it had no value at all before).
    pub fn variety_before(&self) -> f64 {
        if self.before_filled == 0 {
            return 0.0;
        }
        self.before_values as f64 / self.before_filled as f64
    }
}

/// How many different values a column has, in fixed memory: the K smallest
/// of the values' (well-mixed) keys; exact while fewer than K.
const K: usize = 1024;

#[derive(Clone, Default)]
struct Kmv {
    least: Vec<u64>,
}

fn mix(mut x: u64) -> u64 {
    // splitmix64: spreads the keys evenly over all 64 bits
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Kmv {
    fn add(&mut self, key: u64) {
        let h = mix(key);
        if self.least.len() == K && h >= *self.least.last().unwrap() {
            return;
        }
        if let Err(i) = self.least.binary_search(&h) {
            self.least.insert(i, h);
            if self.least.len() > K {
                self.least.pop();
            }
        }
    }
    fn merge(&mut self, o: &Kmv) {
        for &h in &o.least {
            if self.least.len() == K && h >= *self.least.last().unwrap() {
                continue;
            }
            if let Err(i) = self.least.binary_search(&h) {
                self.least.insert(i, h);
                if self.least.len() > K {
                    self.least.pop();
                }
            }
        }
    }
    fn count(&self) -> u64 {
        if self.least.len() < K {
            return self.least.len() as u64;
        }
        let kth = *self.least.last().unwrap() as f64 / u64::MAX as f64;
        ((K - 1) as f64 / kth).round() as u64
    }
}

pub struct NewValues {
    /// Rows of the period and of what it is compared with.
    pub here: u64,
    pub before: u64,
    /// Columns with something new, steadiest first.
    pub columns: Vec<ColumnNew>,
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// Per column: each value's key -> (rows, block, a row in that block).
type Seen = HashMap<u64, (u64, u32, u32)>;

/// The values of `cols` in the rows of `here` never seen in the rows of `before`.
#[allow(clippy::too_many_arguments)]
pub fn new_values(
    st: &Store,
    here: &MatchSet,
    before: &MatchSet,
    cols: &[usize],
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<NewValues> {
    // 1. every value of the period, with its rows and where one of them is
    let parts = over_blocks(
        st,
        cache,
        |si| here.segs[si].count() > 0,
        cancel,
        || (vec![Seen::new(); cols.len()], vec![0u64; cols.len()]),
        |(maps, filled), si| {
            let m = &here.segs[si];
            let blk = cache.block(st, si)?;
            for (j, &c) in cols.iter().enumerate() {
                check(cancel)?;
                let v = View::open(st, &blk, si, c, &[m], fs, cache)?;
                if matches!(v, View::Empty) {
                    continue;
                }
                each_in(m, |r| {
                    let k = v.key(r);
                    if k != NONE {
                        filled[j] += 1;
                        maps[j].entry(k).or_insert((0, si as u32, r as u32)).0 += 1;
                    }
                });
            }
            done.fetch_add(m.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut maps: Vec<Seen> = vec![Seen::new(); cols.len()];
    let mut filled = vec![0u64; cols.len()];
    for (pm, pf) in parts {
        for (j, part) in pm.into_iter().enumerate() {
            filled[j] += pf[j];
            for (k, (n, si, r)) in part {
                maps[j].entry(k).or_insert((0, si, r)).0 += n;
            }
        }
    }
    // 2. what of it was seen before: struck out; and how varied each column was
    struct Before {
        seen: Vec<HashSet<u64>>,
        kinds: Vec<Kmv>,
        filled: Vec<u64>,
    }
    let parts = over_blocks(
        st,
        cache,
        |si| before.segs[si].count() > 0,
        cancel,
        || Before {
            seen: vec![HashSet::new(); cols.len()],
            kinds: vec![Kmv::default(); cols.len()],
            filled: vec![0; cols.len()],
        },
        |b, si| {
            let m = &before.segs[si];
            let blk = cache.block(st, si)?;
            for (j, &c) in cols.iter().enumerate() {
                if maps[j].is_empty() {
                    continue;
                }
                check(cancel)?;
                let v = View::open(st, &blk, si, c, &[m], fs, cache)?;
                if matches!(v, View::Empty) {
                    continue;
                }
                let map = &maps[j];
                let (s, kinds, filled) = (&mut b.seen[j], &mut b.kinds[j], &mut b.filled[j]);
                each_in(m, |r| {
                    let k = v.key(r);
                    if k != NONE {
                        *filled += 1;
                        kinds.add(k);
                        if map.contains_key(&k) {
                            s.insert(k);
                        }
                    }
                });
            }
            done.fetch_add(m.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut kinds: Vec<Kmv> = vec![Kmv::default(); cols.len()];
    let mut before_filled = vec![0u64; cols.len()];
    for part in parts {
        for (j, keys) in part.seen.into_iter().enumerate() {
            for k in keys {
                maps[j].remove(&k);
            }
        }
        for (j, kv) in part.kinds.iter().enumerate() {
            kinds[j].merge(kv);
            before_filled[j] += part.filled[j];
        }
    }
    // 3. the text of each new value, read back block by block
    let mut want: Vec<(u32, usize, u32, u64)> = Vec::new(); // (block, column, row, rows)
    for (j, map) in maps.iter().enumerate() {
        for &(n, si, r) in map.values() {
            want.push((si, j, r, n));
        }
    }
    want.sort_unstable();
    let mut values: Vec<Vec<NewValue>> = vec![Vec::new(); cols.len()];
    let mut i = 0;
    while i < want.len() {
        check(cancel)?;
        let si = want[i].0;
        let m = &here.segs[si as usize];
        let blk = cache.block(st, si as usize)?;
        let mut views: HashMap<usize, View> = HashMap::new();
        while i < want.len() && want[i].0 == si {
            let (_, j, r, n) = want[i];
            if !views.contains_key(&j) {
                views.insert(
                    j,
                    View::open(st, &blk, si as usize, cols[j], &[m], fs, cache)?,
                );
            }
            if let Some(t) = views[&j].text(r as usize) {
                values[j].push(NewValue { value: t, rows: n });
            }
            i += 1;
        }
    }
    let mut columns: Vec<ColumnNew> = values
        .into_iter()
        .enumerate()
        .filter(|(_, v)| !v.is_empty())
        .map(|(j, mut v)| {
            v.sort_by(|a, b| b.rows.cmp(&a.rows).then_with(|| a.value.cmp(&b.value)));
            ColumnNew {
                col: cols[j],
                filled: filled[j],
                new_rows: v.iter().map(|x| x.rows).sum(),
                values: v,
                before_values: kinds[j].count(),
                before_filled: before_filled[j],
            }
        })
        .collect();
    // the fewest different values for their rows before first, then the steadiest
    columns.sort_by(|a, b| {
        a.variety_before()
            .total_cmp(&b.variety_before())
            .then(b.settled().total_cmp(&a.settled()))
            .then(a.col.cmp(&b.col))
    });
    Ok(NewValues {
        here: here.count(),
        before: before.count(),
        columns,
    })
}
