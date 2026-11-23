//! A Follow result as a tree in the grid: each starting row, with the rows
//! that link to it indented beneath, every level foldable.

use std::collections::{HashMap, HashSet};

/// A place in an order for each row, from the values of one column: numbers
/// by size, times by when they are, anything else as text. Equal values share
/// a place, and a row with no value comes last. `desc` turns the order round
/// (rows with no value still last).
pub fn rank_keys(values: Vec<(u64, Option<String>)>, desc: bool) -> HashMap<u64, i64> {
    #[derive(PartialEq, PartialOrd)]
    enum Key {
        Num(f64),
        Text(String),
    }
    let key = |v: &str| {
        let t = v.trim();
        if let Ok(n) = t.parse::<f64>() {
            Key::Num(n)
        } else if let Some(us) = logseek_core::time::parse_time(t) {
            Key::Num(us as f64)
        } else {
            Key::Text(t.to_lowercase())
        }
    };
    let mut have: Vec<(Key, u64)> = Vec::new();
    let mut out = HashMap::new();
    for (row, v) in values {
        match v.as_deref().filter(|v| !v.trim().is_empty()) {
            Some(v) => have.push((key(v), row)),
            None => {
                out.insert(row, i64::MAX);
            }
        }
    }
    have.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut place = 0i64;
    for i in 0..have.len() {
        if i > 0 && have[i].0 != have[i - 1].0 {
            place += 1;
        }
        out.insert(have[i].1, if desc { -place } else { place });
    }
    out
}

pub struct FollowTree {
    /// The Follow run this tree shows.
    pub id: u64,
    roots: Vec<u64>,
    kids: HashMap<u64, Vec<u64>>,
    closed: HashSet<u64>,
    /// The lines on screen, top to bottom: (file row, depth).
    lines: Vec<(u64, u16)>,
    pub max_depth: u16,
    /// Cells loaded for rows that have been on screen.
    cells: HashMap<u64, Vec<Option<String>>>,
    asked: HashSet<u64>,
    seen_reply: usize,
}

impl FollowTree {
    /// `levels`: per step, (row found, the row it links to). Rows are listed
    /// in the order of the file.
    #[cfg(test)]
    pub fn new(id: u64, start: &[u64], levels: &[Vec<(u64, u64)>]) -> FollowTree {
        FollowTree::new_timed(id, start, levels, &HashMap::new())
    }

    /// As `new`, with each row's time (µs) where it has one: the rows under
    /// one row, and the starting rows, are listed oldest first, so a tree
    /// reads in the order things happened. Rows at the same moment, or with
    /// no time, keep the order of the file (those with no time come last).
    pub fn new_timed(
        id: u64,
        start: &[u64],
        levels: &[Vec<(u64, u64)>],
        times: &HashMap<u64, i64>,
    ) -> FollowTree {
        let when = |row: &u64| (times.get(row).copied().unwrap_or(i64::MAX), *row);
        let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
        for l in levels {
            for &(row, parent) in l {
                kids.entry(parent).or_default().push(row);
            }
        }
        for v in kids.values_mut() {
            v.sort_unstable_by_key(when);
        }
        let mut t = FollowTree {
            id,
            roots: start.to_vec(),
            kids,
            closed: HashSet::new(),
            lines: Vec::new(),
            max_depth: 0,
            cells: HashMap::new(),
            asked: HashSet::new(),
            seen_reply: 0,
        };
        t.roots.sort_unstable_by_key(when);
        t.lay_out();
        t
    }

