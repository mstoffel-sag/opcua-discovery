//! Generate a runnable `acquire` TOML config from an interactive selection.
//!
//! This is the write-side counterpart to [`crate::acquire::config`] (which only
//! deserializes). The TUI uses it to turn a set of selected nodes into a ready
//! config that `opcua-scan acquire --config <file>` can run directly.
//!
//! Secrets are never written: a `user` is recorded but the password is left as a
//! commented hint, so the generated file is safe to share or commit.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Result, ScanError};
use crate::opcua::namespaces::NamespaceTable;

/// The acquisition mode chosen for the generated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenMode {
    /// Cyclic reading at a fixed interval.
    Poll,
    /// Real-time subscription with the interval as the publishing interval.
    Subscribe,
}

impl GenMode {
    /// The TOML token for this mode (`poll` / `subscribe`).
    fn token(self) -> &'static str {
        match self {
            GenMode::Poll => "poll",
            GenMode::Subscribe => "subscribe",
        }
    }
}

/// Connection details to embed in the generated `[[device]]` table.
#[derive(Debug, Clone, Default)]
pub struct GenConnection {
    /// Endpoint URL.
    pub endpoint: String,
    /// Whether the server certificate is trusted automatically.
    pub trust_server: bool,
    /// Security policy name, if not the default `None`.
    pub security_policy: Option<String>,
    /// Security mode name, if not the default `None`.
    pub security_mode: Option<String>,
    /// Username, if user/password auth is in use (password is NOT written).
    pub user: Option<String>,
    /// PKI directory, if set.
    pub pki_dir: Option<PathBuf>,
}

/// A selected node to write into the generated config. Carries the namespace
/// URI (so the numeric index can be remapped if the server reorders its
/// namespace array) and a friendly name for recognizability.
#[derive(Debug, Clone, Default)]
pub struct GenNode {
    /// Node id string (e.g. `ns=4;i=1274`).
    pub id: String,
    /// Namespace URI, when known; otherwise resolved from the active table.
    pub namespace: Option<String>,
    /// Friendly display name.
    pub name: Option<String>,
    /// Cached full browse path, when known.
    pub path: Option<String>,
    /// Explicit per-node mode. `None` follows the config default (auto).
    pub mode: Option<GenMode>,
}

#[derive(Debug, Serialize)]
struct GeneratedConfig {
    device: Vec<GenDeviceToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acquire: Option<GenAcquireToml>,
    output: GenOutputToml,
    task: Vec<GenTaskToml>,
}

#[derive(Debug, Serialize)]
struct GenAcquireToml {
    default_mode: String,
}

