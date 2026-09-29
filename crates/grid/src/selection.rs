//! Cell, row and column selection in display coordinates.

use std::ops::Range;

/// A cell position: absolute row, displayed column index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPos {
    pub row: u64,
    pub col: usize,
}

impl CellPos {
    pub fn new(row: u64, col: usize) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// A rectangle of cells.
    Cells,
    /// Whole rows (from the row gutter).
    Rows,
    /// Whole columns (from headers).
    Columns,
}

/// A rectangular selection from `anchor` to `head` (the active cell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub head: CellPos,
    pub kind: SelectionKind,
}

impl Selection {
    pub fn cell(pos: CellPos) -> Self {
        Self {
            anchor: pos,
            head: pos,
            kind: SelectionKind::Cells,
        }
    }

    /// Rows covered, clipped to `row_count`.
    pub fn rows(&self, row_count: u64) -> Range<u64> {
        match self.kind {
            SelectionKind::Columns => 0..row_count,
            _ => {
                let start = self.anchor.row.min(self.head.row);
                let end = (self.anchor.row.max(self.head.row) + 1).min(row_count);
                start.min(end)..end
            }
        }
    }

    /// Displayed columns covered, clipped to `column_count`.
    pub fn columns(&self, column_count: usize) -> Range<usize> {
        match self.kind {
            SelectionKind::Rows => 0..column_count,
            _ => {
                let start = self.anchor.col.min(self.head.col);
                let end = (self.anchor.col.max(self.head.col) + 1).min(column_count);
                start.min(end)..end
            }
        }
    }

    pub fn contains(&self, row: u64, col: usize, row_count: u64, column_count: usize) -> bool {
        self.rows(row_count).contains(&row) && self.columns(column_count).contains(&col)
    }

    pub fn is_single_cell(&self) -> bool {
        self.kind == SelectionKind::Cells && self.anchor == self.head
    }

    pub fn cell_count(&self, row_count: u64, column_count: usize) -> u64 {
        let rows = self.rows(row_count);
        (rows.end - rows.start) * self.columns(column_count).len() as u64
    }

    /// Keep positions inside the table after it shrinks (new filter, hidden column).
    pub fn clamped(&self, row_count: u64, column_count: usize) -> Option<Self> {
        if row_count == 0 || column_count == 0 {
            return None;
        }
        let clamp = |p: CellPos| CellPos {
            row: p.row.min(row_count - 1),
            col: p.col.min(column_count - 1),
        };
        Some(Self {
            anchor: clamp(self.anchor),
            head: clamp(self.head),
            kind: self.kind,
        })
    }
}

/// A keyboard movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    RowStart,
    RowEnd,
    FirstRow,
    LastRow,
}

/// New head position after a movement. `page` is the number of rows per page.
pub fn move_head(head: CellPos, movement: Movement, row_count: u64, column_count: usize, page: u64) -> CellPos {
    if row_count == 0 || column_count == 0 {
        return head;
    }
    let last_row = row_count - 1;
    let last_col = column_count - 1;
    let head = CellPos {
        row: head.row.min(last_row),
        col: head.col.min(last_col),
    };
    match movement {
        Movement::Up => CellPos { row: head.row.saturating_sub(1), ..head },
        Movement::Down => CellPos { row: (head.row + 1).min(last_row), ..head },
        Movement::Left => CellPos { col: head.col.saturating_sub(1), ..head },
        Movement::Right => CellPos { col: (head.col + 1).min(last_col), ..head },
        Movement::PageUp => CellPos { row: head.row.saturating_sub(page.max(1)), ..head },
        Movement::PageDown => CellPos { row: (head.row + page.max(1)).min(last_row), ..head },
        Movement::RowStart => CellPos { col: 0, ..head },
        Movement::RowEnd => CellPos { col: last_col, ..head },
        Movement::FirstRow => CellPos { row: 0, ..head },
        Movement::LastRow => CellPos { row: last_row, ..head },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let s = Selection {
            anchor: CellPos::new(10, 3),
            head: CellPos::new(5, 1),
            kind: SelectionKind::Cells,
        };
        assert_eq!(s.rows(100), 5..11);
        assert_eq!(s.columns(10), 1..4);
        assert_eq!(s.cell_count(100, 10), 18);
        assert!(s.contains(7, 2, 100, 10));
        assert!(!s.contains(11, 2, 100, 10));
        let rows = Selection { kind: SelectionKind::Rows, ..s };
        assert_eq!(rows.columns(7), 0..7);
        let cols = Selection { kind: SelectionKind::Columns, ..s };
        assert_eq!(cols.rows(42), 0..42);
        // Shrunk table.
        assert_eq!(s.rows(8), 5..8);
        let clamped = s.clamped(3, 2).unwrap();
        assert_eq!(clamped.anchor, CellPos::new(2, 1));
        assert!(s.clamped(0, 5).is_none());
    }

    #[test]
    fn movement() {
        let h = CellPos::new(5, 2);
        assert_eq!(move_head(h, Movement::Up, 10, 4, 3), CellPos::new(4, 2));
        assert_eq!(move_head(CellPos::new(0, 0), Movement::Up, 10, 4, 3), CellPos::new(0, 0));
        assert_eq!(move_head(h, Movement::Right, 10, 3, 3), CellPos::new(5, 2));
        assert_eq!(move_head(h, Movement::PageDown, 10, 4, 3), CellPos::new(8, 2));
        assert_eq!(move_head(h, Movement::PageDown, 7, 4, 3), CellPos::new(6, 2));
        assert_eq!(move_head(h, Movement::LastRow, 10, 4, 3), CellPos::new(9, 2));
        assert_eq!(move_head(h, Movement::RowStart, 10, 4, 3), CellPos::new(5, 0));
        assert_eq!(move_head(h, Movement::RowEnd, 10, 4, 3), CellPos::new(5, 3));
        // Out-of-range heads are clamped first.
        assert_eq!(move_head(CellPos::new(50, 9), Movement::Up, 10, 4, 3), CellPos::new(8, 3));
        assert_eq!(move_head(h, Movement::Down, 0, 4, 3), h);
    }
}
