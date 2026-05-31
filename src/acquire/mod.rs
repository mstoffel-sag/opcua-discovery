//! Acquisition runner: connects once and drives all configured tasks.

pub mod config;
pub mod generate;
pub mod selection;
pub mod sink;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use opcua::client::Session;
use opcua::types::NodeId;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::error::{Result, ScanError};
use crate::export::NodeRecord;
use crate::filters::FilterSet;
use crate::opcua::history::{self, HistoryQuery};
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::value::read_readings;
use crate::opcua::Connection;
use crate::scanner::{resolve_root, Engine, ScanConfig};
use crate::subscription::{self, SubscriptionParams};

use config::{AcquireConfig, TaskConfig};
use selection::SelectedNode;
use sink::{Sample, Sink};

/// Run a single acquisition config to completion.
///
/// `duration` optionally bounds continuous tasks (poll/subscribe); one-shot
/// tasks (scan/history) always run to completion regardless.
pub async fn run(
    config: AcquireConfig,
    duration: Option<Duration>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let connection_args = config.connection_args()?;
    let conn = Connection::connect(&connection_args).await?;
    let session = conn.session().clone();

    let namespaces = Arc::new(
        crate::cli::load_namespaces(&session, &connection_args, conn.endpoint()).await?,
    );
    let sink = Arc::new(Sink::new(&config.output)?);

    // Resolve concrete nodes once; shared by poll/subscribe/history tasks.
    let needs_nodes = config
        .tasks
        .iter()
        .any(|t| !matches!(t, TaskConfig::Scan { .. }));
    let nodes = if needs_nodes {
        selection::resolve(&session, &config, &namespaces, cancel.clone()).await?
    } else {
        Vec::new()
    };

    if needs_nodes && nodes.is_empty() {
        warn!("no nodes matched the selection; poll/subscribe/history tasks will be idle");
    } else if needs_nodes {
        info!(count = nodes.len(), "selected nodes for acquisition");
        for node in &nodes {
            tracing::debug!(name = %node.name, path = %node.path, node_id = %node.node_id, "selected");
        }
    }

    let has_continuous = config
        .tasks
        .iter()
        .any(|t| matches!(t, TaskConfig::Poll { .. } | TaskConfig::Subscribe { .. }));

    let mut set: JoinSet<()> = JoinSet::new();
    for task in config.tasks.clone() {
        spawn_task(
            &mut set,
            task,
            session.clone(),
            sink.clone(),
            nodes.clone(),
            namespaces.clone(),
            config.clone(),
            cancel.clone(),
        );
    }

    // Bound continuous tasks by duration if requested.
    if has_continuous {
        if let Some(d) = duration {
            let timer_cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(d).await;
                timer_cancel.store(true, Ordering::Relaxed);
            });
        }
    }

    while set.join_next().await.is_some() {}

    sink.finish()?;
    conn.disconnect().await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_task(
    set: &mut JoinSet<()>,
    task: TaskConfig,
    session: Arc<Session>,
    sink: Arc<Sink>,
    nodes: Vec<SelectedNode>,
    namespaces: Arc<NamespaceTable>,
    config: AcquireConfig,
    cancel: Arc<AtomicBool>,
) {
    match task {
        TaskConfig::Scan {
            strategy,
            workers,
            page_size,
            max_children,
            preview_values,
        } => {
            set.spawn(async move {
                let result = run_scan(
                    &session,
                    &config,
                    &namespaces,
                    &sink,
                    ScanTaskParams {
                        strategy: strategy.into(),
                        workers: workers.unwrap_or(8),
                        page_size: page_size.unwrap_or(200),
                        max_children,
                        preview_values,
                    },
                    cancel,
                )
                .await;
                if let Err(e) = result {
                    warn!(error = %e, "scan task failed");
                }
            });
        }
        TaskConfig::Poll { interval } => {
            set.spawn(async move {
                run_poll(&session, &nodes, &sink, interval, cancel).await;
            });
        }
        TaskConfig::Subscribe {
            publishing_interval,
            sampling_interval,
            queue_size,
        } => {
            set.spawn(async move {
                let defaults = SubscriptionParams::default();
                let params = SubscriptionParams {
                    publishing_interval: publishing_interval
                        .unwrap_or(defaults.publishing_interval),
                    sampling_interval: sampling_interval.unwrap_or(defaults.sampling_interval),
                    queue_size: queue_size.unwrap_or(defaults.queue_size),
                };
                subscription::run_subscription(&session, nodes, params, sink, cancel).await;
            });
        }
        TaskConfig::History {
            start,
            end,
            max_values,
        } => {
            set.spawn(async move {
                let result = run_history(&session, &nodes, &sink, &start, end, max_values).await;
                if let Err(e) = result {
                    warn!(error = %e, "history task failed");
                }
            });
        }
    }
}