#[derive(Debug, Serialize)]
struct GenConnectionToml {
    #[serde(skip_serializing_if = "is_false")]
    trust_server: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pki_dir: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenDeviceToml {
    #[serde(flatten)]
    connection: GenConnectionToml,
    protocol_address: GenProtocolAddressToml,
    point: Vec<GenPointToml>,
}

#[derive(Debug, Serialize)]
struct GenProtocolAddressToml {
    endpoint: String,
    namespaces: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GenPointToml {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    address: GenAddressToml,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenAddressToml {
    node_id: String,
}

#[derive(Debug, Serialize)]
struct GenNodeToml {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

impl From<&GenNode> for GenNodeToml {
    fn from(n: &GenNode) -> Self {
        Self {
            id: n.id.clone(),
            namespace: n.namespace.clone().filter(|s| !s.is_empty()),
            name: n.name.clone().filter(|s| !s.is_empty()),
            path: n.path.clone().filter(|s| !s.is_empty()),
            mode: n.mode.map(|m| m.token().to_string()),
        }
    }
}

/// Stable sort key for a selected node, so the written selection has a
/// deterministic order regardless of how the caller collected the nodes.
/// Without this, re-saving an unchanged selection can reorder the
/// `[[selection.node]]` entries (the TUI gathers them from unordered
/// collections), producing spurious diffs.
fn sort_key(n: &GenNode) -> (String, String) {
    (n.namespace.clone().unwrap_or_default(), n.id.clone())
}

#[derive(Debug, Serialize)]
struct GenOutputToml {
    format: String,
}

#[derive(Debug, Serialize)]
struct GenTaskToml {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publishing_interval: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Build a runnable acquire TOML document from a connection and the selected
/// nodes.
///
/// `default_mode` is the acquisition mode applied to nodes that do not carry an
/// explicit per-node mode; `interval` is the poll interval (and, for the
/// subscribe task, the publishing interval). Task blocks are emitted for
/// whichever modes the nodes actually resolve to, so a mixed selection produces
/// both a `poll` and a `subscribe` task. An `[acquire]` table records the
/// default whenever any node relies on it.
/// The complete live namespace array is stored once on the device.
pub fn generate_toml(
    conn: &GenConnection,
    nodes: Vec<GenNode>,
    default_mode: GenMode,
    interval: Duration,
    namespaces: &NamespaceTable,
) -> Result<String> {
    let interval_str = humantime::format_duration(interval).to_string();

    // Stable ordering so an unchanged selection always serializes identically.
    let mut nodes = nodes;
    nodes.sort_by_key(sort_key);

    // Resolve which modes are in play to decide which task blocks to emit.
    let resolved = |n: &GenNode| n.mode.unwrap_or(default_mode);
    let needs_poll = nodes.iter().any(|n| resolved(n) == GenMode::Poll);
    let needs_subscribe = nodes.iter().any(|n| resolved(n) == GenMode::Subscribe);
    let has_auto = nodes.iter().any(|n| n.mode.is_none());

    let mut task = Vec::new();
    if needs_poll {
        task.push(GenTaskToml {
            mode: "poll".to_string(),
            interval: Some(interval_str.clone()),
            publishing_interval: None,
        });
    }
    if needs_subscribe {
        task.push(GenTaskToml {
            mode: "subscribe".to_string(),
            interval: None,
            publishing_interval: Some(interval_str.clone()),
        });
    }
    // Fall back to a single task for the default mode when the selection is
    // empty, so the generated config is still runnable.
    if task.is_empty() {
        task.push(match default_mode {
            GenMode::Poll => GenTaskToml {
                mode: "poll".to_string(),
                interval: Some(interval_str.clone()),
                publishing_interval: None,
            },
            GenMode::Subscribe => GenTaskToml {
                mode: "subscribe".to_string(),
                interval: None,
                publishing_interval: Some(interval_str.clone()),
            },
        });
    }

    // Record the default only when at least one node relies on it.
    let acquire = has_auto.then(|| GenAcquireToml {
        default_mode: default_mode.token().to_string(),
    });

    let mut stored_namespaces = namespaces.uris().to_vec();
    let mut points = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for node in &nodes {
        let node_id = stored_node_id(node, namespaces, &mut stored_namespaces)?;
        points.push(GenPointToml {
            id: unique_id(&node_id, &mut ids),
            name: node.name.clone().filter(|s| !s.is_empty()),
            address: GenAddressToml { node_id },
            path: node.path.clone().filter(|s| !s.is_empty()),
            mode: node.mode.map(|m| m.token().to_string()),
        });
    }
    let config = GeneratedConfig {
        device: vec![GenDeviceToml {
            connection: GenConnectionToml {
                trust_server: conn.trust_server,
                security_policy: conn.security_policy.clone(),
                security_mode: conn.security_mode.clone(),
                user: conn.user.clone(),
                pki_dir: conn.pki_dir.as_ref().map(|p| p.display().to_string()),
            },
            protocol_address: GenProtocolAddressToml {
                endpoint: conn.endpoint.clone(),
                namespaces: stored_namespaces,
            },
            point: points,
        }],
        acquire,
        output: GenOutputToml {
            format: "tedge".to_string(),
        },
        task,
    };

    let body = toml::to_string_pretty(&config).map_err(|e| ScanError::Export(e.to_string()))?;
    let mut document: toml_edit::DocumentMut = body
        .parse()
        .map_err(|e: toml_edit::TomlError| ScanError::Export(e.to_string()))?;
    if let Some(devices) = document["device"].as_array_of_tables_mut() {
        for device in devices.iter_mut() {
            if let Some(points) = device["point"].as_array_of_tables_mut() {
                for point in points.iter_mut() {
                    if let Some(address) = point["address"].as_table() {
                        point["address"] = toml_edit::value(address.clone().into_inline_table());
                        point.fmt();
                    }
                }
            }
        }
    }

    let mut out = String::new();
    out.push_str("# Generated by `opcua-scan browse --tui` (press 'a').\n");
    out.push_str("# Run with: opcua-scan acquire --config <this file>\n");
    if conn.user.is_some() {
        out.push_str(
            "# NOTE: set the password via `password = \"env:OPCUA_PASSWORD\"` under [[device]].\n",
        );
    }
    out.push('\n');
    out.push_str(&document.to_string());
    Ok(out)
}

/// Update only the selection of an existing acquire config, preserving
/// everything else (connection, tasks, output, comments, and formatting).
/// Existing device points retain their logical IDs and stored namespace indexes;
/// live IDs are matched by namespace URI and identifier. Legacy selections stay
/// in their original schema, with flat `node_ids` arrays migrated to node tables.
pub fn update_selection_in_toml(
    original: &str,
    nodes: &[GenNode],
    namespaces: &NamespaceTable,
) -> Result<String> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

    let mut doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| ScanError::Export(e.to_string()))?;

    if doc.contains_key("device") {
        update_device_points(&mut doc, nodes, namespaces)?;
        return Ok(doc.to_string());
    }

    // Ensure a `[selection]` table exists.
    if !doc.contains_key("selection") {
        doc["selection"] = Item::Table(Table::new());
    }
    let selection = doc["selection"]
        .as_table_mut()
        .ok_or_else(|| ScanError::Export("`selection` is not a table".to_string()))?;

    // Migrate away from the legacy flat array.
    selection.remove("node_ids");

    // Stable ordering so re-saving an unchanged selection does not reorder the
    // `[[selection.node]]` entries.
    let mut nodes: Vec<&GenNode> = nodes.iter().collect();
    nodes.sort_by_key(|n| sort_key(n));

    let existing: Vec<Table> = selection
        .get("node")
        .and_then(|n| n.as_array_of_tables())
        .map(|n| n.iter().cloned().collect())
        .unwrap_or_default();
    let legacy_identity = |id: &str, uri: Option<&str>| {
        node_identity(id, uri, namespaces.uris())
            .unwrap_or_else(|_| (uri.unwrap_or_default().to_string(), id.to_string()))
    };
    let mut by_address: std::collections::HashMap<
        (String, String),
        std::collections::VecDeque<Table>,
    > = std::collections::HashMap::new();
    for table in existing {
        if let Some(id) = table.get("id").and_then(|v| v.as_str()) {
            let uri = table.get("namespace").and_then(|v| v.as_str());
            by_address
                .entry(legacy_identity(id, uri))
                .or_default()
                .push_back(table);
        }
    }
    let mut tables = ArrayOfTables::new();
    for node in nodes {
        let toml = GenNodeToml::from(node);
        let identity = legacy_identity(&node.id, node.namespace.as_deref());
        let mut t = by_address
            .get_mut(&identity)
            .and_then(|tables| tables.pop_front())
            .unwrap_or_default();
        set_table_value(&mut t, "id", toml.id);
        if let Some(ns) = toml.namespace {
            set_table_value(&mut t, "namespace", ns);
        }
        if let Some(name) = toml.name {
            if !t.contains_key("name") {
                t["name"] = toml_edit::value(name);
            }
        }
        if let Some(path) = toml.path {
            fill_cached_path(&mut t, &path);
        }
        if let Some(mode) = toml.mode {
            set_table_value(&mut t, "mode", mode);
        } else if t.get("mode").and_then(|m| m.as_str()) != Some("auto") {
            t.remove("mode");
        }
        tables.push(t);
    }
    selection.insert("node", Item::ArrayOfTables(tables));

    Ok(doc.to_string())
}

fn parse_id(id: &str) -> Result<opcua::types::NodeId> {
    id.parse()
        .map_err(|e| ScanError::Export(format!("invalid node id `{id}`: {e}")))
}

fn node_identity(id: &str, namespace: Option<&str>, uris: &[String]) -> Result<(String, String)> {
    let mut parsed = parse_id(id)?;
    let uri = namespace
        .filter(|s| !s.is_empty())
        .or_else(|| uris.get(parsed.namespace as usize).map(String::as_str))
        .or_else(|| (parsed.namespace == 0).then_some("http://opcfoundation.org/UA/"))
        .ok_or_else(|| {
            ScanError::Export(format!(
                "no namespace URI for node `{id}`; refusing unsafe remap"
            ))
        })?
        .to_string();
    parsed.namespace = 0;
    Ok((uri, parsed.to_string()))
}

fn stored_node_id(
    node: &GenNode,
    live: &NamespaceTable,
    stored: &mut Vec<String>,
) -> Result<String> {
    let (uri, _) = node_identity(&node.id, node.namespace.as_deref(), live.uris())?;
    if stored.is_empty() {
        stored.push("http://opcfoundation.org/UA/".to_string());
    }
    let index = match stored.iter().position(|s| s == &uri) {
        Some(index) => index,
        None => {
            stored.push(uri);
            stored.len() - 1
        }
    };
    let mut id = parse_id(&node.id)?;
    id.namespace = u16::try_from(index)
        .map_err(|_| ScanError::Export("too many namespace URIs".to_string()))?;
    Ok(id.to_string())
}

fn unique_id(base: &str, used: &mut std::collections::HashSet<String>) -> String {
    let mut id = base.to_string();
    let mut suffix = 2;
    while !used.insert(id.clone()) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

fn set_table_value(table: &mut toml_edit::Table, key: &str, value: String) {
    let decor = table
        .get(key)
        .and_then(|v| v.as_value())
        .map(|v| v.decor().clone());
    table[key] = toml_edit::value(value);
    if let Some(decor) = decor {
        *table[key]
            .as_value_mut()
            .expect("just assigned value")
            .decor_mut() = decor;
    }
}

fn fill_cached_path(table: &mut toml_edit::Table, path: &str) {
    // Cached paths are output keys, so refreshing them would rename measurements.
    if table
        .get("path")
        .and_then(|p| p.as_str())
        .map_or(true, str::is_empty)
    {
        set_table_value(table, "path", path.to_string());
    }
}

fn update_device_points(
    doc: &mut toml_edit::DocumentMut,
    nodes: &[GenNode],
    live: &NamespaceTable,
) -> Result<()> {
    use toml_edit::{Array, ArrayOfTables, InlineTable, Item, Table, Value};

    let devices = doc["device"]
        .as_array_of_tables_mut()
        .ok_or_else(|| ScanError::Export("`device` is not an array of tables".to_string()))?;
    if devices.len() != 1 {
        return Err(ScanError::Export(
            "editing requires exactly one device".to_string(),
        ));
    }
    let device = devices.get_mut(0).expect("one device");
    let namespace_item = device
        .get("protocol_address")
        .and_then(|p| p.get("namespaces"));
    let has_namespace_table = namespace_item.is_some();
    let original_namespaces: Vec<String> = match namespace_item {
        Some(item) => {
            let array = item
                .as_array()
                .ok_or_else(|| ScanError::Export("`namespaces` is not an array".to_string()))?;
            array
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).ok_or_else(|| {
                        ScanError::Export("namespace URIs must be strings".to_string())
                    })
                })
                .collect::<Result<_>>()?
        }
        // Without a saved table, existing addresses use literal live indexes.
        None => live.uris().to_vec(),
    };
    let mut stored = original_namespaces.clone();
    let existing: Vec<Table> = device
        .get("point")
        .and_then(|p| p.as_array_of_tables())
        .map(|p| p.iter().cloned().collect())
        .unwrap_or_default();
    let mut used: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|p| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    let mut by_address: std::collections::HashMap<
        (String, String),
        std::collections::VecDeque<Table>,
    > = std::collections::HashMap::new();
    for point in existing {
        let id = point
            .get("address")
            .and_then(|a| a.get("node_id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                ScanError::Export("point address.node_id must be a string".to_string())
            })?;
        let identity = node_identity(id, None, &original_namespaces)?;
        by_address.entry(identity).or_default().push_back(point);
    }
    let mut nodes: Vec<&GenNode> = nodes.iter().collect();
    nodes.sort_by_key(|n| sort_key(n));
    let mut points = ArrayOfTables::new();
    for node in nodes {
        let identity = node_identity(&node.id, node.namespace.as_deref(), live.uris())?;
        let mut point = if let Some(point) = by_address
            .get_mut(&identity)
            .and_then(|points| points.pop_front())
        {
            point
        } else {
            let node_id = stored_node_id(node, live, &mut stored)?;
            let mut point = Table::new();
            point["id"] = toml_edit::value(unique_id(&node_id, &mut used));
            let mut address = InlineTable::new();
            address.insert("node_id", Value::from(node_id));
            point["address"] = toml_edit::value(address);
            if let Some(name) = node.name.as_ref().filter(|n| !n.is_empty()) {
                point["name"] = toml_edit::value(name.clone());
            }
            point
        };
        if let Some(path) = node.path.as_ref().filter(|p| !p.is_empty()) {
            fill_cached_path(&mut point, path);
        }
        if let Some(mode) = node.mode {
            set_table_value(&mut point, "mode", mode.token().to_string());
        } else if point.get("mode").and_then(|m| m.as_str()) != Some("auto") {
            point.remove("mode");
        }
        points.push(point);
    }
    if !has_namespace_table || stored.len() != original_namespaces.len() {
        if device.get("protocol_address").is_none() {
            device["protocol_address"] = Item::Table(Table::new());
        }
        let address = &mut device["protocol_address"];
        if address.get("namespaces").is_none() {
            address["namespaces"] = toml_edit::value(Array::new());
        }
        let array = address["namespaces"]
            .as_array_mut()
            .ok_or_else(|| ScanError::Export("`namespaces` is not an array".to_string()))?;
        let append_from = if has_namespace_table {
            original_namespaces.len()
        } else {
            0
        };
        for uri in &stored[append_from..] {
            array.push(uri.as_str());
        }
    }
    device.insert("point", Item::ArrayOfTables(points));
    Ok(())
}

/// Add cached paths, matching device points by `address.node_id` and legacy
/// selection entries by `id`. Preserves formatting, comments, and other settings.
/// `updates` is a list of `(node id, browse path)` pairs.
pub fn update_node_paths_in_toml(original: &str, updates: &[(String, String)]) -> Result<String> {
    use toml_edit::DocumentMut;

    if updates.is_empty() {
        return Ok(original.to_string());
    }

    let mut doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| ScanError::Export(e.to_string()))?;

