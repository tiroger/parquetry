//! The engine owns one DuckDB database and a pool of worker threads.
//!
//! Every query runs on a worker so the UI thread never blocks. Work is split into
//! lanes so that cheap, latency-sensitive requests (fetching visible rows) are never
//! queued behind slow ones (column statistics, full scans).
//!
//! A [`Job`] is a future for the result. Dropping it cancels the work: queued jobs
//! are skipped and running queries are interrupted.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use duckdb::{Config, Connection, InterruptHandle};
use futures::channel::oneshot;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::s3::S3Service;

/// Which worker lane a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Short queries the person is waiting on right now: visible rows, cell values.
    Interactive,
    /// User-initiated work that may take a while: opening, sorting, filtering, SQL, export.
    Task,
    /// Speculative work: column statistics, metadata the person has not asked for yet.
    Background,
}

impl Lane {
    fn ix(self) -> usize {
        match self {
            Lane::Interactive => 0,
            Lane::Task => 1,
            Lane::Background => 2,
        }
    }
}

const LANE_WORKERS: [usize; 3] = [3, 2, 2];

/// Engine tuning. Persisted by the app as part of its settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    /// DuckDB memory limit, e.g. `"8GB"`. `None` lets DuckDB choose (80% of RAM).
    pub memory_limit: Option<String>,
    /// Local datasets above this many rows get sampled column summaries by default.
    /// Local scans are fast (a column of 50M rows summarizes in ~0.1 s), so this is high.
    pub sample_threshold_rows: u64,
    /// Remote datasets above this many rows get sampled summaries: every scan is a download.
    pub remote_sample_threshold_rows: u64,
    /// Approximate number of rows read when sampling.
    pub sample_rows: u64,
    /// Filtered/sorted results at or below this many rows are copied into memory
    /// so that scrolling them never touches the source files again.
    pub materialize_limit_rows: u64,
    /// Maximum rows kept from a SQL query or from a filter over a non-Parquet
    /// relation (Delta, Iceberg).
    pub result_limit_rows: u64,
    pub s3: S3Settings,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            memory_limit: None,
            sample_threshold_rows: 100_000_000,
            remote_sample_threshold_rows: 2_000_000,
            sample_rows: 200_000,
            materialize_limit_rows: 1_000_000,
            result_limit_rows: 10_000_000,
            s3: S3Settings::default(),
        }
    }
}

/// How to reach S3 (or an S3-compatible store).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Settings {
    /// AWS profile name from `~/.aws/config`. `None` uses the default credential chain.
    pub profile: Option<String>,
    /// Region used when a bucket's region cannot be discovered.
    pub region: Option<String>,
    /// Custom endpoint for S3-compatible stores (MinIO, R2, ...), e.g. `http://localhost:9000`.
    pub endpoint: Option<String>,
    /// Use path-style addressing (needed by most S3-compatible stores).
    pub path_style: bool,
    /// Skip credentials entirely (public buckets).
    pub anonymous: bool,
}

type Work = Box<dyn FnOnce(&Connection, &Arc<InterruptHandle>) + Send>;

pub(crate) struct EngineInner {
    root: Mutex<Connection>,
    lanes: Vec<Sender<Work>>,
    next_id: AtomicU64,
    settings: RwLock<EngineSettings>,
    pub(crate) s3: S3Service,
    bundled_extensions: Option<PathBuf>,
    loaded_extensions: Mutex<Vec<String>>,
}

/// Shared handle to the DuckDB-backed engine. Cheap to clone.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<EngineInner>,
}

/// Where the engine keeps files.
#[derive(Debug, Clone)]
pub struct EnginePaths {
    /// Spill directory for sorts and joins larger than memory.
    pub temp_dir: PathBuf,
    /// Where DuckDB installs extensions (httpfs, delta, iceberg, ...).
    pub extension_dir: PathBuf,
    /// Extensions shipped inside the app bundle, loaded before trying the network.
    pub bundled_extensions: Option<PathBuf>,
}

impl EnginePaths {
    /// Default locations under `~/Library/Caches/Parquetry`.
    pub fn default_for_app() -> Self {
        let base = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Parquetry");
        Self {
            temp_dir: base.join("spill").join(std::process::id().to_string()),
            extension_dir: base.join("duckdb_extensions"),
            bundled_extensions: None,
        }
    }

