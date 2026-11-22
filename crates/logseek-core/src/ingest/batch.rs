//! A compact batch of text rows: one byte buffer + field end offsets,
//! instead of one allocation per cell.

/// Rows per batch upper bound (matches segment size).
pub const MAX_ROWS: usize = 65_536;
/// Field-data bytes per batch upper bound.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

const NULL: u32 = u32::MAX;
const NO_SLOT: u32 = u32::MAX;

#[derive(Debug, Clone, Default)]
pub struct RecordBatch {
    ncols: usize,
    data: Vec<u8>,
    /// Per row, `ncols` entries of `(start, end)` into `data`; `NULL` start = missing/empty.
    cells: Vec<(u32, u32)>,
    offsets: Vec<u64>,
    end_offset: u64,
    overflow: Vec<(u32, Box<str>)>,
    /// Sparse mode (JSON): cells are `(col, start, end)` triples per row.
    sparse: bool,
    row_start: Vec<u32>,
    sp: Vec<(u32, u32, u32)>,
    /// Sparse mode: per column, where its cell last went in `sp` (to find a
    /// repeat in the same row without scanning the row).
    col_slot: Vec<u32>,
}

impl RecordBatch {
    pub fn new(ncols: usize) -> Self {
        RecordBatch {
            ncols,
            ..Default::default()
        }
    }
    /// A batch whose rows hold only the columns they have (JSON with 100s of keys).
    pub fn new_sparse(ncols: usize) -> Self {
        RecordBatch {
            ncols,
            sparse: true,
            ..Default::default()
        }
    }
    pub fn is_sparse(&self) -> bool {
        self.sparse
    }
    /// Sparse batches only: make sure at least `n` columns exist.
    pub fn ensure_cols(&mut self, n: usize) {
        if self.sparse && n > self.ncols {
            self.ncols = n;
        } else {
            while n > self.ncols {
                self.add_column();
            }
        }
    }
    pub fn ncols(&self) -> usize {
        self.ncols
    }
    pub fn rows(&self) -> usize {
        self.offsets.len()
    }
    pub fn data_bytes(&self) -> usize {
        self.data.len()
    }
    pub fn is_full(&self) -> bool {
        self.rows() >= MAX_ROWS || self.data.len() >= MAX_BYTES
    }
    /// Approximate heap size, for budget accounting.
    pub fn heap_bytes(&self) -> usize {
        self.data.capacity()
            + self.cells.capacity() * 8
            + self.offsets.capacity() * 8
            + self.sp.capacity() * 12
            + self.row_start.capacity() * 4
            + self.col_slot.capacity() * 4
            + self
                .overflow
                .iter()
                .map(|(_, s)| s.len() + 24)
                .sum::<usize>()
    }

    /// Start a row at source byte `offset`; all cells begin as missing.
    pub fn begin_row(&mut self, offset: u64) {
        self.offsets.push(offset);
        if self.sparse {
            self.row_start.push(self.sp.len() as u32);
            return;
        }
        self.cells
            .extend(std::iter::repeat((NULL, 0)).take(self.ncols));
    }

    /// Widen the batch when a new column appears (JSON schema drift).
    pub fn add_column(&mut self) -> usize {
        let old = self.ncols;
        self.ncols += 1;
        if self.sparse {
            return old;
        }
        let rows = self.rows();
        let mut cells = Vec::with_capacity(rows * self.ncols);
        for r in 0..rows {
            cells.extend_from_slice(&self.cells[r * old..(r + 1) * old]);
            cells.push((NULL, 0));
        }
        self.cells = cells;
        old
    }