struct ScanTaskParams {
    strategy: crate::cli::Strategy,
    workers: usize,
    page_size: u32,
    max_children: Option<usize>,
    preview_values: bool,
}

async fn run_scan(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    sink: &Arc<Sink>,
    params: ScanTaskParams,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let filter_args = config.filter_args()?;
    let filters = Arc::new(FilterSet::compile(&filter_args, namespaces)?);
    let root = resolve_root(&config.selection.root)?;

    let scan_config = Arc::new(ScanConfig {
        root,
        depth: config.selection.depth()?,
        strategy: params.strategy,
        workers: params.workers,
        page_size: params.page_size,
        max_children: params.max_children,
        retry: config.connection.retry.unwrap_or(3),
        preview_values: params.preview_values,
    });

    let engine = Engine::new(
        session.clone(),
        scan_config,
        filters,
        namespaces.clone(),
        cancel,
    );
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    while let Some(record) = rx.recv().await {
        sink.emit_record(&record)?;
    }
    let _ = engine_task.await;
    Ok(())
}

async fn run_poll(
    session: &Arc<Session>,
    nodes: &[SelectedNode],
    sink: &Arc<Sink>,
    interval: Duration,
    cancel: Arc<AtomicBool>,
) {
    if nodes.is_empty() {
        return;
    }
    let node_ids: Vec<NodeId> = nodes.iter().map(|n| n.node_id.clone()).collect();

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        ticker.tick().await;
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        match read_readings(session, &node_ids).await {
            Ok(readings) => {
                for (node, reading) in nodes.iter().zip(readings.iter()) {
                    let sample = Sample {
                        name: node.name.clone(),
                        node_id: node.node_id.to_string(),
                        value: reading.value.clone(),
                        time: reading.best_time(),
                        status: reading.status.clone(),
                    };
                    if let Err(e) = sink.emit_sample(&sample) {
                        warn!(error = %e, "failed to emit poll sample");
                    }
                }
            }
            Err(e) => warn!(error = %e, "poll read failed"),
        }
    }
}

async fn run_history(
    session: &Arc<Session>,
    nodes: &[SelectedNode],
    sink: &Arc<Sink>,
    start: &str,
    end: Option<String>,
    max_values: Option<u32>,
) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let query = HistoryQuery {
        start: history::parse_time(start)?,
        end: match end {
            Some(e) => history::parse_time(&e)?,
            None => opcua::types::DateTime::now(),
        },
        max_values: max_values.unwrap_or(0),
    };

    for node in nodes {
        match history::read_raw(session, &node.node_id, &query).await {
            Ok(readings) => {
                for reading in &readings {
                    let sample = Sample {
                        name: node.name.clone(),
                        node_id: node.node_id.to_string(),
                        value: reading.value.clone(),
                        time: reading.best_time(),
                        status: reading.status.clone(),
                    };
                    sink.emit_sample(&sample)?;
                }
            }
            Err(e) => warn!(node = %node.node_id, error = %e, "history read failed"),
        }
    }
    Ok(())
}

/// Load every `*.toml` config in a directory (sorted), erroring if none exist.
pub fn load_config_dir(dir: &std::path::Path) -> Result<Vec<(std::path::PathBuf, AcquireConfig)>> {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| ScanError::Config {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
        .collect();
    entries.sort();

    if entries.is_empty() {
        return Err(ScanError::Config {
            path: dir.to_path_buf(),
            reason: "no *.toml config files found in directory".to_string(),
        });
    }

    let mut configs = Vec::with_capacity(entries.len());
    for path in entries {
        let config = AcquireConfig::from_file(&path)?;
        configs.push((path, config));
    }
    Ok(configs)
}
