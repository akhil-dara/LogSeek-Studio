//! Follow: from a starting set of rows, the rows that link to them — a
//! column of the other rows equals a column of the starting rows, optionally
//! inside a time window, optionally repeated until nothing new is found
//! (a tree). One general move: process children and parents, same IP, same
//! session, same hash.

use super::agg::match_rows;
use super::exec::{resolve_column, ChunkCache, MatchSet, SegMatch};
use crate::error::{Error, Result};
use crate::store::Store;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// How near in time a linked row must be to the row it links to (µs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Window {
    #[default]
    Any,
    /// At the same time or up to this long after.
    After(i64),
    /// At the same time or up to this long before.
    Before(i64),
    /// Within this long either side.
    Around(i64),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FollowSpec {
    /// The starting rows' column.
    pub from_col: String,
    /// The column of the other rows that must equal it.
    pub to_col: String,
    /// More columns that may equal it instead (any one is enough): e.g.
    /// children point back with a parent id, other events with a context id.
    pub also_to: Vec<String>,
    pub window: Window,
    /// Follow again from what was found, until nothing new appears.
    pub repeat: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FollowResult {
    /// Per step: (row found, the row it links to), in file order.
    pub levels: Vec<Vec<(u64, u64)>>,
}

impl FollowResult {
    pub fn found(&self) -> u64 {
        self.levels.iter().map(|l| l.len() as u64).sum()
    }
}

/// (time µs, row) of the rows a value came from.
type Parents = HashMap<String, Vec<(Option<i64>, u64)>>;

/// The values of `col` on `rows`, with each row's time.
fn values_of(
    st: &Store,
    rows: &MatchSet,
    col: u32,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Parents> {
    let tc = st.schema().time_col.map(|t| t as u32);
    let mut out: Parents = HashMap::new();
    for (si, sm) in rows.segs.iter().enumerate() {
        if matches!(sm, SegMatch::None) {
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let meta = st.segments()[si].meta();
        let Some(c) = cache.get(st, si, col)? else {
            continue;
        };
        let t = tc.map(|t| cache.get(st, si, t)).transpose()?.flatten();
        for r in match_rows(sm, meta.rows) {
            let Some(v) = c.text(r).filter(|v| !v.is_empty()) else {
                continue;
            };
            let time = t
                .as_ref()
                .and_then(|t| t.int(r))
                .map(|x| st.schema().to_us(x));
            out.entry(v.into_owned())
                .or_default()
                .push((time, meta.row_start + r as u64));
        }
    }
    Ok(out)
}

fn inside(w: Window, parent: Option<i64>, row: Option<i64>) -> bool {
    let (Some(p), Some(r)) = (parent, row) else {
        return w == Window::Any;
    };
    match w {
        Window::Any => true,
        Window::After(n) => r >= p && r - p <= n,
        Window::Before(n) => r <= p && p - r <= n,
        Window::Around(n) => (r - p).abs() <= n,
    }
}

/// Follow from `start` over the whole file.
pub fn follow(
    st: &Store,
    start: &MatchSet,
    spec: &FollowSpec,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<FollowResult> {
    let from = resolve_column(st, &spec.from_col)? as u32;
    let mut tos = vec![resolve_column(st, &spec.to_col)? as u32];
    for c in &spec.also_to {
        let id = resolve_column(st, c)? as u32;
        if !tos.contains(&id) {
            tos.push(id);
        }
    }
    let tc = st.schema().time_col.map(|t| t as u32);
    if spec.window != Window::Any && tc.is_none() {
        return Err(Error::Invalid(
            "a time window needs a timeline, and this file has none".into(),
        ));
    }
    // rows already in the tree (the starting rows are its roots)
    let mut seen: HashSet<u64> = HashSet::new();
    for (si, sm) in start.segs.iter().enumerate() {
        let meta = st.segments()[si].meta();
        seen.extend(match_rows(sm, meta.rows).map(|r| meta.row_start + r as u64));
    }
    let all = MatchSet::all(st);
    let mut frontier = values_of(st, start, from, cache, cancel)?;
    let mut res = FollowResult::default();
    while !frontier.is_empty() {
        let mut level: Vec<(u64, u64)> = Vec::new();
        for si in 0..st.segments().len() {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let meta = st.segments()[si].meta();
            let t = tc.map(|t| cache.get(st, si, t)).transpose()?.flatten();
            let before = level.len();
            let mut taken: HashSet<u64> = HashSet::new();
            for &to in &tos {
                let Some(c) = cache.get(st, si, to)? else {
                    continue;
                };
                for r in 0..meta.rows as usize {
                    let Some(v) = c.text(r).filter(|v| !v.is_empty()) else {
                        continue;
                    };
                    let Some(parents) = frontier.get(v.as_ref()) else {
                        continue;
                    };
                    let row = meta.row_start + r as u64;
                    if seen.contains(&row) || taken.contains(&row) {
                        continue;
                    }
                    let time = t
                        .as_ref()
                        .and_then(|t| t.int(r))
                        .map(|x| st.schema().to_us(x));
                    // the earliest row it can link to
                    let parent = parents
                        .iter()
                        .filter(|(pt, _)| inside(spec.window, *pt, time))
                        .min_by_key(|(pt, p)| (*pt, *p))
                        .map(|(_, p)| *p);
                    if let Some(p) = parent {
                        level.push((row, p));
                        taken.insert(row);
                    }
                }
            }
            // file order within the segment, whichever column matched
            level[before..].sort_unstable();
        }
        if level.is_empty() {
            break;
        }
        seen.extend(level.iter().map(|(r, _)| *r));
        let found: Vec<u64> = level.iter().map(|(r, _)| *r).collect();
        res.levels.push(level);
        if !spec.repeat {
            break;
        }
        frontier = values_of(st, &all.restrict(st, &found), from, cache, cancel)?;
    }
    Ok(res)
}
