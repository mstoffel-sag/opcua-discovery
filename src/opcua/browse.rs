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
