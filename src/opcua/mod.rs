//! OPC UA integration layer.
//!
//! This module wraps the `async-opcua` client API with the pieces `opcua-scan`
//! needs: building a session from CLI connection arguments, reading the
//! namespace table, batch-reading node attributes and values, and resolving
//! node ids / data types.

pub mod attributes;
pub mod browse;
pub mod client;
pub mod history;
pub mod namespaces;
pub mod value;

pub use client::Connection;
pub use namespaces::NamespaceTable;

use opcua::types::NodeClass;

use crate::cli::NodeClassArg;

/// Convert a CLI [`NodeClassArg`] to the wire [`NodeClass`].
pub fn node_class_of(arg: NodeClassArg) -> NodeClass {
    match arg {
        NodeClassArg::Object => NodeClass::Object,
        NodeClassArg::Variable => NodeClass::Variable,
        NodeClassArg::Method => NodeClass::Method,
        NodeClassArg::ObjectType => NodeClass::ObjectType,
        NodeClassArg::VariableType => NodeClass::VariableType,
        NodeClassArg::ReferenceType => NodeClass::ReferenceType,
        NodeClassArg::DataType => NodeClass::DataType,
        NodeClassArg::View => NodeClass::View,
    }
}

/// Human friendly name for a [`NodeClass`].
pub fn node_class_name(class: NodeClass) -> &'static str {
    match class {
        NodeClass::Object => "Object",
        NodeClass::Variable => "Variable",
        NodeClass::Method => "Method",
        NodeClass::ObjectType => "ObjectType",
        NodeClass::VariableType => "VariableType",
        NodeClass::ReferenceType => "ReferenceType",
        NodeClass::DataType => "DataType",
        NodeClass::View => "View",
        NodeClass::Unspecified => "Unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::NodeClassArg;

    #[test]
    fn cli_arg_maps_to_wire_node_class() {
        assert_eq!(node_class_of(NodeClassArg::Object), NodeClass::Object);
        assert_eq!(node_class_of(NodeClassArg::Variable), NodeClass::Variable);
        assert_eq!(node_class_of(NodeClassArg::Method), NodeClass::Method);
        assert_eq!(
            node_class_of(NodeClassArg::ObjectType),
            NodeClass::ObjectType
        );
        assert_eq!(
            node_class_of(NodeClassArg::VariableType),
            NodeClass::VariableType
        );
        assert_eq!(
            node_class_of(NodeClassArg::ReferenceType),
            NodeClass::ReferenceType
        );
        assert_eq!(node_class_of(NodeClassArg::DataType), NodeClass::DataType);
        assert_eq!(node_class_of(NodeClassArg::View), NodeClass::View);
    }

    #[test]
    fn every_node_class_has_a_stable_name() {
        for (class, name) in [
            (NodeClass::Object, "Object"),
            (NodeClass::Variable, "Variable"),
            (NodeClass::Method, "Method"),
            (NodeClass::ObjectType, "ObjectType"),
            (NodeClass::VariableType, "VariableType"),
            (NodeClass::ReferenceType, "ReferenceType"),
            (NodeClass::DataType, "DataType"),
            (NodeClass::View, "View"),
            (NodeClass::Unspecified, "Unspecified"),
        ] {
            assert_eq!(node_class_name(class), name);
        }
    }
}
