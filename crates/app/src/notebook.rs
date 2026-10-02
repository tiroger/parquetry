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
/// cell showing `df`. The header's `requires-python` floor matters: uv resolves
/// added packages for every Python it allows, and current releases (matplotlib,
/// for one) already need more than 3.10.
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
    let heading = markdown_literal(&format!("# {title}\n\n{summary}\n\nOpened from Parquetry. `df` holds the rows shown there."));
    format!(
        r#"# /// script
# requires-python = ">=3.12"
# dependencies = [
{deps}# ]
#
# [tool.marimo.display]
# theme = "system"
# ///

import marimo

app = marimo.App(width="full")


@app.cell
def _():
    import marimo as mo
    return (mo,)


@app.cell
def _(mo):
    mo.md(
        {heading}
    )
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
    )
}

/// Markdown as a Python string marimo shows as editable text: `r"""…"""` when
/// possible.
fn markdown_literal(text: &str) -> String {
    if text.contains("\"\"\"") || text.ends_with('\\') || text.ends_with('"') {
        serde_json::to_string(text).unwrap_or_default()
    } else {
        let indented: String = text.lines().map(|l| if l.is_empty() { "\n".to_string() } else { format!("        {l}\n") }).collect();
        format!("r\"\"\"\n{indented}        \"\"\"")
    }
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

/// Project settings for marimo in the notebooks folder: run notebooks when they
/// open (marimo's default is not to, and a notebook can't turn it on itself).
const FOLDER_SETTINGS: &str = "# Written by Parquetry: marimo runs the notebooks in this folder when they open,\n# so `df` is ready. Delete this file to turn that off.\n[tool.marimo.runtime]\nauto_instantiate = true\n";

/// Save a notebook as `dir/file_name`.
pub fn write_notebook(dir: &Path, file_name: &str, notebook: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("couldn’t create {}", dir.display()))?;
    // Never replace a project file of the user's.
    let settings = dir.join("pyproject.toml");
    if !settings.exists() {
        let _ = std::fs::write(&settings, FOLDER_SETTINGS);
    }
    let path = dir.join(file_name);
    std::fs::write(&path, notebook).with_context(|| format!("couldn’t write {}", path.display()))?;
    Ok(path)
}

/// `uv tool run marimo edit --sandbox [extra] <notebook>`, ready to spawn.
fn marimo_command(notebook: &Path, extra: &[&str]) -> Result<Command> {
    let uv = find_uv().ok_or_else(|| anyhow!("uv isn’t installed. Install it from https://docs.astral.sh/uv/ and try again."))?;
    // marimo's --sandbox calls uv itself, so put uv's folder on its PATH.
    let mut path_var = uv.parent().map(|p| p.as_os_str().to_owned()).unwrap_or_default();
    if let Some(existing) = std::env::var_os("PATH") {
        path_var.push(if cfg!(windows) { ";" } else { ":" });
        path_var.push(existing);
    }
    let mut command = Command::new(&uv);
    command
        .args(["tool", "run", "marimo", "edit", "--sandbox"])
        .args(extra)
        .arg(notebook)
        .env("PATH", path_var)
        .stdin(Stdio::null());
    if let Some(dir) = notebook.parent() {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // Its own process group, so stopping it also stops the Python it starts.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    Ok(command)
}

/// Open a saved notebook in marimo in the browser. marimo keeps running after
/// Parquetry quits; its page has a shutdown button.
pub fn open_in_browser(notebook: &Path, log_dir: &Path) -> Result<()> {
    let mut command = marimo_command(notebook, &[])?;
    std::fs::create_dir_all(log_dir).ok();
    match std::fs::File::create(log_dir.join("marimo.log")).ok().and_then(|f| Some((f.try_clone().ok()?, f))) {
        Some((out, err)) => command.stdout(out).stderr(err),
        None => command.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    command.spawn().context("couldn’t start marimo")?;
    Ok(())
}

/// A marimo server run by Parquetry (for the notebook window): headless, stopped
/// with the window or when Parquetry quits.
pub struct MarimoServer {
    pid: u32,
}

/// Servers still running, so quitting can stop them.
static RUNNING: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

impl MarimoServer {
    /// Start marimo on `notebook`. The receiver gets the page URL (with its access
    /// token) once marimo is serving, or why it couldn't start.
    pub fn start(notebook: &Path, log_dir: &Path) -> Result<(Self, futures::channel::oneshot::Receiver<Result<String>>)> {
        use std::io::{BufRead as _, Write as _};
        let mut command = marimo_command(notebook, &["--headless"])?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().context("couldn’t start marimo")?;
        let pid = child.id();
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
        std::fs::create_dir_all(log_dir).ok();
        let log_path = log_dir.join("marimo.log");
        let log = std::sync::Arc::new(std::sync::Mutex::new(std::fs::File::create(&log_path).ok()));
        let (tx, rx) = futures::channel::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        // marimo prints its URL on stdout; keep both streams in the log.
        for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
            let (tx, log) = (tx.clone(), log.clone());
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stream).lines().map_while(std::result::Result::ok) {
                    if let Some(file) = log.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                        let _ = writeln!(file, "{line}");
                    }
                    if let Some(url) = served_url(&line)
                        && let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take()
                    {
                        // marimo prints the URL just before it starts accepting
                        // connections; wait for the port so the page loads first time.
                        std::thread::spawn(move || {
                            wait_for_port(&url, std::time::Duration::from_secs(20));
                            let _ = tx.send(Ok(url));
                        });
                    }
                }
            });
        }
        // If marimo exits before serving, say so (with the end of its output).
        let waiter_tx = tx.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            if let Some(tx) = waiter_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let tail = std::fs::read_to_string(&log_path)
                    .map(|text| text.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let _ = tx.send(Err(anyhow!("marimo stopped ({}) before opening the notebook.\n{tail}", status.map(|s| s.to_string()).unwrap_or_default())));
            }
            RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != pid);
        });
        Ok((Self { pid }, rx))
    }

    pub fn stop(&self) {
        stop_process(self.pid);
    }

    /// Stop every server still running (when Parquetry quits).
    pub fn stop_all() {
        let pids: Vec<u32> = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        pids.into_iter().for_each(stop_process);
    }
}