    /// Set cell `col` of the current (last) row. Empty strings are stored as missing.
    ///
    /// A row holds at most one cell per column: setting a column again in the
    /// same row (a JSON key repeated, or two keys that flatten to one dotted
    /// name) replaces the earlier value, so the last one wins.
    pub fn set(&mut self, col: usize, value: &str) {
        if value.is_empty() {
            return;
        }
        let r = self.rows() - 1;
        let start = self.data.len() as u32;
        self.data.extend_from_slice(value.as_bytes());
        let end = self.data.len() as u32;
        if self.sparse {
            if col >= self.ncols {
                self.ncols = col + 1;
            }
            if col >= self.col_slot.len() {
                self.col_slot.resize(col + 1, NO_SLOT);
            }
            let row_lo = self.row_start.last().map_or(0, |&x| x as usize);
            let slot = self.col_slot[col] as usize;
            // the slot counts only if it is still this row's cell for `col`
            // (an earlier row's, or one rolled back, does not)
            if self.col_slot[col] != NO_SLOT
                && slot >= row_lo
                && self.sp.get(slot).is_some_and(|c| c.0 as usize == col)
            {
                self.sp[slot] = (col as u32, start, end);
            } else {
                self.col_slot[col] = self.sp.len() as u32;
                self.sp.push((col as u32, start, end));
            }
            return;
        }
        self.cells[r * self.ncols + col] = (start, end);
    }

    /// Remember the current fill level so a half-written row can be undone.
    pub fn mark(&self) -> (usize, usize) {
        (self.data.len(), self.sp.len())
    }

    /// Drop every cell written to the current row since `mark` (row stays, empty).
    pub fn rollback_row_cells(&mut self, mark: (usize, usize)) {
        self.data.truncate(mark.0);
        if self.sparse {
            self.sp.truncate(mark.1);
        } else if let Some(r) = self.rows().checked_sub(1) {
            for c in &mut self.cells[r * self.ncols..(r + 1) * self.ncols] {
                *c = (NULL, 0);
            }
        }
    }

    pub fn set_overflow(&mut self, value: String) {
        let r = (self.rows() - 1) as u32;
        self.overflow.push((r, value.into_boxed_str()));
    }

    pub fn set_end_offset(&mut self, end: u64) {
        self.end_offset = end;
    }

    /// Every non-missing `(col, text)` of a row, in one pass (cheap for sparse rows).
    pub fn row_cells(&self, row: usize) -> Vec<(usize, &str)> {
        let txt = |a: u32, b: u32| {
            // SAFETY: only valid UTF-8 (&str) is ever written into `data`.
            unsafe { std::str::from_utf8_unchecked(&self.data[a as usize..b as usize]) }
        };
        if self.sparse {
            let lo = self.row_start[row] as usize;
            let hi = self
                .row_start
                .get(row + 1)
                .map_or(self.sp.len(), |&x| x as usize);
            self.sp[lo..hi]
                .iter()
                .map(|&(c, a, b)| (c as usize, txt(a, b)))
                .collect()
        } else {
            self.cells[row * self.ncols..(row + 1) * self.ncols]
                .iter()
                .enumerate()
                .filter(|(_, (a, _))| *a != NULL)
                .map(|(c, &(a, b))| (c, txt(a, b)))
                .collect()
        }
    }

    pub fn field(&self, row: usize, col: usize) -> Option<&str> {
        if col >= self.ncols {
            return None;
        }
        let (a, b) = if self.sparse {
            let lo = self.row_start[row] as usize;
            let hi = self
                .row_start
                .get(row + 1)
                .map_or(self.sp.len(), |&x| x as usize);
            match self.sp[lo..hi].iter().find(|c| c.0 as usize == col) {
                Some(&(_, a, b)) => (a, b),
                None => return None,
            }
        } else {
            self.cells[row * self.ncols + col]
        };
        if a == NULL {
            return None;
        }
        // SAFETY: only valid UTF-8 (&str) is ever written into `data`.
        Some(unsafe { std::str::from_utf8_unchecked(&self.data[a as usize..b as usize]) })
    }

    pub fn overflow(&self, row: usize) -> Option<&str> {
        self.overflow
            .binary_search_by_key(&(row as u32), |(r, _)| *r)
            .ok()
            .map(|i| &*self.overflow[i].1)
    }

    pub fn has_overflow(&self) -> bool {
        !self.overflow.is_empty()
    }

    pub fn offset(&self, row: usize) -> u64 {
        self.offsets[row]
    }
    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}