    /// Every row in the tree.
    pub fn all_rows(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.roots.clone();
        v.extend(self.kids.values().flatten().copied());
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Lists the rows under each row, and the starting rows, in the order the
    /// keys give (smallest first; equal keys and missing ones in file order).
    /// What was folded shut stays shut.
    pub fn reorder(&mut self, keys: &HashMap<u64, i64>) {
        let place = |row: &u64| (keys.get(row).copied().unwrap_or(i64::MAX), *row);
        for v in self.kids.values_mut() {
            v.sort_unstable_by_key(place);
        }
        self.roots.sort_unstable_by_key(place);
        self.lay_out();
    }

    fn lay_out(&mut self) {
        let mut lines = Vec::new();
        let mut max = 0u16;
        // depth-first, children oldest first (no recursion: trees can be deep)
        let mut stack: Vec<(u64, u16)> = self.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((row, d)) = stack.pop() {
            lines.push((row, d));
            max = max.max(d);
            if self.closed.contains(&row) {
                continue;
            }
            if let Some(k) = self.kids.get(&row) {
                stack.extend(k.iter().rev().map(|&c| (c, d.saturating_add(1))));
            }
        }
        self.lines = lines;
        self.max_depth = max;
    }

    pub fn len(&self) -> u64 {
        self.lines.len() as u64
    }

    /// (file row, depth) on line `i`.
    pub fn line(&self, i: u64) -> Option<(u64, u16)> {
        self.lines.get(i as usize).copied()
    }

    /// Rows linked beneath `row` (0 = a leaf).
    pub fn children(&self, row: u64) -> usize {
        self.kids.get(&row).map_or(0, Vec::len)
    }

    pub fn is_open(&self, row: u64) -> bool {
        !self.closed.contains(&row)
    }

    /// Fold or unfold what is beneath `row`.
    pub fn toggle(&mut self, row: u64) {
        if self.children(row) == 0 {
            return;
        }
        if !self.closed.remove(&row) {
            self.closed.insert(row);
        }
        self.lay_out();
    }

    /// The line of the row `row` links to, if it is on screen.
    pub fn parent_line(&self, i: u64) -> Option<u64> {
        let (_, d) = self.line(i)?;
        if d == 0 {
            return None;
        }
        (0..i).rev().find(|&k| self.lines[k as usize].1 < d)
    }

    pub fn cells(&self, row: u64) -> Option<&Vec<Option<String>>> {
        self.cells.get(&row)
    }

    /// Take in rows the engine sent, and ask for those on screen not yet loaded.
    pub fn sync(
        &mut self,
        snap: &crate::bridge::Snapshot,
        bridge: &crate::bridge::Bridge,
        top: u64,
        visible: u64,
    ) {
        if let Some(r) = snap.rows_by_id.as_ref().filter(|r| r.0 == self.id) {
            let stamp = std::sync::Arc::as_ptr(r) as usize;
            if stamp != self.seen_reply {
                self.seen_reply = stamp;
                for (row, cells) in r.1.rows.iter().zip(&r.1.cells) {
                    self.cells.insert(*row, cells.clone());
                }
            }
        }
        let end = (top + visible + 40).min(self.len());
        let want: Vec<u64> = (top.saturating_sub(40)..end)
            .filter_map(|i| self.line(i))
            .map(|(r, _)| r)
            .filter(|r| !self.cells.contains_key(r) && !self.asked.contains(r))
            .collect();
        if !want.is_empty() {
            self.asked.extend(want.iter().copied());
            bridge.send(crate::bridge::Cmd::RowsById {
                key: self.id,
                rows: want,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> FollowTree {
        // 10 -> 20, 40 ; 20 -> 30 ; second root 60 -> 70
        FollowTree::new(
            1,
            &[60, 10],
            &[vec![(20, 10), (40, 10), (70, 60)], vec![(30, 20)]],
        )
    }

    fn rows(t: &FollowTree) -> Vec<(u64, u16)> {
        (0..t.len()).filter_map(|i| t.line(i)).collect()
    }

    #[test]
    fn linked_rows_sit_under_the_row_they_link_to() {
        let t = tree();
        assert_eq!(
            rows(&t),
            vec![(10, 0), (20, 1), (30, 2), (40, 1), (60, 0), (70, 1)]
        );
        assert_eq!(t.max_depth, 2);
        assert_eq!(t.children(10), 2);
        assert_eq!(t.children(30), 0);
        assert_eq!(t.parent_line(2), Some(1));
        assert_eq!(t.parent_line(3), Some(0));
        assert_eq!(t.parent_line(0), None);
    }

    #[test]
    fn folding_a_row_hides_everything_beneath_it() {
        let mut t = tree();
        t.toggle(10);
        assert_eq!(rows(&t), vec![(10, 0), (60, 0), (70, 1)]);
        assert!(!t.is_open(10));
        t.toggle(10);
        assert_eq!(t.len(), 6);
        // a leaf does not fold
        t.toggle(30);
        assert_eq!(t.len(), 6);
    }

    #[test]
    fn a_very_deep_chain_lays_out_without_recursion() {
        let levels: Vec<Vec<(u64, u64)>> = (1..50_000u64).map(|i| vec![(i, i - 1)]).collect();
        let t = FollowTree::new(1, &[0], &levels);
        assert_eq!(t.len(), 50_000);
        assert_eq!(t.line(49_999), Some((49_999, 49_999)));
    }

    #[test]
    fn the_rows_under_one_row_are_listed_oldest_first() {
        // row 10 has three rows beneath it; the file holds them out of time order
        let times: HashMap<u64, i64> = [(10, 100), (20, 300), (40, 200), (50, 250)].into();
        let t = FollowTree::new_timed(1, &[10], &[vec![(20, 10), (40, 10), (50, 10)]], &times);
        assert_eq!(rows(&t), vec![(10, 0), (40, 1), (50, 1), (20, 1)]);
        // the same moment: the order of the file decides
        let same: HashMap<u64, i64> = [(10, 100), (20, 200), (40, 200)].into();
        let t = FollowTree::new_timed(1, &[10], &[vec![(40, 10), (20, 10)]], &same);
        assert_eq!(rows(&t), vec![(10, 0), (20, 1), (40, 1)]);
        // a row with no time comes after the ones that have one
        let some: HashMap<u64, i64> = [(10, 100), (40, 200)].into();
        let t = FollowTree::new_timed(1, &[10], &[vec![(20, 10), (40, 10)]], &some);
        assert_eq!(rows(&t), vec![(10, 0), (40, 1), (20, 1)]);
        // the starting rows too
        let roots: HashMap<u64, i64> = [(10, 500), (60, 100)].into();
        let t = FollowTree::new_timed(1, &[10, 60], &[], &roots);
        assert_eq!(rows(&t), vec![(60, 0), (10, 0)]);
    }

    #[test]
    fn a_tree_is_put_in_a_new_order_without_losing_what_was_folded() {
        // 10 -> 20, 40 ; 20 -> 30 ; second root 60 -> 70 ; row 20 folded shut
        let mut t = tree();
        t.toggle(20);
        assert_eq!(rows(&t), vec![(10, 0), (20, 1), (40, 1), (60, 0), (70, 1)]);
        // a new order: 40 before 20, and root 60 before root 10
        let keys: HashMap<u64, i64> = [(60, 1), (10, 2), (40, 1), (20, 2), (70, 0), (30, 0)].into();
        t.reorder(&keys);
        assert_eq!(rows(&t), vec![(60, 0), (70, 1), (10, 0), (40, 1), (20, 1)]);
        assert!(!t.is_open(20));
    }

    #[test]
    fn values_rank_as_numbers_times_or_text_whichever_they_are() {
        let v = |x: &[(u64, Option<&str>)]| -> Vec<(u64, Option<String>)> {
            x.iter().map(|(r, s)| (*r, s.map(str::to_string))).collect()
        };
        // numbers by size, not as text: 9 before 10
        let k = rank_keys(v(&[(1, Some("10")), (2, Some("9")), (3, Some("100"))]), false);
        assert!(k[&2] < k[&1] && k[&1] < k[&3]);
        // largest first turns it round
        let k = rank_keys(v(&[(1, Some("10")), (2, Some("9"))]), true);
        assert!(k[&1] < k[&2]);
        // times by when they are
        let k = rank_keys(
            v(&[(1, Some("2026-11-12 06:38:00.685")), (2, Some("2026-11-12 06:38:00.183"))]),
            false,
        );
        assert!(k[&2] < k[&1]);
        // text A to Z; equal values share a place; no value comes last
        let k = rank_keys(v(&[(1, Some("pear")), (2, Some("apple")), (3, Some("apple")), (4, None)]), false);
        assert!(k[&2] == k[&3] && k[&2] < k[&1] && k[&1] < k[&4]);
    }
}
