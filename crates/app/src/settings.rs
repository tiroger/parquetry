//! Persisted preferences, recent locations and SQL history.

use std::path::PathBuf;

use parquetry_engine::{EngineSettings, Format};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Match System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }

    pub fn all() -> [Appearance; 3] {
        [Appearance::System, Appearance::Light, Appearance::Dark]
    }
}

/// The dataframe library notebooks are written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NotebookLibrary {
    #[default]
    Polars,
    Pandas,
}

impl NotebookLibrary {
    pub fn label(self) -> &'static str {
        match self {
            NotebookLibrary::Polars => "Polars",
            NotebookLibrary::Pandas => "pandas",
        }
    }

    pub fn all() -> [NotebookLibrary; 2] {
        [NotebookLibrary::Polars, NotebookLibrary::Pandas]
    }
}

/// The palette used in dark mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DarkTheme {
    #[default]
    NavyOak,
    Slate,
}

impl DarkTheme {
    pub fn label(self) -> &'static str {
        match self {
            DarkTheme::NavyOak => "Navy & Oak",
            DarkTheme::Slate => "Slate",
        }
    }

    /// Name in `themes/parquetry.json`.
    pub fn theme_name(self) -> &'static str {
        self.label()
    }

    pub fn all() -> [DarkTheme; 2] {
        [DarkTheme::NavyOak, DarkTheme::Slate]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentItem {
    pub location: String,
    pub format: Option<Format>,
    /// Seconds since the Unix epoch.
    pub opened_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub engine: EngineSettings,
    pub appearance: Appearance,
    pub dark_theme: DarkTheme,
    /// Base font size in points; the whole interface scales with it.
    pub font_size: f32,
    pub show_summaries: bool,
    /// Reopen the windows and tabs of the last session at launch.
    pub reopen_last_session: bool,
    /// Open in marimo: the dataframe library, and where notebooks are saved
    /// (`None`: ~/Documents/Parquetry Notebooks).
    pub notebook_library: NotebookLibrary,
    pub notebooks_dir: Option<String>,
    pub recents: Vec<RecentItem>,
    pub sql_history: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: EngineSettings::default(),
            appearance: Appearance::System,
            dark_theme: DarkTheme::NavyOak,
            font_size: DEFAULT_FONT_SIZE,
            show_summaries: true,
            reopen_last_session: true,
            notebook_library: NotebookLibrary::Polars,
            notebooks_dir: None,
            recents: Vec::new(),
            sql_history: Vec::new(),
        }
    }
}

pub const DEFAULT_FONT_SIZE: f32 = 15.0;
pub const MIN_FONT_SIZE: f32 = 10.0;
pub const MAX_FONT_SIZE: f32 = 26.0;
const MAX_RECENTS: usize = 20;
const MAX_HISTORY: usize = 200;

impl Settings {
    pub fn directory() -> PathBuf {
        // Tests (and anyone wanting a separate profile) can point this elsewhere.
        if let Some(dir) = std::env::var_os("PARQUETRY_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        dirs::config_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir)
            .join(crate::variant::APP_NAME)
    }

    pub fn path() -> PathBuf {
        Self::directory().join("settings.json")
    }

    /// Load settings. A missing file gives defaults; an unreadable one is kept aside
    /// as `settings.json.bak` so it isn't silently lost.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.font_size = settings.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                settings
            }
            Err(error) => {
                log::warn!("settings unreadable ({error}); starting fresh");
                let _ = std::fs::rename(path, path.with_extension("json.bak"));
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        if let Err(error) = self.save_to(&Self::path()) {
            log::warn!("couldn't save settings: {error}");
        }
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write-then-rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn add_recent(&mut self, location: &str, format: Option<Format>) {
        self.recents.retain(|r| r.location != location);
        self.recents.insert(
            0,
            RecentItem {
                location: location.to_string(),
                format,
                opened_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            },
        );
        self.recents.truncate(MAX_RECENTS);
    }

    pub fn remove_recent(&mut self, location: &str) {
        self.recents.retain(|r| r.location != location);
    }

    pub fn add_history(&mut self, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        self.sql_history.retain(|s| s != sql);
        self.sql_history.insert(0, sql.to_string());
        self.sql_history.truncate(MAX_HISTORY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());

        let mut settings = Settings::default();
        settings.add_recent("/a.parquet", Some(Format::Parquet));
        settings.add_recent("/b.csv", None);
        settings.add_recent("/a.parquet", Some(Format::Parquet));
        assert_eq!(settings.recents[0].location, "/a.parquet");
        assert_eq!(settings.recents.len(), 2);
        settings.add_history("select 1");
        settings.add_history("  ");
        settings.engine.s3.profile = Some("dev".into());
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        assert!(path.with_extension("json.bak").exists());

        // Unknown and missing fields are tolerated.
        std::fs::write(&path, r#"{"font_size": 99, "future_field": true}"#).unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.font_size, MAX_FONT_SIZE);
        assert!(loaded.show_summaries);
    }

    #[test]
    fn recents_are_capped() {
        let mut settings = Settings::default();
        for i in 0..50 {
            settings.add_recent(&format!("/f{i}"), None);
        }
        assert_eq!(settings.recents.len(), MAX_RECENTS);
        assert_eq!(settings.recents[0].location, "/f49");
    }
}