    fn update_tables(
        nodes: &mut toml_edit::ArrayOfTables,
        preferred: bool,
        updates: &[(String, String)],
    ) {
        for table in nodes.iter_mut() {
            let id = if preferred {
                table.get("address").and_then(|a| a.get("node_id"))
            } else {
                table.get("id")
            }
            .and_then(|v| v.as_str())
            .map(str::to_string);
            if let Some(id) = id {
                if let Some((_, path)) = updates.iter().find(|(uid, _)| uid == &id) {
                    set_table_value(table, "path", path.clone());
                }
            }
        }
    }
    if let Some(devices) = doc
        .get_mut("device")
        .and_then(|d| d.as_array_of_tables_mut())
    {
        for device in devices.iter_mut() {
            if let Some(points) = device
                .get_mut("point")
                .and_then(|p| p.as_array_of_tables_mut())
            {
                update_tables(points, true, updates);
            }
        }
    } else if let Some(nodes) = doc
        .get_mut("selection")
        .and_then(|s| s.get_mut("node"))
        .and_then(|n| n.as_array_of_tables_mut())
    {
        update_tables(nodes, false, updates);
    }

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquire::config::{AcquireConfig, TaskConfig};

    fn namespaces() -> NamespaceTable {
        NamespaceTable::from_uris(vec![
            "http://opcfoundation.org/UA/".into(),
            "urn:server".into(),
            "urn:example".into(),
        ])
    }

