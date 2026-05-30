//! Traversal engine and scan orchestration.

pub mod engine;
pub mod stats;

pub use engine::{Engine, ScanConfig};

use std::str::FromStr;

use opcua::types::{NodeId, ObjectId};

use crate::error::{Result, ScanError};

/// Resolve the root node id from an optional CLI string, defaulting to the
/// standard `Objects` folder (`i=85`).
pub fn resolve_root(root: &Option<String>) -> Result<NodeId> {
    match root {
        Some(s) => NodeId::from_str(s)
            .map_err(|_| ScanError::InvalidArgument(format!("invalid --root node id '{s}'"))),
        None => Ok(NodeId::from(ObjectId::ObjectsFolder)),
    }
}
