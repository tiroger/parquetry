//! Open in marimo: write the current view as a marimo notebook and open it with uv.
//!
//! The notebook carries its dependencies in a PEP 723 header, so
//! `uv tool run marimo edit --sandbox` builds an isolated environment for it: no
//! Python setup beyond uv. Notebooks are kept (in the notebooks folder) so they can
//! be reopened and edited later.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow};

/// The notebook text: a header cell, the data cell (`code`, defining `df`) and a
/// cell showing `df`.
pub fn marimo_notebook(title: &str, summary: &str, code: &str, packages: &[&str]) -> String {
    let mut deps = vec!["marimo"];
    deps.extend(packages.iter().copied().filter(|p| *p != "marimo"));
    let deps: String = deps.iter().map(|d| format!("#     \"{d}\",\n")).collect();
    let names = defined_names(code);
    let returns = match names.len() {
        0 => "return".to_string(),
        1 => format!("return ({},)", names[0]),
        _ => format!("return ({})", names.join(", ")),
    };
    let body: String = code.lines().map(|l| if l.is_empty() { "\n".to_string() } else { format!("    {l}\n") }).collect();
    let heading = format!("# {title}\n\n{summary}\n\nOpened from Parquetry. `df` holds the rows shown there.");
    format!(
        r#"# /// script
# requires-python = ">=3.10"
# dependencies = [
{deps}# ]
# ///

import marimo

app = marimo.App(width="full")


@app.cell
def _():
    import marimo as mo
    return (mo,)


@app.cell
def _(mo):
    mo.md({heading})
    return


@app.cell
def _():
{body}    {returns}


@app.cell
def _(df):
    df
    return


if __name__ == "__main__":
    app.run()
"#,
        heading = serde_json::to_string(&heading).unwrap_or_default(),
    )
}

/// Top-level names the generated code defines (imports and assignments), in a
/// stable order, for the marimo cell's return.
fn defined_names(code: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    for line in code.lines() {
        if line.starts_with(' ') || line.starts_with('#') || line.starts_with(')') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("import ") {
            match rest.split_once(" as ") {
                Some((_, alias)) => add(alias),
                None => add(rest.split('.').next().unwrap_or(rest)),
            }
        } else if let Some(rest) = line.strip_prefix("from ") {
            if let Some((_, imported)) = rest.split_once(" import ") {
                imported.split(',').for_each(&mut add);
            }
        } else if let Some((target, _)) = line.split_once(" = ") {
            add(target);
        }
    }
    names.sort();
    names
}

/// A file name for a new notebook: `<name>-<date>-<time>.py`.
pub fn notebook_file_name(dataset_name: &str) -> String {
    let stem = Path::new(dataset_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dataset_name.to_string());
    let clean: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let clean = clean.trim_matches('_');
    let clean = if clean.is_empty() { "data" } else { clean };
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let stamp = parquetry_engine::format_epoch_micros(secs as f64 * 1e6, false).replace([':', ' '], "").replace('-', "");
    format!("{clean}-{stamp}.py")
}

/// The default folder for notebooks: `~/Documents/Parquetry Notebooks`.
pub fn default_notebooks_dir() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("Parquetry Notebooks")
}