    fn node(id: &str, namespace: Option<&str>, name: Option<&str>) -> GenNode {
        GenNode {
            id: id.to_string(),
            namespace: namespace.map(str::to_string),
            name: name.map(str::to_string),
            path: None,
            mode: None,
        }
    }

    #[test]
    fn poll_config_round_trips() {
        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            trust_server: true,
            ..Default::default()
        };
        let toml = generate_toml(
            &conn,
            vec![
                node("ns=2;s=Foo", Some("urn:example"), Some("Foo")),
                node("i=2258", None, None),
            ],
            GenMode::Poll,
            Duration::from_secs(1),
            &namespaces(),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        assert_eq!(
            parsed.connection.endpoint.as_deref(),
            Some("opc.tcp://host:4840")
        );
        assert!(parsed.connection.trust_server);
        assert_eq!(parsed.selection.nodes.len(), 2);
        // Nodes are written in a deterministic `(namespace, id)` order so an
        // unchanged selection always serializes identically; the base-namespace
        // node (empty URI) therefore sorts ahead of the `urn:example` node.
        assert_eq!(parsed.selection.nodes[0].id, "i=2258");
        assert!(parsed.selection.nodes[0].namespace.is_none());
        assert_eq!(parsed.selection.nodes[1].id, "ns=2;s=Foo");
        assert_eq!(
            parsed.selection.nodes[1].namespace.as_deref(),
            Some("urn:example")
        );
        assert_eq!(parsed.selection.nodes[1].name.as_deref(), Some("Foo"));
        assert_eq!(parsed.tasks.len(), 1);
        assert!(matches!(parsed.tasks[0], TaskConfig::Poll { .. }));
    }

