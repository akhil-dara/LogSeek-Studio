//! Grouping for the grid: every value of a column within a result, with its
//! row count and first/last time, in a chosen order, read a page at a time.
//! No limits: the full group table is kept (spilled to disk by `run_stats`
//! when large) and any page of it can be read.

use super::agg::{run_stats, StatsResult};
use super::ast::{Agg, AggFn};
use super::exec::{ChunkCache, MatchSet};
use crate::store::Store;
use crate::types::ColType;
use crate::Result;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GroupSort {
    #[default]
    MostRows,
    FewestRows,
    FirstSeen,
    LastSeen,
    Name,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow {
    /// `None` = rows with no value in this column.
    pub value: Option<String>,
    pub rows: u64,
    /// Earliest and latest time in the group, epoch µs (when there is a timeline).
    pub first: Option<i64>,
    pub last: Option<i64>,
}

pub struct GroupLevel {
    stats: StatsResult,
    /// Group indexes in display order (narrowed by a search).
    order: Vec<u32>,
    /// Every group in display order (what a cleared search shows).
    all: Vec<u32>,
    timed: bool,
    unit: i64,
}

impl GroupLevel {
    /// Group `m`'s rows by column `by`.
    pub fn build(
        st: &Store,
        m: &MatchSet,
        by: &str,
        sort: GroupSort,
        cache: &ChunkCache,
        cancel: &AtomicBool,
    ) -> Result<GroupLevel> {
        let schema = st.schema();
        let tc = schema.time_col.and_then(|t| schema.columns.get(t));
        let mut aggs = vec![Agg {
            func: AggFn::Count,
            col: None,
        }];
        if let Some(t) = tc {
            for func in [AggFn::Min, AggFn::Max] {
                aggs.push(Agg {
                    func,
                    col: Some(t.name.clone()),
                });
            }
        }
        // an epoch-number timeline is aggregated in its own unit
        let unit = match tc {
            Some(t) if t.ty == ColType::Int => schema.time_unit_us.unwrap_or(1),
            _ => 1,
        };
        let stats = run_stats(st, m, &aggs, &[by.to_string()], cache, cancel)?;
        let mut lvl = GroupLevel {
            stats,
            order: Vec::new(),
            all: Vec::new(),
            timed: tc.is_some(),
            unit,
        };
        lvl.sort(sort);
        Ok(lvl)
    }

    /// A level over counted values (e.g. a made column): rows, no times.
    pub fn from_stats(stats: StatsResult, sort: GroupSort) -> GroupLevel {
        let mut lvl = GroupLevel {
            stats,
            order: Vec::new(),
            all: Vec::new(),
            timed: false,
            unit: 1,
        };
        lvl.sort(sort);
        lvl
    }

    fn row_of(&self, key: Vec<String>, vals: &[f64]) -> GroupRow {
        let t = |v: Option<&f64>| {
            v.filter(|x| x.is_finite())
                .map(|x| (*x as i64).saturating_mul(self.unit))
        };
        let value = key.into_iter().next().filter(|v| !v.is_empty());
        GroupRow {
            value,
            rows: vals.first().copied().unwrap_or(0.0) as u64,
            first: if self.timed { t(vals.get(1)) } else { None },
            last: if self.timed { t(vals.get(2)) } else { None },
        }
    }

    /// Put the groups in `sort` order (ties by name; no value last).
    pub fn sort(&mut self, sort: GroupSort) {
        let rows: Vec<GroupRow> = self.stats.iter().map(|(k, v)| self.row_of(k, &v)).collect();
        let mut order: Vec<u32> = (0..rows.len() as u32).collect();
        let name = |r: &GroupRow| (r.value.is_none(), r.value.clone());
        order.sort_by(|&a, &b| {
            let (x, y) = (&rows[a as usize], &rows[b as usize]);
            let primary = match sort {
                GroupSort::MostRows => y.rows.cmp(&x.rows),
                GroupSort::FewestRows => x.rows.cmp(&y.rows),
                GroupSort::FirstSeen => x
                    .first
                    .unwrap_or(i64::MAX)
                    .cmp(&y.first.unwrap_or(i64::MAX)),
                GroupSort::LastSeen => y.last.unwrap_or(i64::MIN).cmp(&x.last.unwrap_or(i64::MIN)),
                GroupSort::Name => std::cmp::Ordering::Equal,
            };
            primary.then_with(|| name(x).cmp(&name(y)))
        });
        self.all = order.clone();
        self.order = order;
    }

    /// Keep only values containing `needle` (any case), in the same order;
    /// an empty needle shows every group again. Searches every value.
    pub fn search(&mut self, needle: &str) {
        let n = needle.trim().to_lowercase();
        if n.is_empty() {
            self.order = self.all.clone();
            return;
        }
        let hit: std::collections::HashSet<u32> = self
            .stats
            .iter()
            .enumerate()
            .filter(|(_, (k, _))| k.first().is_some_and(|v| v.to_lowercase().contains(&n)))
            .map(|(i, _)| i as u32)
            .collect();
        self.order = self
            .all
            .iter()
            .copied()
            .filter(|i| hit.contains(i))
            .collect();
    }

    /// Groups that have a value (all of them but the "no value" group),
    /// counted by streaming through the table once.
    pub fn with_value(&self) -> u64 {
        let empty = self
            .stats
            .iter()
            .any(|(k, _)| k.first().is_none_or(|v| v.is_empty()));
        self.stats.len() as u64 - u64::from(empty)
    }

    pub fn len(&self) -> u64 {
        self.order.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Groups `start..start+len` in display order.
    pub fn page(&self, start: u64, len: usize) -> Vec<GroupRow> {
        let s = (start as usize).min(self.order.len());
        let e = s.saturating_add(len).min(self.order.len());
        self.order[s..e]
            .iter()
            .filter_map(|&i| self.stats.get(i as usize))
            .map(|(k, v)| self.row_of(k, &v))
            .collect()
    }
}

/// The order of a summary's groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SummarySort {
    #[default]
    MostRows,
    FewestRows,
    /// The measure, largest first.
    Value,
    /// The key columns, A to Z.
    Key,
}

/// One group of a summary.
#[derive(Clone, Debug, PartialEq)]
pub struct SummaryLine {
    pub key: Vec<String>,
    pub rows: u64,
    /// The measure (the row count when there is none).
    pub value: f64,
}

/// A summary's every group, sorted any way and read by page (no limits).
pub struct SummaryTable {
    stats: StatsResult,
    order: Vec<u32>,
    widths: Vec<usize>,
}

impl SummaryTable {
    pub fn new(stats: StatsResult) -> SummaryTable {
        let mut widths: Vec<usize> = Vec::new();
        for (k, _) in stats.iter() {
            if widths.len() < k.len() {
                widths.resize(k.len(), 0);
            }
            for (i, v) in k.iter().enumerate() {
                widths[i] = widths[i].max(v.chars().count());
            }
        }
        let mut t = SummaryTable {
            stats,
            order: Vec::new(),
            widths,
        };
        t.sort(SummarySort::MostRows);
        t
    }

    fn line(k: Vec<String>, v: &[f64]) -> SummaryLine {
        let rows = v.first().copied().unwrap_or(0.0) as u64;
        SummaryLine {
            key: k,
            rows,
            value: v.get(1).copied().unwrap_or(rows as f64),
        }
    }

    pub fn sort(&mut self, sort: SummarySort) {
        let lines: Vec<SummaryLine> = self.stats.iter().map(|(k, v)| Self::line(k, &v)).collect();
        let mut order: Vec<u32> = (0..lines.len() as u32).collect();
        order.sort_by(|&a, &b| {
            let (x, y) = (&lines[a as usize], &lines[b as usize]);
            let primary = match sort {
                SummarySort::MostRows => y.rows.cmp(&x.rows),
                SummarySort::FewestRows => x.rows.cmp(&y.rows),
                SummarySort::Value => y.value.total_cmp(&x.value),
                SummarySort::Key => std::cmp::Ordering::Equal,
            };
            primary.then_with(|| x.key.cmp(&y.key))
        });
        self.order = order;
    }

    pub fn len(&self) -> u64 {
        self.order.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The longest value of each key column, in characters.
    pub fn widths(&self) -> &[usize] {
        &self.widths
    }

    /// Groups `start..start+len` in the current order.
    pub fn page(&self, start: u64, len: usize) -> Vec<SummaryLine> {
        let s = (start as usize).min(self.order.len());
        let e = s.saturating_add(len).min(self.order.len());
        self.order[s..e]
            .iter()
            .filter_map(|&i| self.stats.get(i as usize))
            .map(|(k, v)| Self::line(k, &v))
            .collect()
    }
}

/// The search term that finds the rows of one group again: the rows where
/// `col` has `value`, or (`None`) the rows where it has no value at all.
pub fn narrow_term(col: &str, value: Option<&str>) -> String {
    use crate::formula::{column_ref, quote};
    match value {
        Some(v) => format!("{}={}", column_ref(col), quote(v)),
        // "equals nothing" finds no row: a missing value is not an empty text
        None => format!("NOT {}:\"*\"", column_ref(col)),
    }
}
