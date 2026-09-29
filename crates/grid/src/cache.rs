//! Cache of fetched cell blocks, keyed by row block and column block.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use parquetry_engine::Page;

/// Rows per fetched block for views read in order.
pub const ROW_BLOCK: u64 = 256;
/// Rows per block when rows are scattered through the source (sorted views of
/// Parquet): each row costs a vector decode, so smaller blocks arrive sooner.
pub const SCATTERED_ROW_BLOCK: u64 = 64;
/// Columns per fetched block.
pub const COL_BLOCK: usize = 24;
/// Blocks kept before evicting the least recently used ones.
const CAPACITY: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockKey {
    pub row_block: u64,
    pub col_block: usize,
    /// Rows per block (the same for every key of one cache).
    pub block_rows: u64,
}

impl BlockKey {
    pub fn rows(&self) -> Range<u64> {
        self.row_block * self.block_rows..(self.row_block + 1) * self.block_rows
    }

    pub fn columns(&self) -> Range<usize> {
        self.col_block * COL_BLOCK..(self.col_block + 1) * COL_BLOCK
    }
}

/// What the grid knows about a cell.
#[derive(Debug, Clone, PartialEq)]
pub enum CellState<'a> {
    Loaded(Option<&'a Arc<str>>),
    Loading,
    Failed,
}

pub struct PageCache {
    block_rows: u64,
    blocks: HashMap<BlockKey, Arc<Page>>,
    failed: HashSet<BlockKey>,
    /// Access clock for LRU eviction.
    used: HashMap<BlockKey, u64>,
    clock: u64,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            block_rows: ROW_BLOCK,
            blocks: HashMap::new(),
            failed: HashSet::new(),
            used: HashMap::new(),
            clock: 0,
        }
    }
}

impl PageCache {
    pub fn block_rows(&self) -> u64 {
        self.block_rows
    }

    /// Change the block size (clears the cache).
    pub fn set_block_rows(&mut self, block_rows: u64) {
        self.clear();
        self.block_rows = block_rows.max(1);
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.failed.clear();
        self.used.clear();
    }

    pub fn contains(&self, key: &BlockKey) -> bool {
        self.blocks.contains_key(key)
    }

    pub fn has_failed(&self, key: &BlockKey) -> bool {
        self.failed.contains(key)
    }

    pub fn insert(&mut self, key: BlockKey, page: Arc<Page>) {
        self.failed.remove(&key);
        self.blocks.insert(key, page);
        self.touch(key);
        self.evict();
    }

    pub fn mark_failed(&mut self, key: BlockKey) {
        self.failed.insert(key);
    }

    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    pub fn touch(&mut self, key: BlockKey) {
        self.clock += 1;
        self.used.insert(key, self.clock);
    }


    pub fn cell(&self, row: u64, column: usize) -> CellState<'_> {
        let key = key_for(row, column, self.block_rows);
        match self.blocks.get(&key) {
            Some(page) => match page.cell(row, column) {
                Some(value) => CellState::Loaded(value.as_ref()),
                // Past the end of a short final block: nothing there.
                None => CellState::Loaded(None),
            },
            None if self.failed.contains(&key) => CellState::Failed,
            None => CellState::Loading,
        }
    }

    fn evict(&mut self) {
        if self.blocks.len() <= CAPACITY {
            return;
        }
        let mut by_age: Vec<(u64, BlockKey)> = self.used.iter().map(|(k, t)| (*t, *k)).collect();
        by_age.sort_unstable();
        let excess = self.blocks.len() - CAPACITY;
        for (_, key) in by_age.into_iter().take(excess) {
            self.blocks.remove(&key);
            self.used.remove(&key);
        }
    }
}

pub fn key_for(row: u64, column: usize, block_rows: u64) -> BlockKey {
    BlockKey {
        row_block: row / block_rows,
        col_block: column / COL_BLOCK,
        block_rows,
    }
}

/// Blocks covering `rows` × `columns`, nearest to `focus_row` first.
pub fn blocks_for(rows: Range<u64>, columns: Range<usize>, focus_row: u64, block_rows: u64) -> Vec<BlockKey> {
    if rows.is_empty() || columns.is_empty() {
        return Vec::new();
    }
    let first_row_block = rows.start / block_rows;
    let last_row_block = (rows.end - 1) / block_rows;
    let first_col_block = columns.start / COL_BLOCK;
    let last_col_block = (columns.end - 1) / COL_BLOCK;
    let focus_block = focus_row / block_rows;
    let mut keys = Vec::new();
    for row_block in first_row_block..=last_row_block {
        for col_block in first_col_block..=last_col_block {
            keys.push(BlockKey {
                row_block,
                col_block,
                block_rows,
            });
        }
    }
    keys.sort_by_key(|k| (k.row_block.abs_diff(focus_block), k.col_block));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_blocks() {
        assert_eq!(key_for(0, 0, 256), BlockKey { row_block: 0, col_block: 0, block_rows: 256 });
        assert_eq!(key_for(256, 24, 256), BlockKey { row_block: 1, col_block: 1, block_rows: 256 });
        assert_eq!(key_for(130, 0, 64).rows(), 128..192);
        let keys = blocks_for(250..600, 0..30, 520, 256);
        assert_eq!(keys.len(), 6);
        // Nearest row block to the focus comes first.
        assert_eq!(keys[0].row_block, 2);
        assert!(blocks_for(5..5, 0..3, 0, 256).is_empty());
        assert_eq!(blocks_for(0..130, 0..3, 0, 64).len(), 3);
    }

    #[test]
    fn missing_cells_are_loading() {
        let cache = PageCache::default();
        assert_eq!(cache.cell(10, 3), CellState::Loading);
    }
}
