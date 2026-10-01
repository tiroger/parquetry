# Parquetry

A fast viewer for Parquet, CSV, JSON, Arrow, Excel, Delta Lake and Iceberg,
local or on S3. Every column header shows its distribution, nulls and range, like
marimo's table preview. Files of any size open instantly.

Built in Rust with [GPUI](https://gpui-kit.com) and [DuckDB](https://duckdb.org).

## Install

**macOS** (13+, Apple Silicon or Intel). The app updates itself.

```sh
brew install --cask tiroger/tap/parquetry
```

**Windows** (10/11, x64) with [Scoop](https://scoop.sh). Update with `scoop update parquetry`.

```powershell
scoop bucket add tiroger https://github.com/tiroger/scoop-bucket
scoop install parquetry
```

Or download from [Releases](https://github.com/tiroger/parquetry/releases/latest).

```sh
parquetry data.parquet              # file
parquetry exports/                  # folder (Hive partitions become columns)
parquetry 'logs/2024-*/*.parquet'   # glob
parquetry s3://bucket/path/         # S3
```

## Features

- **Any size, no lag.** Only visible rows are read, on background threads. A
  600M-row file scrolls like a small one.
- **Column summaries** in every header: histogram or top values, nulls, min/max,
  distinct count. Exact up to 100M rows, sampled beyond. Click a bar to filter to
  it; click again to drill down.
- **Value counts**: every distinct value with its count, searchable; keep or
  exclude the ones you pick.
- **Sort, filter, search**: typed filters or SQL `WHERE`; copy cells as
  TSV/CSV/JSON/Markdown.
- **Columns, Metadata and SQL tabs**: per-column stats, Parquet internals (row
  groups, encodings, compression), and DuckDB SQL across open files.
- **Go to column** (⌘P) for wide tables, and **reopen where you left off**:
  windows, tabs, filters, sort and column layout come back at launch.
- **Export** the current view to Parquet, CSV, TSV or JSON.
- **Compare** two datasets: schema, missing rows, changed values.
- **S3**: bucket browser, AWS profiles and SSO, MinIO/R2 endpoints.
- **Quick Look** (macOS): press Space on a `.parquet` file in Finder.

## Performance

19 GB, 600M-row Parquet file on an M5 Max:

| | |
|---|---|
| Open | < 1 s |
| Jump to last row | < 1 s |
| Sort all rows | 10–14 s (cancellable; UI stays responsive) |
| Jump deep into sorted view | ~0.5 s |
| Filter 50M rows | 49 ms |

## Development

```sh
cargo run -- path/to/file.parquet
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/bundle.sh                   # release .app in target/dist
```

| Crate | |
|---|---|
| `crates/engine` | DuckDB data engine: sources, views, stats, SQL, export, S3. No UI. |
| `crates/grid` | GPU data grid and header charts. |
| `crates/app` | The application. |
| `crates/quicklook` | Quick Look preview (macOS). |

Releases, signing and the Homebrew tap: [packaging/README.md](packaging/README.md).

## License

MIT
