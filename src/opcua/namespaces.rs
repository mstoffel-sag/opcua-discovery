//! Namespace table handling.
//!
//! Namespace *indexes* are session-local and can change between connections, so
//! `opcua-scan` resolves them to stable namespace *URIs* for display and
//! filtering. The table is read once from the server's `Server_NamespaceArray`
//! variable (NodeId `i=2255`).

use std::collections::HashMap;

use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, NodeId, ReadValueId, TimestampsToReturn, VariableId, Variant,
};

use crate::error::{Result, ScanError};

/// Bi-directional mapping between namespace indexes and URIs.
#[derive(Debug, Clone, Default)]
pub struct NamespaceTable {
    /// URIs indexed by namespace index.
    uris: Vec<String>,
    /// Reverse lookup: URI -> index.
    by_uri: HashMap<String, u16>,
}

impl NamespaceTable {
    /// Read the namespace array from the server.
    pub async fn read(session: &Session) -> Result<Self> {
        let read_id = ReadValueId {
            node_id: NodeId::from(VariableId::Server_NamespaceArray),
            attribute_id: AttributeId::Value as u32,
            index_range: opcua::types::NumericRange::None,
            data_encoding: opcua::types::QualifiedName::null(),
        };

        let values: Vec<DataValue> = session
            .read(&[read_id], TimestampsToReturn::Neither, 0.0)
            .await
            .map_err(|s| ScanError::service("Read(NamespaceArray)", s))?;

        let uris = values
            .into_iter()
            .next()
            .and_then(|dv| dv.value)
            .map(extract_string_array)
            .unwrap_or_default();

        Ok(Self::from_uris(uris))
    }

    /// Build a table directly from a list of URIs (index = position).
    pub fn from_uris(uris: Vec<String>) -> Self {
        let by_uri = uris
            .iter()
            .enumerate()
            .map(|(i, uri)| (uri.clone(), i as u16))
            .collect();
        Self { uris, by_uri }
    }

    /// Look up the URI for a namespace index.
    pub fn uri(&self, index: u16) -> Option<&str> {
        self.uris.get(index as usize).map(String::as_str)
    }

    /// Look up the index for a namespace URI.
    pub fn index(&self, uri: &str) -> Option<u16> {
        self.by_uri.get(uri).copied()
    }

    /// All namespace URIs in index order.
    pub fn uris(&self) -> &[String] {
        &self.uris
    }

    /// Render a namespace index for display, preferring the URI.
    pub fn display(&self, index: u16) -> String {
        match self.uri(index) {
            Some(uri) if index != 0 => uri.to_string(),
            Some(_) => String::new(), // ns 0 (base) is implied; keep paths clean
            None => format!("ns={index}"),
        }
    }
}

/// Extract a `Vec<String>` from a `Variant` that holds a string array.
fn extract_string_array(value: Variant) -> Vec<String> {
    match value {
        Variant::Array(arr) => arr
            .values
            .into_iter()
            .map(|v| match v {
                Variant::String(s) => s.to_string(),
                other => other.to_string(),
            })
            .collect(),
        Variant::String(s) => vec![s.to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> NamespaceTable {
        NamespaceTable::from_uris(vec![
            "http://opcfoundation.org/UA/".to_string(),
            "urn:server:local".to_string(),
            "urn:vendor:custom".to_string(),
        ])
    }

    #[test]
    fn index_and_uri_round_trip() {
        let t = table();
        assert_eq!(t.uri(0), Some("http://opcfoundation.org/UA/"));
        assert_eq!(t.uri(2), Some("urn:vendor:custom"));
        assert_eq!(t.index("urn:server:local"), Some(1));
        assert_eq!(t.index("urn:vendor:custom"), Some(2));
    }

    #[test]
    fn unknown_index_and_uri_return_none() {
        let t = table();
        assert_eq!(t.uri(99), None);
        assert_eq!(t.index("urn:not:present"), None);
    }

    #[test]
    fn display_hides_base_namespace_but_shows_others() {
        let t = table();
        // ns 0 (the OPC UA base namespace) is implied and rendered empty so
        // browse paths stay clean.
        assert_eq!(t.display(0), "");
        assert_eq!(t.display(1), "urn:server:local");
        // An index with no URI falls back to an explicit `ns=N` marker.
        assert_eq!(t.display(7), "ns=7");
    }

    #[test]
    fn extract_string_array_handles_array_scalar_and_other() {
        let arr = Variant::from(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            extract_string_array(arr),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            extract_string_array(Variant::String("solo".into())),
            vec!["solo".to_string()]
        );
        assert!(extract_string_array(Variant::Boolean(true)).is_empty());
    }
}
