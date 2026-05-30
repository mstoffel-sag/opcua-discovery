//! Scan statistics and progress counters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Thread-safe counters describing scan progress.
#[derive(Debug)]
pub struct ScanStats {
    visited: AtomicU64,
    matched: AtomicU64,
    pruned: AtomicU64,
    errors: AtomicU64,
    subscribable: AtomicU64,
    truncated: AtomicU64,
    start: Instant,
}

impl Default for ScanStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanStats {
    /// Create a new set of counters, starting the clock.
    pub fn new() -> Self {
        Self {
            visited: AtomicU64::new(0),
            matched: AtomicU64::new(0),
            pruned: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            subscribable: AtomicU64::new(0),
            truncated: AtomicU64::new(0),
            start: Instant::now(),
        }
    }

    /// Increment the count of nodes visited (discovered references).
    pub fn add_visited(&self, n: u64) {
        self.visited.fetch_add(n, Ordering::Relaxed);
    }
    /// Increment matched nodes.
    pub fn inc_matched(&self) {
        self.matched.fetch_add(1, Ordering::Relaxed);
    }
    /// Increment subscribable nodes.
    pub fn inc_subscribable(&self) {
        self.subscribable.fetch_add(1, Ordering::Relaxed);
    }
    /// Increment pruned subtrees.
    pub fn inc_pruned(&self) {
        self.pruned.fetch_add(1, Ordering::Relaxed);
    }
    /// Increment errors.
    pub fn inc_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    /// Increment truncated branches.
    pub fn inc_truncated(&self) {
        self.truncated.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the current counters.
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            visited: self.visited.load(Ordering::Relaxed),
            matched: self.matched.load(Ordering::Relaxed),
            pruned: self.pruned.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            subscribable: self.subscribable.load(Ordering::Relaxed),
            truncated: self.truncated.load(Ordering::Relaxed),
            elapsed: self.start.elapsed(),
        }
    }
}

/// An immutable snapshot of [`ScanStats`].
#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    /// Number of nodes visited.
    pub visited: u64,
    /// Number of nodes matched and emitted.
    pub matched: u64,
    /// Number of subtrees pruned by exclude rules.
    pub pruned: u64,
    /// Number of errors encountered (and skipped).
    pub errors: u64,
    /// Number of subscribable nodes.
    pub subscribable: u64,
    /// Number of branches truncated by --max-children.
    pub truncated: u64,
    /// Elapsed wall-clock time.
    pub elapsed: std::time::Duration,
}
