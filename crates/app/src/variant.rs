//! Which app this is: Parquetry, or Parquetry Preview (branch builds that install
//! next to it). Chosen at build time with `PARQUETRY_VARIANT=preview` (see build.rs).
//! The preview has its own name, settings, session and cache, so trying it never
//! touches the real app's state.

pub const PREVIEW: bool = cfg!(parquetry_preview);

pub const APP_NAME: &str = if PREVIEW { "Parquetry Preview" } else { "Parquetry" };

/// The macOS bundle identifier / window app id.
pub const APP_ID: &str = if PREVIEW { "io.parquetry.app.preview" } else { "io.parquetry.app" };
