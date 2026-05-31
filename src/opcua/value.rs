//! Reading current values of variable nodes for `--preview-values`.

use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, NodeId, NumericRange, QualifiedName, ReadValueId, TimestampsToReturn,
    Variant,
};

use crate::error::{Result, ScanError};

/// A value snapshot read from a variable node.
#[derive(Debug, Clone, Default)]
pub struct ValueSnapshot {
    /// Rendered value (best-effort string form).
    pub value: Option<String>,
}

/// A value sample with timestamp and status, used by the acquisition tasks.
#[derive(Debug, Clone)]
pub struct ValueReading {
    /// The value as a JSON value (numbers stay numeric for thin-edge JSON).
    pub value: serde_json::Value,
    /// Source timestamp (RFC3339), when provided by the server.
    pub source_time: Option<String>,
    /// Server timestamp (RFC3339), when provided by the server.
    pub server_time: Option<String>,
    /// Status code, when not Good.
    pub status: Option<String>,
}

impl ValueReading {
    /// Build a reading from an OPC UA [`DataValue`].
    pub fn from_data_value(dv: &DataValue) -> Self {
        let value = dv
            .value
            .as_ref()
            .map(variant_to_json)
            .unwrap_or(serde_json::Value::Null);
        let status = dv
            .status
            .filter(|s| !s.is_good())
            .map(|s| s.to_string());
        ValueReading {
            value,
            source_time: dv.source_timestamp.as_ref().map(|t| t.to_rfc3339()),
            server_time: dv.server_timestamp.as_ref().map(|t| t.to_rfc3339()),
            status,
        }
    }

    /// Best available timestamp (source preferred over server).
    pub fn best_time(&self) -> Option<String> {
        self.source_time.clone().or_else(|| self.server_time.clone())
    }
}

/// Convert an OPC UA [`Variant`] into a JSON value, keeping numeric and boolean
/// types native so downstream consumers (e.g. thin-edge.io) receive real
/// numbers rather than quoted strings. Unsupported types fall back to a string.
pub fn variant_to_json(v: &Variant) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Variant::Empty => Value::Null,
        Variant::Boolean(b) => Value::Bool(*b),
        Variant::SByte(n) => Value::from(*n),
        Variant::Byte(n) => Value::from(*n),
        Variant::Int16(n) => Value::from(*n),
        Variant::UInt16(n) => Value::from(*n),
        Variant::Int32(n) => Value::from(*n),
        Variant::UInt32(n) => Value::from(*n),
        Variant::Int64(n) => Value::from(*n),
        Variant::UInt64(n) => Value::from(*n),
        Variant::Float(n) => serde_json::Number::from_f64(*n as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Variant::Double(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Variant::String(s) => Value::String(s.to_string()),
        Variant::Array(arr) => {
            Value::Array(arr.values.iter().map(variant_to_json).collect())
        }
        other => Value::String(other.to_string()),
    }
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

/// Read current values for the given nodes, returning full readings (value,
/// timestamps and status) in the same order as `node_ids`.
pub async fn read_readings(session: &Session, node_ids: &[NodeId]) -> Result<Vec<ValueReading>> {
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

    Ok(values.iter().map(ValueReading::from_data_value).collect())
}
