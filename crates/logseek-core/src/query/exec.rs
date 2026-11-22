//! Filter execution over segments: prune (time range, zone maps, trigram
//! Bloom), evaluate per segment in parallel (dictionary columns test each
//! distinct value once), and materialise only the visible page.

use super::ast::{CmpOp, Expr, Query, Term};
use crate::budget::Budget;
use crate::error::{Error, Result};
use crate::ingest::infer::{parse_float, parse_int};
use crate::segment::{ColData, DecodedColumn, SegmentReader};
use crate::store::{Store, OFFSET_COL};
use crate::time::parse_time;
use crate::types::ColType;
use regex::bytes::Regex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use xxhash_rust::xxh3::xxh3_64;

/// Stage timers (ns) for profiling: raw read, raw search, decode, match.
pub static PROF: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
fn prof(i: usize, t: std::time::Instant) {
    PROF[i].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

// ───────────────────────── cache ─────────────────────────

/// Byte-budgeted LRU of decoded column chunks shared by all queries.
pub struct ChunkCache {
    budget: Budget,
    inner: Mutex<CacheInner>,
    pub decodes: AtomicU64,
}

#[derive(Default)]
struct CacheInner {
    map: HashMap<(u64, usize, u32), (Arc<DecodedColumn>, u64)>,
    tick: u64,
    /// Segments rebuilt from the source: their other columns, still compressed.
    /// Key: (store, segment, window or WHOLE).
    rebuilt: HashMap<(u64, usize, u32), RebuiltSeg>,
    /// One rebuild per segment (or window) at a time; others wait for it.
    building: HashMap<(u64, usize, u32), Arc<Mutex<()>>>,
    /// Decoded columns dropped to make room, and when: asking for one again
    /// soon after means the cache is too small for the work in hand.
    dropped: HashMap<(u64, usize, u32), std::time::Instant>,
    /// Such asks in the current burst, and when the burst began.
    again: u32,
    again_since: Option<std::time::Instant>,
    last_use: Option<std::time::Instant>,
    /// (tick, when) about once a second: which tick the clock was at, so
    /// "not used for 30 s" can be read off an entry's tick.
    marks: std::collections::VecDeque<(u64, std::time::Instant)>,
    /// A made column worked out for a whole block: (store, block, formula fingerprint).
    results: HashMap<(u64, usize, u64), FormulaResult>,
}

struct FormulaResult {
    vals: Arc<Kept>,
    tick: u64,
    _res: crate::budget::Reservation,
}

/// A made column's values for some rows: each distinct result once, and per
/// row which one it is (a column with few values costs little to keep).
pub(crate) struct Kept {
    vals: Vec<crate::formula::FValue>,
    idx: Vec<u32>,
}

impl Kept {
    fn get(&self, r: usize) -> crate::formula::FValue {
        self.idx.get(r).map_or(crate::formula::FValue::Null, |&k| {
            self.vals[k as usize].clone()
        })
    }
    /// Memory held: the values, each text's own allocation, and the row index.
    fn bytes(&self) -> u64 {
        use crate::formula::FValue;
        let text: u64 = self
            .vals
            .iter()
            .map(|v| match v {
                FValue::Text(s) | FValue::Error(s) if s.capacity() > 0 => {
                    (s.capacity() as u64).div_ceil(8) * 8 + 16
                }
                _ => 0,
            })
            .sum();
        std::mem::size_of::<FValue>() as u64 * self.vals.len() as u64
            + text
            + 4 * self.idx.len() as u64
    }
}

/// Where kept results of a made column are noted among dropped entries.
fn result_key(k: (u64, usize, u64)) -> (u64, usize, u32) {
    (k.0 ^ k.2, k.1, u32::MAX - 1)
}

/// A dropped column asked for again within this long counts as pressure.
const AGAIN_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);
/// This many of them inside one burst raise the cache's limit a step.
const AGAIN_STEP: u32 = 24;
const BURST: std::time::Duration = std::time::Duration::from_secs(5);

struct RebuiltSeg {
    cols: Arc<Vec<crate::segment::EncodedColumn>>,
    tick: u64,
    bytes: u64,
    _res: Option<crate::budget::Reservation>,
}

impl CacheInner {
    /// Free the least recently used entry: a decoded column first, else a
    /// rebuilt segment held compressed. False when nothing is left to free.
    fn evict_one(&mut self) -> bool {
        // a made column's kept results go before a decoded column used more lately
        let oldest_result = self
            .results
            .iter()
            .min_by_key(|(_, v)| v.tick)
            .map(|(k, v)| (*k, v.tick));
        let oldest_col = self.map.values().map(|v| v.1).min();
        if let Some((k, t)) = oldest_result {
            if oldest_col.is_none_or(|c| t <= c) {
                self.results.remove(&k);
                // asked for again soon, it counts as pressure like a decoded column
                self.dropped
                    .insert(result_key(k), std::time::Instant::now());
                return true;
            }
        }
        if let Some(k) = self.map.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) {
            self.map.remove(&k);
            let now = std::time::Instant::now();
            if self.dropped.len() >= 16_384 {
                self.dropped
                    .retain(|_, t| now.duration_since(*t) < AGAIN_WITHIN);
                if self.dropped.len() >= 16_384 {
                    self.dropped.clear();
                }
            }
            self.dropped.insert(k, now);
            return true;
        }
        if let Some(k) = self
            .rebuilt
            .iter()
            .min_by_key(|(_, v)| v.tick)
            .map(|(k, _)| *k)
        {
            self.rebuilt.remove(&k);
            return true;
        }
        false
    }
}

/// Rebuilt segments or windows held compressed at once (beyond this the oldest go).
/// Nothing left to evict: other threads of the same query hold the memory
/// and give it back within moments. Wait for them (a request bigger than the
/// whole cache never fits, so it fails at once), up to 5 s.
fn wait_for_room(since: std::time::Instant, bytes: u64, cap: u64) -> bool {
    if bytes > cap || since.elapsed() > std::time::Duration::from_secs(5) {
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(2));
    true
}

/// Rows per window when only the rows on screen are re-parsed.
pub const WINDOW_ROWS: usize = 1024;
const WHOLE: u32 = u32::MAX;

