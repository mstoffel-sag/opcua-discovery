//! Error types for `opcua-scan`.
//!
//! A single [`ScanError`] enum is used across the crate so that the binary can
//! map failures to clear, actionable messages and stable process exit codes.

use std::path::PathBuf;

use opcua::types::StatusCode;
use thiserror::Error;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ScanError>;

/// All errors that `opcua-scan` can produce.
#[derive(Debug, Error)]
pub enum ScanError {
    /// The provided endpoint URL could not be parsed or is unsupported.
    #[error("invalid endpoint URL '{0}': must start with opc.tcp://")]
    InvalidEndpoint(String),

    /// Failed to build the OPC UA client (bad certificate paths, etc.).
    #[error("failed to build OPC UA client: {0}")]
    ClientBuild(String),

    /// The session failed to connect within the configured timeout.
    #[error("failed to connect to '{endpoint}': {reason}")]
    Connect {
        /// The endpoint that was being connected to.
        endpoint: String,
        /// Human readable reason.
        reason: String,
    },

    /// Connection timed out.
    #[error("timed out after {0:?} waiting for the session to connect")]
    ConnectTimeout(std::time::Duration),

    /// An OPC UA service call returned a bad status code.
    #[error("OPC UA service '{service}' failed: {status}")]
    Service {
        /// The service that was called (e.g. "Browse").
        service: &'static str,
        /// The OPC UA status code returned.
        status: StatusCode,
    },

    /// A requested credential could not be resolved.
    #[error("credential error: {0}")]
    Credential(String),

    /// A profile or configuration file problem.
    #[error("config error in '{path}': {reason}")]
    Config {
        /// Path to the offending config file.
        path: PathBuf,
        /// Human readable reason.
        reason: String,
    },

    /// Invalid command-line argument combination or value.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// An invalid include/exclude pattern.
    #[error("invalid pattern '{pattern}': {reason}")]
    Pattern {
        /// The offending pattern text.
        pattern: String,
        /// Reason the pattern is invalid.
        reason: String,
    },

    /// Output/export failure.
    #[error("export error: {0}")]
    Export(String),

    /// Terminal/TUI failure.
    #[error("terminal error: {0}")]
    Terminal(String),

    /// Wrapper for I/O errors.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Wrapper for any other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ScanError {
    /// Build a [`ScanError::Service`] from a service name and status code.
    pub fn service(service: &'static str, status: StatusCode) -> Self {
        ScanError::Service { service, status }
    }

    /// Process exit code associated with this error.
    ///
    /// Exit codes are kept stable so the tool can be used reliably in scripts:
    /// * `1` general failure
    /// * `2` invalid usage / arguments
    /// * `3` connection failure
    /// * `4` cancelled
    pub fn exit_code(&self) -> i32 {
        match self {
            ScanError::InvalidArgument(_)
            | ScanError::InvalidEndpoint(_)
            | ScanError::Pattern { .. }
            | ScanError::Config { .. } => 2,
            ScanError::Connect { .. }
            | ScanError::ConnectTimeout(_)
            | ScanError::ClientBuild(_) => 3,
            _ => 1,
        }
    }
}
