//! S3 access: browsing with the AWS SDK and credentials for DuckDB.
//!
//! DuckDB reads the data (httpfs), but it cannot browse "folders" efficiently and its
//! credential chain does not know which region each bucket lives in. The AWS SDK does
//! both, and resolves the same profiles, SSO sessions and environment variables the
//! `aws` CLI uses. Resolved credentials are handed to DuckDB as a per-bucket secret
//! and refreshed shortly before they expire.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use aws_config::BehaviorVersion;
use aws_credential_types::provider::ProvideCredentials;
use duckdb::Connection;
use parking_lot::Mutex;

use crate::engine::S3Settings;
use crate::error::{Error, Result};
use crate::sql::literal;

/// An entry in an S3 listing.
#[derive(Debug, Clone, PartialEq)]
pub struct S3Entry {
    /// Display name relative to the listed prefix (folders end with `/`).
    pub name: String,
    /// Full `s3://bucket/key` URL.
    pub url: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Last modification time as an RFC 3339 string.
    pub modified: Option<String>,
}

/// Parsed `s3://bucket/key` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Url {
    pub bucket: String,
    pub key: String,
}

impl S3Url {
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("s3://")
            .or_else(|| url.strip_prefix("s3a://"))?;
        let (bucket, key) = match rest.split_once('/') {
            Some((bucket, key)) => (bucket, key),
            None => (rest, ""),
        };
        Some(Self {
            bucket: bucket.to_string(),
            key: key.to_string(),
        })
    }
}

struct Credentials {
    key_id: String,
    secret: String,
    session_token: Option<String>,
    expiry: Option<SystemTime>,
}

#[derive(Default)]
struct S3State {
    settings: S3Settings,
    runtime: Option<tokio::runtime::Runtime>,
    config: Option<aws_config::SdkConfig>,
    bucket_regions: HashMap<String, String>,
    /// No credentials were found (no profile, environment or instance role), so
    /// requests go unsigned and only public data can be read.
    unsigned: bool,
    /// Buckets with a DuckDB secret, and when that secret's credentials expire.
    secrets: HashMap<String, Option<SystemTime>>,
}

pub struct S3Service {
    state: Mutex<S3State>,
}

const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

// Messages the app recognizes (`credential_issue`) to offer a way out.
const NO_CREDENTIALS: &str = "No AWS credentials were found";
const NO_CREDENTIALS_LIST: &str = "No AWS credentials were found, so only public buckets can be read. Type a bucket path such as s3://bucket/prefix/, or choose an AWS profile in Settings ▸ Amazon S3.";
const SIGNED_OUT: &str = "Your AWS SSO session has expired or isn’t signed in";
const EXPIRED: &str = "Your AWS credentials have expired";
const DENIED: &str = "Access denied: these credentials aren’t allowed to read this location.";

/// A credentials problem behind an S3 error, which the person can fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialIssue {
    /// No credentials anywhere (only public data can be read).
    Missing,
    /// An SSO session (or temporary credentials) expired: sign in again.
    SignedOut,
    /// The credentials work but may not read this location.
    Denied,
}

/// Which credentials problem, if any, `error` (from an S3 operation) reports.
pub fn credential_issue(error: &Error) -> Option<CredentialIssue> {
    let Error::Other(message) = error else { return None };
    if message.contains(NO_CREDENTIALS) {
        Some(CredentialIssue::Missing)
    } else if message.contains(SIGNED_OUT) || message.contains(EXPIRED) {
        Some(CredentialIssue::SignedOut)
    } else if message.contains(DENIED) {
        Some(CredentialIssue::Denied)
    } else {
        None
    }
}

impl S3Service {
    pub fn new(settings: S3Settings) -> Self {
        Self {
            state: Mutex::new(S3State {
                settings,
                ..Default::default()
            }),
        }
    }

    pub fn reconfigure(&self, settings: S3Settings) {
        let mut state = self.state.lock();
        state.settings = settings;
        state.config = None;
        state.unsigned = false;
        state.bucket_regions.clear();
        state.secrets.clear();
    }

