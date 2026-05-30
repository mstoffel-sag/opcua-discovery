//! `browse` subcommand: the primary discovery command.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Args;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tokio::sync::mpsc;
use tracing::info;

use crate::cli::{ConnectionArgs, FilterArgs, OutputArgs, TraversalArgs};
use crate::error::Result;
use crate::export::{render_display, write_export, NodeRecord};
use crate::filters::FilterSet;
use crate::opcua::{Connection, NamespaceTable};
use crate::scanner::{resolve_root, Engine, ScanConfig};
use crate::subscription;

/// Arguments for the `browse` command.
#[derive(Debug, Args)]
pub struct BrowseArgs {
    /// Connection / authentication options.
    #[command(flatten)]
    pub connection: ConnectionArgs,
    /// Traversal control.
    #[command(flatten)]
    pub traversal: TraversalArgs,
    /// Node filters.
    #[command(flatten)]
    pub filters: FilterArgs,
    /// Output / export options.
    #[command(flatten)]
    pub output: OutputArgs,

    /// Validate that subscribable nodes can actually be monitored.
    #[arg(long)]
    pub validate_subscription: bool,
}

/// Run the `browse` command.
pub async fn run(args: BrowseArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    let conn = Connection::connect(&args.connection).await?;
    let session = conn.session().clone();

    let namespaces =
        Arc::new(crate::cli::load_namespaces(&session, &args.connection, conn.endpoint()).await?);
    let filters = Arc::new(FilterSet::compile(&args.filters, &namespaces)?);
    let root = resolve_root(&args.traversal.root)?;

    // Interactive explorer: opt-in, replaces streaming/export output entirely.
    if args.output.tui {
        let options = crate::tui::TuiOptions {
            root,
            page_size: args.traversal.page_size,
            export_file: args.output.file.clone(),
        };
        let result = crate::tui::run(session, namespaces, filters, options).await;
        conn.disconnect().await;
        return result;
    }

    let config = Arc::new(ScanConfig {
        root,
        depth: args.traversal.depth,
        strategy: args.traversal.strategy,
        workers: args.traversal.workers,
        page_size: args.traversal.page_size,
        max_children: args.traversal.max_children,
        retry: args.connection.retry,
        preview_values: args.output.preview_values,
    });

    // Watch mode repeats the scan periodically until interrupted.
    loop {
        scan_once(
            &session,
            &config,
            &filters,
            &namespaces,
            &args,
            cancel.clone(),
        )
        .await?;

        match args.output.watch {
            Some(interval) if !cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                eprintln!("\n— watching; next scan in {interval:?} (Ctrl+C to stop) —");
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = wait_for_cancel(cancel.clone()) => {}
                }
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
            _ => break,
        }
    }

    conn.disconnect().await;
    Ok(())
}

/// Poll the cancel flag until it is set.
async fn wait_for_cancel(cancel: Arc<AtomicBool>) {
    while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Perform a single scan, then render or export the results.
async fn scan_once(
    session: &Arc<opcua::client::Session>,
    config: &Arc<ScanConfig>,
    filters: &Arc<FilterSet>,
    namespaces: &Arc<NamespaceTable>,
    args: &BrowseArgs,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let engine = Engine::new(
        session.clone(),
        config.clone(),
        filters.clone(),
        namespaces.clone(),
        cancel.clone(),
    );
    let stats = engine.stats();

    // Channel for streaming matched records.
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);

    // Run the engine on a task so we can consume results concurrently.
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    // A lightweight progress spinner on stderr (only when attached to a TTY).
    let progress = make_progress();

    let mut records: Vec<NodeRecord> = Vec::new();
    while let Some(record) = rx.recv().await {
        records.push(record);
        if let Some(pb) = &progress {
            let snap = stats.snapshot();
            pb.set_message(format!(
                "visited {}  matched {}  subscribable {}",
                snap.visited, snap.matched, snap.subscribable
            ));
            pb.tick();
        }
    }
    let _ = engine_task.await;
    if let Some(pb) = &progress {
        pb.finish_and_clear();
    }

    // Optionally validate subscriptions for subscribable nodes.
    if args.validate_subscription {
        subscription::validate_records(session, &mut records, cancel.clone()).await;
    }

    // Render or export.
    if let Some(encoding) = args.output.output {
        write_export(&mut records, encoding, args.output.file.as_deref())?;
    } else {
        render_display(&mut records, args.output.format)?;
    }

    let snap = stats.snapshot();
    info!(
        visited = snap.visited,
        matched = snap.matched,
        subscribable = snap.subscribable,
        pruned = snap.pruned,
        truncated = snap.truncated,
        errors = snap.errors,
        elapsed = ?snap.elapsed,
        "scan complete"
    );
    eprintln!(
        "\nvisited: {}  matched: {}  subscribable: {}  pruned: {}  truncated: {}  errors: {}  elapsed: {:.1?}",
        snap.visited,
        snap.matched,
        snap.subscribable,
        snap.pruned,
        snap.truncated,
        snap.errors,
        snap.elapsed
    );

    Ok(())
}

/// Create a spinner progress bar, but only when stderr is an interactive
/// terminal (so piped/redirected output stays clean).
fn make_progress() -> Option<ProgressBar> {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.set_style(
        ProgressStyle::with_template("{spinner} scanning… {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    Some(pb)
}
