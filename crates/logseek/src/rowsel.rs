//! Which rows are selected, like a spreadsheet: click one, Shift+click for
//! a range, Ctrl+click to add or take away one, select all. Kept as ranges
//! of places in the list, so selecting a hundred million rows costs nothing.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowSel {
    /// Inclusive ranges, sorted, never touching or overlapping.
    ranges: Vec<(u64, u64)>,
    /// Where a Shift+click range starts from.
    anchor: Option<u64>,
}

impl RowSel {
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.anchor = None;
    }

    fn tidy(&mut self) {
        self.ranges.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(self.ranges.len());
        for &(a, b) in &self.ranges {
            match out.last_mut() {
                Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        self.ranges = out;
    }

    /// A plain click: only this row.
    pub fn click(&mut self, p: u64) {
        self.ranges = vec![(p, p)];
        self.anchor = Some(p);
    }

    /// Shift+click: every row from where the selection started to here.
    pub fn shift_click(&mut self, p: u64) {
        let a = self.anchor.unwrap_or(p);
        self.ranges = vec![(a.min(p), a.max(p))];
        self.anchor = Some(a);
    }

    /// Ctrl+click: add this row, or take it away if it was selected.
    pub fn ctrl_click(&mut self, p: u64) {
        if let Some(i) = self.ranges.iter().position(|&(a, b)| a <= p && p <= b) {
            let (a, b) = self.ranges.remove(i);
            if a < p {
                self.ranges.push((a, p - 1));
            }
            if p < b {
                self.ranges.push((p + 1, b));
            }
        } else {
            self.ranges.push((p, p));
        }
        self.anchor = Some(p);
        self.tidy();
    }

    /// Every row of a list of `total` rows.
    pub fn all(&mut self, total: u64) {
        self.ranges = if total == 0 {
            Vec::new()
        } else {
            vec![(0, total - 1)]
        };
        self.anchor = Some(0);
    }

    pub fn contains(&self, p: u64) -> bool {
        let i = self.ranges.partition_point(|&(_, b)| b < p);
        self.ranges.get(i).is_some_and(|&(a, _)| a <= p)
    }

    pub fn count(&self) -> u64 {
        self.ranges.iter().map(|&(a, b)| b - a + 1).sum()
    }

    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_shift_click_and_ctrl_click_work_like_a_spreadsheet() {
        let mut s = RowSel::default();
        s.click(5);
        assert_eq!(s.ranges(), [(5, 5)]);
        // Shift+click below, then above: always from the first click
        s.shift_click(9);
        assert_eq!(s.ranges(), [(5, 9)]);
        s.shift_click(2);
        assert_eq!(s.ranges(), [(2, 5)]);
        // Ctrl+click adds a separate row, and a neighbour joins the range
        s.ctrl_click(20);
        s.ctrl_click(6);
        assert_eq!(s.ranges(), [(2, 6), (20, 20)]);
        assert_eq!(s.count(), 6);
        // Ctrl+click on a selected row takes it out of the middle
        s.ctrl_click(4);
        assert_eq!(s.ranges(), [(2, 3), (5, 6), (20, 20)]);
        assert!(s.contains(3) && !s.contains(4) && s.contains(20) && !s.contains(21));
        // a plain click starts over
        s.click(1);
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn select_all_is_one_range_however_many_rows() {
        let mut s = RowSel::default();
        s.all(100_000_000_000);
        assert_eq!(s.count(), 100_000_000_000);
        assert!(s.contains(99_999_999_999));
        s.all(0);
        assert_eq!(s.count(), 0);
    }
}