    fn runtime(state: &mut S3State) -> Result<&tokio::runtime::Runtime> {
        if state.runtime.is_none() {
            state.runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("parquetry-s3")
                    .enable_all()
                    .build()
                    .map_err(|e| Error::other(e.to_string()))?,
            );
        }
        Ok(state.runtime.as_ref().unwrap())
    }

    fn sdk_config(state: &mut S3State) -> Result<aws_config::SdkConfig> {
        if let Some(config) = &state.config {
            return Ok(config.clone());
        }
        let settings = state.settings.clone();
        let runtime = Self::runtime(state)?;
        let (config, unsigned) = runtime.block_on(async move {
            let loader = || {
                let mut loader = aws_config::defaults(BehaviorVersion::latest());
                if let Some(profile) = settings.profile.as_deref().filter(|p| !p.is_empty()) {
                    loader = loader.profile_name(profile);
                }
                if let Some(region) = settings.region.as_deref().filter(|r| !r.is_empty()) {
                    loader = loader.region(aws_config::Region::new(region.to_string()));
                }
                if let Some(endpoint) = settings.endpoint.as_deref().filter(|e| !e.is_empty()) {
                    loader = loader.endpoint_url(endpoint);
                }
                loader
            };
            let config = loader().load().await;
            // Nothing in the credential chain at all: read public data unsigned. Other
            // failures (an expired SSO session, say) are reported when used instead.
            let missing = match config.credentials_provider() {
                None => true,
                Some(provider) => matches!(
                    provider.provide_credentials().await,
                    Err(aws_credential_types::provider::error::CredentialsError::CredentialsNotLoaded(_))
                ),
            };
            if missing { (loader().no_credentials().load().await, true) } else { (config, false) }
        });
        state.config = Some(config.clone());
        state.unsigned = unsigned;
        Ok(config)
    }

    fn client(state: &mut S3State, region: Option<&str>) -> Result<aws_sdk_s3::Client> {
        let config = Self::sdk_config(state)?;
        let mut builder = aws_sdk_s3::config::Builder::from(&config);
        if state.settings.path_style {
            builder = builder.force_path_style(true);
        }
        if let Some(region) = region {
            builder = builder.region(aws_sdk_s3::config::Region::new(region.to_string()));
        } else if config.region().is_none() {
            builder = builder.region(aws_sdk_s3::config::Region::new("us-east-1"));
        }
        Ok(aws_sdk_s3::Client::from_conf(builder.build()))
    }

    fn resolve_credentials(state: &mut S3State) -> Result<Option<Credentials>> {
        let config = Self::sdk_config(state)?;
        if state.unsigned {
            return Ok(None);
        }
        let Some(provider) = config.credentials_provider() else {
            return Ok(None);
        };
        let runtime = Self::runtime(state)?;
        let creds = runtime
            .block_on(async move { provider.provide_credentials().await })
            .map_err(|e| {
                Error::other(aws_error_text(&e).to_string())
            })?;
        Ok(Some(Credentials {
            key_id: creds.access_key_id().to_string(),
            secret: creds.secret_access_key().to_string(),
            session_token: creds.session_token().map(str::to_string),
            expiry: creds.expiry(),
        }))
    }

    /// Find the bucket's region (cached). Falls back to the configured region.
    fn bucket_region(state: &mut S3State, bucket: &str) -> Result<String> {
        if let Some(region) = state.bucket_regions.get(bucket) {
            return Ok(region.clone());
        }
        let fallback = state
            .settings
            .region
            .clone()
            .filter(|r| !r.is_empty())
            .or_else(|| {
                state
                    .config
                    .as_ref()
                    .and_then(|c| c.region().map(|r| r.to_string()))
            })
            .unwrap_or_else(|| "us-east-1".to_string());
        if state.settings.endpoint.as_deref().is_some_and(|e| !e.is_empty()) {
            state.bucket_regions.insert(bucket.to_string(), fallback.clone());
            return Ok(fallback);
        }
        let client = Self::client(state, Some("us-east-1"))?;
        let runtime = Self::runtime(state)?;
        let bucket_name = bucket.to_string();
        let region = runtime.block_on(async move {
            match client.head_bucket().bucket(&bucket_name).send().await {
                Ok(output) => output.bucket_region().map(str::to_string),
                Err(err) => err
                    .raw_response()
                    .and_then(|r| r.headers().get("x-amz-bucket-region"))
                    .map(str::to_string),
            }
        });
        let region = region.unwrap_or(fallback);
        state
            .bucket_regions
            .insert(bucket.to_string(), region.clone());
        Ok(region)
    }

    /// Make sure DuckDB can read from `url`'s bucket: httpfs loaded, a secret with
    /// fresh credentials and the right region. Cheap when already prepared.
    pub fn prepare_duckdb(
        &self,
        engine: &crate::Engine,
        conn: &Connection,
        url: &str,
    ) -> Result<()> {
        let Some(parsed) = S3Url::parse(url) else {
            if is_remote_http(url) {
                engine.ensure_extension(conn, "httpfs")?;
            }
            return Ok(());
        };
        engine.ensure_extension(conn, "httpfs")?;
        let mut state = self.state.lock();
        if let Some(expiry) = state.secrets.get(&parsed.bucket) {
            let fresh = match expiry {
                None => true,
                Some(expiry) => SystemTime::now() + REFRESH_MARGIN < *expiry,
            };
            if fresh {
                return Ok(());
            }
        }
        let region = Self::bucket_region(&mut state, &parsed.bucket)?;
        let creds = Self::resolve_credentials(&mut state)?;
        let mut options = vec![
            "TYPE s3".to_string(),
            format!("REGION {}", literal(&region)),
            format!("SCOPE {}", literal(&format!("s3://{}", parsed.bucket))),
        ];
        if let Some(creds) = &creds {
            options.push(format!("KEY_ID {}", literal(&creds.key_id)));
            options.push(format!("SECRET {}", literal(&creds.secret)));
            if let Some(token) = &creds.session_token {
                options.push(format!("SESSION_TOKEN {}", literal(token)));
            }
        }
        if let Some(endpoint) = state.settings.endpoint.as_deref().filter(|e| !e.is_empty()) {
            let use_ssl = !endpoint.starts_with("http://");
            let host = endpoint
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/');
            options.push(format!("ENDPOINT {}", literal(host)));
            options.push(format!("USE_SSL {use_ssl}"));
        }
        if state.settings.path_style {
            options.push("URL_STYLE 'path'".to_string());
        }
        let name = format!("pq_s3_{}", sanitize(&parsed.bucket));
        conn.execute_batch(&format!(
            "CREATE OR REPLACE SECRET {name} ({})",
            options.join(", ")
        ))?;
        state
            .secrets
            .insert(parsed.bucket.clone(), creds.and_then(|c| c.expiry));
        Ok(())
    }

    /// A client for `bucket`'s region (or the default region) and a runtime handle,
    /// so network calls happen without holding the state lock.
    fn client_for(&self, bucket: Option<&str>) -> Result<(aws_sdk_s3::Client, tokio::runtime::Handle)> {
        let mut state = self.state.lock();
        let region = match bucket {
            Some(bucket) => Some(Self::bucket_region(&mut state, bucket)?),
            None => None,
        };
        let client = Self::client(&mut state, region.as_deref())?;
        let handle = Self::runtime(&mut state)?.handle().clone();
        Ok((client, handle))
    }

    /// List buckets (for `s3://`) or the folders and objects directly under a prefix.
    pub fn list(&self, url: &str) -> Result<Vec<S3Entry>> {
        let trimmed = url.trim();
        let parsed = S3Url::parse(trimmed).unwrap_or(S3Url {
            bucket: String::new(),
            key: String::new(),
        });
        if parsed.bucket.is_empty() {
            let unsigned = {
                let mut state = self.state.lock();
                Self::sdk_config(&mut state)?;
                state.unsigned
            };
            if unsigned {
                return Err(Error::other(NO_CREDENTIALS_LIST));
            }
            let (client, runtime) = self.client_for(None)?;
            let buckets = runtime
                .block_on(async move { client.list_buckets().send().await })
                .map_err(|e| Error::other(format!("Couldn’t list buckets: {}", aws_error_text(&e))))?;
            let mut entries: Vec<S3Entry> = buckets
                .buckets()
                .iter()
                .filter_map(|b| b.name())
                .map(|name| S3Entry {
                    name: format!("{name}/"),
                    url: format!("s3://{name}/"),
                    is_dir: true,
                    size: None,
                    modified: None,
                })
                .collect();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(entries);
        }
        let (client, runtime) = self.client_for(Some(&parsed.bucket))?;
        let prefix = if parsed.key.is_empty() || parsed.key.ends_with('/') {
            parsed.key.clone()
        } else {
            format!("{}/", parsed.key)
        };
        let bucket = parsed.bucket.clone();
        let result: Result<Vec<S3Entry>> = runtime.block_on(async move {
            let mut entries = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let mut request = client
                    .list_objects_v2()
                    .bucket(&bucket)
                    .prefix(&prefix)
                    .delimiter("/");
                if let Some(t) = &token {
                    request = request.continuation_token(t);
                }
                let page = request.send().await.map_err(|e| {
                    Error::other(format!("Couldn’t list s3://{bucket}/{prefix}: {}", aws_error_text(&e)))
                })?;
                for common in page.common_prefixes() {
                    if let Some(p) = common.prefix() {
                        let name = p.strip_prefix(prefix.as_str()).unwrap_or(p).to_string();
                        entries.push(S3Entry {
                            name,
                            url: format!("s3://{bucket}/{p}"),
                            is_dir: true,
                            size: None,
                            modified: None,
                        });
                    }
                }
                for object in page.contents() {
                    let Some(key) = object.key() else { continue };
                    if key == prefix {
                        continue;
                    }
                    let name = key.strip_prefix(prefix.as_str()).unwrap_or(key).to_string();
                    entries.push(S3Entry {
                        name,
                        url: format!("s3://{bucket}/{key}"),
                        is_dir: false,
                        size: object.size().map(|s| s.max(0) as u64),
                        modified: object.last_modified().map(|t| t.to_string()),
                    });
                }
                token = page.next_continuation_token().map(str::to_string);
                if token.is_none() || entries.len() >= 20_000 {
                    break;
                }
            }
            Ok(entries)
        });
        result
    }

    /// All object keys under a prefix (recursively), for opening a dataset folder.
    pub fn list_recursive(&self, url: &str, limit: usize) -> Result<Vec<S3Entry>> {
        let parsed = S3Url::parse(url).ok_or_else(|| Error::other("Not an s3:// URL"))?;
        let (client, runtime) = self.client_for(Some(&parsed.bucket))?;
        let bucket = parsed.bucket.clone();
        let prefix = parsed.key.clone();
        runtime.block_on(async move {
            let mut entries = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let mut request = client.list_objects_v2().bucket(&bucket).prefix(&prefix);
                if let Some(t) = &token {
                    request = request.continuation_token(t);
                }
                let page = request.send().await.map_err(|e| {
                    Error::other(format!("Couldn’t list s3://{bucket}/{prefix}: {}", aws_error_text(&e)))
                })?;
                for object in page.contents() {
                    let Some(key) = object.key() else { continue };
                    entries.push(S3Entry {
                        name: key.to_string(),
                        url: format!("s3://{bucket}/{key}"),
                        is_dir: false,
                        size: object.size().map(|s| s.max(0) as u64),
                        modified: object.last_modified().map(|t| t.to_string()),
                    });
                }
                token = page.next_continuation_token().map(str::to_string);
                if token.is_none() || entries.len() >= limit {
                    break;
                }
            }
            Ok(entries)
        })
    }
}

