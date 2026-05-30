//! Best-effort on-disk metadata cache (`--cache DIR`).
//!
//! The cache currently stores each server's **namespace table** keyed by
//! endpoint URL. Namespace indexes are session-local, but the *array of URIs* is
//! stable for a given server, so caching it lets repeated invocations (and
//! `--watch` iterations) skip a round-trip. This is intentionally a small
//! metadata cache, not a full offline mirror of the address space.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A handle to an on-disk cache directory.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

/// The cached namespace data for one endpoint.
#[derive(Debug, Serialize, Deserialize)]
struct CachedNamespaces {
    /// The endpoint this was read from (for human inspection / collision safety).
    endpoint: String,
    /// Namespace URIs in index order.
    uris: Vec<String>,
}

impl Cache {
    /// Open (creating if needed) a cache rooted at `dir`.
    pub fn open(dir: &Path) -> Self {
        Cache {
            dir: dir.to_path_buf(),
        }
    }

    /// Load cached namespace URIs for an endpoint, if present and valid.
    pub fn load_namespaces(&self, endpoint: &str) -> Option<Vec<String>> {
        let path = self.namespaces_path(endpoint);
        let text = std::fs::read_to_string(&path).ok()?;
        let cached: CachedNamespaces = serde_json::from_str(&text).ok()?;
        if cached.endpoint != endpoint {
            // Hash collision or stale file: ignore.
            return None;
        }
        debug!(endpoint, path = %path.display(), "namespace cache hit");
        Some(cached.uris)
    }

    /// Store namespace URIs for an endpoint. Failures are logged, not fatal.
    pub fn store_namespaces(&self, endpoint: &str, uris: &[String]) {
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            debug!(error = %e, "could not create cache directory");
            return;
        }
        let payload = CachedNamespaces {
            endpoint: endpoint.to_string(),
            uris: uris.to_vec(),
        };
        let path = self.namespaces_path(endpoint);
        match serde_json::to_string_pretty(&payload) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    debug!(error = %e, "could not write namespace cache");
                }
            }
            Err(e) => debug!(error = %e, "could not serialize namespace cache"),
        }
    }

    fn namespaces_path(&self, endpoint: &str) -> PathBuf {
        self.dir
            .join(format!("{}.namespaces.json", sanitize(endpoint)))
    }
}

/// Turn an endpoint URL into a filesystem-safe file stem.
fn sanitize(endpoint: &str) -> String {
    endpoint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
