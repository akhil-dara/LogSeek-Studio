//! What sets some rows apart from the others.
//!
//! Given the rows of interest ("here": a busy period, the errors, the tagged
//! rows) and the rows to compare with ("the rest"), find the values, and the
//! pairs of values, that are far more common here than in the rest. Each
//! answer is a plain statement with exact counts: "App is v2 and Region is ap:
//! in 2,700 of these 3,000 rows (90%), 460 of the other 23,000 (2%), 45 times
//! more common here".
//!
//! The method follows the "explain the outliers" line of work (support in the
//! rows of interest, then the ratio of shares), done the way this tool reads
//! data:
//!
//! 1. each column is read for the rows of interest only, and its values are
//!    counted (by the index's value codes where it has them), keeping those
//!    in enough of these rows to matter;
//! 2. one read of both sides, of those columns only, counts those values
//!    exactly everywhere;
//! 3. one more read, of the columns whose values are commoner here, counts
//!    every pair of them exactly.
//!
//! Blocks are worked on side by side, on every core the machine has as far
//! as memory allows; a block kept in the source file is parsed once per read.
//!
//! Nothing about the file is assumed: a column whose values are all
//! different, or the same everywhere, simply never stands out.

use super::exec::{
    each_in, over_blocks, page_with, Block, ChunkCache, FormulaSet, MatchSet, SegMatch,
};
use crate::error::{Error, Result};
use crate::segment::{ColData, DecodedColumn};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// How much stronger two values together must be than the stronger of the
/// two alone for the pair to be offered.
const PAIR_GAIN: f64 = 1.5;

/// How strong a difference must be to be offered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContrastOpts {
    /// In at least this share of the rows of interest (0.05 = 5%).
    pub min_share: f64,
    /// At least this many times more common there than in the rest.
    pub min_ratio: f64,
}

impl Default for ContrastOpts {
    fn default() -> Self {
        ContrastOpts {
            min_share: 0.05,
            min_ratio: 2.0,
        }
    }
}

/// "Column is value"; `None` means the cell is empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cond {
    /// Index into the store's columns (made columns after the stored ones).
    pub col: usize,
    pub value: Option<String>,
}

/// One difference: a value, or two values together.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// One or two conditions, by column order.
    pub conds: Vec<Cond>,
    /// For each condition, the others that hold in exactly the same rows
    /// (on both sides): said once, not as lines of their own.
    pub same: Vec<Vec<Cond>>,
    /// A pair whose second part could be any of several conditions with the
    /// very same counts: which condition (its place in `conds`) and the
    /// others that could stand in for it. Said once, not as lines of their own.
    pub instead: Option<(usize, Vec<Cond>)>,
    /// Rows of interest that meet every condition, and how many there are.
    pub here: u64,
    pub here_total: u64,
    /// The same for the rows compared with.
    pub rest: u64,
    pub rest_total: u64,
}

impl Finding {
    pub fn share_here(&self) -> f64 {
        share(self.here, self.here_total)
    }
    pub fn share_rest(&self) -> f64 {
        share(self.rest, self.rest_total)
    }
    /// How many times more common here; infinite when the rest never has it.
    pub fn ratio(&self) -> f64 {
        ratio(self.here, self.here_total, self.rest, self.rest_total)
    }
}

fn share(a: u64, n: u64) -> f64 {
    if n == 0 {
        0.0
    } else {
        a as f64 / n as f64
    }
}