pub(crate) fn is_remote_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Whether a location refers to object storage or the web rather than the local disk.
pub fn is_remote(location: &str) -> bool {
    S3Url::parse(location).is_some()
        || is_remote_http(location)
        || location.starts_with("gs://")
        || location.starts_with("gcs://")
        || location.starts_with("r2://")
        || location.starts_with("az://")
        || location.starts_with("abfss://")
        || location.starts_with("hf://")
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn aws_error_text<E: std::error::Error>(error: &E) -> String {
    describe_aws_error(&raw_aws_error_text(error))
}

/// Turn AWS SDK error chains into one sentence a person can act on.
pub(crate) fn describe_aws_error(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("no credentials")
        || lower.contains("credential provider was not enabled")
        || lower.contains("could not load credentials")
        || lower.contains("no providers in chain")
    {
        return format!("{NO_CREDENTIALS}. Choose an AWS profile in Settings ▸ Amazon S3.");
    }
    if lower.contains("sso") && (lower.contains("expired") || lower.contains("token")) {
        return format!("{SIGNED_OUT}. Run `aws sso login` for this profile, then try again.");
    }
    if lower.contains("expiredtoken") || lower.contains("token has expired") || lower.contains("security token included in the request is expired") {
        return format!("{EXPIRED}. Refresh them (e.g. `aws sso login`), then try again.");
    }
    if lower.contains("accessdenied") || lower.contains("access denied") || lower.contains("403") {
        return DENIED.into();
    }
    if lower.contains("nosuchbucket") {
        return "That bucket doesn’t exist.".into();
    }
    if lower.contains("dns error") || lower.contains("connect") || lower.contains("timed out") || lower.contains("dispatch failure") {
        return "Couldn’t reach S3. Check your network connection (and the endpoint in Settings, if you use one).".into();
    }
    let mut short: String = text.chars().take(300).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}

fn raw_aws_error_text<E: std::error::Error>(error: &E) -> String {
    // SDK errors nest the useful message several sources deep.
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let inner_text = inner.to_string();
        if !inner_text.is_empty() && !text.contains(&inner_text) {
            text = format!("{text}: {inner_text}");
        }
        source = inner.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_errors() {
        assert!(describe_aws_error("dispatch failure: other: the credential provider was not enabled").starts_with("No AWS credentials"));
        assert!(describe_aws_error("service error: AccessDenied: Access Denied").starts_with("Access denied"));
        assert!(describe_aws_error("The SSO session associated with this profile has expired").contains("aws sso login"));
        assert!(describe_aws_error("NoSuchBucket").contains("doesn’t exist"));
        assert_eq!(describe_aws_error("weird"), "weird");
    }

    #[test]
    fn credential_issues() {
        let issue = |raw: &str| credential_issue(&Error::other(format!("Couldn’t list buckets: {}", describe_aws_error(raw))));
        assert_eq!(issue("no providers in chain provided credentials"), Some(CredentialIssue::Missing));
        assert_eq!(credential_issue(&Error::other(NO_CREDENTIALS_LIST)), Some(CredentialIssue::Missing));
        assert_eq!(issue("The SSO session associated with this profile has expired"), Some(CredentialIssue::SignedOut));
        assert_eq!(issue("ExpiredToken: The provided token has expired"), Some(CredentialIssue::SignedOut));
        assert_eq!(issue("service error: AccessDenied: Access Denied"), Some(CredentialIssue::Denied));
        assert_eq!(issue("NoSuchBucket"), None);
        assert_eq!(credential_issue(&Error::Query("Access denied: x".into())), None, "only S3 errors");
    }

    #[test]
    fn parse_urls() {
        assert_eq!(
            S3Url::parse("s3://bucket/a/b.parquet"),
            Some(S3Url {
                bucket: "bucket".into(),
                key: "a/b.parquet".into()
            })
        );
        assert_eq!(
            S3Url::parse("s3://bucket"),
            Some(S3Url {
                bucket: "bucket".into(),
                key: "".into()
            })
        );
        assert!(S3Url::parse("/tmp/x").is_none());
        assert!(is_remote("https://x.com/a.parquet"));
        assert!(!is_remote("/Users/me/a.parquet"));
    }
}