    #[test]
    fn subscribe_config_round_trips() {
        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            ..Default::default()
        };
        let toml = generate_toml(
            &conn,
            vec![node("ns=2;s=Bar", None, None)],
            GenMode::Subscribe,
            Duration::from_millis(500),
            &namespaces(),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        assert!(matches!(parsed.tasks[0], TaskConfig::Subscribe { .. }));
    }

    #[test]
    fn mixed_mode_config_emits_both_tasks() {
        use crate::acquire::config::NodeMode;

        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            ..Default::default()
        };
        let mut poll_node = node("ns=2;s=Poll", None, Some("P"));
        poll_node.mode = Some(GenMode::Poll);
        let mut sub_node = node("ns=2;s=Sub", None, Some("S"));
        sub_node.mode = Some(GenMode::Subscribe);
        let auto_node = node("ns=2;s=Auto", None, Some("A"));

        let toml = generate_toml(
            &conn,
            vec![poll_node, sub_node, auto_node],
            GenMode::Subscribe,
            Duration::from_secs(2),
            &namespaces(),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        // Both poll and subscribe task blocks are present.
        assert!(parsed
            .tasks
            .iter()
            .any(|t| matches!(t, TaskConfig::Poll { .. })));
        assert!(parsed
            .tasks
            .iter()
            .any(|t| matches!(t, TaskConfig::Subscribe { .. })));
        // Per-node modes survive; the auto node has no explicit mode.
        let by_id = |id: &str| {
            parsed
                .selection
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.mode)
        };
        assert_eq!(by_id("ns=2;s=Poll"), Some(NodeMode::Poll));
        assert_eq!(by_id("ns=2;s=Sub"), Some(NodeMode::Subscribe));
        assert_eq!(by_id("ns=2;s=Auto"), Some(NodeMode::Auto));
        // The default is recorded because an auto node relies on it.
        assert_eq!(parsed.acquire.default_mode, Some(NodeMode::Subscribe));
    }

    #[test]
    fn update_selection_preserves_rest() {
        let original = "\
# my acquire config
[connection]
endpoint = \"opc.tcp://host:4840\"
trust_server = true

[selection]
node_ids = [\"ns=2;s=Old\"]

[output]
format = \"tedge\"

[[task]]
mode = \"poll\"
interval = \"5s\"
";
        let updated = update_selection_in_toml(
            original,
            &[
                node("ns=2;s=A", Some("urn:example"), Some("A")),
                node("ns=2;s=B", None, None),
            ],
            &namespaces(),
        )
        .expect("update");

        // Comment and other tables survive.
        assert!(updated.contains("# my acquire config"));
        assert!(updated.contains("trust_server = true"));
        assert!(updated.contains("interval = \"5s\""));
        // Legacy flat array is migrated away.
        assert!(!updated.contains("node_ids"));

        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse updated config");
        assert_eq!(parsed.selection.nodes.len(), 2);
        // Deterministic `(namespace, id)` ordering: the base-namespace node
        // (empty URI) sorts ahead of the `urn:example` node.
        assert_eq!(parsed.selection.nodes[0].id, "ns=2;s=B");
        assert!(parsed.selection.nodes[0].namespace.is_none());
        assert_eq!(parsed.selection.nodes[1].id, "ns=2;s=A");
        assert_eq!(
            parsed.selection.nodes[1].namespace.as_deref(),
            Some("urn:example")
        );
        assert_eq!(parsed.selection.nodes[1].name.as_deref(), Some("A"));
        assert!(parsed.selection.node_ids.is_empty());
        assert!(matches!(parsed.tasks[0], TaskConfig::Poll { .. }));
    }

    #[test]
    fn update_selection_adds_table_when_missing() {
        let original = "\
[connection]
endpoint = \"opc.tcp://host:4840\"
trust_server = true

[[task]]
mode = \"scan\"
";
        let updated =
            update_selection_in_toml(original, &[node("i=2258", None, None)], &namespaces())
                .expect("update");
        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse");
        assert_eq!(parsed.selection.nodes.len(), 1);
        assert_eq!(parsed.selection.nodes[0].id, "i=2258");
    }

    #[test]
    fn update_node_paths_caches_browse_path() {
        let original = "\
# keep me
[connection]
endpoint = \"opc.tcp://host:4840\"

[[selection.node]]
id = \"ns=4;i=1276\"
namespace = \"urn:boiler\"
name = \"ControlOut\"

[[selection.node]]
id = \"ns=4;i=1244\"
name = \"Output\"

[output]
format = \"ndjson\"

[[task]]
mode = \"poll\"
interval = \"1s\"
";
        let updated = update_node_paths_in_toml(
            original,
            &[
                (
                    "ns=4;i=1276".to_string(),
                    "Boiler #1/LX001/ControlOut".to_string(),
                ),
                (
                    "ns=4;i=1244".to_string(),
                    "Boiler #1/CC001/Output".to_string(),
                ),
            ],
        )
        .expect("update paths");

        assert!(updated.contains("# keep me"));
        assert!(updated.contains("path = \"Boiler #1/LX001/ControlOut\""));
        assert!(updated.contains("path = \"Boiler #1/CC001/Output\""));

        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse");
        assert_eq!(
            parsed.selection.nodes[0].path.as_deref(),
            Some("Boiler #1/LX001/ControlOut")
        );
        assert_eq!(
            parsed.selection.nodes[1].path.as_deref(),
            Some("Boiler #1/CC001/Output")
        );
    }

    #[test]
    fn update_node_paths_noop_without_updates() {
        let original = "[connection]\nendpoint = \"opc.tcp://host:4840\"\n";
        let updated = update_node_paths_in_toml(original, &[]).expect("noop");
        assert_eq!(updated, original);
    }

    #[test]
    fn preferred_generation_keeps_complete_table_and_duplicate_names() {
        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".into(),
            ..Default::default()
        };
        let nodes = vec![
            node("ns=2;s=A", None, Some("Temperature")),
            node("ns=2;s=B", None, Some("Temperature")),
        ];
        let generated = generate_toml(
            &conn,
            nodes.clone(),
            GenMode::Poll,
            Duration::from_secs(1),
            &namespaces(),
        )
        .unwrap();
        let document: toml::Value = toml::from_str(&generated).unwrap();
        let device = &document["device"][0];
        assert_eq!(
            device["protocol_address"]["namespaces"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let points = device["point"].as_array().unwrap();
        assert_ne!(points[0]["id"], points[1]["id"]);
        assert_eq!(points[0]["name"].as_str(), Some("Temperature"));
        assert_eq!(points[1]["name"].as_str(), Some("Temperature"));
        assert!(generated.contains("address = {"), "{generated}");
        let parsed = AcquireConfig::from_toml(&generated).unwrap();
        assert_eq!(parsed.selection.nodes.len(), 2);
        assert_eq!(
            parsed.selection.nodes[0].namespace.as_deref(),
            Some("urn:example")
        );
        let saved = update_selection_in_toml(&generated, &nodes, &namespaces()).unwrap();
        let again = update_selection_in_toml(&saved, &nodes, &namespaces()).unwrap();
        assert_eq!(saved, again);
    }

    const PREFERRED: &str = r#"# keep device
[[device]]
name = "Boiler"
trust_server = true

[device.protocol_address]
endpoint = "opc.tcp://host:4840"
# namespace order belongs to this file
namespaces = ["http://opcfoundation.org/UA/", "urn:a", "urn:b"] # keep array comment

[[device.point]]
# keep point
id = "stable-temperature" # logical key
name = "Published name" # custom output name
address = { node_id = "ns=1;s=Temperature" } # address comment
path = "Old/Temperature" # path comment
mode = "poll" # mode comment

[[task]]
mode = "poll"
interval = "7s"
"#;

    fn reordered_namespaces() -> NamespaceTable {
        NamespaceTable::from_uris(vec![
            "http://opcfoundation.org/UA/".into(),
            "urn:b".into(),
            "urn:a".into(),
            "urn:new".into(),
        ])
    }

    #[test]
    fn preferred_resave_remaps_by_uri_and_preserves_original_tables() {
        let mut selected = node("ns=2;s=Temperature", None, Some("Live display name"));
        selected.mode = Some(GenMode::Poll);
        selected.path = Some("New/Temperature".into());
        let added = node("ns=3;s=Other", None, Some("Live display name"));
        let updated = update_selection_in_toml(
            PREFERRED,
            &[selected.clone(), added.clone()],
            &reordered_namespaces(),
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&updated).unwrap();
        let device = &parsed["device"][0];
        assert_eq!(
            device["protocol_address"]["namespaces"].as_array().unwrap(),
            &vec![
                toml::Value::String("http://opcfoundation.org/UA/".into()),
                toml::Value::String("urn:a".into()),
                toml::Value::String("urn:b".into()),
                toml::Value::String("urn:new".into()),
            ]
        );
        let points = device["point"].as_array().unwrap();
        let existing = points
            .iter()
            .find(|p| p["id"].as_str() == Some("stable-temperature"))
            .unwrap();
        assert_eq!(
            existing["address"]["node_id"].as_str(),
            Some("ns=1;s=Temperature")
        );
        assert_eq!(existing["name"].as_str(), Some("Published name"));
        assert_eq!(existing["path"].as_str(), Some("Old/Temperature"));
        for comment in [
            "# keep device",
            "# namespace order belongs to this file",
            "# keep array comment",
            "# keep point",
            "# logical key",
            "# custom output name",
            "# address comment",
            "# path comment",
            "# mode comment",
        ] {
            assert!(updated.contains(comment), "lost {comment}");
        }
        assert!(updated.contains("interval = \"7s\""));
        let normalized = AcquireConfig::from_toml(&updated).unwrap();
        let existing = normalized
            .selection
            .nodes
            .iter()
            .find(|n| n.id == "ns=1;s=Temperature")
            .unwrap();
        assert_eq!(existing.namespace.as_deref(), Some("urn:a"));
        let again = update_selection_in_toml(&updated, &[selected, added], &reordered_namespaces())
            .unwrap();
        assert_eq!(updated, again);
    }

    #[test]
    fn preferred_resave_uses_explicit_uri_and_avoids_logical_id_collisions() {
        let original = PREFERRED.replace("stable-temperature", "ns=3;s=Other");
        let mut selected = node("ns=1;s=Temperature", Some("urn:a"), Some("Temperature"));
        selected.mode = Some(GenMode::Poll);
        let updated = update_selection_in_toml(
            &original,
            &[selected, node("ns=3;s=Other", None, Some("Temperature"))],
            &reordered_namespaces(),
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&updated).unwrap();
        let points = parsed["device"][0]["point"].as_array().unwrap();
        assert_eq!(points.len(), 2);
        assert_ne!(points[0]["id"], points[1]["id"]);
        assert!(points
            .iter()
            .any(|p| p["id"].as_str() == Some("ns=3;s=Other-2")));
    }

    #[test]
    fn preferred_path_cache_matches_address_and_keeps_comments() {
        let unchanged = update_node_paths_in_toml(
            PREFERRED,
            &[("stable-temperature".into(), "Wrong/Path".into())],
        )
        .unwrap();
        assert!(!unchanged.contains("Wrong/Path"));
        let updated = update_node_paths_in_toml(
            PREFERRED,
            &[("ns=1;s=Temperature".into(), "Cached/Temperature".into())],
        )
        .unwrap();
        assert!(updated.contains("path = \"Cached/Temperature\" # path comment"));
        assert!(updated.contains("id = \"stable-temperature\" # logical key"));
        assert!(AcquireConfig::from_toml(&updated).is_ok());
    }

    #[test]
    fn preferred_resave_rejects_unknown_live_namespace_instead_of_reusing_index() {
        let result = update_selection_in_toml(
            PREFERRED,
            &[node("ns=9;s=Temperature", None, None)],
            &reordered_namespaces(),
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing unsafe remap"));
    }

    #[test]
    fn preferred_resave_retains_explicit_auto_mode_comment() {
        let original = PREFERRED.replace(
            "mode = \"poll\" # mode comment",
            "mode = \"auto\" # mode comment",
        );
        let updated = update_selection_in_toml(
            &original,
            &[node("ns=2;s=Temperature", None, None)],
            &reordered_namespaces(),
        )
        .unwrap();
        assert!(updated.contains("mode = \"auto\" # mode comment"));
    }

    #[test]
    fn preferred_literal_indexes_resave_keeps_identity_and_caches_full_live_table() {
        let original = PREFERRED.replace(
            "namespaces = [\"http://opcfoundation.org/UA/\", \"urn:a\", \"urn:b\"] # keep array comment\n",
            "",
        );
        let live = reordered_namespaces();
        let mut selected = node("ns=1;s=Temperature", None, Some("Live display name"));
        selected.mode = Some(GenMode::Poll);
        selected.path = Some("Live/URI-qualified/Temperature".into());
        let updated = update_selection_in_toml(
            &original,
            &[selected, node("ns=3;s=Other", None, None)],
            &live,
        )
        .unwrap();
        let document: toml::Value = toml::from_str(&updated).unwrap();
        let device = &document["device"][0];
        let stored: Vec<_> = device["protocol_address"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            stored,
            live.uris().iter().map(String::as_str).collect::<Vec<_>>()
        );
        let point = device["point"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"].as_str() == Some("stable-temperature"))
            .unwrap();
        assert_eq!(point["name"].as_str(), Some("Published name"));
        assert_eq!(
            point["address"]["node_id"].as_str(),
            Some("ns=1;s=Temperature")
        );
        assert_eq!(point["path"].as_str(), Some("Old/Temperature"));
        assert!(updated.contains("# logical key"));
        assert!(updated.contains("# address comment"));
        assert!(updated.contains("# mode comment"));
    }

    #[test]
    fn preferred_resave_rejects_malformed_namespace_tables_without_compacting() {
        let selected = node("ns=2;s=Temperature", None, None);
        for table in [
            "[\"http://opcfoundation.org/UA/\", 42, \"urn:a\"]",
            "\"not an array\"",
        ] {
            let original = PREFERRED.replace(
                "[\"http://opcfoundation.org/UA/\", \"urn:a\", \"urn:b\"]",
                table,
            );
            assert!(update_selection_in_toml(
                &original,
                std::slice::from_ref(&selected),
                &reordered_namespaces(),
            )
            .is_err());
        }
    }

    #[test]
    fn resave_fills_missing_cached_paths_in_both_schemas() {
        let preferred = PREFERRED.replace("path = \"Old/Temperature\" # path comment\n", "");
        let legacy = r#"[connection]
endpoint = "opc.tcp://host:4840"
[[selection.node]]
id = "ns=1;s=Temperature"
namespace = "urn:a"
name = "Published name"
"#;
        let mut selected = node(
            "ns=2;s=Temperature",
            Some("urn:a"),
            Some("Live display name"),
        );
        selected.mode = Some(GenMode::Poll);
        selected.path = Some("New/Temperature".into());
        for source in [preferred.as_str(), legacy] {
            let updated = update_selection_in_toml(
                source,
                std::slice::from_ref(&selected),
                &reordered_namespaces(),
            )
            .unwrap();
            let document: toml::Value = toml::from_str(&updated).unwrap();
            let point = if document.get("device").is_some() {
                &document["device"][0]["point"][0]
            } else {
                &document["selection"]["node"][0]
            };
            assert_eq!(point["path"].as_str(), Some("New/Temperature"));
            assert_eq!(point["name"].as_str(), Some("Published name"));
        }
    }

    #[test]
    fn legacy_resave_preserves_point_comments_names_and_cached_paths() {
        let original = r#"[connection]
endpoint = "opc.tcp://host:4840"
[[selection.node]]
# selected point
id = "ns=2;s=Temperature" # address
namespace = "urn:example"
name = "Published name"
path = "Cached/Temperature"
mode = "subscribe" # retained acquisition mode

[[task]]
mode = "poll"
interval = "1s"
"#;
        let mut selected = node(
            "ns=2;s=Temperature",
            Some("urn:example"),
            Some("Live display name"),
        );
        selected.mode = Some(GenMode::Subscribe);
        selected.path = Some("Live/URI-qualified/Temperature".into());
        let updated = update_selection_in_toml(original, &[selected], &namespaces()).unwrap();
        assert!(updated.contains("# selected point"));
        assert!(updated.contains("# address"));
        assert!(updated.contains("mode = \"subscribe\" # retained acquisition mode"));
        let parsed = AcquireConfig::from_toml(&updated).unwrap();
        assert_eq!(
            parsed.selection.nodes[0].name.as_deref(),
            Some("Published name")
        );
        assert_eq!(
            parsed.selection.nodes[0].path.as_deref(),
            Some("Cached/Temperature")
        );
    }
}