fn ratio(a: u64, n: u64, b: u64, m: u64) -> f64 {
    let (x, y) = (share(a, n), share(b, m));
    if y == 0.0 {
        if x > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else {
        x / y
    }
}

pub struct Contrast {
    /// Strongest first (most times more common here), then the most rows here.
    pub findings: Vec<Finding>,
    pub here_total: u64,
    pub rest_total: u64,
    /// Columns looked at, and how many values were counted on both sides.
    pub columns: usize,
    pub candidates: usize,
}

// ───────────── reading one column of one block ─────────────

/// The key of an empty cell.
pub(crate) const NONE: u64 = 0;

/// A value's key: a 64-bit hash of its text (never 0, which is "empty").
fn key_of(b: &[u8]) -> u64 {
    if b.is_empty() {
        return NONE;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= h >> 29;
    h | 1
}

/// One column of one block, ready to give each row's key and text.
pub(crate) enum View {
    /// The block has no value in this column.
    Empty,
    /// A stored column, with the key of each dictionary entry when it has one.
    Stored(Arc<DecodedColumn>, Vec<u64>),
    /// A made column, worked out for the rows asked for (row in block -> text).
    Made(HashMap<u32, String>),
}

impl View {
    pub(crate) fn open(
        st: &Store,
        blk: &Block,
        si: usize,
        col: usize,
        rows: &[&SegMatch],
        fs: &FormulaSet,
        cache: &ChunkCache,
    ) -> Result<View> {
        if col >= st.schema().len() {
            let mut segs = vec![SegMatch::None; st.segments().len()];
            let mut out = HashMap::new();
            let base = st.segments()[si].meta().row_start;
            for m in rows {
                segs[si] = (*m).clone();
                let one = MatchSet::from_segs(segs.clone());
                let mut start = 0;
                while start < one.count() {
                    let p = page_with(st, &one, start, 20_000, &[col], fs, cache)?;
                    if p.rows.is_empty() {
                        break;
                    }
                    start += p.rows.len() as u64;
                    for (g, cells) in p.rows.iter().zip(p.cells) {
                        if let Some(v) =
                            cells.into_iter().next().flatten().filter(|v| !v.is_empty())
                        {
                            out.insert((g - base) as u32, v);
                        }
                    }
                }
            }
            return Ok(View::Made(out));
        }
        let Some(c) = blk.column(col as u32)? else {
            return Ok(View::Empty);
        };
        let keys = match c.data() {
            ColData::Dict { dict_off, .. } => (0..dict_off.len().saturating_sub(1))
                .map(|k| c.dict_entry(k).map_or(NONE, |s| key_of(s.as_bytes())))
                .collect(),
            _ => Vec::new(),
        };
        Ok(View::Stored(c, keys))
    }

    #[inline]
    pub(crate) fn key(&self, r: usize) -> u64 {
        match self {
            View::Empty => NONE,
            View::Made(m) => m.get(&(r as u32)).map_or(NONE, |s| key_of(s.as_bytes())),
            View::Stored(c, keys) => {
                if c.is_null(r) {
                    return NONE;
                }
                match c.data() {
                    ColData::Dict { codes, .. } => keys[codes[r] as usize],
                    ColData::Plain { .. } => c.text_bytes(r).map_or(NONE, key_of),
                    _ => c.text(r).map_or(NONE, |t| key_of(t.as_bytes())),
                }
            }
        }
    }

    pub(crate) fn text(&self, r: usize) -> Option<String> {
        match self {
            View::Empty => None,
            View::Made(m) => m.get(&(r as u32)).cloned(),
            View::Stored(c, _) => c.text(r).map(|t| t.into_owned()).filter(|t| !t.is_empty()),
        }
    }
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// A value being counted: how often, and one row that holds it (block, row)
/// so its text can be read back once it is known to matter.
#[derive(Clone, Copy)]
struct Tally {
    n: u64,
    si: u32,
    r: u32,
}

/// Put two summaries of `k` counters together, keeping the guarantee that
/// any value in more than 1/(k+1) of the rows counted is kept, whatever the
/// number of different values: add the counts, then take away the (k+1)-th
/// largest from every counter. An exact count of one block is a summary too,
/// so each block is counted exactly and folded in at once. The counts it ends
/// with are lower bounds; the values kept are counted exactly afterwards.
fn merge_summaries(a: &mut HashMap<u64, Tally>, b: HashMap<u64, Tally>, k: usize) {
    for (key, t) in b {
        match a.get_mut(&key) {
            Some(e) => e.n += t.n,
            None => {
                a.insert(key, t);
            }
        }
    }
    if a.len() > k {
        let mut ns: Vec<u64> = a.values().map(|t| t.n).collect();
        let (_, &mut cut, _) = ns.select_nth_unstable_by(k, |x, y| y.cmp(x));
        for t in a.values_mut() {
            t.n = t.n.saturating_sub(cut);
        }
        a.retain(|_, t| t.n > 0);
    }
}

/// Per column of `cols`, the values that may be in at least `need` of the
/// rows of `here`, with their text. Read block by block, every column of a
/// block from one parse.
#[allow(clippy::too_many_arguments)]
fn frequent(
    st: &Store,
    here: &MatchSet,
    cols: &[usize],
    k: usize,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Vec<Vec<(u64, Option<String>)>>> {
    let parts = over_blocks(
        st,
        cache,
        |si| here.segs[si].count() > 0,
        cancel,
        || vec![HashMap::<u64, Tally>::new(); cols.len()],
        |tops, si| {
            let m = &here.segs[si];
            let blk = cache.block(st, si)?;
            let mut local: HashMap<u64, (u64, u32)> = HashMap::new();
            for (j, &c) in cols.iter().enumerate() {
                check(cancel)?;
                let v = View::open(st, &blk, si, c, &[m], fs, cache)?;
                // this block's counts first, then into the column's summary
                local.clear();
                match &v {
                    View::Stored(col, keys) if matches!(col.data(), ColData::Dict { .. }) => {
                        let ColData::Dict { codes, .. } = col.data() else {
                            unreachable!()
                        };
                        let mut by_code: Vec<(u64, u32)> = vec![(0, 0); keys.len()];
                        let mut nulls = (0u64, 0u32);
                        each_in(m, |r| {
                            let e = if col.is_null(r) {
                                &mut nulls
                            } else {
                                &mut by_code[codes[r] as usize]
                            };
                            e.0 += 1;
                            e.1 = r as u32;
                        });
                        if nulls.0 > 0 {
                            local.insert(NONE, nulls);
                        }
                        for (code, &(n, r)) in by_code.iter().enumerate() {
                            if n > 0 {
                                local.entry(keys[code]).or_insert((0, r)).0 += n;
                            }
                        }
                    }
                    View::Empty => {
                        local.insert(NONE, (m.count(), 0));
                    }
                    _ => each_in(m, |r| {
                        local.entry(v.key(r)).or_insert((0, r as u32)).0 += 1;
                    }),
                }
                // this block's exact counts, folded into the column's summary
                let part: HashMap<u64, Tally> = local
                    .drain()
                    .map(|(key, (n, r))| {
                        (
                            key,
                            Tally {
                                n,
                                si: si as u32,
                                r,
                            },
                        )
                    })
                    .collect();
                merge_summaries(&mut tops[j], part, k);
            }
            done.fetch_add(m.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut tops: Vec<HashMap<u64, Tally>> = vec![HashMap::new(); cols.len()];
    for part in parts {
        for (j, t) in part.into_iter().enumerate() {
            merge_summaries(&mut tops[j], t, k);
        }
    }
    // the text of each value kept, read back block by block
    let mut want: Vec<(u32, usize, u32, u64)> = Vec::new(); // (block, column, row, key)
    let mut out: Vec<Vec<(u64, Option<String>)>> = vec![Vec::new(); cols.len()];
    for (j, t) in tops.iter().enumerate() {
        for (&key, e) in t {
            if key == NONE {
                out[j].push((NONE, None));
            } else {
                want.push((e.si, j, e.r, key));
            }
        }
    }
    want.sort_unstable();
    let mut i = 0;
    while i < want.len() {
        let si = want[i].0;
        let m = &here.segs[si as usize];
        let blk = cache.block(st, si as usize)?;
        while i < want.len() && want[i].0 == si {
            let (_, j, r, key) = want[i];
            let v = View::open(st, &blk, si as usize, cols[j], &[m], fs, cache)?;
            out[j].push((key, v.text(r as usize)));
            i += 1;
        }
    }
    Ok(out)
}

/// Pairs that share one condition and have the very same counts on both
/// sides become one line: the shared condition with one partner, and the
/// other partners named as able to stand in for it. The largest such group
/// is folded first; a pair already folded into one is not folded again.
/// Order is kept.
pub fn fold_alternatives(findings: Vec<Finding>) -> Vec<Finding> {
    // (shared condition, here, rest) -> the pairs having it
    let mut groups: HashMap<(Cond, u64, u64), Vec<usize>> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if f.conds.len() == 2 {
            for c in &f.conds {
                groups
                    .entry((c.clone(), f.here, f.rest))
                    .or_default()
                    .push(i);
            }
        }
    }
    let mut order: Vec<((Cond, u64, u64), Vec<usize>)> =
        groups.into_iter().filter(|(_, v)| v.len() > 1).collect();
    order.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.1[0].cmp(&b.1[0])));
    let mut gone = vec![false; findings.len()];
    let mut instead: HashMap<usize, (Cond, Vec<Cond>)> = HashMap::new();
    for ((shared, _, _), members) in order {
        let live: Vec<usize> = members
            .into_iter()
            .filter(|&i| !gone[i] && !instead.contains_key(&i))
            .collect();
        if live.len() < 2 {
            continue;
        }
        let other = |i: usize| -> Cond {
            findings[i]
                .conds
                .iter()
                .find(|c| **c != shared)
                .cloned()
                .unwrap_or_else(|| findings[i].conds[0].clone())
        };
        let keep = live[0];
        let alts: Vec<Cond> = live[1..].iter().map(|&i| other(i)).collect();
        for &i in &live[1..] {
            gone[i] = true;
        }
        instead.insert(keep, (other(keep), alts));
    }
    findings
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !gone[*i])
        .map(|(i, mut f)| {
            if let Some((partner, alts)) = instead.remove(&i) {
                let k = f.conds.iter().position(|c| *c == partner).unwrap_or(0);
                f.instead = Some((k, alts));
            }
            f
        })
        .collect()
}

/// The values, and pairs of values, of `cols` that set the rows of `here`
/// apart from the rows of `rest`. Counts are exact. `done` counts rows read.
#[allow(clippy::too_many_arguments)]
pub fn contrast(
    st: &Store,
    here: &MatchSet,
    rest: &MatchSet,
    cols: &[usize],
    fs: &FormulaSet,
    cache: &ChunkCache,
    o: &ContrastOpts,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Contrast> {
    check(cancel)?;
    let (n_here, n_rest) = (here.count(), rest.count());
    let empty = |candidates| Contrast {
        findings: Vec::new(),
        here_total: n_here,
        rest_total: n_rest,
        columns: cols.len(),
        candidates,
    };
    if n_here == 0 || n_rest == 0 || cols.is_empty() {
        return Ok(empty(0));
    }
    // dev switch: LOGSEEK_CONTRAST_PROF=1 says how long each read took
    let t0 = std::time::Instant::now();
    let show = cfg!(any(debug_assertions, feature = "devtools"))
        && std::env::var_os("LOGSEEK_CONTRAST_PROF").is_some();
    let prof = |what: &str, n: usize| {
        if show {
            eprintln!("  {:>7.1} s  {what} ({n})", t0.elapsed().as_secs_f64());
        }
    };
    let min_share = o.min_share.clamp(0.0, 1.0);
    let need = ((min_share * n_here as f64).ceil() as u64).max(1);
    let k = ((1.0 / min_share.max(1e-6)).ceil() as usize + 1).clamp(2, 1 << 20);
    let none = SegMatch::None;
    let rest_seg = |si: usize| rest.segs.get(si).unwrap_or(&none);
    let either = |si: usize| here.segs[si].count() > 0 || rest_seg(si).count() > 0;

    // 1. per column, the values in enough of the rows of interest
    struct Item {
        col: usize,
        key: u64,
        text: Option<String>,
    }
    let mut items: Vec<Item> = Vec::new();
    for (j, vals) in frequent(st, here, cols, k, fs, cache, cancel, done)?
        .into_iter()
        .enumerate()
    {
        for (key, text) in vals {
            items.push(Item {
                col: cols[j],
                key,
                text,
            });
        }
    }
    let candidates = items.len();
    prof("values common enough here", candidates);
    if items.is_empty() {
        return Ok(empty(0));
    }
    let by_col = |want: &dyn Fn(usize) -> bool| -> Vec<(usize, HashMap<u64, usize>)> {
        let mut m: Vec<(usize, HashMap<u64, usize>)> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if !want(i) {
                continue;
            }
            match m.iter_mut().find(|(c, _)| *c == it.col) {
                Some((_, h)) => {
                    h.insert(it.key, i);
                }
                None => m.push((it.col, HashMap::from([(it.key, i)]))),
            }
        }
        m
    };

    // 2. those values counted exactly on both sides, in one read
    let singles = by_col(&|_| true);
    let parts = over_blocks(
        st,
        cache,
        either,
        cancel,
        || (vec![0u64; items.len()], vec![0u64; items.len()]),
        |(hn, rn), si| {
            let (h, r) = (&here.segs[si], rest_seg(si));
            let blk = cache.block(st, si)?;
            for (c, map) in &singles {
                let v = View::open(st, &blk, si, *c, &[h, r], fs, cache)?;
                each_in(h, |row| {
                    if let Some(&i) = map.get(&v.key(row)) {
                        hn[i] += 1;
                    }
                });
                each_in(r, |row| {
                    if let Some(&i) = map.get(&v.key(row)) {
                        rn[i] += 1;
                    }
                });
            }
            done.fetch_add(h.count() + r.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut here_n = vec![0u64; items.len()];
    let mut rest_n = vec![0u64; items.len()];
    for (hn, rn) in parts {
        for i in 0..items.len() {
            here_n[i] += hn[i];
            rest_n[i] += rn[i];
        }
    }
    let cond = |i: usize| Cond {
        col: items[i].col,
        value: items[i].text.clone(),
    };
    let single_ratio: Vec<f64> = (0..items.len())
        .map(|i| ratio(here_n[i], n_here, rest_n[i], n_rest))
        .collect();
    // what to offer, as items with their counts here and in the rest
    let mut raw: Vec<(Vec<usize>, u64, u64)> = Vec::new();
    for i in 0..items.len() {
        if here_n[i] >= need && single_ratio[i] >= o.min_ratio {
            raw.push((vec![i], here_n[i], rest_n[i]));
        }
    }
    // values that hold in exactly the same rows, on both sides, are one
    // thing said once: every row here and none in the rest is such a case;
    // the pairs below find the others (a pair as common as each of its two
    // values, on both sides)
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn root(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    fn join(p: &mut [usize], a: usize, b: usize) {
        let (x, y) = (root(p, a), root(p, b));
        if x != y {
            // the earlier item (column order) speaks for the group
            let (lo, hi) = (x.min(y), x.max(y));
            p[hi] = lo;
        }
    }
    let everywhere: Vec<usize> = (0..items.len())
        .filter(|&i| here_n[i] == n_here && rest_n[i] == 0)
        .collect();
    for w in everywhere.windows(2) {
        join(&mut parent, w[0], w[1]);
    }

    prof("values counted on both sides", candidates);

    // 3. pairs of values: each in enough rows here, commoner here than in the
    // rest, and not already never seen in the rest (a pair cannot say more
    // than "never elsewhere"). In each block, the rows holding each value are
    // marked in a bit set, and a pair's rows are the marks two sets share,
    // counted 64 rows at a time. Counted here first; only the pairs in
    // enough rows here are then counted in the rest.
    let pair_items: Vec<usize> = (0..items.len())
        .filter(|&i| here_n[i] >= need && single_ratio[i] > 1.0 && single_ratio[i].is_finite())
        .collect();
    let np = pair_items.len();
    if np >= 2 {
        // position of an item among the pairable ones
        let mut pos = vec![usize::MAX; items.len()];
        for (k, &i) in pair_items.iter().enumerate() {
            pos[i] = k;
        }
        let pairable = by_col(&|i| pos[i] != usize::MAX);
        // for the rows of `m` in block `si`: the pairable items present,
        // each with the bit set of the rows (counted among `m`'s rows) holding it
        let bits_of = |blk: &Block, si: usize, m: &SegMatch, keep: &dyn Fn(usize) -> bool| {
            let n = m.count() as usize;
            let words = n.div_ceil(64);
            let mut sets: HashMap<usize, Vec<u64>> = HashMap::new();
            for (c, mm) in &pairable {
                let v = View::open(st, blk, si, *c, &[m], fs, cache)?;
                let mut at = 0usize;
                each_in(m, |row| {
                    if let Some(&i) = mm.get(&v.key(row)) {
                        let k = pos[i];
                        if keep(k) {
                            sets.entry(k).or_insert_with(|| vec![0u64; words])[at / 64] |=
                                1 << (at % 64);
                        }
                    }
                    at += 1;
                });
            }
            let mut out: Vec<(usize, Vec<u64>)> = sets.into_iter().collect();
            out.sort_unstable_by_key(|x| x.0);
            Ok::<_, Error>(out)
        };
        let shared = |a: &[u64], b: &[u64]| -> u64 {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x & y).count_ones() as u64)
                .sum()
        };
        // a pair (x < y) of pairable positions, as one number
        let tri = |x: usize, y: usize| x * np + y;
        let parts = over_blocks(
            st,
            cache,
            |si| here.segs[si].count() > 0,
            cancel,
            HashMap::<usize, u64>::new,
            |pair_n, si| {
                let h = &here.segs[si];
                let blk = cache.block(st, si)?;
                let sets = bits_of(&blk, si, h, &|_| true)?;
                for x in 0..sets.len() {
                    for y in x + 1..sets.len() {
                        let n = shared(&sets[x].1, &sets[y].1);
                        if n > 0 {
                            *pair_n.entry(tri(sets[x].0, sets[y].0)).or_default() += n;
                        }
                    }
                }
                done.fetch_add(h.count(), Ordering::Relaxed);
                Ok(())
            },
        )?;
        let mut here_p: HashMap<usize, u64> = HashMap::new();
        for part in parts {
            for (key, n) in part {
                *here_p.entry(key).or_default() += n;
            }
        }
        here_p.retain(|_, n| *n >= need);
        prof("pairs common enough here", here_p.len());
        if !here_p.is_empty() {
            let mut in_pair = vec![false; np];
            for &key in here_p.keys() {
                in_pair[key / np] = true;
                in_pair[key % np] = true;
            }
            let wanted: Vec<usize> = {
                let mut w: Vec<usize> = here_p.keys().copied().collect();
                w.sort_unstable();
                w
            };
            let parts = over_blocks(
                st,
                cache,
                |si| rest_seg(si).count() > 0,
                cancel,
                HashMap::<usize, u64>::new,
                |pair_n, si| {
                    let r = rest_seg(si);
                    let blk = cache.block(st, si)?;
                    let sets = bits_of(&blk, si, r, &|k| in_pair[k])?;
                    let mut at: Vec<usize> = vec![usize::MAX; np];
                    for (j, (k, _)) in sets.iter().enumerate() {
                        at[*k] = j;
                    }
                    for &key in &wanted {
                        let (x, y) = (at[key / np], at[key % np]);
                        if x != usize::MAX && y != usize::MAX {
                            let n = shared(&sets[x].1, &sets[y].1);
                            if n > 0 {
                                *pair_n.entry(key).or_default() += n;
                            }
                        }
                    }
                    done.fetch_add(r.count(), Ordering::Relaxed);
                    Ok(())
                },
            )?;
            let mut rest_p: HashMap<usize, u64> = HashMap::new();
            for part in parts {
                for (key, n) in part {
                    *rest_p.entry(key).or_default() += n;
                }
            }
            for (&key, &h) in &here_p {
                let (a, b) = (pair_items[key / np], pair_items[key % np]);
                let r = rest_p.get(&key).copied().unwrap_or(0);
                let pr = ratio(h, n_here, r, n_rest);
                // a pair is offered only when it says clearly more than the
                // stronger of its two values alone; otherwise it is that
                // value with a passenger
                let stronger = single_ratio[a].max(single_ratio[b]);
                if h == here_n[a] && h == here_n[b] && r == rest_n[a] && r == rest_n[b] {
                    join(&mut parent, a, b);
                } else if pr >= o.min_ratio && pr >= stronger * PAIR_GAIN {
                    raw.push((vec![a, b], h, r));
                }
            }
            prof("pairs counted in the rest", raw.len());
        }
    }

    // each group once, spoken for by its first item, the rest named with it
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..items.len() {
        let r = root(&mut parent, i);
        if r != i {
            members.entry(r).or_default().push(i);
        }
    }
    let mut seen: std::collections::HashSet<Vec<usize>> = Default::default();
    let mut findings: Vec<Finding> = Vec::new();
    for (its, h, r) in raw {
        let mut reps: Vec<usize> = its.iter().map(|&i| root(&mut parent, i)).collect();
        reps.sort_unstable();
        reps.dedup();
        if reps.len() < its.len() || !seen.insert(reps.clone()) {
            continue;
        }
        let mut parts: Vec<(Cond, Vec<Cond>)> = reps
            .iter()
            .map(|&x| {
                let same = members
                    .get(&x)
                    .map(|v| v.iter().map(|&i| cond(i)).collect())
                    .unwrap_or_default();
                (cond(x), same)
            })
            .collect();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        let (conds, same) = parts.into_iter().unzip();
        findings.push(Finding {
            conds,
            same,
            instead: None,
            here: h,
            here_total: n_here,
            rest: r,
            rest_total: n_rest,
        });
    }
    findings.sort_by(|x, y| {
        y.ratio()
            .total_cmp(&x.ratio())
            .then(y.here.cmp(&x.here))
            .then(x.conds.len().cmp(&y.conds.len()))
            .then(x.conds.cmp(&y.conds))
    });
    let findings = fold_alternatives(findings);
    Ok(Contrast {
        findings,
        here_total: n_here,
        rest_total: n_rest,
        columns: cols.len(),
        candidates,
    })
}