    /// Isolated locations for tests.
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            temp_dir: dir.join("spill"),
            extension_dir: dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("Parquetry")
                .join("duckdb_extensions"),
            bundled_extensions: None,
        }
    }
}

impl Engine {
    pub fn new(paths: EnginePaths, settings: EngineSettings) -> Result<Self> {
        std::fs::create_dir_all(&paths.temp_dir)?;
        std::fs::create_dir_all(&paths.extension_dir)?;
        let config = Config::default()
            .with("temp_directory", paths.temp_dir.to_string_lossy())?
            .with("extension_directory", paths.extension_dir.to_string_lossy())?
            .with("preserve_insertion_order", "true")?
            .enable_autoload_extension(true)?;
        let root = Connection::open_in_memory_with_flags(config)?;
        root.execute_batch(
            "SET autoinstall_known_extensions = true;
             SET enable_http_metadata_cache = true;
             SET enable_object_cache = true;",
        )?;

        let mut lanes = Vec::new();
        let mut receivers: Vec<(usize, Receiver<Work>)> = Vec::new();
        for (lane, workers) in LANE_WORKERS.iter().enumerate() {
            let (tx, rx) = unbounded::<Work>();
            lanes.push(tx);
            for _ in 0..*workers {
                receivers.push((lane, rx.clone()));
            }
        }

        let inner = Arc::new(EngineInner {
            s3: S3Service::new(settings.s3.clone()),
            root: Mutex::new(root),
            lanes,
            next_id: AtomicU64::new(1),
            settings: RwLock::new(settings.clone()),
            bundled_extensions: paths.bundled_extensions.clone(),
            loaded_extensions: Mutex::new(Vec::new()),
        });

        for (worker_ix, (lane, rx)) in receivers.into_iter().enumerate() {
            let conn = inner.root.lock().try_clone()?;
            thread::Builder::new()
                .name(format!("duckdb-{lane}-{worker_ix}"))
                .spawn(move || {
                    let interrupt = conn.interrupt_handle();
                    while let Ok(work) = rx.recv() {
                        work(&conn, &interrupt);
                    }
                })
                .map_err(|e| Error::other(e.to_string()))?;
        }

        let engine = Engine { inner };
        engine.apply_settings(&settings)?;
        Ok(engine)
    }

    pub fn settings(&self) -> EngineSettings {
        self.inner.settings.read().clone()
    }

    /// Apply new settings. DuckDB globals change immediately; S3 credentials are
    /// re-resolved on the next S3 access.
    pub fn apply_settings(&self, settings: &EngineSettings) -> Result<()> {
        {
            let root = self.inner.root.lock();
            match &settings.memory_limit {
                Some(limit) if !limit.trim().is_empty() => {
                    root.execute_batch(&format!(
                        "SET GLOBAL memory_limit = {}",
                        crate::sql::literal(limit.trim())
                    ))?;
                }
                _ => {
                    root.execute_batch("RESET GLOBAL memory_limit")?;
                }
            }
        }
        let s3_changed = self.inner.settings.read().s3 != settings.s3;
        *self.inner.settings.write() = settings.clone();
        if s3_changed {
            self.inner.s3.reconfigure(settings.s3.clone());
        }
        Ok(())
    }