/// Cache key of a window's decoded column (kept apart from whole segments).
fn window_key(sid: u64, w: u32) -> u64 {
    sid ^ (w as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

impl ChunkCache {
    pub fn new(budget: Budget) -> Self {
        ChunkCache {
            budget,
            inner: Mutex::new(CacheInner::default()),
            decodes: AtomicU64::new(0),
        }
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        g.map.clear();
        g.rebuilt.clear();
        g.dropped.clear();
        g.results.clear();
    }

    /// `key` is not in the cache. If it was dropped moments ago to make room,
    /// the cache is too small for what is being looked at (scrolling back
    /// and forth, a page that needs more columns than fit): enough of those
    /// in a burst raise the limit by a quarter, up to the budget's ceiling.
    /// Reading a file once from end to end never asks twice, so a scan does
    /// not grow anything.
    fn missed(&self, g: &mut CacheInner, key: (u64, usize, u32)) {
        let now = std::time::Instant::now();
        g.last_use = Some(now);
        let Some(t) = g.dropped.remove(&key) else {
            return;
        };
        if now.duration_since(t) >= AGAIN_WITHIN {
            return;
        }
        if g.again_since.is_none_or(|s| now.duration_since(s) > BURST) {
            g.again = 0;
            g.again_since = Some(now);
        }
        g.again += 1;
        if g.again >= AGAIN_STEP {
            g.again = 0;
            g.again_since = None;
            let cap = self.budget.cap();
            self.budget.set_cap(cap + (cap / 8).max(16 << 20));
        }
    }

    /// Give back what was grown, as soon as it is not being used. Called
    /// often (several times a second is fine). While the limit is above
    /// where it starts, every decoded column not touched for `keep` goes,
    /// and the limit comes down to just above what is left; with nothing
    /// read at all for `keep` it is back where it started. So memory follows
    /// what is being looked at now, not what was looked at a minute ago.
    /// Says whether anything changed.
    pub fn relax(&self, keep: std::time::Duration) -> bool {
        let base = self.budget.base();
        let now = std::time::Instant::now();
        let mut g = self.inner.lock().unwrap();
        // note where the clock is, about once a second
        if g.marks
            .back()
            .is_none_or(|(_, t)| now.duration_since(*t).as_millis() >= 1000)
        {
            let tick = g.tick;
            g.marks.push_back((tick, now));
        }
        let cap = self.budget.cap();
        if cap <= base {
            // nothing grown: keep only the marks that can still matter
            while g.marks.len() > 2
                && g.marks
                    .front()
                    .is_some_and(|(_, t)| now.duration_since(*t) > keep * 2)
            {
                g.marks.pop_front();
            }
            return false;
        }
        // the newest mark that is at least `keep` old: anything last used before it is stale
        while g.marks.len() >= 2 && now.duration_since(g.marks[1].1) >= keep {
            g.marks.pop_front();
        }
        let Some(cut) = g
            .marks
            .front()
            .filter(|(_, t)| now.duration_since(*t) >= keep)
            .map(|(tick, _)| *tick)
        else {
            return false;
        };
        let before = g.map.len() + g.results.len();
        g.map.retain(|_, e| e.1 >= cut);
        g.results.retain(|_, e| e.tick >= cut);
        let dropped = before - g.map.len() - g.results.len();
        let used = self.budget.used();
        // a little headroom over what is still in use, never under the base
        let want = (used + used / 8).max(base);
        if dropped == 0 && want >= cap {
            return false;
        }
        if want < cap {
            self.budget.set_cap(want);
        }
        if self.budget.cap() <= base {
            g.dropped.clear();
            g.again = 0;
            g.again_since = None;
        }
        true
    }

    /// Reserve working memory for a query, evicting cached chunks (LRU) to make room.
    pub fn reserve(
        &self,
        bytes: u64,
        who: crate::budget::Component,
    ) -> Result<crate::budget::Reservation> {
        let t0 = std::time::Instant::now();
        loop {
            match self.budget.try_reserve(bytes, who) {
                Ok(r) => return Ok(r),
                Err(b) => {
                    let mut g = self.inner.lock().unwrap();
                    if !g.evict_one() {
                        drop(g);
                        if !wait_for_room(t0, bytes, self.budget.cap()) {
                            return Err(Error::Budget(b));
                        }
                    }
                }
            }
        }
    }

    /// A made column's results for a whole block, when kept.
    fn formula_result(&self, st: &Store, seg: usize, fp: u64) -> Option<Arc<Kept>> {
        let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
        let mut g = self.inner.lock().unwrap();
        g.tick += 1;
        let t = g.tick;
        let key = (sid, seg, fp);
        match g.results.get_mut(&key) {
            Some(e) => {
                e.tick = t;
                Some(e.vals.clone())
            }
            None => {
                self.missed(&mut g, result_key(key));
                None
            }
        }
    }

    /// Keep a made column's results for a whole block, within the budget;
    /// not kept when there is no room.
    fn keep_formula_result(&self, st: &Store, seg: usize, fp: u64, vals: Arc<Kept>) {
        let bytes = vals.bytes();
        // worked-out results are what the next question needs: the limit grows
        // to hold them, as far as the machine has room (given back when idle)
        let before = self.budget.cap();
        let need = self.budget.used() + bytes;
        if need > before {
            self.budget.set_cap(need + need / 16);
        }
        // keeping is optional: make room from the cache, never wait for it
        let res = loop {
            match self
                .budget
                .try_reserve(bytes, crate::budget::Component::Cache)
            {
                Ok(r) => break r,
                Err(_) => {
                    let mut g = self.inner.lock().unwrap();
                    if !g.evict_one() {
                        drop(g);
                        self.budget.set_cap(before.max(self.budget.used()));
                        return;
                    }
                }
            }
        };
        let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
        let mut g = self.inner.lock().unwrap();
        g.tick += 1;
        let tick = g.tick;
        g.results.insert(
            (sid, seg, fp),
            FormulaResult {
                vals,
                tick,
                _res: res,
            },
        );
    }

    pub fn get(&self, st: &Store, seg: usize, col: u32) -> Result<Option<Arc<DecodedColumn>>> {
        let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
        let key = (sid, seg, col);
        {
            let mut g = self.inner.lock().unwrap();
            g.tick += 1;
            let t = g.tick;
            if let Some(e) = g.map.get_mut(&key) {
                e.1 = t;
                let c = e.0.clone();
                g.last_use = Some(std::time::Instant::now());
                return Ok(Some(c));
            }
            self.missed(&mut g, key);
        }
        let reader = &st.segments()[seg];
        if !reader.meta().has_col(col) {
            return Ok(None);
        }
        if reader.in_source(col) {
            return self.get_rebuilt(st, seg, col, WHOLE);
        }
        let t0 = std::time::Instant::now();
        loop {
            match reader.column(col, &self.budget) {
                Ok(Some(c)) => {
                    self.decodes.fetch_add(1, Ordering::Relaxed);
                    let c = Arc::new(c);
                    let mut g = self.inner.lock().unwrap();
                    g.tick += 1;
                    let t = g.tick;
                    let e = g.map.entry(key).or_insert((c, t));
                    return Ok(Some(e.0.clone()));
                }
                Ok(None) => return Ok(None),
                Err(Error::Budget(b)) => {
                    let mut g = self.inner.lock().unwrap();
                    if !g.evict_one() {
                        drop(g);
                        if !wait_for_room(t0, b.requested, self.budget.cap()) {
                            return Err(Error::Budget(b));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl ChunkCache {
    /// A column read from the source: the segment (or one window of
    /// `WINDOW_ROWS` rows) is re-parsed once whatever the number of threads
    /// asking, its other columns kept compressed for the next request, and the
    /// asked column decoded into the cache.
    fn get_rebuilt(
        &self,
        st: &Store,
        seg: usize,
        col: u32,
        part: u32,
    ) -> Result<Option<Arc<DecodedColumn>>> {
        let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
        let m = st.segments()[seg].meta();
        let range = if part == WHOLE {
            0..m.rows as usize
        } else {
            let a = part as usize * WINDOW_ROWS;
            a..(a + WINDOW_ROWS).min(m.rows as usize)
        };
        let key = if part == WHOLE {
            (sid, seg, col)
        } else {
            (window_key(sid, part), seg, col)
        };
        if let Some(c) = self.cached(key) {
            return Ok(Some(c));
        }
        let skey = (sid, seg, part);
        let gate = {
            let mut g = self.inner.lock().unwrap();
            g.building.entry(skey).or_default().clone()
        };
        let cols = {
            let _one = gate.lock().unwrap_or_else(|e| e.into_inner());
            // another thread may have finished while this one waited
            if let Some(c) = self.cached(key) {
                return Ok(Some(c));
            }
            let held = {
                let mut g = self.inner.lock().unwrap();
                g.tick += 1;
                let t = g.tick;
                g.rebuilt.get_mut(&skey).map(|e| {
                    e.tick = t;
                    e.cols.clone()
                })
            };
            match held {
                Some(c) => c,
                None => {
                    // parse working memory, for the bytes of exactly these rows
                    let cols = Arc::new(st.rebuild_reserving(seg, range.clone(), |n| {
                        self.reserve(n, crate::budget::Component::Query)
                    })?);
                    let bytes: u64 = cols.iter().map(|c| c.compressed_len() as u64).sum();
                    // held compressed for the next request: at most a quarter of
                    // the cache, and given up first when a query needs memory
                    let mut g = self.inner.lock().unwrap();
                    let cap = self.budget.cap() / 4;
                    while !g.rebuilt.is_empty()
                        && g.rebuilt.values().map(|v| v.bytes).sum::<u64>() + bytes > cap
                    {
                        let old = g
                            .rebuilt
                            .iter()
                            .min_by_key(|(_, v)| v.tick)
                            .map(|(k, _)| *k);
                        if let Some(k) = old {
                            g.rebuilt.remove(&k);
                        }
                    }
                    let keep = (bytes <= cap)
                        .then(|| {
                            self.budget
                                .try_reserve(bytes, crate::budget::Component::Cache)
                                .ok()
                        })
                        .flatten();
                    g.tick += 1;
                    let t = g.tick;
                    if keep.is_some() {
                        g.rebuilt.insert(
                            skey,
                            RebuiltSeg {
                                cols: cols.clone(),
                                tick: t,
                                bytes,
                                _res: keep,
                            },
                        );
                    }
                    cols
                }
            }
        };
        let rows = range.len() as u32;
        let Some(enc) = cols.iter().find(|c| c.col_id() == col) else {
            return Ok(None);
        };
        let t0 = std::time::Instant::now();
        loop {
            match enc.decode(rows, &self.budget) {
                Ok(c) => {
                    self.decodes.fetch_add(1, Ordering::Relaxed);
                    let c = Arc::new(c);
                    let mut g = self.inner.lock().unwrap();
                    g.tick += 1;
                    let t = g.tick;
                    let e = g.map.entry(key).or_insert((c, t));
                    return Ok(Some(e.0.clone()));
                }
                Err(Error::Budget(b)) => {
                    let mut g = self.inner.lock().unwrap();
                    if !g.evict_one() {
                        drop(g);
                        if !wait_for_room(t0, b.requested, self.budget.cap()) {
                            return Err(Error::Budget(b));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One block, for reading many of its columns in turn: a block kept in
    /// the source file is parsed once here (or taken from what is already
    /// held), and each column asked for is decoded from that one parse.
    pub fn block<'a>(&'a self, st: &'a Store, seg: usize) -> Result<Block<'a>> {
        let m = st.segments()[seg].meta();
        let rebuilt = if m.cols.iter().any(|c| c.in_source) {
            let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
            let held = {
                let mut g = self.inner.lock().unwrap();
                g.tick += 1;
                let t = g.tick;
                g.rebuilt.get_mut(&(sid, seg, WHOLE)).map(|e| {
                    e.tick = t;
                    e.cols.clone()
                })
            };
            Some(match held {
                Some(c) => c,
                None => Arc::new(st.rebuild_reserving(seg, 0..m.rows as usize, |n| {
                    self.reserve(n, crate::budget::Component::Query)
                })?),
            })
        } else {
            None
        };
        Ok(Block {
            cache: self,
            st,
            seg,
            rebuilt,
        })
    }

    /// True when the whole segment's column is already decoded.
    fn has_whole(&self, st: &Store, seg: usize, col: u32) -> bool {
        let sid = xxh3_64(st.dir().as_os_str().as_encoded_bytes());
        self.inner
            .lock()
            .unwrap()
            .map
            .contains_key(&(sid, seg, col))
    }

    fn cached(&self, key: (u64, usize, u32)) -> Option<Arc<DecodedColumn>> {
        let mut g = self.inner.lock().unwrap();
        g.tick += 1;
        let t = g.tick;
        let hit = g.map.get_mut(&key).map(|e| {
            e.1 = t;
            e.0.clone()
        });
        if hit.is_none() {
            self.missed(&mut g, key);
        } else {
            g.last_use = Some(std::time::Instant::now());
        }
        hit
    }
}

/// One block of a store, opened for reading many of its columns (see
/// [`ChunkCache::block`]).
pub struct Block<'a> {
    cache: &'a ChunkCache,
    st: &'a Store,
    seg: usize,
    rebuilt: Option<Arc<Vec<crate::segment::EncodedColumn>>>,
}

impl Block<'_> {
    /// A column of the block (`None`: the block has no value in it). Columns
    /// kept in the source come from the block's one parse and are not added
    /// to the cache (a pass over every column must not push out what the
    /// screen is using); the others come through the cache as usual.
    pub fn column(&self, col: u32) -> Result<Option<Arc<DecodedColumn>>> {
        let reader = &self.st.segments()[self.seg];
        if !reader.meta().has_col(col) {
            return Ok(None);
        }
        let Some(rebuilt) = self.rebuilt.as_ref().filter(|_| reader.in_source(col)) else {
            return self.cache.get(self.st, self.seg, col);
        };
        let Some(enc) = rebuilt.iter().find(|c| c.col_id() == col) else {
            return Ok(None);
        };
        let rows = reader.meta().rows;
        let t0 = std::time::Instant::now();
        loop {
            match enc.decode(rows, &self.cache.budget) {
                Ok(c) => return Ok(Some(Arc::new(c))),
                Err(Error::Budget(b)) => {
                    let mut g = self.cache.inner.lock().unwrap();
                    if !g.evict_one() {
                        drop(g);
                        if !wait_for_room(t0, b.requested, self.cache.budget.cap()) {
                            return Err(Error::Budget(b));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// How many blocks to work on at once: every core, as far as memory allows
/// (a block kept in the source needs about five times its bytes while it is
/// parsed and decoded).
pub fn threads_for(st: &Store, cache: &ChunkCache) -> usize {
    let n = st.segments().len();
    let per_thread = st
        .segments()
        .iter()
        .filter(|s| s.meta().cols.iter().any(|c| c.in_source))
        .map(|s| s.meta().byte_end.saturating_sub(s.meta().byte_start) * 5 + (1 << 20))
        .max()
        .unwrap_or(0);
    let fit = if per_thread == 0 {
        usize::MAX
    } else {
        (cache.budget().cap() / per_thread).max(1) as usize
    };
    scan_cores().min(fit).min(n.max(1))
}

/// How many threads a scan may use: every core but one. The one left is for
/// the window, so the screen keeps drawing smoothly while a long scan runs.
pub fn scan_cores() -> usize {
    cores_for_scans(
        std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4),
    )
}

fn cores_for_scans(machine: usize) -> usize {
    machine.saturating_sub(1).max(1)
}

#[cfg(test)]
mod scan_core_tests {
    #[test]
    fn a_scan_leaves_one_core_for_the_window_but_always_has_one() {
        assert_eq!(super::cores_for_scans(12), 11);
        assert_eq!(super::cores_for_scans(2), 1);
        assert_eq!(super::cores_for_scans(1), 1);
        assert_eq!(super::cores_for_scans(0), 1);
    }
}

/// Every row of a block's match, in order.
pub(crate) fn each_in(m: &SegMatch, mut f: impl FnMut(usize)) {
    match m {
        SegMatch::None => {}
        SegMatch::All(n) => (0..*n as usize).for_each(f),
        SegMatch::Bits { bits, .. } => {
            for (wi, &w) in bits.iter().enumerate() {
                let mut w = w;
                while w != 0 {
                    f(wi * 64 + w.trailing_zeros() as usize);
                    w &= w - 1;
                }
            }
        }
    }
}

/// Work on the blocks `want` says yes to, side by side: each thread keeps its
/// own tally (made by `init`) and the tallies come back to be put together.
pub(crate) fn over_blocks<T: Send>(
    st: &Store,
    cache: &ChunkCache,
    want: impl Fn(usize) -> bool + Sync,
    cancel: &AtomicBool,
    init: impl Fn() -> T + Sync,
    work: impl Fn(&mut T, usize) -> Result<()> + Sync,
) -> Result<Vec<T>> {
    let n = st.segments().len();
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    let threads = threads_for(st, cache);
    let out: Vec<T> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut t = init();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n || failure.lock().unwrap().is_some() {
                            return t;
                        }
                        if !want(i) {
                            continue;
                        }
                        if let Err(e) = (if cancel.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        })
                        .and_then(|_| work(&mut t, i))
                        {
                            failure.lock().unwrap().get_or_insert(e);
                            return t;
                        }
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    match failure.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

// ───────────────────────── match sets ─────────────────────────

#[derive(Debug, Clone)]
pub enum SegMatch {
    None,
    All(u32),
    Bits { bits: Vec<u64>, count: u32 },
}

impl SegMatch {
    pub fn count(&self) -> u64 {
        match self {
            SegMatch::None => 0,
            SegMatch::All(n) => *n as u64,
            SegMatch::Bits { count, .. } => *count as u64,
        }
    }
    fn from_bits(bits: Vec<u64>, rows: u32) -> SegMatch {
        let count: u32 = bits.iter().map(|w| w.count_ones()).sum();
        if count == 0 {
            SegMatch::None
        } else if count == rows {
            SegMatch::All(rows)
        } else {
            SegMatch::Bits { bits, count }
        }
    }
    /// The rows `rows` (numbered within the block) of a block of `seg_rows` rows.
    pub fn of_rows(rows: &[u32], seg_rows: u32) -> SegMatch {
        let mut bits = vec![0u64; (seg_rows as usize).div_ceil(64)];
        for &r in rows {
            if r < seg_rows {
                bits[r as usize / 64] |= 1 << (r % 64);
            }
        }
        SegMatch::from_bits(bits, seg_rows)
    }
    pub fn contains(&self, r: u32) -> bool {
        match self {
            SegMatch::None => false,
            SegMatch::All(rows) => r < *rows,
            SegMatch::Bits { bits, .. } => bits
                .get(r as usize / 64)
                .is_some_and(|w| w >> (r % 64) & 1 == 1),
        }
    }
    /// Row (within the segment) of the n-th match.
    fn nth(&self, mut n: u32) -> Option<u32> {
        match self {
            SegMatch::None => None,
            SegMatch::All(rows) => (n < *rows).then_some(n),
            SegMatch::Bits { bits, count } => {
                if n >= *count {
                    return None;
                }
                for (wi, &w) in bits.iter().enumerate() {
                    let c = w.count_ones();
                    if n < c {
                        let mut w = w;
                        for _ in 0..n {
                            w &= w - 1;
                        }
                        return Some(wi as u32 * 64 + w.trailing_zeros());
                    }
                    n -= c;
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub segments_total: usize,
    pub segments_pruned: usize,
    pub segments_scanned: usize,
    pub columns_skipped: usize,
}

#[derive(Debug, Clone)]
pub struct MatchSet {
    pub segs: Vec<SegMatch>,
    prefix: Vec<u64>,
    pub stats: ScanStats,
}

impl MatchSet {
    pub fn all(st: &Store) -> MatchSet {
        let segs: Vec<SegMatch> = st
            .segments()
            .iter()
            .map(|s| SegMatch::All(s.meta().rows))
            .collect();
        MatchSet::new(
            segs,
            ScanStats {
                segments_total: st.segments().len(),
                ..Default::default()
            },
        )
    }
    /// A match set from one entry per block of the store.
    pub fn from_segs(segs: Vec<SegMatch>) -> MatchSet {
        MatchSet::new(segs, ScanStats::default())
    }
    fn new(segs: Vec<SegMatch>, stats: ScanStats) -> MatchSet {
        let mut prefix = Vec::with_capacity(segs.len() + 1);
        let mut acc = 0u64;
        prefix.push(0);
        for s in &segs {
            acc += s.count();
            prefix.push(acc);
        }
        MatchSet {
            segs,
            prefix,
            stats,
        }
    }
    pub fn count(&self) -> u64 {
        *self.prefix.last().unwrap_or(&0)
    }
    /// Only the matches whose global row id is in `rows` (any order, duplicates
    /// and out-of-range ids ignored): "show tagged rows" over any search.
    pub fn restrict(&self, st: &Store, rows: &[u64]) -> MatchSet {
        let mut sorted: Vec<u64> = rows.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let segs: Vec<SegMatch> = st
            .segments()
            .iter()
            .zip(&self.segs)
            .map(|(seg, m)| {
                let (start, n) = (seg.meta().row_start, seg.meta().rows);
                let a = sorted.partition_point(|&r| r < start);
                let b = sorted.partition_point(|&r| r < start + n as u64);
                if a == b {
                    return SegMatch::None;
                }
                let mut bits = vec![0u64; (n as usize).div_ceil(64)];
                for &g in &sorted[a..b] {
                    let r = (g - start) as u32;
                    if m.contains(r) {
                        bits[r as usize / 64] |= 1 << (r % 64);
                    }
                }
                SegMatch::from_bits(bits, n)
            })
            .collect();
        MatchSet::new(
            segs,
            ScanStats {
                segments_total: st.segments().len(),
                ..Default::default()
            },
        )
    }
    /// This set without `rows` (sorted or not).
    pub fn without(&self, st: &Store, rows: &[u64]) -> MatchSet {
        let mut sorted: Vec<u64> = rows.to_vec();
        sorted.sort_unstable();
        let segs: Vec<SegMatch> = st
            .segments()
            .iter()
            .zip(&self.segs)
            .map(|(seg, m)| {
                let (start, n) = (seg.meta().row_start, seg.meta().rows);
                let a = sorted.partition_point(|&r| r < start);
                let b = sorted.partition_point(|&r| r < start + n as u64);
                if a == b {
                    return m.clone();
                }
                let mut bits = vec![0u64; (n as usize).div_ceil(64)];
                for r in 0..n {
                    if m.contains(r) {
                        bits[r as usize / 64] |= 1 << (r % 64);
                    }
                }
                for &g in &sorted[a..b] {
                    let r = (g - start) as u32;
                    bits[r as usize / 64] &= !(1 << (r % 64));
                }
                SegMatch::from_bits(bits, n)
            })
            .collect();
        MatchSet::new(segs, self.stats.clone())
    }
    /// The rows of this set that are not in `other` (both over the same store).
    pub fn minus(&self, other: &MatchSet) -> MatchSet {
        let segs: Vec<SegMatch> = self
            .segs
            .iter()
            .zip(other.segs.iter().chain(std::iter::repeat(&SegMatch::None)))
            .map(|(a, b)| match (a, b) {
                (SegMatch::None, _) => SegMatch::None,
                (_, SegMatch::None) => a.clone(),
                (_, SegMatch::All(_)) => SegMatch::None,
                (SegMatch::All(n), SegMatch::Bits { bits, .. }) => {
                    let mut out: Vec<u64> = bits.iter().map(|w| !w).collect();
                    // clear the bits past the end of the block
                    let n = *n as usize;
                    out.resize(n.div_ceil(64), u64::MAX);
                    if n % 64 != 0 {
                        if let Some(last) = out.last_mut() {
                            *last &= (1u64 << (n % 64)) - 1;
                        }
                    }
                    SegMatch::from_bits(out, n as u32)
                }
                (SegMatch::Bits { bits: x, .. }, SegMatch::Bits { bits: y, .. }) => {
                    let rows = (x.len() * 64) as u32;
                    let out: Vec<u64> = x
                        .iter()
                        .zip(y.iter().chain(std::iter::repeat(&0)))
                        .map(|(p, q)| p & !q)
                        .collect();
                    // `rows` only matters for "all"; a Bits set never covers every row here
                    match SegMatch::from_bits(out, rows) {
                        SegMatch::All(_) => unreachable!(),
                        s => s,
                    }
                }
            })
            .collect();
        MatchSet::new(segs, self.stats.clone())
    }
    /// `(segment, row within segment)` of the n-th match.
    pub fn locate(&self, n: u64) -> Option<(usize, u32)> {
        if n >= self.count() {
            return None;
        }
        let si = self.prefix.partition_point(|&p| p <= n) - 1;
        let r = self.segs[si].nth((n - self.prefix[si]) as u32)?;
        Some((si, r))
    }
}

// ───────────────────────── compiled predicates ─────────────────────────

enum Pred {
    /// A made column's value could not be worked out (never a stored column's).
    IsError {
        col: u32,
    },
    Free {
        needle: Vec<u8>,
        finder: Needle,
    },
    /// A pattern looked for in every field. `lit`: text every match must
    /// contain (lower case), when the pattern is plain text with at most a
    /// start, end or word mark around it: columns that cannot hold it are skipped.
    AnyRe {
        re: Regex,
        lit: Option<Vec<u8>>,
    },
    Cmp {
        col: u32,
        op: CmpOp,
        value: String,
        lower: Vec<u8>,
        num: Option<f64>,
        int: Option<i64>,
        time: Option<i64>,
        re: Option<Regex>,
    },
}

enum CExpr {
    And(Box<CExpr>, Box<CExpr>),
    Or(Box<CExpr>, Box<CExpr>),
    Not(Box<CExpr>),
    P(Pred),
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (
        a.to_lowercase().chars().collect(),
        b.to_lowercase().chars().collect(),
    );
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The next row of `m` after `after` (before it when `!forward`), wrapping
/// round the ends; `None` only when `m` is empty.
pub fn next_row(st: &Store, m: &MatchSet, after: Option<u64>, forward: bool) -> Option<u64> {
    use super::agg::match_rows;
    let segs = st.segments();
    let rows_of = |si: usize| {
        let meta = segs[si].meta();
        match_rows(&m.segs[si], meta.rows).map(move |r| meta.row_start + r as u64)
    };
    let n = segs.len();
    if forward {
        let hit = (0..n)
            .filter(|&si| {
                let meta = segs[si].meta();
                after.is_none_or(|a| meta.row_start + meta.rows as u64 > a + 1)
            })
            .find_map(|si| rows_of(si).find(|&r| after.is_none_or(|a| r > a)));
        hit.or_else(|| (0..n).find_map(|si| rows_of(si).next()))
    } else {
        let hit = (0..n)
            .rev()
            .filter(|&si| after.is_none_or(|a| segs[si].meta().row_start < a))
            .find_map(|si| rows_of(si).filter(|&r| after.is_none_or(|a| r < a)).last());
        hit.or_else(|| (0..n).rev().find_map(|si| rows_of(si).last()))
    }
}

/// How many rows of `m` come before `row` (its place in the result).
pub fn rank_of(st: &Store, m: &MatchSet, row: u64) -> u64 {
    use super::agg::match_rows;
    let mut n = 0u64;
    for (si, sm) in m.segs.iter().enumerate() {
        let meta = st.segments()[si].meta();
        if meta.row_start + meta.rows as u64 <= row {
            n += sm.count();
        } else if meta.row_start <= row {
            let local = (row - meta.row_start) as usize;
            n += match_rows(sm, meta.rows).take_while(|&r| r < local).count() as u64;
        }
    }
    n
}

pub fn resolve_column(st: &Store, name: &str) -> Result<usize> {
    let s = st.schema();
    if let Some(i) = s.index_of(name) {
        return Ok(i);
    }
    if let Some(i) = s
        .columns
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name))
    {
        return Ok(i);
    }
    let best = s
        .columns
        .iter()
        .min_by_key(|c| edit_distance(&c.name, name))
        .map(|c| c.name.clone());
    Err(Error::Invalid(match best {
        Some(b) => format!("unknown column {name} — did you mean {b}?"),
        None => format!("unknown column {name}"),
    }))
}

/// Formula (virtual) columns for a query. Column numbers continue after the
/// stored columns: the i-th formula is column `schema.len() + i`.
#[derive(Debug, Clone, Default)]
pub struct FormulaSet {
    pub items: Vec<(String, crate::formula::Formula)>,
}

impl FormulaSet {
    pub fn add(
        &mut self,
        st: &Store,
        name: &str,
        src: &str,
    ) -> std::result::Result<usize, crate::formula::FormulaError> {
        // names resolve ignoring case, so a clash ignoring case would hide this column
        let clash = st
            .schema()
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name))
            || self.items.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
        if clash {
            return Err(crate::formula::FormulaError {
                span: (0, 0),
                msg: format!("a column named {name} already exists"),
            });
        }
        let cols: Vec<(String, ColType)> = st
            .schema()
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.ty))
            .collect();
        let f = crate::formula::compile_with(src, &cols, &self.items)?;
        self.items.push((name.to_string(), f));
        Ok(st.schema().len() + self.items.len() - 1)
    }

    /// Made columns from their definitions (name, formula), which may use one
    /// another in any order. The working ones keep the order given; each one
    /// that cannot be made has its own error: a cycle names the columns in it
    /// ("risk uses is_tor, which uses risk"); a column using one that failed
    /// says so ("uses risk, which has an error").
    pub fn from_defs(
        st: &Store,
        defs: &[(String, String)],
    ) -> (FormulaSet, Vec<(String, crate::formula::FormulaError)>) {
        use crate::formula::{compile_with, names_used, Formula, FormulaError};
        let err = |msg: String| FormulaError { span: (0, 0), msg };
        let cols: Vec<(String, ColType)> = st
            .schema()
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.ty))
            .collect();
        let names: Vec<String> = defs.iter().map(|(n, _)| n.clone()).collect();
        let n = defs.len();
        let mut failed: Vec<Option<FormulaError>> = vec![None; n];
        // names clash with a stored column or an earlier made column
        for (i, (name, _)) in defs.iter().enumerate() {
            let stored = cols.iter().any(|(c, _)| c.eq_ignore_ascii_case(name));
            let twice = names[..i].iter().any(|c| c.eq_ignore_ascii_case(name));
            if stored || twice {
                failed[i] = Some(err(format!("a column named {name} already exists")));
            }
        }
        let index = |w: &str| names.iter().position(|c| c == w);
        // a name that is also a stored column's reads the stored column (as
        // the compiler resolves it), so it is no dependency
        let reachable: Vec<String> = names
            .iter()
            .filter(|n| !cols.iter().any(|(c, _)| c.eq_ignore_ascii_case(n)))
            .cloned()
            .collect();
        let deps: Vec<Vec<usize>> = defs
            .iter()
            .map(|(_, src)| {
                names_used(src, &reachable)
                    .into_iter()
                    .filter_map(index)
                    .collect()
            })
            .collect();
        // cycles: the shortest way from a column back to itself, in words
        for i in 0..n {
            if failed[i].is_some() {
                continue;
            }
            let mut back: Vec<Option<usize>> = vec![None; n];
            let mut queue = std::collections::VecDeque::new();
            let mut seen = vec![false; n];
            let mut found = None;
            for &d in &deps[i] {
                if d == i {
                    found = Some(i);
                    break;
                }
                if !seen[d] {
                    seen[d] = true;
                    queue.push_back(d);
                }
            }
            while found.is_none() {
                let Some(x) = queue.pop_front() else { break };
                for &d in &deps[x] {
                    if d == i {
                        found = Some(x);
                        break;
                    }
                    if !seen[d] {
                        seen[d] = true;
                        back[d] = Some(x);
                        queue.push_back(d);
                    }
                }
            }
            if let Some(last) = found {
                let mut path = vec![last];
                while let Some(p) = back[*path.last().unwrap()] {
                    path.push(p);
                }
                path.reverse();
                let mut msg = format!("{} uses {}", names[i], names[path[0]]);
                for &x in &path[1..] {
                    msg.push_str(&format!(", which uses {}", names[x]));
                }
                if path[0] != i {
                    msg.push_str(&format!(", which uses {}", names[i]));
                }
                failed[i] = Some(err(msg));
            }
        }
        // the rest in the order they can be made
        let mut made: Vec<Option<Formula>> = vec![None; n];
        let mut done = vec![false; n];
        for i in 0..n {
            done[i] = failed[i].is_some();
        }
        loop {
            let mut moved = false;
            for i in 0..n {
                if done[i] || deps[i].iter().any(|&d| !done[d]) {
                    continue;
                }
                done[i] = true;
                moved = true;
                if let Some(&d) = deps[i].iter().find(|&&d| failed[d].is_some()) {
                    failed[i] = Some(err(format!("uses {}, which has an error", names[d])));
                    continue;
                }
                let have: Vec<(String, Formula)> = deps[i]
                    .iter()
                    .filter_map(|&d| made[d].clone().map(|f| (names[d].clone(), f)))
                    .collect();
                match compile_with(&defs[i].1, &cols, &have) {
                    Ok(f) => made[i] = Some(f),
                    Err(e) => failed[i] = Some(e),
                }
            }
            if !moved {
                break;
            }
        }
        // anything left waits on a cycle
        for i in 0..n {
            if !done[i] {
                let d = deps[i].iter().find(|&&d| failed[d].is_some() || !done[d]);
                let on = d.map_or(String::new(), |&d| names[d].clone());
                failed[i] = Some(err(format!("uses {on}, which has an error")));
            }
        }
        let mut fs = FormulaSet::default();
        let mut errors = Vec::new();
        for i in 0..n {
            match (made[i].take(), failed[i].take()) {
                (Some(f), None) => fs.items.push((names[i].clone(), f)),
                (_, Some(e)) => errors.push((names[i].clone(), e)),
                (None, None) => {}
            }
        }
        (fs, errors)
    }
    pub fn get(&self, nstored: usize, col: u32) -> Option<&crate::formula::Formula> {
        (col as usize)
            .checked_sub(nstored)
            .and_then(|i| self.items.get(i))
            .map(|x| &x.1)
    }
}

/// The made columns that use made column `name`, directly or through others,
/// in the order given (names compared ignoring case). Removing `name` would
/// leave each of them unable to be worked out.
pub fn made_dependents(defs: &[(String, String)], name: &str) -> Vec<String> {
    let names: Vec<String> = defs.iter().map(|(n, _)| n.clone()).collect();
    let uses: Vec<Vec<&str>> = defs
        .iter()
        .map(|(_, src)| crate::formula::names_used(src, &names))
        .collect();
    let mut hit = vec![false; defs.len()];
    let mut grew = true;
    while grew {
        grew = false;
        for i in 0..defs.len() {
            if hit[i] || names[i].eq_ignore_ascii_case(name) {
                continue;
            }
            let on = uses[i].iter().any(|u| {
                u.eq_ignore_ascii_case(name)
                    || names.iter().position(|n| n == u).is_some_and(|k| hit[k])
            });
            if on {
                hit[i] = true;
                grew = true;
            }
        }
    }
    (0..defs.len())
        .filter(|&i| hit[i])
        .map(|i| names[i].clone())
        .collect()
}
/// Resolve a column name against stored columns, then formula columns.
pub fn resolve_column_f(st: &Store, fs: &FormulaSet, name: &str) -> Result<usize> {
    match resolve_column(st, name) {
        Ok(i) => Ok(i),
        Err(e) => fs
            .items
            .iter()
            .position(|(n, _)| n == name)
            .or_else(|| {
                fs.items
                    .iter()
                    .position(|(n, _)| n.eq_ignore_ascii_case(name))
            })
            .map(|i| st.schema().len() + i)
            .ok_or(e),
    }
}

fn col_fvalue(c: &Option<Arc<DecodedColumn>>, r: usize) -> crate::formula::FValue {
    use crate::formula::FValue;
    let Some(c) = c else { return FValue::Null };
    if c.is_null(r) {
        return FValue::Null;
    }
    let text = || {
        c.text(r)
            .map_or(FValue::Null, |s| FValue::Text(s.into_owned()))
    };
    match c.ty {
        ColType::Text => text(),
        ColType::Time => c.int(r).map(FValue::Time).unwrap_or_else(text),
        ColType::Bool => c.bool(r).map(FValue::Bool).unwrap_or_else(text),
        ColType::Int | ColType::Float => c.float(r).map(FValue::Num).unwrap_or_else(text),
    }
}

/// Distinct input combinations remembered per block; beyond this, row by row.
const FORMULA_MEMO: usize = 65_536;

/// Evaluate a formula for the given rows of one segment: from the block's kept
/// results when there are some, otherwise worked out for just these rows.
fn formula_rows(
    st: &Store,
    si: usize,
    f: &crate::formula::Formula,
    rows: impl Iterator<Item = usize>,
    cache: &ChunkCache,
) -> Result<Vec<crate::formula::FValue>> {
    if let Some(all) = cache.formula_result(st, si, f.fingerprint()) {
        return Ok(rows.map(|r| all.get(r)).collect());
    }
    let k = work_out(st, si, f, rows, cache)?;
    Ok((0..k.idx.len()).map(|i| k.get(i)).collect())
}

/// A formula for the given rows (in their order): when every input keeps its
/// values as a dictionary, worked out once per distinct combination of them
/// (rows whose value is kept apart from the dictionary on their own).
fn work_out(
    st: &Store,
    si: usize,
    f: &crate::formula::Formula,
    rows: impl Iterator<Item = usize>,
    cache: &ChunkCache,
) -> Result<Kept> {
    use crate::formula::FValue;
    let decoded: Vec<(usize, Option<Arc<DecodedColumn>>)> = f
        .refs()
        .iter()
        .map(|&i| Ok((i, cache.get(st, si, i as u32)?)))
        .collect::<Result<_>>()?;
    let one = |r: usize| {
        let get = |i: usize| {
            decoded
                .iter()
                .find(|(k, _)| *k == i)
                .map_or(FValue::Null, |(_, c)| col_fvalue(c, r))
        };
        f.eval(&get)
    };
    let mut vals: Vec<FValue> = Vec::new();
    let mut idx: Vec<u32> = Vec::with_capacity(rows.size_hint().0);
    // 17 bits a key (dictionary codes are 16 bits, plus empty): up to 7 inputs in 128 bits
    let by_value = decoded.len() <= 7
        && decoded
            .iter()
            .all(|(_, c)| c.as_ref().is_none_or(|c| c.is_dict()));
    let mut memo: HashMap<u128, u32> = HashMap::new();
    for r in rows {
        let key = if by_value {
            let mut key: Option<u128> = Some(0);
            for (_, c) in &decoded {
                let k = match c {
                    None => Some(0),
                    Some(c) => c.dict_key(r),
                };
                key = match (key, k) {
                    (Some(acc), Some(k)) => Some((acc << 17) | k as u128),
                    _ => None,
                };
            }
            key
        } else {
            None
        };
        if let Some(i) = key.and_then(|k| memo.get(&k)) {
            idx.push(*i);
            continue;
        }
        vals.push(one(r));
        let i = (vals.len() - 1) as u32;
        if let Some(k) = key {
            if memo.len() < FORMULA_MEMO {
                memo.insert(k, i);
            }
        }
        idx.push(i);
    }
    Ok(Kept { vals, idx })
}

/// A made column for every row of a block, kept for the next question.
fn formula_block(
    st: &Store,
    si: usize,
    f: &crate::formula::Formula,
    cache: &ChunkCache,
) -> Result<Arc<Kept>> {
    let fp = f.fingerprint();
    if let Some(all) = cache.formula_result(st, si, fp) {
        return Ok(all);
    }
    let rows = st.segments()[si].meta().rows as usize;
    let kept = Arc::new(work_out(st, si, f, 0..rows, cache)?);
    cache.keep_formula_result(st, si, fp, kept.clone());
    Ok(kept)
}

fn fvalue_pred(
    v: &crate::formula::FValue,
    op: CmpOp,
    value: &str,
    lower: &[u8],
    num: Option<f64>,
    time: Option<i64>,
    re: &Option<Regex>,
) -> bool {
    use crate::formula::FValue;
    let numeric_op = matches!(
        op,
        CmpOp::Eq | CmpOp::Ne | CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le
    );
    match v {
        // a missing value and an error match no comparison; an error is not empty,
        // though: `col:*` (has a value, what NOT EMPTY writes) keeps it
        FValue::Error(_) => {
            op == CmpOp::Glob && !value.is_empty() && value.bytes().all(|b| b == b'*')
        }
        // empty text is empty, as everywhere else (value lists, the grid)
        FValue::Null => false,
        FValue::Text(s) if s.is_empty() => false,
        FValue::Num(x) if numeric_op && num.is_some() => num_pred(op, *x, num.unwrap()),
        FValue::Time(t) if numeric_op && time.is_some() => {
            num_pred(op, *t as f64, time.unwrap() as f64)
        }
        other => other
            .text()
            .is_some_and(|t| text_pred(op, value, lower, num, re, t.as_bytes())),
    }
}

fn compile(st: &Store, fs: &FormulaSet, e: &Expr) -> Result<CExpr> {
    Ok(match e {
        Expr::And(a, b) => CExpr::And(Box::new(compile(st, fs, a)?), Box::new(compile(st, fs, b)?)),
        Expr::Or(a, b) => CExpr::Or(Box::new(compile(st, fs, a)?), Box::new(compile(st, fs, b)?)),
        Expr::Not(a) => CExpr::Not(Box::new(compile(st, fs, a)?)),
        Expr::Term(Term::FreeText(s)) => {
            let needle = s.to_ascii_lowercase().into_bytes();
            CExpr::P(Pred::Free {
                finder: Needle::new(&needle),
                needle,
            })
        }
        Expr::Term(Term::Cmp { col, op, value })
            if col == super::ast::ANY_FIELD && *op == CmpOp::Regex =>
        {
            let re = regex::bytes::RegexBuilder::new(value)
                .size_limit(8 << 20)
                .dfa_size_limit(8 << 20)
                .build()
                .map_err(|e| Error::Invalid(format!("invalid regex: {e}")))?;
            CExpr::P(Pred::AnyRe {
                re,
                lit: plain_text_of(value).map(|s| s.to_ascii_lowercase().into_bytes()),
            })
        }
        Expr::Term(Term::IsError(col)) => CExpr::P(Pred::IsError {
            col: resolve_column_f(st, fs, col)? as u32,
        }),
        Expr::Term(Term::Cmp { col, op, value }) => {
            let ci = resolve_column_f(st, fs, col)? as u32;
            let re = if *op == CmpOp::Regex {
                Some(
                    regex::bytes::RegexBuilder::new(value)
                        .size_limit(8 << 20)
                        .dfa_size_limit(8 << 20)
                        .build()
                        .map_err(|e| Error::Invalid(format!("invalid regex: {e}")))?,
                )
            } else {
                None
            };
            let mut num = parse_float(value).or_else(|| parse_int(value).map(|i| i as f64));
            let mut int = parse_int(value);
            let time = parse_time(value);
            // the timeline kept as a number (epoch seconds, milliseconds or
            // microseconds): a date typed against it is compared in the
            // column's own unit, not as text
            let sch = st.schema();
            if let (None, Some(t), Some(unit)) = (num, time, sch.time_unit_us) {
                if sch.time_col == Some(ci as usize) && unit > 0 {
                    num = Some(t as f64 / unit as f64);
                    int = Some(t.div_euclid(unit));
                }
            }
            // "is not x" is "not (is x)": one question, one answer. A row
            // with no value is not x, so it is kept either way.
            let (op, negate) = match *op {
                CmpOp::Ne => (CmpOp::Eq, true),
                o => (o, false),
            };
            let p = CExpr::P(Pred::Cmp {
                col: ci,
                op,
                value: value.clone(),
                lower: value.to_ascii_lowercase().into_bytes(),
                num,
                int,
                time,
                re,
            });
            if negate {
                CExpr::Not(Box::new(p))
            } else {
                p
            }
        }
    })
}

/// The text a pattern is made of when it is plain text with at most
/// `(?i)`, `^` or `\b` in front and `$` or `\b` behind (what the search
/// options write): every match contains it. `None` for any other pattern.
pub fn plain_text_of(pattern: &str) -> Option<String> {
    let mut p = pattern.strip_prefix("(?i)").unwrap_or(pattern);
    p = p.strip_prefix('^').unwrap_or(p);
    p = p.strip_prefix("\\b").unwrap_or(p);
    p = p.strip_suffix("\\b").unwrap_or(p);
    // a `$` that is not itself escaped
    if p.ends_with('$') && !p.ends_with("\\$") {
        p = &p[..p.len() - 1];
    }
    let mut out = String::new();
    let mut it = p.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                // an escaped mark is that character; a class like \d is not text
                Some(n) if !n.is_alphanumeric() => out.push(n),
                _ => return None,
            },
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => {
                return None
            }
            _ => out.push(c),
        }
    }
    (!out.is_empty()).then_some(out)
}

/// How many rows to read at a time when every one of `ncols` columns is
/// wanted as text: about 400,000 cells a page, so a file of 800 columns is
/// read 500 rows at a time instead of 20,000 (17 million cells, hundreds of
/// megabytes that no budget counted).
pub fn rows_per_page(ncols: usize) -> usize {
    (400_000 / ncols.max(1)).clamp(64, 20_000)
}

/// A search term compiled once per query: SIMD substring search when the
/// term has no letters (case can't matter), case-insensitive scan otherwise.
pub struct Needle {
    lower: Vec<u8>,
    simd: Option<memchr::memmem::Finder<'static>>,
}

impl Needle {
    pub fn new(lower: &[u8]) -> Needle {
        let simd = (!lower.iter().any(u8::is_ascii_alphabetic))
            .then(|| memchr::memmem::Finder::new(lower).into_owned());
        Needle {
            lower: lower.to_vec(),
            simd,
        }
    }
    #[inline]
    pub fn found_in(&self, hay: &[u8]) -> bool {
        match &self.simd {
            Some(f) => f.find(hay).is_some(),
            None => contains_ci(hay, &self.lower),
        }
    }
}

#[inline]
pub fn contains_ci(hay: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if hay.len() < needle_lower.len() {
        return false;
    }
    if !needle_lower.iter().any(u8::is_ascii_alphabetic) {
        return memchr::memmem::find(hay, needle_lower).is_some();
    }
    let f = needle_lower[0];
    let rest = &needle_lower[1..];
    let last = hay.len() - needle_lower.len();
    for i in memchr::memchr2_iter(f, f.to_ascii_uppercase(), &hay[..=last]) {
        if hay[i + 1..i + needle_lower.len()].eq_ignore_ascii_case(rest) {
            return true;
        }
    }
    false
}

/// Case-insensitive wildcard match (`*` any run, `?` one byte), whole value.
pub fn glob_ci(pat_lower: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while i < s.len() {
        if p < pat_lower.len()
            && (pat_lower[p] == b'?' || pat_lower[p] == s[i].to_ascii_lowercase())
        {
            p += 1;
            i += 1;
        } else if p < pat_lower.len() && pat_lower[p] == b'*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while p < pat_lower.len() && pat_lower[p] == b'*' {
        p += 1;
    }
    p == pat_lower.len()
}

fn text_pred(
    op: CmpOp,
    value: &str,
    lower: &[u8],
    num: Option<f64>,
    re: &Option<Regex>,
    t: &[u8],
) -> bool {
    use std::cmp::Ordering as O;
    let ord = || -> O {
        if let (Some(a), Some(b)) = (std::str::from_utf8(t).ok().and_then(parse_float), num) {
            return a.partial_cmp(&b).unwrap_or(O::Equal);
        }
        let tl: Vec<u8> = t.iter().map(|c| c.to_ascii_lowercase()).collect();
        tl.as_slice().cmp(lower)
    };
    match op {
        CmpOp::Eq => t.eq_ignore_ascii_case(value.as_bytes()),
        CmpOp::Ne => !t.eq_ignore_ascii_case(value.as_bytes()),
        CmpOp::Contains => contains_ci(t, lower),
        CmpOp::Glob => glob_ci(lower, t),
        CmpOp::Regex => re.as_ref().is_some_and(|r| r.is_match(t)),
        CmpOp::Gt => ord() == O::Greater,
        CmpOp::Ge => ord() != O::Less,
        CmpOp::Lt => ord() == O::Less,
        CmpOp::Le => ord() != O::Greater,
    }
}

fn num_pred(op: CmpOp, a: f64, b: f64) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        _ => false,
    }
}

// ───────────────────────── bitmaps ─────────────────────────

fn words(rows: u32) -> usize {
    (rows as usize).div_ceil(64)
}
fn full(rows: u32) -> Vec<u64> {
    let mut v = vec![u64::MAX; words(rows)];
    if rows % 64 != 0 {
        if let Some(l) = v.last_mut() {
            *l = (1u64 << (rows % 64)) - 1;
        }
    }
    v
}
#[inline]
fn set(b: &mut [u64], r: usize) {
    b[r / 64] |= 1 << (r % 64);
}

// ───────────────────────── per-segment evaluation ─────────────────────────

struct SegCtx<'a> {
    st: &'a Store,
    si: usize,
    seg: &'a SegmentReader,
    cache: &'a ChunkCache,
    skipped: usize,
    raw_ok: bool,
    fs: &'a FormulaSet,
}

/// Can a free-text needle occur in a column's rendered (= source-spelled)
/// values? Text/Int columns are decided by their Bloom filter. Typed columns
/// render through a known format, so the needle's characters decide it; rows
/// kept verbatim are covered by a small Bloom filter over just those rows.
pub(crate) fn eligible(
    seg: &SegmentReader,
    c: &crate::segment::ColMeta,
    needle_lower: &[u8],
) -> bool {
    let s = std::str::from_utf8(needle_lower).unwrap_or("");
    match c.ty {
        ColType::Text | ColType::Int => seg.may_contain_substring(c.col_id, s),
        _ => {
            let verbatim_hit = (c.ex > 0 || c.side > 0) && seg.may_contain_substring(c.col_id, s);
            verbatim_hit || format_can_contain(c, needle_lower)
        }
    }
}

/// As [`eligible`], with the column's filter already read (`None`: the
/// column has no filter, so anything may be in it). For asking about many
/// texts at once: the filter is read from disk once, not once per text.
pub(crate) fn eligible_with(
    bloom: Option<&[u8]>,
    c: &crate::segment::ColMeta,
    needle_lower: &[u8],
) -> bool {
    let may = || bloom.is_none_or(|b| crate::segment::bloom::may_contain(b, needle_lower));
    match c.ty {
        ColType::Text | ColType::Int => may(),
        _ => ((c.ex > 0 || c.side > 0) && may()) || format_can_contain(c, needle_lower),
    }
}

fn format_can_contain(c: &crate::segment::ColMeta, n: &[u8]) -> bool {
    let dots = n.iter().filter(|&&x| x == b'.').count();
    match c.ty {
        ColType::Float => {
            n.iter().any(u8::is_ascii_digit)
                && dots <= 1
                && n.iter().all(|x| {
                    x.is_ascii_digit()
                        || matches!(x, b'.' | b'-' | b'e' | b'+' | b'i' | b'n' | b'f' | b'a')
                })
        }
        ColType::Time => match &c.fmt {
            Some(crate::segment::ColFormat::Time(t)) => {
                let lit: Vec<u8> = t
                    .mask
                    .bytes()
                    .filter(|b| *b != b'9' && *b != b'@')
                    .map(|b| b.to_ascii_lowercase())
                    .collect();
                n.iter().all(|x| {
                    x.is_ascii_digit() || lit.contains(x) || matches!(x, b'a' | b'p' | b'm')
                })
            }
            _ => n
                .iter()
                .all(|x| x.is_ascii_digit() || matches!(x, b'-' | b':' | b' ' | b'.')),
        },
        ColType::Bool => {
            let w: [&[u8]; 2] = [b"true", b"false"];
            w.iter()
                .any(|v| n.len() <= v.len() && v.windows(n.len().max(1)).any(|win| win == n))
        }
        _ => true,
    }
}

impl<'a> SegCtx<'a> {
    fn may_match(&self, e: &CExpr) -> bool {
        match e {
            CExpr::And(a, b) => self.may_match(a) && self.may_match(b),
            CExpr::Or(a, b) => self.may_match(a) || self.may_match(b),
            CExpr::Not(_) => true,
            // a stored column is never an error
            CExpr::P(Pred::IsError { col }) => *col as usize >= self.st.schema().len(),
            CExpr::P(Pred::Free { needle, .. }) => self
                .seg
                .meta()
                .cols
                .iter()
                .any(|c| c.col_id != OFFSET_COL && eligible(self.seg, c, needle)),
            CExpr::P(Pred::AnyRe { lit, .. }) => lit.as_ref().is_none_or(|n| {
                self.seg
                    .meta()
                    .cols
                    .iter()
                    .any(|c| c.col_id != OFFSET_COL && eligible(self.seg, c, n))
            }),
            CExpr::P(Pred::Cmp {
                col,
                op,
                lower,
                time,
                num,
                ..
            }) => {
                if *col as usize >= self.st.schema().len() {
                    return true;
                }
                let Some(m) = self.seg.meta().col(*col) else {
                    return false;
                };
                if m.nulls == self.seg.meta().rows {
                    return false;
                }
                let zone = |v: Option<f64>| -> bool {
                    let (Some(lo), Some(hi), Some(v)) = (
                        m.min_i.map(|x| x as f64).or(m.min_f),
                        m.max_i.map(|x| x as f64).or(m.max_f),
                        v,
                    ) else {
                        return true;
                    };
                    if m.side > 0 {
                        return true;
                    }
                    match op {
                        CmpOp::Eq => lo <= v && v <= hi,
                        CmpOp::Gt => hi > v,
                        CmpOp::Ge => hi >= v,
                        CmpOp::Lt => lo < v,
                        CmpOp::Le => lo <= v,
                        _ => true,
                    }
                };
                match m.ty {
                    ColType::Int | ColType::Float => match op {
                        CmpOp::Eq | CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le
                            if num.is_some() =>
                        {
                            zone(*num)
                        }
                        _ => true,
                    },
                    ColType::Time => zone(time.map(|t| t as f64)),
                    ColType::Text => match op {
                        CmpOp::Eq | CmpOp::Contains if lower.len() >= 3 => self
                            .seg
                            .may_contain_substring(*col, std::str::from_utf8(lower).unwrap_or("")),
                        _ => true,
                    },
                    ColType::Bool => true,
                }
            }
        }
    }

    fn col(&self, c: u32) -> Result<Option<Arc<DecodedColumn>>> {
        let t = std::time::Instant::now();
        let r = self.cache.get(self.st, self.si, c);
        prof(2, t);
        r
    }

    fn eval(&mut self, e: &CExpr, rows: u32) -> Result<Vec<u64>> {
        Ok(match e {
            CExpr::And(a, b) => {
                let mut x = self.eval(a, rows)?;
                if x.iter().all(|w| *w == 0) {
                    return Ok(x);
                }
                let y = self.eval(b, rows)?;
                for (p, q) in x.iter_mut().zip(y) {
                    *p &= q;
                }
                x
            }
            CExpr::Or(a, b) => {
                let mut x = self.eval(a, rows)?;
                let y = self.eval(b, rows)?;
                for (p, q) in x.iter_mut().zip(y) {
                    *p |= q;
                }
                x
            }
            CExpr::Not(a) => {
                let x = self.eval(a, rows)?;
                full(rows).into_iter().zip(x).map(|(f, v)| f & !v).collect()
            }
            CExpr::P(p) => self.eval_pred(p, rows)?,
        })
    }

    /// Check candidate rows against columns read from the source, re-parsing
    /// only the windows that hold a candidate.
    fn verify_from_source(
        &self,
        cand: &[u64],
        cols: &[u32],
        finder: &Needle,
        rows: u32,
        out: &mut [u64],
    ) -> Result<()> {
        let mut r = 0usize;
        while r < rows as usize {
            let w = r / WINDOW_ROWS;
            let end = ((w + 1) * WINDOW_ROWS).min(rows as usize);
            let any = (r..end).any(|k| cand[k / 64] >> (k % 64) & 1 == 1);
            if any {
                for &c in cols {
                    let Some(col) = self.cache.get_rebuilt(self.st, self.si, c, w as u32)? else {
                        continue;
                    };
                    for k in r..end {
                        if cand[k / 64] >> (k % 64) & 1 == 1
                            && col
                                .text(k - w * WINDOW_ROWS)
                                .is_some_and(|t| finder.found_in(t.as_bytes()))
                        {
                            set(out, k);
                        }
                    }
                }
            }
            r = end;
        }
        Ok(())
    }

    /// Mark rows whose original source bytes contain the needle (case-insensitive).
    fn raw_scan(&self, needle: &[u8], rows: u32, out: &mut [u64]) -> Result<()> {
        let m = self.seg.meta();
        let (a, b) = (m.byte_start, m.byte_end.max(m.byte_start));
        let len = (b - a) as usize;
        let _res = self
            .cache
            .reserve(len as u64, crate::budget::Component::Query)?;
        let f = self.st.source_file_of(m.source)?;
        let t0 = std::time::Instant::now();
        let mut buf = vec![0u8; len];
        crate::segment::read_at(f, &mut buf, a)?;
        prof(0, t0);
        let t1 = std::time::Instant::now();
        let offs = self
            .col(OFFSET_COL)?
            .ok_or_else(|| Error::Corrupt("segment without offsets".into()))?;
        if needle.is_empty() || buf.len() < needle.len() {
            return Ok(());
        }
        let mut r = 0usize;
        let mut mark = |p: usize, out: &mut [u64]| {
            let abs = (a + p as u64) as i64;
            while r + 1 < rows as usize && offs.int(r + 1).is_some_and(|o| o <= abs) {
                r += 1;
            }
            set(out, r);
        };
        if !needle.iter().any(u8::is_ascii_alphabetic) {
            // no letters → case doesn't matter → SIMD substring search
            for p in memchr::memmem::find_iter(&buf, needle) {
                mark(p, out);
            }
        } else {
            let first = needle[0];
            let last = buf.len() - needle.len();
            for p in memchr::memchr2_iter(first, first.to_ascii_uppercase(), &buf[..=last]) {
                if buf[p + 1..p + needle.len()].eq_ignore_ascii_case(&needle[1..]) {
                    mark(p, out);
                }
            }
        }
        prof(1, t1);
        Ok(())
    }

    fn eval_pred(&mut self, p: &Pred, rows: u32) -> Result<Vec<u64>> {
        let mut out = vec![0u64; words(rows)];
        match p {
            Pred::IsError { col } => {
                if let Some(f) = self.fs.get(self.st.schema().len(), *col) {
                    let kept = formula_block(self.st, self.si, f, self.cache)?;
                    let bad: Vec<bool> = kept
                        .vals
                        .iter()
                        .map(|v| matches!(v, crate::formula::FValue::Error(_)))
                        .collect();
                    for (r, &k) in kept.idx.iter().enumerate() {
                        if bad[k as usize] {
                            set(&mut out, r);
                        }
                    }
                }
            }
            Pred::Free { needle, finder } => {
                let cols: Vec<(u32, bool, bool)> = self
                    .seg
                    .meta()
                    .cols
                    .iter()
                    .filter(|c| c.col_id != OFFSET_COL)
                    .map(|c| {
                        (
                            c.col_id,
                            eligible(self.seg, c, needle),
                            matches!(c.ty, ColType::Text | ColType::Int),
                        )
                    })
                    .collect();
                let mut typed: Vec<u32> = Vec::new();
                let mut from_source: Vec<u32> = Vec::new();
                for (c, ok, textual) in cols {
                    if !ok {
                        self.skipped += 1;
                        continue;
                    }
                    if !textual {
                        typed.push(c);
                        continue;
                    }
                    if self.seg.in_source(c) {
                        from_source.push(c);
                        continue;
                    }
                    let Some(col) = self.col(c)? else { continue };
                    match_col(&col, rows, &mut out, |t| finder.found_in(t), None);
                }
                // Text columns read from the source: scan the source bytes
                // instead of parsing them. Exact on its own for a clean CSV and a
                // plain word; otherwise its hits are candidates, checked by
                // re-parsing only the windows that hold them.
                let mut raw: Option<Vec<u64>> = None;
                if !from_source.is_empty() {
                    let spec = self.st.part(self.seg.meta().source).1;
                    let mut bits = vec![0u64; words(rows)];
                    if self.raw_ok
                        && raw_settles(needle, spec)
                        && self.raw_scan(needle, rows, &mut bits).is_ok()
                    {
                        if raw_is_exact(self.st, spec) {
                            for (o, b) in out.iter_mut().zip(&bits) {
                                *o |= b;
                            }
                        } else {
                            // JSON escapes hide characters from a byte scan: "\u0063"
                            // is "c", "\/" is "/"; rows holding them are candidates too
                            let mut esc = bits.clone();
                            self.raw_scan(b"\\u", rows, &mut esc)?;
                            if needle.contains(&b'/') {
                                self.raw_scan(b"\\/", rows, &mut esc)?;
                            }
                            let cand: Vec<u64> =
                                esc.iter().zip(&out).map(|(b, o)| b & !o).collect();
                            self.verify_from_source(&cand, &from_source, finder, rows, &mut out)?;
                        }
                        raw = Some(bits);
                    } else {
                        for c in from_source {
                            let Some(col) = self.col(c)? else { continue };
                            match_col(&col, rows, &mut out, |t| finder.found_in(t), None);
                        }
                    }
                }
                if let (false, Some(bits)) = (typed.is_empty(), &raw) {
                    // the typed columns' raw scan below would be this same scan
                    for (o, b) in out.iter_mut().zip(bits) {
                        *o |= b;
                    }
                    typed.clear();
                }
                // Typed columns (times, decimals, bools) are matched against the
                // original source bytes: exact spelling, and gigabytes per second
                // instead of rendering every value. Fallback: render.
                if !typed.is_empty()
                    && !(self.raw_ok && self.raw_scan(needle, rows, &mut out).is_ok())
                {
                    for c in typed {
                        let Some(col) = self.col(c)? else { continue };
                        match_col(&col, rows, &mut out, |t| finder.found_in(t), None);
                    }
                }
            }
            Pred::AnyRe { re, lit } => {
                let cols: Vec<u32> = self
                    .seg
                    .meta()
                    .cols
                    .iter()
                    .filter(|c| c.col_id != OFFSET_COL)
                    .filter(|c| lit.as_ref().is_none_or(|n| eligible(self.seg, c, n)))
                    .map(|c| c.col_id)
                    .collect();
                for c in cols {
                    let Some(col) = self.col(c)? else { continue };
                    match_col(&col, rows, &mut out, |t| re.is_match(t), None);
                }
            }
            Pred::Cmp {
                col,
                op,
                value,
                lower,
                num,
                int,
                time,
                re,
            } => {
                if let Some(f) = self.fs.get(self.st.schema().len(), *col) {
                    let kept = formula_block(self.st, self.si, f, self.cache)?;
                    // each distinct result asked once
                    let hit: Vec<bool> = kept
                        .vals
                        .iter()
                        .map(|v| fvalue_pred(v, *op, value, lower, *num, *time, re))
                        .collect();
                    for (r, &k) in kept.idx.iter().enumerate() {
                        if hit[k as usize] {
                            set(&mut out, r);
                        }
                    }
                    return Ok(out);
                }
                let Some(c) = self.col(*col)? else {
                    return Ok(out);
                };
                let (op, value, lower, num, re) = (*op, value.as_str(), lower.as_slice(), *num, re);
                let tp = |t: &[u8]| text_pred(op, value, lower, num, re, t);
                match c.ty {
                    ColType::Int | ColType::Float
                        if matches!(
                            op,
                            CmpOp::Eq | CmpOp::Ne | CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le
                        ) && num.is_some() =>
                    {
                        let b = num.unwrap();
                        let _ = int;
                        match_col(&c, rows, &mut out, tp, Some(&|v: f64| num_pred(op, v, b)));
                    }
                    ColType::Time
                        if time.is_some()
                            && !matches!(op, CmpOp::Contains | CmpOp::Glob | CmpOp::Regex) =>
                    {
                        let b = time.unwrap() as f64;
                        match_col(&c, rows, &mut out, tp, Some(&|v: f64| num_pred(op, v, b)));
                    }
                    _ => match_col(&c, rows, &mut out, tp, None),
                }
            }
        }
        Ok(out)
    }
}

/// Apply `text` (on text bytes) or `num` (on numeric values) to every non-null row.
/// A raw scan finds every row whose values contain `needle`: plain printable
/// ASCII with no quote, backslash or (CSV) delimiter, so a value's text
/// appears verbatim in the source and a hit cannot straddle two fields.
fn raw_settles(needle: &[u8], spec: &crate::store::SourceSpec) -> bool {
    let plain = !needle.is_empty()
        && needle
            .iter()
            .all(|&b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\');
    plain
        && match spec {
            crate::store::SourceSpec::Csv { opts, .. } => {
                !needle.contains(&opts.delimiter) && !needle.contains(&opts.quote)
            }
            crate::store::SourceSpec::Text { .. } => !needle
                .iter()
                .any(|b| matches!(b, b' ' | b'[' | b']' | b',')),
            _ => true,
        }
}

/// Every source byte between two row offsets belongs to that row's fields
/// (a CSV with no repeated headers, re-read quotes or skipped spans), so a
/// raw hit is a value hit. JSON keys sit in the bytes too: never exact.
fn raw_is_exact(st: &Store, spec: &crate::store::SourceSpec) -> bool {
    let r = &st.manifest().report;
    matches!(spec, crate::store::SourceSpec::Csv { .. })
        && r.header_repeats == 0
        && r.bad_quote_spans.is_empty()
        && r.unreadable_spans.is_empty()
}

fn match_col(
    c: &DecodedColumn,
    rows: u32,
    out: &mut [u64],
    text: impl Fn(&[u8]) -> bool,
    num: Option<&dyn Fn(f64) -> bool>,
) {
    let _t = std::time::Instant::now();
    match_col_inner(c, rows, out, text, num);
    prof(3, _t);
}

fn match_col_inner(
    c: &DecodedColumn,
    rows: u32,
    out: &mut [u64],
    text: impl Fn(&[u8]) -> bool,
    num: Option<&dyn Fn(f64) -> bool>,
) {
    match c.data() {
        ColData::Dict { codes, .. } => {
            let nd = codes.iter().copied().max().map_or(0, |m| m as usize + 1);
            let hit: Vec<bool> = (0..nd)
                .map(|k| c.dict_entry(k).is_some_and(|e| text(e.as_bytes())))
                .collect();
            for r in 0..rows as usize {
                if hit[codes[r] as usize] && !c.is_null(r) {
                    set(out, r);
                }
            }
        }
        ColData::Plain { .. } => {
            for r in 0..rows as usize {
                if let Some(t) = c.text_bytes(r) {
                    if text(t) {
                        set(out, r);
                    }
                }
            }
        }
        _ => {
            for r in 0..rows as usize {
                if c.is_null(r) {
                    continue;
                }
                let ok = match (num, c.float(r)) {
                    (Some(f), Some(v)) => f(v),
                    _ => c.text(r).is_some_and(|t| text(t.as_bytes())),
                };
                if ok {
                    set(out, r);
                }
            }
        }
    }
}

fn time_mask(ctx: &SegCtx, tc: u32, a: i64, b: i64, rows: u32) -> Result<Vec<u64>> {
    let mut out = vec![0u64; words(rows)];
    let unit = ctx.st.schema().time_unit_us.unwrap_or(1);
    if let Some(c) = ctx.col(tc)? {
        for r in 0..rows as usize {
            if let Some(t) = c.int(r) {
                let t = t.saturating_mul(unit);
                if t >= a && t < b {
                    set(&mut out, r);
                }
            }
        }
    }
    Ok(out)
}

// ───────────────────────── public API ─────────────────────────

/// Evaluate the filter part of `q` (expression + time range) over all segments.
pub fn filter(st: &Store, q: &Query, cache: &ChunkCache, cancel: &AtomicBool) -> Result<MatchSet> {
    filter_with(st, q, &FormulaSet::default(), cache, cancel)
}

/// `filter` with formula (virtual) columns available to the query.
pub fn filter_with(
    st: &Store,
    q: &Query,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<MatchSet> {
    if cancel.load(Ordering::SeqCst) {
        return Err(Error::Cancelled);
    }
    let expr = q.filter.as_ref().map(|e| compile(st, fs, e)).transpose()?;
    let time = match (q.time, st.schema().time_col) {
        (Some(r), Some(tc)) => Some((tc as u32, r.0, r.1)),
        (Some(_), None) => return Err(Error::Invalid("this source has no time column".into())),
        _ => None,
    };
    // original-bytes search is only valid while the source matches the index
    let raw_ok = !st.is_stale();
    let n = st.segments().len();
    let results: Vec<Mutex<Option<SegMatch>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let pruned = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    // as many threads as the cache can feed: a segment read from the source
    // needs about 5× its bytes while it is rebuilt and decoded
    let per_thread = st
        .segments()
        .iter()
        .filter(|s| s.meta().cols.iter().any(|c| c.in_source))
        .map(|s| s.meta().byte_end.saturating_sub(s.meta().byte_start) * 5 + (1 << 20))
        .max()
        .unwrap_or(0);
    let fit = if per_thread == 0 {
        usize::MAX
    } else {
        (cache.budget().cap() / per_thread).max(1) as usize
    };
    // every core but the window's, as far as memory allows
    let threads = scan_cores().min(fit).min(n.max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n || failure.lock().unwrap().is_some() {
                    return;
                }
                if cancel.load(Ordering::SeqCst) {
                    *failure.lock().unwrap() = Some(Error::Cancelled);
                    return;
                }
                let seg = &st.segments()[i];
                let rows = seg.meta().rows;
                let mut ctx = SegCtx {
                    st,
                    si: i,
                    seg,
                    cache,
                    skipped: 0,
                    raw_ok,
                    fs,
                };
                let time_ok = time.is_none_or(|(_, a, b)| {
                    let m = seg.meta();
                    !(m.time_max.is_some_and(|mx| mx < a) || m.time_min.is_some_and(|mn| mn >= b))
                });
                if !time_ok || expr.as_ref().is_some_and(|e| !ctx.may_match(e)) {
                    pruned.fetch_add(1, Ordering::Relaxed);
                    *results[i].lock().unwrap() = Some(SegMatch::None);
                    continue;
                }
                let r: Result<Vec<u64>> = (|| {
                    let mut bits = match &expr {
                        Some(e) => ctx.eval(e, rows)?,
                        None => full(rows),
                    };
                    if let Some((tc, a, b)) = time {
                        let m = time_mask(&ctx, tc, a, b, rows)?;
                        for (x, y) in bits.iter_mut().zip(m) {
                            *x &= y;
                        }
                    }
                    Ok(bits)
                })();
                skipped.fetch_add(ctx.skipped, Ordering::Relaxed);
                match r {
                    Ok(bits) => *results[i].lock().unwrap() = Some(SegMatch::from_bits(bits, rows)),
                    Err(e) => {
                        *failure.lock().unwrap() = Some(e);
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    let segs: Vec<SegMatch> = results
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap_or(SegMatch::None))
        .collect();
    let pr = pruned.into_inner();
    Ok(MatchSet::new(
        segs,
        ScanStats {
            segments_total: n,
            segments_pruned: pr,
            segments_scanned: n - pr,
            columns_skipped: skipped.into_inner(),
        },
    ))
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    /// Global row ids.
    pub rows: Vec<u64>,
    /// `cells[i][j]` = text of row i, requested column j.
    pub cells: Vec<Vec<Option<String>>>,
    /// Made-column cells that could not be worked out: (page row, requested
    /// column, the reason in words). Their cell has no text.
    pub errors: Vec<(usize, usize, String)>,
}

/// Materialise matches `[start, start+len)` for the given columns only.
pub fn page(
    st: &Store,
    m: &MatchSet,
    start: u64,
    len: usize,
    cols: &[usize],
    cache: &ChunkCache,
) -> Result<Page> {
    page_with(st, m, start, len, cols, &FormulaSet::default(), cache)
}

/// `page` where `cols` may include formula columns (numbered after stored ones).
pub fn page_with(
    st: &Store,
    m: &MatchSet,
    start: u64,
    len: usize,
    cols: &[usize],
    fs: &FormulaSet,
    cache: &ChunkCache,
) -> Result<Page> {
    let ns = st.schema().len();
    let mut p = Page::default();
    let mut locs = Vec::with_capacity(len);
    for n in start..(start + len as u64).min(m.count()) {
        if let Some(l) = m.locate(n) {
            locs.push(l);
        }
    }
    let mut i = 0;
    while i < locs.len() {
        let si = locs[i].0;
        let j = locs[i..]
            .iter()
            .position(|l| l.0 != si)
            .map_or(locs.len(), |k| i + k);
        let seg_rows: Vec<usize> = locs[i..j].iter().map(|l| l.1 as usize).collect();
        // stored columns decode; formula columns evaluate for just these rows
        let mut stored: Vec<Option<Arc<DecodedColumn>>> = Vec::with_capacity(cols.len());
        let mut computed: Vec<Option<Vec<crate::formula::FValue>>> = Vec::with_capacity(cols.len());
        // columns read from the source: only the windows holding these rows
        let mut windowed: Vec<Option<HashMap<u32, Option<Arc<DecodedColumn>>>>> =
            Vec::with_capacity(cols.len());
        let reader = &st.segments()[si];
        for &c in cols {
            match fs.get(ns, c as u32) {
                Some(f) => {
                    let vals = formula_rows(st, si, f, seg_rows.iter().copied(), cache)?;
                    computed.push(Some(vals));
                    stored.push(None);
                    windowed.push(None);
                }
                None if reader.in_source(c as u32) && !cache.has_whole(st, si, c as u32) => {
                    let mut wins = HashMap::new();
                    for &r in &seg_rows {
                        let w = (r / WINDOW_ROWS) as u32;
                        if let std::collections::hash_map::Entry::Vacant(e) = wins.entry(w) {
                            e.insert(cache.get_rebuilt(st, si, c as u32, w)?);
                        }
                    }
                    windowed.push(Some(wins));
                    stored.push(None);
                    computed.push(None);
                }
                None => {
                    stored.push(cache.get(st, si, c as u32)?);
                    computed.push(None);
                    windowed.push(None);
                }
            }
        }
        let base = st.segments()[si].meta().row_start;
        for (k, &r) in seg_rows.iter().enumerate() {
            let at = p.rows.len();
            p.rows.push(base + r as u64);
            let errors = &mut p.errors;
            p.cells.push(
                (0..cols.len())
                    .map(|ci| match (&computed[ci], &windowed[ci]) {
                        (Some(v), _) => match &v[k] {
                            crate::formula::FValue::Error(e) => {
                                errors.push((at, ci, e.clone()));
                                None
                            }
                            v => v.text(),
                        },
                        (None, Some(wins)) => wins
                            .get(&((r / WINDOW_ROWS) as u32))
                            .and_then(|c| c.as_ref())
                            .and_then(|c| c.text(r % WINDOW_ROWS).map(|s| s.into_owned())),
                        (None, None) => stored[ci]
                            .as_ref()
                            .and_then(|c| c.text(r).map(|s| s.into_owned())),
                    })
                    .collect(),
            );
        }
        i = j;
    }
    Ok(p)
}
