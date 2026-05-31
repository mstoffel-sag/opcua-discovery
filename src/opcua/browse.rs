//! A small, standalone "browse one node's children" helper.
//!
//! The traversal [`crate::scanner::engine::Engine`] has its own internal browse
//! loop tuned for the concurrent scan. The interactive TUI, however, needs to
//! expand a *single* node on demand, so this module exposes a simpler one-shot
//! browse that follows continuation points.

use opcua::client::Session;
use opcua::types::{
    BrowseDescription, BrowseDirection, BrowseResultMask, NodeId, ReferenceDescription,
    ReferenceTypeId,
};

use crate::error::{Result, ScanError};

/// Browse all hierarchical forward children of `node_id`, following any
/// continuation points. Used for lazy expansion in the TUI.
pub async fn browse_children(
    session: &Session,
    node_id: &NodeId,
    page_size: u32,
) -> Result<Vec<ReferenceDescription>> {
    let desc = BrowseDescription {
        node_id: node_id.clone(),
        browse_direction: BrowseDirection::Forward,
        reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
        include_subtypes: true,
        node_class_mask: 0,
        result_mask: BrowseResultMask::All as u32,
    };

    let mut results = session
        .browse(&[desc], page_size, None)
        .await
        .map_err(|s| ScanError::service("Browse", s))?;
    let mut result = results.drain(..).next().ok_or_else(|| {
        ScanError::service("Browse", opcua::types::StatusCode::BadUnexpectedError)
    })?;

    let mut refs = Vec::new();
    loop {
        if !result.status_code.is_good() && result.references.is_none() {
            return Err(ScanError::service("Browse", result.status_code));
        }
        if let Some(rs) = result.references.take() {
            refs.extend(rs);
        }
        if result.continuation_point.is_null() {
            break;
        }
        let cp = result.continuation_point.clone();
        let mut next = session
            .browse_next(false, &[cp])
            .await
            .map_err(|s| ScanError::service("BrowseNext", s))?;
        result = next.drain(..).next().ok_or_else(|| {
            ScanError::service("BrowseNext", opcua::types::StatusCode::BadUnexpectedError)
        })?;
    }

    Ok(refs)
}

/// Browse the forward `HasProperty` references of each node, returning one list
/// of property references per input node (aligned to `node_ids`).
///
/// Used to discover metadata properties such as `EURange` and
/// `EngineeringUnits` that hang off an AnalogItem variable. Property sets are
/// tiny, so continuation points are not followed here.
pub async fn browse_properties(
    session: &Session,
    node_ids: &[NodeId],
) -> Result<Vec<Vec<ReferenceDescription>>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let to_browse: Vec<BrowseDescription> = node_ids
        .iter()
        .map(|node_id| BrowseDescription {
            node_id: node_id.clone(),
            browse_direction: BrowseDirection::Forward,
            reference_type_id: ReferenceTypeId::HasProperty.into(),
            include_subtypes: true,
            node_class_mask: 0,
            result_mask: BrowseResultMask::All as u32,
        })
        .collect();

    let results = session
        .browse(&to_browse, 0, None)
        .await
        .map_err(|s| ScanError::service("Browse(Properties)", s))?;

    let mut out: Vec<Vec<ReferenceDescription>> = Vec::with_capacity(node_ids.len());
    for result in results {
        out.push(result.references.unwrap_or_default());
    }
    out.resize(node_ids.len(), Vec::new());
    Ok(out)
}

/// Reconstruct the full hierarchical browse path of a node by walking inverse
/// `HierarchicalReferences` up to the `Objects` (or `Root`) folder.
///
/// `leaf` is the node's own path segment (its browse name). The returned path
/// uses `/`-separated browse names and excludes the stop folder, matching the
/// convention used by the scan engine (e.g. `Boiler #1/LX001/ControlOut`).
/// Returns `None` if no hierarchical parent chain can be found.
pub async fn reconstruct_browse_path(
    session: &Session,
    node_id: &NodeId,
    leaf: &str,
) -> Result<Option<String>> {
    /// `Objects` folder node id (ns=0, i=85) — the conventional path root.
    const OBJECTS: u32 = 85;
    /// `Root` folder node id (ns=0, i=84) — the absolute address-space root.
    const ROOT: u32 = 84;
    /// Safety bound on the number of ancestors walked (guards against cycles).
    const MAX_DEPTH: usize = 32;

    let is_stop = |id: &NodeId| {
        id.namespace == 0
            && matches!(&id.identifier, opcua::types::Identifier::Numeric(n) if *n == OBJECTS || *n == ROOT)
    };

    let mut segments: Vec<String> = vec![leaf.to_string()];
    let mut current = node_id.clone();

    for _ in 0..MAX_DEPTH {
        let Some((parent_id, parent_name)) = browse_parent(session, &current).await? else {
            break;
        };
        if is_stop(&parent_id) {
            break;
        }
        segments.push(parent_name);
        current = parent_id;
    }

    if segments.iter().all(|s| s.is_empty()) {
        return Ok(None);
    }
    segments.reverse();
    Ok(Some(segments.join("/")))
}

/// Browse the first inverse `HierarchicalReferences` parent of `node_id`,
/// returning its node id and browse-name path segment.
async fn browse_parent(session: &Session, node_id: &NodeId) -> Result<Option<(NodeId, String)>> {
    let desc = BrowseDescription {
        node_id: node_id.clone(),
        browse_direction: BrowseDirection::Inverse,
        reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
        include_subtypes: true,
        node_class_mask: 0,
        result_mask: BrowseResultMask::All as u32,
    };

    let mut results = session
        .browse(&[desc], 0, None)
        .await
        .map_err(|s| ScanError::service("Browse(Parent)", s))?;
    let Some(result) = results.drain(..).next() else {
        return Ok(None);
    };
    let Some(refs) = result.references else {
        return Ok(None);
    };

    for reference in refs {
        let parent_id = reference.node_id.node_id.clone();
        if parent_id.is_null() {
            continue;
        }
        let segment = {
            let browse = reference.browse_name.name.to_string();
            if !browse.is_empty() {
                browse
            } else {
                reference.display_name.text.to_string()
            }
        };
        return Ok(Some((parent_id, segment)));
    }
    Ok(None)
}
