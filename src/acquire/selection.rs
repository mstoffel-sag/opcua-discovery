//! Node selection for acquisition tasks.
//!
//! Selection combines two sources: explicit node ids listed in the config and
//! filter-based discovery that runs the scan engine and keeps matching variable
//! nodes. The result is a de-duplicated list of nodes with a friendly
//! measurement name for output.

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use opcua::client::Session;
use opcua::types::NodeId;
use tokio::sync::mpsc;
use tracing::warn;

use crate::acquire::config::AcquireConfig;
use crate::error::{Result, ScanError};
use crate::export::NodeRecord;
use crate::filters::FilterSet;
use crate::opcua::attributes::read_node_attributes;
use crate::opcua::namespaces::NamespaceTable;
use crate::scanner::{resolve_root, Engine, ScanConfig};

/// A selected node ready for sampling.
#[derive(Debug, Clone)]
pub struct SelectedNode {
    /// Measurement name used in output.
    pub name: String,
    /// Full browse path (for diagnostics).
    pub path: String,
    /// Node id.
    pub node_id: NodeId,
}

/// Resolve the configured selection into a concrete list of nodes.
pub async fn resolve(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<SelectedNode>> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut nodes: Vec<SelectedNode> = Vec::new();

    // 1. Explicit nodes (rich `[[selection.node]]` entries) + legacy `node_ids`.
    let explicit = resolve_explicit(
        session,
        &config.selection.nodes,
        &config.selection.node_ids,
        namespaces,
    )
    .await?;
    for node in explicit {
        if seen.insert(node.node_id.clone()) {
            nodes.push(node);
        }
    }

    // 2. Filter-based discovery (only when filters are present).
    if has_filters(config) {
        let discovered = resolve_filtered(session, config, namespaces, cancel).await?;
        for node in discovered {
            if seen.insert(node.node_id.clone()) {
                nodes.push(node);
            }
        }
    }

    Ok(nodes)
}

fn has_filters(config: &AcquireConfig) -> bool {
    let s = &config.selection;
    !s.include.is_empty()
        || !s.include_regex.is_empty()
        || !s.node_class.is_empty()
        || !s.datatype.is_empty()
        || s.root.is_some()
}

async fn resolve_explicit(
    session: &Session,
    nodes: &[crate::acquire::config::NodeSelector],
    legacy_ids: &[String],
    namespaces: &NamespaceTable,
) -> Result<Vec<SelectedNode>> {
    if nodes.is_empty() && legacy_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Parse + remap every selector to a concrete (live) NodeId, keeping any
    // configured friendly name alongside it.
    let mut parsed: Vec<(NodeId, Option<String>)> = Vec::with_capacity(nodes.len() + legacy_ids.len());
    for node in nodes {
        let id = remap_node_id(&node.id, node.namespace.as_deref(), namespaces)?;
        parsed.push((id, node.name.clone()));
    }
    for id in legacy_ids {
        // Legacy bare ids carry no namespace URI: use the literal index as-is.
        let node_id = remap_node_id(id, None, namespaces)?;
        parsed.push((node_id, None));
    }

    let ids: Vec<NodeId> = parsed.iter().map(|(id, _)| id.clone()).collect();
    let attrs = read_node_attributes(session, &ids).await?;
    let mut out = Vec::with_capacity(parsed.len());
    for ((id, configured_name), attr) in parsed.into_iter().zip(attrs) {
        let name = configured_name
            .or_else(|| attr.display_name.clone())
            .unwrap_or_else(|| id.to_string());
        out.push(SelectedNode {
            path: name_or_id(&attr.display_name, &id),
            name,
            node_id: id,
        });
    }
    Ok(out)
}

/// Resolve a node id string to a concrete [`NodeId`], remapping the namespace
/// index from a stable URI when one is supplied.
///
/// When `ns_uri` is `Some`, the index encoded in `id` is replaced with the
/// server's current index for that URI, so the selection survives the server
/// reordering its namespace array. An error is returned if the URI is not
/// present on the server (better to fail loudly than silently sample the wrong
/// node). When `ns_uri` is `None` (legacy ids), the id is used verbatim.
pub(crate) fn remap_node_id(
    id: &str,
    ns_uri: Option<&str>,
    namespaces: &NamespaceTable,
) -> Result<NodeId> {
    let parsed =
        NodeId::from_str(id).map_err(|_| ScanError::InvalidArgument(format!("invalid node id '{id}'")))?;
    match ns_uri {
        Some(uri) if !uri.is_empty() => {
            let index = namespaces.index(uri).ok_or_else(|| {
                ScanError::InvalidArgument(format!(
                    "namespace '{uri}' (for node '{id}') is not present on the server"
                ))
            })?;
            Ok(NodeId::new(index, parsed.identifier.clone()))
        }
        _ => Ok(parsed),
    }
}

fn name_or_id(display_name: &Option<String>, id: &NodeId) -> String {
    display_name.clone().unwrap_or_else(|| id.to_string())
}

async fn resolve_filtered(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<SelectedNode>> {
    // Only variables can be sampled.
    let mut filter_args = config.filter_args()?;
    filter_args.subscribable = true;
    let filters = Arc::new(FilterSet::compile(&filter_args, namespaces)?);
    let root = resolve_root(&config.selection.root)?;

    let scan_config = Arc::new(ScanConfig {
        root,
        depth: config.selection.depth()?,
        strategy: crate::cli::Strategy::Bfs,
        workers: 8,
        page_size: 200,
        max_children: None,
        retry: config.connection.retry.unwrap_or(3),
        preview_values: false,
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

    let mut nodes = Vec::new();
    while let Some(record) = rx.recv().await {
        match NodeId::from_str(&record.node_id) {
            Ok(node_id) => nodes.push(SelectedNode {
                name: measurement_name(&record),
                path: record.browse_path,
                node_id,
            }),
            Err(_) => warn!(node_id = %record.node_id, "skipping unparseable node id"),
        }
    }
    let _ = engine_task.await;
    Ok(nodes)
}

/// Choose a measurement name for a discovered record (display name, else the
/// last browse path segment, else the node id).
fn measurement_name(record: &NodeRecord) -> String {
    if !record.display_name.is_empty() {
        return record.display_name.clone();
    }
    record
        .browse_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| record.node_id.clone())
}
