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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = temp_dir();
        let cache = Cache::open(dir.path());
        let endpoint = "opc.tcp://host:4840/";
        let uris = vec![
            "http://opcfoundation.org/UA/".to_string(),
            "urn:vendor:custom".to_string(),
        ];
        cache.store_namespaces(endpoint, &uris);
        assert_eq!(cache.load_namespaces(endpoint), Some(uris));
    }

    #[test]
    fn missing_entry_returns_none() {
        let dir = temp_dir();
        let cache = Cache::open(dir.path());
        assert_eq!(cache.load_namespaces("opc.tcp://absent/"), None);
    }

    #[test]
    fn corrupt_cache_file_is_ignored_not_fatal() {
        let dir = temp_dir();
        let cache = Cache::open(dir.path());
        let endpoint = "opc.tcp://host:4840/";
        // Write a corrupt file at the expected path.
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(cache.namespaces_path(endpoint), b"{ not valid json").unwrap();
        assert_eq!(cache.load_namespaces(endpoint), None);
    }

    #[test]
    fn endpoint_mismatch_in_payload_is_rejected() {
        // Two endpoints that sanitize to *different* stems but we deliberately
        // store one payload and read with a mismatched endpoint string by
        // hand-writing the file, to exercise the collision-safety check.
        let dir = temp_dir();
        let cache = Cache::open(dir.path());
        let endpoint = "opc.tcp://host:4840/";
        std::fs::create_dir_all(dir.path()).unwrap();
        let payload = serde_json::json!({
            "endpoint": "opc.tcp://other:4840/",
            "uris": ["http://opcfoundation.org/UA/"]
        });
        std::fs::write(
            cache.namespaces_path(endpoint),
            serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();
        assert_eq!(cache.load_namespaces(endpoint), None);
    }

    #[test]
    fn store_is_non_fatal_when_dir_unwritable() {
        // Pointing the cache at a path whose parent is a file makes directory
        // creation fail; store must swallow the error rather than panic.
        let dir = temp_dir();
        let file_path = dir.path().join("a-file");
        std::fs::write(&file_path, b"x").unwrap();
        let cache = Cache::open(&file_path.join("nested"));
        cache.store_namespaces("opc.tcp://host/", &["u".to_string()]);
        // Nothing to assert beyond "did not panic"; load returns None.
        assert_eq!(cache.load_namespaces("opc.tcp://host/"), None);
    }
}
