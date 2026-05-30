//! Batch reading of node attributes.

use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, NodeClass, NodeId, NumericRange, QualifiedName, ReadValueId,
    TimestampsToReturn, Variant,
};

use crate::error::{Result, ScanError};

/// AccessLevel / UserAccessLevel bit flags (OPC UA Part 3).
pub mod access {
    /// The current value can be read.
    pub const CURRENT_READ: u8 = 0x01;
    /// The current value can be written.
    pub const CURRENT_WRITE: u8 = 0x02;
}

/// Metadata attributes for a single node.
#[derive(Debug, Clone, Default)]
pub struct NodeAttributes {
    /// The node's class, if returned.
    pub node_class: Option<NodeClass>,
    /// Display name.
    pub display_name: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// DataType node id (for Variables / VariableTypes).
    pub data_type: Option<NodeId>,
    /// Value rank (scalar = -1, array dimensions otherwise).
    pub value_rank: Option<i32>,
    /// AccessLevel bitfield.
    pub access_level: Option<u8>,
    /// UserAccessLevel bitfield.
    pub user_access_level: Option<u8>,
    /// Historizing flag.
    pub historizing: Option<bool>,
}

impl NodeAttributes {
    /// Whether the current value is readable.
    pub fn is_readable(&self) -> bool {
        self.access_level
            .map(|a| a & access::CURRENT_READ != 0)
            .unwrap_or(false)
    }

    /// Whether the current value is writable.
    pub fn is_writable(&self) -> bool {
        self.access_level
            .map(|a| a & access::CURRENT_WRITE != 0)
            .unwrap_or(false)
    }

    /// Whether this node is historizing.
    pub fn is_historizing(&self) -> bool {
        self.historizing.unwrap_or(false)
    }
}

/// Attributes requested for each node, in a fixed order.
const REQUESTED: &[AttributeId] = &[
    AttributeId::NodeClass,
    AttributeId::DisplayName,
    AttributeId::Description,
    AttributeId::DataType,
    AttributeId::ValueRank,
    AttributeId::AccessLevel,
    AttributeId::UserAccessLevel,
    AttributeId::Historizing,
];

/// Read the standard attribute set for many nodes in a single service call.
///
/// Returns one [`NodeAttributes`] per input node, in the same order. Missing or
/// failed attributes are left as `None` rather than failing the whole batch.
pub async fn read_node_attributes(
    session: &Session,
    node_ids: &[NodeId],
) -> Result<Vec<NodeAttributes>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut to_read = Vec::with_capacity(node_ids.len() * REQUESTED.len());
    for node_id in node_ids {
        for attr in REQUESTED {
            to_read.push(ReadValueId {
                node_id: node_id.clone(),
                attribute_id: *attr as u32,
                index_range: NumericRange::None,
                data_encoding: QualifiedName::null(),
            });
        }
    }

    let values = session
        .read(&to_read, TimestampsToReturn::Neither, 0.0)
        .await
        .map_err(|s| ScanError::service("Read(Attributes)", s))?;

    let stride = REQUESTED.len();
    let mut result = Vec::with_capacity(node_ids.len());
    for chunk in values.chunks(stride) {
        result.push(parse_attributes(chunk));
    }
    // Defensive: ensure length matches even if the server returned fewer values.
    result.resize(node_ids.len(), NodeAttributes::default());
    Ok(result)
}

/// Parse a single node's attribute chunk (aligned to [`REQUESTED`]).
fn parse_attributes(chunk: &[DataValue]) -> NodeAttributes {
    let mut attrs = NodeAttributes::default();
    for (attr, dv) in REQUESTED.iter().zip(chunk.iter()) {
        let Some(value) = dv.value.as_ref() else {
            continue;
        };
        match attr {
            AttributeId::NodeClass => {
                if let Variant::Int32(n) = value {
                    attrs.node_class = node_class_from_i32(*n);
                }
            }
            AttributeId::DisplayName => {
                if let Variant::LocalizedText(t) = value {
                    attrs.display_name = Some(t.text.to_string());
                }
            }
            AttributeId::Description => {
                if let Variant::LocalizedText(t) = value {
                    let text = t.text.to_string();
                    if !text.is_empty() {
                        attrs.description = Some(text);
                    }
                }
            }
            AttributeId::DataType => {
                if let Variant::NodeId(id) = value {
                    attrs.data_type = Some((**id).clone());
                }
            }
            AttributeId::ValueRank => {
                if let Variant::Int32(n) = value {
                    attrs.value_rank = Some(*n);
                }
            }
            AttributeId::AccessLevel => {
                attrs.access_level = as_u8(value);
            }
            AttributeId::UserAccessLevel => {
                attrs.user_access_level = as_u8(value);
            }
            AttributeId::Historizing => {
                if let Variant::Boolean(b) = value {
                    attrs.historizing = Some(*b);
                }
            }
            _ => {}
        }
    }
    attrs
}

fn as_u8(value: &Variant) -> Option<u8> {
    match value {
        Variant::Byte(b) => Some(*b),
        Variant::SByte(b) => Some(*b as u8),
        Variant::UInt16(n) => Some(*n as u8),
        Variant::Int32(n) => Some(*n as u8),
        _ => None,
    }
}

fn node_class_from_i32(n: i32) -> Option<NodeClass> {
    Some(match n {
        1 => NodeClass::Object,
        2 => NodeClass::Variable,
        4 => NodeClass::Method,
        8 => NodeClass::ObjectType,
        16 => NodeClass::VariableType,
        32 => NodeClass::ReferenceType,
        64 => NodeClass::DataType,
        128 => NodeClass::View,
        _ => return None,
    })
}
