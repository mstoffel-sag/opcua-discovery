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
    /// Browse name (namespace-qualified text, without the index prefix).
    pub browse_name: Option<String>,
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

    /// Whether this node is a Variable the server reports as *not* currently
    /// readable (its AccessLevel is present but lacks the CurrentRead bit).
    ///
    /// Reading such a node returns `BadNotReadable`, so it cannot be sampled by
    /// any acquisition mode. Returns `false` when the AccessLevel is unknown so
    /// nodes are only excluded on explicit evidence.
    pub fn is_unreadable_variable(&self) -> bool {
        matches!(self.node_class, Some(NodeClass::Variable))
            && self
                .access_level
                .map(|a| a & access::CURRENT_READ == 0)
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
    AttributeId::BrowseName,
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
            AttributeId::BrowseName => {
                if let Variant::QualifiedName(q) = value {
                    let text = q.name.to_string();
                    if !text.is_empty() {
                        attrs.browse_name = Some(text);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(access: Option<u8>) -> NodeAttributes {
        NodeAttributes {
            node_class: Some(NodeClass::Variable),
            access_level: access,
            ..Default::default()
        }
    }

    #[test]
    fn readable_and_writable_track_access_level_bits() {
        let ro = variable(Some(access::CURRENT_READ));
        assert!(ro.is_readable());
        assert!(!ro.is_writable());

        let rw = variable(Some(access::CURRENT_READ | access::CURRENT_WRITE));
        assert!(rw.is_readable());
        assert!(rw.is_writable());

        let wo = variable(Some(access::CURRENT_WRITE));
        assert!(!wo.is_readable());
        assert!(wo.is_writable());
    }

    #[test]
    fn unknown_access_level_is_treated_as_not_readable() {
        // No evidence either way -> not readable/writable, but also NOT flagged
        // as an unreadable variable (we only exclude on explicit evidence).
        let unknown = variable(None);
        assert!(!unknown.is_readable());
        assert!(!unknown.is_writable());
        assert!(!unknown.is_unreadable_variable());
    }

    #[test]
    fn unreadable_variable_requires_explicit_no_read_bit() {
        // A Variable whose AccessLevel is present but lacks CurrentRead.
        let no_read = variable(Some(access::CURRENT_WRITE));
        assert!(no_read.is_unreadable_variable());

        // A readable variable is not "unreadable".
        let readable = variable(Some(access::CURRENT_READ));
        assert!(!readable.is_unreadable_variable());

        // Non-variables are never flagged regardless of access level.
        let object = NodeAttributes {
            node_class: Some(NodeClass::Object),
            access_level: Some(0),
            ..Default::default()
        };
        assert!(!object.is_unreadable_variable());
    }

    #[test]
    fn historizing_defaults_to_false_when_unknown() {
        let attrs = NodeAttributes::default();
        assert!(!attrs.is_historizing());
        let hist = NodeAttributes {
            historizing: Some(true),
            ..Default::default()
        };
        assert!(hist.is_historizing());
    }

    #[test]
    fn node_class_from_i32_maps_known_bits_only() {
        assert_eq!(node_class_from_i32(2), Some(NodeClass::Variable));
        assert_eq!(node_class_from_i32(128), Some(NodeClass::View));
        assert_eq!(node_class_from_i32(0), None);
        assert_eq!(node_class_from_i32(3), None);
    }

    #[test]
    fn as_u8_accepts_common_integer_variants() {
        assert_eq!(as_u8(&Variant::Byte(5)), Some(5));
        assert_eq!(as_u8(&Variant::Int32(1)), Some(1));
        assert_eq!(as_u8(&Variant::UInt16(3)), Some(3));
        assert_eq!(as_u8(&Variant::Boolean(true)), None);
    }
}
