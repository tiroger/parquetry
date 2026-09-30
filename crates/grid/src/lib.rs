//! A GPU-painted data grid for very large tables, with marimo-style column summaries.

mod cache;
pub mod chart;
mod element;
mod grid;
pub mod layout;
mod selection;
mod state;

pub use cache::CellState;
pub use grid::*;
pub use selection::{CellPos, Movement, Selection, SelectionKind};
pub use state::{ColumnArrangement, GridColumn, GridEvent, GridState, Hit, MAX_COPY_ROWS, SummaryState};
