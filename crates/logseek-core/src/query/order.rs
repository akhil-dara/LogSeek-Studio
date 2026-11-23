//! Whether a file's rows run in time order: oldest first, newest first, or
//! neither. Real logs are rarely perfectly ordered (threads write a few
//! seconds out of turn), so rows count as in order when none falls behind the
//! latest time seen by more than a small share of the file's time span; how far
//! behind the worst row falls is kept as the slack. Only then is a gap between
//! two rows next to each other a real silence, and a time a place in the rows.

use super::exec::ChunkCache;
use crate::error::{Error, Result};
use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    OldestFirst,
    NewestFirst,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeOrder {
    pub direction: Direction,
    /// How far (µs) the most out-of-place row falls behind the order.
    pub slack_us: i64,
}

/// Rows out of place by more than this share of the time span: not in order.
const SHARE: i64 = 50; // 2%

/// Follows the times as they come, and stops as soon as both ways are ruled out.
struct Judge {
    tol: i64,
    max: Option<i64>,
    min: Option<i64>,
    behind_up: i64,
    behind_down: i64,
}

impl Judge {
    fn new(span: i64) -> Self {
        Judge {
            tol: (span / SHARE).max(0),
            max: None,
            min: None,
            behind_up: 0,
            behind_down: 0,
        }
    }

    /// Takes one time; false once neither way can hold.
    fn take(&mut self, t: i64) -> bool {
        let max = *self.max.get_or_insert(t);
        let min = *self.min.get_or_insert(t);
        self.behind_up = self.behind_up.max(max - t);
        self.behind_down = self.behind_down.max(t - min);
        self.max = Some(max.max(t));
        self.min = Some(min.min(t));
        self.behind_up <= self.tol || self.behind_down <= self.tol
    }

    fn verdict(&self) -> TimeOrder {
        let (up, down) = (self.behind_up, self.behind_down);
        let direction = if up <= self.tol && up <= down {
            Direction::OldestFirst
        } else if down <= self.tol {
            Direction::NewestFirst
        } else {
            Direction::None
        };
        TimeOrder {
            direction,
            slack_us: match direction {
                Direction::OldestFirst => up,
                Direction::NewestFirst => down,
                Direction::None => up.min(down),
            },
        }
    }
}

/// The order of a run of times (µs), in the order the rows come.
pub fn judge(times: impl Iterator<Item = i64>) -> TimeOrder {
    let v: Vec<i64> = times.collect();
    let span = match (v.iter().min(), v.iter().max()) {
        (Some(a), Some(b)) => b - a,
        _ => 0,
    };
    let mut j = Judge::new(span);
    for t in v {
        if !j.take(t) {
            break;
        }
    }
    j.verdict()
}

/// The order of a store's rows by its time column, reading block by block
/// and stopping at the first block that rules both ways out (a file in no
/// time order is known after a few rows). `None` without a time column.
pub fn time_order(
    st: &Store,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Option<TimeOrder>> {
    let Some(tc) = st.schema().time_col else {
        return Ok(None);
    };
    let unit = st.schema().time_unit_us.unwrap_or(1);
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for seg in st.segments() {
        if let (Some(a), Some(b)) = (seg.meta().time_min, seg.meta().time_max) {
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    if lo > hi {
        return Ok(None);
    }
    let mut j = Judge::new(hi - lo);
    for (si, seg) in st.segments().iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let blk = cache.block(st, si)?;
        let Some(c) = blk.column(tc as u32)? else {
            continue;
        };
        for r in 0..seg.meta().rows as usize {
            if let Some(t) = c.int(r) {
                if !j.take(t.saturating_mul(unit)) {
                    return Ok(Some(j.verdict()));
                }
            }
        }
    }
    Ok(Some(j.verdict()))
}