    /// A process-unique number for naming temporary tables and views.
    pub fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Run `work` on a worker connection. The returned [`Job`] resolves with its result;
    /// dropping the job cancels it.
    pub fn run<T, F>(&self, lane: Lane, work: F) -> Job<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let control = Arc::new(JobControl::default());
        let job_control = control.clone();
        let boxed: Work = Box::new(move |conn, interrupt| {
            // Dropping a non-detached Job sets `cancelled`; detached jobs have no receiver
            // but must still run.
            if job_control.cancelled.load(Ordering::Acquire) {
                return;
            }
            *job_control.running.lock() = Some(interrupt.clone());
            // Re-check after publishing the handle so a cancel between the two
            // checks is never lost.
            let result = if job_control.cancelled.load(Ordering::Acquire) {
                Err(Error::Cancelled)
            } else {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(conn)))
                    .unwrap_or_else(|panic| {
                        let message = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_else(|| "unknown panic".into());
                        Err(Error::other(format!("Internal error: {message}")))
                    })
            };
            *job_control.running.lock() = None;
            let result = if job_control.cancelled.load(Ordering::Acquire) {
                Err(Error::Cancelled)
            } else {
                result
            };
            let _ = tx.send(result);
        });
        if self.inner.lanes[lane.ix()].send(boxed).is_err() {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Err(Error::other("Engine stopped")));
            return Job {
                rx,
                control,
                detached: false,
            };
        }
        Job {
            rx,
            control,
            detached: false,
        }
    }

    /// Run a statement on a worker and ignore the outcome. Used for cleanup.
    pub(crate) fn run_detached(&self, sql: String) {
        self.run(Lane::Background, move |conn| {
            if let Err(error) = conn.execute_batch(&sql) {
                log::warn!("cleanup failed: {sql}: {error}");
            }
            Ok(())
        })
        .detach();
    }

    /// Load an extension, preferring a copy bundled with the app, then DuckDB's
    /// extension repository.
    pub(crate) fn ensure_extension(&self, conn: &Connection, name: &str) -> Result<()> {
        if self.inner.loaded_extensions.lock().iter().any(|n| n == name) {
            // Extensions load per database, so any connection sees it.
            return Ok(());
        }
        let mut loaded = false;
        if let Some(dir) = &self.inner.bundled_extensions {
            // Two layouts are supported: a raw `<dir>/<platform>/<name>.duckdb_extension`
            // and DuckDB's own repository layout `<dir>/<version>/<platform>/<name>.duckdb_extension.gz`
            // (the one used in notarized builds, where no loose Mach-O files may be shipped).
            let platform = if cfg!(target_arch = "aarch64") { "osx_arm64" } else { "osx_amd64" };
            let raw = dir.join(platform).join(format!("{name}.duckdb_extension"));
            if raw.exists() {
                let sql = format!("LOAD {}", crate::sql::literal(&raw.to_string_lossy()));
                loaded = conn.execute_batch(&sql).is_ok();
            }
            if !loaded && dir.is_dir() {
                let sql = format!(
                    "INSTALL {name} FROM {}; LOAD {name};",
                    crate::sql::literal(&dir.to_string_lossy())
                );
                loaded = conn.execute_batch(&sql).is_ok();
            }
        }
        if !loaded {
            let loaded_already: bool = conn
                .query_row(
                    "SELECT coalesce(bool_or(loaded), false) FROM duckdb_extensions() WHERE extension_name = ?",
                    [name],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if !loaded_already {
                conn.execute_batch(&format!("INSTALL {name}; LOAD {name};"))
                    .map_err(|e| {
                        Error::other(format!(
                            "Couldn’t load the DuckDB “{name}” extension. It is downloaded the first time it is needed, so check your internet connection. ({})",
                            Error::from(e)
                        ))
                    })?;
            }
        }
        self.inner.loaded_extensions.lock().push(name.to_string());
        Ok(())
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        // Workers exit when their channel closes; the spill directory is ours alone.
        let temp_dir: Option<String> = self
            .root
            .lock()
            .query_row("SELECT current_setting('temp_directory')", [], |r| r.get(0))
            .ok();
        if let Some(dir) = temp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[derive(Default)]
struct JobControl {
    cancelled: AtomicBool,
    running: Mutex<Option<Arc<InterruptHandle>>>,
}

impl JobControl {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(handle) = self.running.lock().as_ref() {
            handle.interrupt();
        }
    }
}

/// A pending engine result. Await it (or call [`Job::wait`]); drop it to cancel.
pub struct Job<T> {
    rx: oneshot::Receiver<Result<T>>,
    control: Arc<JobControl>,
    detached: bool,
}

impl<T> Job<T> {
    /// Block the current thread until the job finishes. For tests and tools, never the UI.
    pub fn wait(self) -> Result<T> {
        futures::executor::block_on(self)
    }

    /// Let the job run to completion even if nobody awaits it.
    pub fn detach(mut self) {
        self.detached = true;
    }

    /// Request cancellation without dropping the handle.
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// A handle that can cancel this job from elsewhere (e.g. a Cancel button).
    pub fn canceller(&self) -> Canceller {
        Canceller(self.control.clone())
    }
}

impl<T> Future for Job<T> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Cancelled)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        if !self.detached {
            self.control.cancel();
        }
    }
}

/// Cancels a job it was taken from.
#[derive(Clone)]
pub struct Canceller(Arc<JobControl>);

impl Canceller {
    pub fn cancel(&self) {
        self.0.cancel();
    }
}
