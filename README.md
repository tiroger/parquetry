# Parquetry

A fast macOS viewer for Parquet — and CSV, JSON, Arrow/Feather, Excel, Delta Lake
and Iceberg — on disk or in S3. Every column header shows its distribution
(histogram or top values), null share and range, in the spirit of marimo's table
preview. Files of any size open instantly: only the rows on screen are read.

Built in Rust with [GPUI](https://gpui-kit.com) (Zed's GPU UI framework) and
[DuckDB](https://duckdb.org).

## Install

```sh
brew install --cask tiroger/tap/parquetry
```

This installs `Parquetry.app` and a `parquetry` command:

```sh
parquetry data.parquet                  # a file
parquetry exports/                      # a folder (Hive partitions become columns)
parquetry 'logs/2024-*/*.parquet'       # a glob
parquetry s3://bucket/path/             # S3 prefix or object
parquetry https://example.com/x.parquet
```

The app updates itself: it checks for new releases daily (*Parquetry ▸ Check for
Updates…* checks now; Settings turns the daily check off).

Maintainers: see [packaging/README.md](packaging/README.md) for signing,
notarization, the Homebrew tap and the release workflow.

## Features

- **Instant opening, smooth scrolling at any size.** The grid is a single
  GPU-painted element that shapes only visible cells. Rows are fetched in blocks
  around the viewport on background threads; nothing on the UI thread waits for
  I/O. Positions are tracked in rows (not pixels), so a 600-million-row file
  scrolls as precisely as a small one.
- **Column summaries in the header**: type, null share, histogram (numbers and
  dates) or top values (categories), min/max, distinct count. Hover a chart for
  details and the value of the bar under the pointer. Local datasets are summarized
  exactly up to 100M rows; larger and remote ones use an even sample (marked with a
  dot), and footer statistics keep null counts and min/max exact. *View ▸ Compute
  Exact Summaries* scans everything.
- **Sort, filter, search.** Click a header for sort (including secondary sort),
  filters, pin/hide/fit. Right-click cells to filter by a value or copy as TSV, CSV,
  JSON, Markdown or a SQL `IN` list. Free-text search across every column, typed
  filters with friendly validation, and free-form SQL `WHERE` clauses.
- **Columns tab**: every column with its chart, nulls, distinct, min, max, mean.
- **Metadata tab**: file facts plus Parquet internals — per-column storage
  (compressed/uncompressed size, codec, encodings, share of the file), row groups,
  column chunks with statistics, the Parquet schema, key/value metadata.
- **SQL tab and SQL console**: DuckDB SQL with highlighting and history. The file is
  `t`; every open dataset is available by name. Results can be opened as a tab to
  filter, summarize and export them.
- **Inspector**: the full value of the selected cell (nested values pretty-printed
  as JSON) and a detailed column summary.
- **Export** the current view (filters and order applied) to Parquet, CSV, TSV,
  JSON Lines or JSON, all columns or the visible ones.
- **Compare** two datasets: schema changes, rows only on one side, and changed
  values per column when matched on key columns.
- **S3**: browse buckets and prefixes, AWS profiles and SSO, per-bucket regions
  detected automatically, anonymous access for public data, custom endpoints
  (MinIO, R2).
- **Quick Look**: press Space on a `.parquet` file in Finder.
- Tabs, drag and drop, recents, light/dark/system appearance, interface zoom,
  keyboard navigation throughout (⌘/ lists the shortcuts).

## Performance (M5 Max, 19 GB / 600M-row Parquet file)

| | |
|---|---|
| Open and show first rows | < 1 s |
| Jump to the last row | < 1 s |
| Sort all 600M rows by a column | 10–14 s (UI stays responsive, cancellable) |
| Jump to row 300,000,000 of the sorted view | ~0.5 s |
| Filter 50M rows (`amount > 9990`) | 49 ms |
| Search every column of 50M rows | 1.9 s |

## Development

```sh
curl https://sh.rustup.rs -sSf | sh     # Rust
cargo run -- path/to/file.parquet       # debug build
cargo test --workspace                  # engine, grid, app UI tests, Quick Look
cargo clippy --workspace --all-targets -- -D warnings
scripts/bundle.sh                       # release .app in target/dist
```

GPUI compiles its Metal shaders at runtime (`runtime_shaders`), so only the Xcode
Command Line Tools are required. Network tests against public S3 data:
`cargo test -p parquetry-engine --test s3_public -- --ignored`.

### Layout

| Crate | |
|---|---|
| `crates/engine` | DuckDB-backed data engine, no UI: sources and format detection, views (filter/sort/search) with fast random row access, column statistics, Parquet metadata, SQL, export, comparison, S3. Worker threads in three priority lanes; every request is a cancellable `Job`. |
| `crates/grid` | The GPU data grid: geometry, block cache, selection, painting, header charts, keyboard. |
| `crates/app` | The application: workspace and tabs, documents, SQL, dialogs, settings, menus. |
| `crates/quicklook` + `quicklook/` | Quick Look preview (Rust library + Swift extension). |

### How large files stay fast

- *Unsorted*: `LIMIT/OFFSET` on the Parquet scan; DuckDB skips row groups from
  footer counts, so any page costs tens of milliseconds.
- *Filtered/sorted*: the matching rows' positions (file, row number) are written
  once, in order, to a narrow index table — reading only the filter and sort
  columns. A page then reads exactly those rows with `file_row_number IN (…)`
  filters pushed into the scan. Small results are copied into memory in the
  background so scrolling them is instant; scattered (sorted) views fetch in
  smaller blocks.
- *Statistics* read one local copy of an even sample for large or remote data.