/// Find `uv`. Apps opened from the Dock or Start menu don't get the shell's PATH,
/// so also look where uv's installers put it.
pub fn find_uv() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "uv.exe" } else { "uv" };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".cargo").join("bin"));
    }
    if cfg!(windows) {
        if let Some(local) = dirs::data_local_dir() {
            dirs.push(local.join("Programs").join("uv"));
        }
    } else {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

/// Write `notebook` to `dir/file_name` and open it in marimo (in the browser).
/// marimo keeps running after Parquetry quits; its page has a shutdown button.
pub fn open_in_marimo(dir: &Path, file_name: &str, notebook: &str, log_dir: &Path) -> Result<PathBuf> {
    let uv = find_uv().ok_or_else(|| anyhow!("uv isn’t installed. Install it from https://docs.astral.sh/uv/ and try again."))?;
    std::fs::create_dir_all(dir).with_context(|| format!("couldn’t create {}", dir.display()))?;
    let path = dir.join(file_name);
    std::fs::write(&path, notebook).with_context(|| format!("couldn’t write {}", path.display()))?;
    std::fs::create_dir_all(log_dir).ok();
    let log = std::fs::File::create(log_dir.join("marimo.log")).ok();
    // marimo's --sandbox calls uv itself, so put uv's folder on its PATH.
    let mut path_var = uv.parent().map(|p| p.as_os_str().to_owned()).unwrap_or_default();
    if let Some(existing) = std::env::var_os("PATH") {
        path_var.push(if cfg!(windows) { ";" } else { ":" });
        path_var.push(existing);
    }
    let mut command = Command::new(&uv);
    command
        .args(["tool", "run", "marimo", "edit", "--sandbox"])
        .arg(&path)
        .current_dir(dir)
        .env("PATH", path_var)
        .stdin(Stdio::null());
    match log.and_then(|f| Some((f.try_clone().ok()?, f))) {
        Some((out, err)) => command.stdout(out).stderr(err),
        None => command.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().with_context(|| format!("couldn’t start {}", uv.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLARS: &str = "from datetime import date, datetime\nimport polars as pl\n\ndf = (\n    pl.scan_parquet(\"/d/x.parquet\")\n    .collect()\n)\n";

    #[test]
    fn notebook_shape() {
        assert_eq!(defined_names(POLARS), ["date", "datetime", "df", "pl"]);
        assert_eq!(
            defined_names("import duckdb\n\ncon = duckdb.connect()\nrel = con.sql(q)\ndf = rel.limit(5).pl()  # x\n"),
            ["con", "df", "duckdb", "rel"]
        );
        let nb = marimo_notebook("x.parquet", "10 rows", POLARS, &["polars"]);
        assert!(nb.starts_with("# /// script\n"));
        assert!(nb.contains("#     \"marimo\",\n#     \"polars\",\n"));
        assert!(nb.contains("    return (date, datetime, df, pl)\n"));
        assert!(nb.contains("    df = (\n        pl.scan_parquet"));
        let name = notebook_file_name("sales 2024.parquet");
        assert!(name.starts_with("sales_2024-") && name.ends_with(".py"), "{name}");
        assert!(!name.contains(' ') && !name.contains(':'));
    }

    /// The generated notebook runs: `uv run` installs its header's dependencies and
    /// executes every cell. Skipped without uv (required in CI).
    #[test]
    fn notebooks_run() {
        if std::process::Command::new("uv").arg("--version").output().is_err() {
            assert!(std::env::var("PARQUETRY_REQUIRE_PYTHON_TESTS").is_err(), "uv is required");
            eprintln!("skipping: uv not installed");
            return;
        }
        use parquetry_engine::{
            CodeFlavor, CodeOptions, Dataset, Engine, EnginePaths, EngineSettings, Filter, FilterOp, SortKey, SourceSpec,
            ViewSpec, python_packages, view_code,
        };
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sales data.parquet");
        duckdb::Connection::open_in_memory()
            .unwrap()
            .execute_batch(&format!(
                "COPY (SELECT i AS id, ['eu','us'][1 + i % 2] AS region, DATE '2024-01-01' + CAST(i % 30 AS INTEGER) AS day FROM range(1000) t(i)) TO '{}' (FORMAT parquet)",
                file.display()
            ))
            .unwrap();
        let engine = Engine::new(EnginePaths::in_dir(dir.path()), EngineSettings::default()).unwrap();
        let dataset = Dataset::open(&engine, SourceSpec::new(file.to_string_lossy())).wait().unwrap();
        let spec = ViewSpec {
            filters: vec![Filter::new("region", FilterOp::Equals, "eu"), Filter::new("day", FilterOp::GreaterOrEqual, "2024-01-10")],
            sort: vec![SortKey::desc("id")],
            ..Default::default()
        };
        let rows = parquetry_engine::View::build(&dataset, spec.clone()).wait().unwrap().row_count;
        // Each library, eagerly and lazily (big views), ends with `df` of the right size.
        for (flavor, preview) in [(CodeFlavor::Polars, None), (CodeFlavor::Polars, Some(10)), (CodeFlavor::Pandas, None), (CodeFlavor::Pandas, Some(10))] {
            let options = CodeOptions { preview_rows: preview, ..Default::default() };
            let code = view_code(&dataset, &spec, flavor, &options).unwrap();
            let check = match preview {
                Some(n) => format!("assert len(df) == {n}, len(df)\n"),
                None => format!("assert len(df) == {rows}, len(df)\n"),
            };
            let notebook = marimo_notebook("sales data.parquet", "rows", &format!("{code}{check}"), &python_packages(&code));
            let path = dir.path().join(notebook_file_name("sales data.parquet"));
            std::fs::write(&path, &notebook).unwrap();
            let output = std::process::Command::new("uv")
                .args(["run", "--quiet", "--no-project", "--python", "3.12", "--script"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{flavor:?} {preview:?} notebook failed:\n{}\n--- notebook ---\n{notebook}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
