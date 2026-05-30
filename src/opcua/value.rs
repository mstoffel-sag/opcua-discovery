//! Reading current values of variable nodes for `--preview-values`.

use opcua::client::Session;
use opcua::types::{
    AttributeId, NodeId, NumericRange, QualifiedName, ReadValueId, TimestampsToReturn,
};

use crate::error::{Result, ScanError};

/// A value snapshot read from a variable node.
#[derive(Debug, Clone, Default)]
pub struct ValueSnapshot {
    /// Rendered value (best-effort string form).
    pub value: Option<String>,
}

/// Read current values for the given variable nodes, in order.
pub async fn read_values(session: &Session, node_ids: &[NodeId]) -> Result<Vec<ValueSnapshot>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let to_read: Vec<ReadValueId> = node_ids
        .iter()
        .map(|node_id| ReadValueId {
            node_id: node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
        })
        .collect();

    let values = session
        .read(&to_read, TimestampsToReturn::Both, 0.0)
        .await
        .map_err(|s| ScanError::service("Read(Value)", s))?;

    Ok(values
        .into_iter()
        .map(|dv| ValueSnapshot {
            value: dv.value.map(|v| v.to_string()),
        })
        .collect())
}