impl Drop for MarimoServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stop a marimo server and the processes it started.
fn stop_process(pid: u32) {
    #[cfg(unix)]
    let mut command = {
        // The whole process group (uv, marimo, its kernel).
        let mut c = Command::new("kill");
        c.args(["-TERM", &format!("-{pid}")]);
        c
    };
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt as _;
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000);
        c
    };
    let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != pid);
}

/// Wait until the server in `url` accepts connections (or `limit` passes).
fn wait_for_port(url: &str, limit: std::time::Duration) {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_string();
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        if std::net::TcpStream::connect(host.as_str()).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The page URL in a marimo output line: `➜  URL: http://localhost:2718?access_token=…`.
fn served_url(line: &str) -> Option<String> {
    let at = line.find("URL:")?;
    let url = line[at + 4..].trim();
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.split_whitespace().next().unwrap_or(url).to_string())
}

/// Open a link in the system browser.
pub fn open_externally(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let _ = Command::new("cmd").args(["/C", "start", "", url]).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = Command::new("xdg-open").arg(url).spawn();
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
        assert_eq!(
            served_url("        ➜  URL: http://localhost:2718?access_token=abc-123").as_deref(),
            Some("http://localhost:2718?access_token=abc-123")
        );
        assert_eq!(served_url("Edit nb.py in your browser"), None);
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

    /// A managed server starts, serves the notebook, and stops with everything it
    /// started. Skipped without uv (required in CI).
    #[test]
    fn managed_server_starts_and_stops() {
        if std::process::Command::new("uv").arg("--version").output().is_err() {
            assert!(std::env::var("PARQUETRY_REQUIRE_PYTHON_TESTS").is_err(), "uv is required");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let code = "import polars as pl\n\ndf = pl.DataFrame({\"id\": [1, 2, 3]})\n";
        let path = write_notebook(dir.path(), "server.py", &marimo_notebook("x", "3 rows", code, &["polars"])).unwrap();
        let (server, ready) = MarimoServer::start(&path, dir.path()).unwrap();
        let url = futures::executor::block_on(ready).expect("sender kept").expect("marimo serves the notebook");
        assert!(url.starts_with("http://") && url.contains("access_token="), "{url}");
        // The page answers (with the token).
        let host = url.trim_start_matches("http://").split(['/', '?']).next().unwrap().to_string();
        let mut stream = std::net::TcpStream::connect(host.as_str()).unwrap();
        use std::io::{Read as _, Write as _};
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
        write!(stream, "GET /?{query} HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        // 200, or 303 when marimo trades the token for a cookie.
        let status = response.split_whitespace().nth(1).unwrap_or("");
        assert!(["200", "303"].contains(&status), "{}", &response[..response.len().min(200)]);
        drop(server); // stops it
        // Nothing of it is left serving.
        let gone = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::net::TcpStream::connect(host.as_str()).is_err()
        });
        assert!(gone, "marimo still serving after stop");
    }

    /// Adding a library from marimo (`uv add --script`) works: the header's Python
    /// floor must suit current releases. Skipped without uv (required in CI).
    #[test]
    fn libraries_can_be_added() {
        if std::process::Command::new("uv").arg("--version").output().is_err() {
            assert!(std::env::var("PARQUETRY_REQUIRE_PYTHON_TESTS").is_err(), "uv is required");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let code = "import polars as pl\n\ndf = pl.DataFrame({\"id\": [1]})\n";
        let path = write_notebook(dir.path(), "add.py", &marimo_notebook("x", "1 row", code, &["polars"])).unwrap();
        let output = std::process::Command::new("uv")
            .args(["add", "--quiet", "--script"])
            .arg(&path)
            // marimo pins the latest release; these need Python 3.11+.
            .args(["matplotlib>=3.11", "seaborn"])
            .output()
            .unwrap();
        assert!(output.status.success(), "uv add failed:\n{}", String::from_utf8_lossy(&output.stderr));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"matplotlib") && text.contains("\"seaborn"), "{text}");
    }
}
