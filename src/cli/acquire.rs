//! `acquire` subcommand: run config-driven data acquisition (scan, poll,
//! subscribe, history) against an OPC UA server.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use tracing::info;

use crate::acquire::{self, config::AcquireConfig};
use crate::cli::parse_duration;
use crate::error::{Result, ScanError};

/// Arguments for the `acquire` command.
#[derive(Debug, Args)]
pub struct AcquireArgs {
    /// Acquisition config file (TOML).
    #[arg(long, value_name = "FILE", conflicts_with = "config_dir")]
    pub config: Option<PathBuf>,

    /// Directory of acquisition config files (`*.toml`), run in order.
    #[arg(long = "config-dir", value_name = "DIR")]
    pub config_dir: Option<PathBuf>,

    /// Bound continuous tasks (poll/subscribe) to this duration, e.g. 30s, 5m.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub duration: Option<Duration>,
}

/// Run the `acquire` command.
pub async fn run(args: AcquireArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    let configs: Vec<(PathBuf, AcquireConfig)> = match (&args.config, &args.config_dir) {
        (Some(path), _) => vec![(path.clone(), AcquireConfig::from_file(path)?)],
        (None, Some(dir)) => acquire::load_config_dir(dir)?,
        (None, None) => {
            return Err(ScanError::InvalidArgument(
                "either --config or --config-dir is required".into(),
            ))
        }
    };

    for (path, config) in configs {
        info!(config = %path.display(), "starting acquisition");
        acquire::run(config, args.duration, Some(path.as_path()), cancel.clone()).await?;
    }

    Ok(())
}
