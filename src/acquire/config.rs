//! TOML configuration schema for the `acquire` command.
//!
//! A single config file describes one acquisition run: how to connect, which
//! nodes to select (explicit ids and/or filter-based discovery), where to send
//! the output, and one or more tasks (scan / poll / subscribe / history) that
//! run concurrently against the same session.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;

use crate::cli::{
    ConnectionArgs, Depth, FilterArgs, NodeClassArg, SecurityModeArg, SecurityPolicyArg, Strategy,
};
use crate::error::{Result, ScanError};

/// A fully parsed acquisition configuration (one TOML file).
///
/// Device/point configs are normalized into the same connection and selection
/// model as legacy configs before any acquisition task is started.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawAcquireConfig")]
pub struct AcquireConfig {
    /// Connection / security settings.
    pub connection: ConnectionConfig,
    /// Acquisition policy (default per-node mode, global poll-only override).
    pub acquire: AcquireOptions,
    /// Node selection (explicit ids and/or filters).
    pub selection: SelectionConfig,
    /// Output destination and encoding.
    pub output: OutputConfig,
    /// One or more acquisition tasks to run concurrently.
    pub tasks: Vec<TaskConfig>,
}

const BASE_NAMESPACE: &str = "http://opcfoundation.org/UA/";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAcquireConfig {
    connection: Option<ConnectionConfig>,
    #[serde(default)]
    acquire: AcquireOptions,
    selection: Option<toml::Value>,
    #[serde(default)]
    output: OutputConfig,
    #[serde(default, rename = "task")]
    tasks: Vec<TaskConfig>,
    device: Option<Vec<toml::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolAddress {
    endpoint: String,
    namespaces: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointConfig {
    id: String,
    name: Option<String>,
    path: Option<String>,
    #[serde(default)]
    mode: NodeMode,
    address: NodeAddress,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeAddress {
    node_id: String,
}

impl TryFrom<RawAcquireConfig> for AcquireConfig {
    type Error = String;

    fn try_from(raw: RawAcquireConfig) -> std::result::Result<Self, String> {
        let explicit_selection = raw.selection.as_ref().is_some_and(|selection| {
            selection.get("node").is_some() || selection.get("node_ids").is_some()
        });
        let mut selection: SelectionConfig = raw
            .selection
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let had_connection = raw.connection.is_some();
        let mut connection = raw.connection.unwrap_or_default();
        if let Some(mut devices) = raw.device {
            if devices.len() != 1 {
                return Err("exactly one [[device]] must be defined when device is present".into());
            }
            if had_connection {
                return Err("[connection] cannot be combined with [[device]]".into());
            }
            if explicit_selection {
                return Err("[selection] node/node_ids cannot be combined with [[device]]".into());
            }
            let mut device = devices.remove(0);
            let table = device.as_table_mut().ok_or("device must be a table")?;
            let protocol: ProtocolAddress = table
                .remove("protocol_address")
                .ok_or("device.protocol_address is required")?
                .try_into()
                .map_err(|e| format!("device.protocol_address: {e}"))?;
            if protocol.endpoint.trim().is_empty() {
                return Err("device.protocol_address.endpoint must not be blank".into());
            }
            if table.contains_key("endpoint") {
                return Err("device endpoint belongs in protocol_address.endpoint".into());
            }
            let points: Vec<PointConfig> = table
                .remove("point")
                .map(toml::Value::try_into)
                .transpose()
                .map_err(|e| format!("device.point: {e}"))?
                .unwrap_or_default();
            connection = device.try_into().map_err(|e| format!("device: {e}"))?;
            connection.endpoint = Some(protocol.endpoint);
            if let Some(namespaces) = &protocol.namespaces {
                if namespaces.first().map(String::as_str) != Some(BASE_NAMESPACE) {
                    return Err(format!(
                        "device.protocol_address.namespaces must start with namespace 0 URI '{BASE_NAMESPACE}'"
                    ));
                }
                if namespaces.len() > usize::from(u16::MAX) + 1 {
                    return Err("device namespace table exceeds the namespace index range".into());
                }
                for (index, uri) in namespaces.iter().enumerate() {
                    if uri.trim().is_empty() {
                        return Err(format!("namespace URI at index {index} must not be blank"));
                    }
                }
            }
            let mut point_ids = HashSet::new();
            for point in points {
                if point.id.trim().is_empty() || !point_ids.insert(point.id.clone()) {
                    return Err(format!(
                        "device point id '{}' must be non-blank and unique",
                        point.id
                    ));
                }
                let parsed = parse_node_id(&point.address.node_id)?;
                let namespace = protocol.namespaces.as_ref().map(|table| {
                    table.get(usize::from(parsed.namespace)).cloned().ok_or_else(|| {
                        format!(
                            "point '{}': namespace index {} out of range for device namespace table",
                            point.id, parsed.namespace
                        )
                    })
                }).transpose()?.filter(|_| parsed.namespace != 0);
                selection.nodes.push(NodeSelector {
                    id: point.address.node_id,
                    namespace,
                    name: point.name,
                    path: point.path,
                    mode: point.mode,
                });
            }
        }
        let config = Self {
            connection,
            acquire: raw.acquire,
            selection,
            output: raw.output,
            tasks: raw.tasks,
        };
        config.validate()?;
        Ok(config)
    }
}

fn parse_node_id(id: &str) -> std::result::Result<opcua::types::NodeId, String> {
    opcua::types::NodeId::from_str(id).map_err(|_| format!("invalid node id '{id}'"))
}

/// How an individual node should be acquired when poll and subscribe tasks
/// coexist. Stored per `[[selection.node]]` and resolved against the
/// effective default at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeMode {
    /// Decide automatically: subscribe when the server supports monitoring the
    /// node, otherwise fall back to polling.
    #[default]
    Auto,
    /// Always subscribe (real-time monitored item).
    Subscribe,
    /// Always poll (cyclic read).
    Poll,
}

/// `[acquire]` table: acquisition policy that governs how per-node modes are
/// resolved when both poll and subscribe behaviours are possible.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireOptions {
    /// Default mode for nodes that do not specify one. When unset, it is
    /// inferred from the configured tasks (see
    /// [`AcquireConfig::effective_default_mode`]) so single-task configs keep
    /// their historic behaviour.
    #[serde(default)]
    pub default_mode: Option<NodeMode>,
    /// Force every node to be polled, ignoring any per-node or default
    /// subscribe preference. Useful for servers with poor subscription support.
    #[serde(default)]
    pub poll_only: bool,
}

impl AcquireConfig {
    /// Parse a configuration from a TOML file on disk.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| ScanError::Config {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        Self::from_toml(&text).map_err(|reason| ScanError::Config {
            path: path.to_path_buf(),
            reason,
        })
    }

    /// Parse a configuration from a TOML string.
    pub fn from_toml(text: &str) -> std::result::Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.tasks.is_empty() {
            return Err("at least one [[task]] must be defined".to_string());
        }
        if self.selection.node_ids.is_empty()
            && self.selection.nodes.is_empty()
            && self.selection.include.is_empty()
            && self.selection.include_regex.is_empty()
            && self.selection.node_class.is_empty()
            && self.selection.datatype.is_empty()
            && self.selection.root.is_none()
        {
            // A scan-only run may legitimately use the default root, so only warn
            // for sample-style tasks that need concrete nodes.
            let needs_nodes = self
                .tasks
                .iter()
                .any(|t| !matches!(t, TaskConfig::Scan { .. }));
            if needs_nodes {
                return Err(
                    "[selection] must specify node_ids or filters for poll/subscribe/history tasks"
                        .to_string(),
                );
            }
        }
        for id in &self.selection.node_ids {
            parse_node_id(id)?;
        }
        for node in &self.selection.nodes {
            let parsed = parse_node_id(&node.id)?;
            if let Some(uri) = &node.namespace {
                if uri.trim().is_empty() {
                    return Err(format!(
                        "namespace URI for node '{}' must not be blank",
                        node.id
                    ));
                }
                if parsed.namespace == 0 && uri != BASE_NAMESPACE {
                    return Err(format!(
                        "namespace 0 for node '{}' must be '{BASE_NAMESPACE}'",
                        node.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Build [`ConnectionArgs`] (reused by the OPC UA client layer) from config.
    pub fn connection_args(&self) -> Result<ConnectionArgs> {
        self.connection.to_args()
    }

    /// Build a [`FilterArgs`] for filter-based node selection.
    pub fn filter_args(&self) -> Result<FilterArgs> {
        self.selection.to_filter_args()
    }

    /// The default mode applied to nodes that don't specify one.
    ///
    /// When `[acquire].default_mode` is set it wins. Otherwise it is inferred
    /// from the configured tasks so legacy single-task configs are unchanged:
    /// a config with only a poll task polls everything, one with only a
    /// subscribe task subscribes everything, and anything else defaults to
    /// `auto` (subscribe-if-supported, else poll).
    pub fn effective_default_mode(&self) -> NodeMode {
        if let Some(m) = self.acquire.default_mode {
            return m;
        }
        let has_poll = self
            .tasks
            .iter()
            .any(|t| matches!(t, TaskConfig::Poll { .. }));
        let has_subscribe = self
            .tasks
            .iter()
            .any(|t| matches!(t, TaskConfig::Subscribe { .. }));
        match (has_poll, has_subscribe) {
            (true, false) => NodeMode::Poll,
            (false, true) => NodeMode::Subscribe,
            _ => NodeMode::Auto,
        }
    }

    /// The poll interval to use for poll-routed nodes, taken from the first
    /// poll task if present, otherwise a sensible default.
    pub fn poll_interval(&self) -> Duration {
        self.tasks
            .iter()
            .find_map(|t| match t {
                TaskConfig::Poll { interval } => Some(*interval),
                _ => None,
            })
            .unwrap_or(DEFAULT_POLL_INTERVAL)
    }

    /// Subscription parameters to use for subscribe-routed nodes, taken from
    /// the first subscribe task if present, otherwise all defaults.
    pub fn subscribe_params(&self) -> SubscribeParams {
        self.tasks
            .iter()
            .find_map(|t| match t {
                TaskConfig::Subscribe {
                    publishing_interval,
                    sampling_interval,
                    queue_size,
                } => Some(SubscribeParams {
                    publishing_interval: *publishing_interval,
                    sampling_interval: *sampling_interval,
                    queue_size: *queue_size,
                }),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// Default poll interval applied when nodes are routed to polling but no poll
/// task block supplied an interval.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Subscription parameters extracted from a `subscribe` task block (all
/// optional; the subscription layer fills in its own defaults).
#[derive(Debug, Clone, Copy, Default)]
pub struct SubscribeParams {
    /// Publishing interval.
    pub publishing_interval: Option<Duration>,
    /// Server-side sampling interval.
    pub sampling_interval: Option<Duration>,
    /// Monitored item queue size.
    pub queue_size: Option<u32>,
}

/// `[connection]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    /// Endpoint URL, e.g. `opc.tcp://host:4840`.
    pub endpoint: Option<String>,
    /// Optional human-readable name for this connection / data source. Surfaced
    /// in the output as `meta.source.name` so downstream consumers can identify
    /// where the data came from.
    pub name: Option<String>,
    /// Username for user/password authentication.
    pub user: Option<String>,
    /// Password (supports `env:VAR`).
    pub password: Option<String>,
    /// Client certificate (PEM/DER).
    pub cert: Option<PathBuf>,
    /// Client private key (PEM).
    pub key: Option<PathBuf>,
    /// Security policy: `None`, `Basic256Sha256`, `Aes128Sha256RsaOaep`, `Aes256Sha256RsaPss`.
    pub security_policy: Option<String>,
    /// Security mode: `None`, `Sign`, `SignAndEncrypt`.
    pub security_mode: Option<String>,
    /// Automatically trust the server certificate (insecure).
    #[serde(default)]
    pub trust_server: bool,
    /// PKI directory.
    pub pki_dir: Option<PathBuf>,
    /// Per-request timeout (e.g. `10s`).
    #[serde(default, deserialize_with = "de_opt_duration")]
    pub timeout: Option<Duration>,
    /// Retry attempts for transient failures.
    pub retry: Option<u32>,
    /// Metadata cache directory.
    pub cache: Option<PathBuf>,
}

impl ConnectionConfig {
    fn to_args(&self) -> Result<ConnectionArgs> {
        Ok(ConnectionArgs {
            endpoint: self.endpoint.clone(),
            user: self.user.clone(),
            password: self.password.clone(),
            cert: self.cert.clone(),
            key: self.key.clone(),
            security_policy: parse_security_policy(self.security_policy.as_deref())?,
            security_mode: parse_security_mode(self.security_mode.as_deref())?,
            trust_server: self.trust_server,
            pki_dir: self.pki_dir.clone(),
            timeout: self.timeout.unwrap_or_else(|| Duration::from_secs(10)),
            retry: self.retry.unwrap_or(3),
            cache: self.cache.clone(),
        })
    }

    /// Fill in connection settings from this config where the CLI did not
    /// provide them. CLI flags take precedence: a value is only taken from the
    /// config when the corresponding CLI argument is unset or still at its
    /// default (e.g. no endpoint, security policy `None`, `trust_server` false).
    pub fn apply_to(&self, args: &mut ConnectionArgs) -> Result<()> {
        if args.endpoint.is_none() {
            args.endpoint = self.endpoint.clone();
        }
        if args.user.is_none() {
            args.user = self.user.clone();
        }
        if args.password.is_none() {
            args.password = self.password.clone();
        }
        if args.cert.is_none() {
            args.cert = self.cert.clone();
        }
        if args.key.is_none() {
            args.key = self.key.clone();
        }
        if args.pki_dir.is_none() {
            args.pki_dir = self.pki_dir.clone();
        }
        if args.cache.is_none() {
            args.cache = self.cache.clone();
        }
        if args.security_policy == SecurityPolicyArg::None {
            args.security_policy = parse_security_policy(self.security_policy.as_deref())?;
        }
        if args.security_mode == SecurityModeArg::None {
            args.security_mode = parse_security_mode(self.security_mode.as_deref())?;
        }
        if !args.trust_server {
            args.trust_server = self.trust_server;
        }
        Ok(())
    }
}

/// `[selection]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionConfig {
    /// Explicit node ids (e.g. `ns=2;s=Foo`).
    ///
    /// Legacy form: bare id strings whose namespace index is taken literally.
    /// Prefer `[[selection.node]]` entries (see [`NodeSelector`]) which also
    /// record the namespace URI so the id stays correct if the server reorders
    /// its namespace array.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Explicit nodes with namespace URI and a friendly name.
    #[serde(default, rename = "node")]
    pub nodes: Vec<NodeSelector>,
    /// Starting node id for filter-based discovery (defaults to Objects).
    pub root: Option<String>,
    /// Maximum traversal depth (number or `unlimited`).
    pub depth: Option<String>,
    /// Include globs against the browse path.
    #[serde(default)]
    pub include: Vec<String>,
    /// Include regexes against the browse path.
    #[serde(default)]
    pub include_regex: Vec<String>,
    /// Exclude globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Exclude regexes.
    #[serde(default)]
    pub exclude_regex: Vec<String>,
    /// Restrict to these namespace URIs.
    #[serde(default)]
    pub include_ns: Vec<String>,
    /// Node classes to keep (e.g. `["Variable"]`).
    #[serde(default)]
    pub node_class: Vec<String>,
    /// Restrict Variables to these data types.
    #[serde(default)]
    pub datatype: Vec<String>,
}

impl SelectionConfig {
    fn to_filter_args(&self) -> Result<FilterArgs> {
        let node_class = self
            .node_class
            .iter()
            .map(|s| parse_node_class(s))
            .collect::<Result<Vec<_>>>()?;
        Ok(FilterArgs {
            include: self.include.clone(),
            include_regex: self.include_regex.clone(),
            exclude: self.exclude.clone(),
            exclude_regex: self.exclude_regex.clone(),
            include_ns: self.include_ns.clone(),
            ns_uri: None,
            ns_index: None,
            node_class,
            datatype: self.datatype.clone(),
            readable: false,
            writable: false,
            historizing: false,
            subscribable: false,
        })
    }

    /// Parsed traversal depth (defaults to 3 when unspecified).
    pub fn depth(&self) -> Result<Depth> {
        match &self.depth {
            None => Ok(Depth::Limited(3)),
            Some(s) => crate::cli::parse_depth(s).map_err(ScanError::InvalidArgument),
        }
    }
}

/// A single explicit node in `[[selection.node]]`.
///
/// The `namespace` URI makes the selection robust against the server reordering
/// its namespace array: the stored numeric index in `id` is remapped to the
/// server's current index for `namespace` at acquisition time.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSelector {
    /// Node id string (e.g. `ns=4;i=1274`).
    pub id: String,
    /// Namespace URI the node belongs to. When present, the index in `id` is
    /// remapped to the server's current index for this URI.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Friendly display name (used as the measurement name in output).
    #[serde(default)]
    pub name: Option<String>,
    /// Cached full browse path (e.g. `Boiler #1/LX001/ControlOut`). When set it
    /// is used directly; when absent it is reconstructed from the server at
    /// startup and (for file configs) written back here for later runs.
    #[serde(default)]
    pub path: Option<String>,
    /// How this node should be acquired (`subscribe`, `poll`, or omitted for
    /// `auto`). Routes the node to the matching task at runtime.
    #[serde(default)]
    pub mode: NodeMode,
}

/// `[output]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Output format.
    #[serde(default)]
    pub format: OutputFormat,
    /// Write to this file instead of stdout.
    pub file: Option<PathBuf>,
    /// MQTT topic to prefix each line with (for piping to `tedge mqtt pub`).
    pub topic: Option<String>,
    /// How much descriptive context to attach to each sample.
    #[serde(default)]
    pub metadata: MetadataMode,
}

/// Controls whether per-sample descriptive context (browse path, browse name,
/// display name, namespace, engineering range/units) is included in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataMode {
    /// Attach metadata. For `ndjson`/`json`/`csv` the fields are flattened into
    /// each record; for `tedge` they are nested under a `meta` object.
    #[default]
    Nested,
    /// Omit all metadata (strict thin-edge.io measurement output).
    Off,
}

/// Output encoding for acquired data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// thin-edge.io JSON measurements, one object per line.
    #[default]
    Tedge,
    /// JSON array (batch) for records / NDJSON for streamed samples.
    Json,
    /// Newline-delimited JSON.
    Ndjson,
    /// Comma separated values.
    Csv,
}

/// A single `[[task]]` entry. The `mode` field selects the variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum TaskConfig {
    /// Address space scanning (structural discovery).
    Scan {
        /// Traversal strategy.
        #[serde(default)]
        strategy: ScanStrategy,
        /// Concurrent browse workers.
        workers: Option<usize>,
        /// References per browse call.
        page_size: Option<u32>,
        /// Per-branch child cap.
        max_children: Option<usize>,
        /// Read current values for matched variables.
        #[serde(default)]
        preview_values: bool,
    },
    /// Cyclic reading (polling) of selected variables.
    Poll {
        /// Polling interval (e.g. `5s`).
        #[serde(deserialize_with = "de_duration")]
        interval: Duration,
    },
    /// Real-time subscription to selected variables.
    Subscribe {
        /// Publishing interval (e.g. `500ms`).
        #[serde(default, deserialize_with = "de_opt_duration")]
        publishing_interval: Option<Duration>,
        /// Server-side sampling interval (e.g. `250ms`).
        #[serde(default, deserialize_with = "de_opt_duration")]
        sampling_interval: Option<Duration>,
        /// Monitored item queue size.
        queue_size: Option<u32>,
    },
    /// Historical (raw) data retrieval for selected variables.
    History {
        /// Inclusive start time (RFC3339, e.g. `2024-01-01T00:00:00Z`).
        start: String,
        /// Exclusive end time (RFC3339). Defaults to now.
        end: Option<String>,
        /// Maximum values per node per request.
        max_values: Option<u32>,
    },
}

/// Scan traversal strategy (serde mirror of [`Strategy`]).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanStrategy {
    /// Breadth-first.
    #[default]
    Bfs,
    /// Depth-first.
    Dfs,
}

impl From<ScanStrategy> for Strategy {
    fn from(s: ScanStrategy) -> Self {
        match s {
            ScanStrategy::Bfs => Strategy::Bfs,
            ScanStrategy::Dfs => Strategy::Dfs,
        }
    }
}

fn parse_security_policy(s: Option<&str>) -> Result<SecurityPolicyArg> {
    match s.map(str::trim) {
        None | Some("") => Ok(SecurityPolicyArg::None),
        Some(v) if v.eq_ignore_ascii_case("none") => Ok(SecurityPolicyArg::None),
        Some(v) if v.eq_ignore_ascii_case("Basic256Sha256") => {
            Ok(SecurityPolicyArg::Basic256Sha256)
        }
        Some(v) if v.eq_ignore_ascii_case("Aes128Sha256RsaOaep") => {
            Ok(SecurityPolicyArg::Aes128Sha256RsaOaep)
        }
        Some(v) if v.eq_ignore_ascii_case("Aes256Sha256RsaPss") => {
            Ok(SecurityPolicyArg::Aes256Sha256RsaPss)
        }
        Some(v) => Err(ScanError::InvalidArgument(format!(
            "invalid security_policy '{v}'"
        ))),
    }
}

fn parse_security_mode(s: Option<&str>) -> Result<SecurityModeArg> {
    match s.map(str::trim) {
        None | Some("") => Ok(SecurityModeArg::None),
        Some(v) if v.eq_ignore_ascii_case("none") => Ok(SecurityModeArg::None),
        Some(v) if v.eq_ignore_ascii_case("sign") => Ok(SecurityModeArg::Sign),
        Some(v) if v.eq_ignore_ascii_case("SignAndEncrypt") => Ok(SecurityModeArg::SignAndEncrypt),
        Some(v) => Err(ScanError::InvalidArgument(format!(
            "invalid security_mode '{v}'"
        ))),
    }
}

fn parse_node_class(s: &str) -> Result<NodeClassArg> {
    let v = s.trim();
    let class = match v.to_ascii_lowercase().as_str() {
        "object" => NodeClassArg::Object,
        "variable" => NodeClassArg::Variable,
        "method" => NodeClassArg::Method,
        "objecttype" => NodeClassArg::ObjectType,
        "variabletype" => NodeClassArg::VariableType,
        "referencetype" => NodeClassArg::ReferenceType,
        "datatype" => NodeClassArg::DataType,
        "view" => NodeClassArg::View,
        _ => {
            return Err(ScanError::InvalidArgument(format!(
                "invalid node class '{v}'"
            )))
        }
    };
    Ok(class)
}

/// Deserialize a required humantime duration string (e.g. `5s`).
fn de_duration<'de, D>(de: D) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(de)?;
    humantime::parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Deserialize an optional humantime duration string.
fn de_opt_duration<'de, D>(de: D) -> std::result::Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(de)?;
    match s {
        None => Ok(None),
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINT: &str = r#"
[[device.point]]
id = "logical-temperature"
address = { node_id = "ns=4;i=123" }
"#;
    const NAMESPACES: &str = r#", namespaces = [
        "http://opcfoundation.org/UA/", "urn:server", "urn:other", "urn:unused", "urn:plant"
    ]"#;

    fn device_config(namespaces: &str, points: &str) -> String {
        format!(
            r#"[[device]]
protocol_address = {{ endpoint = "opc.tcp://host:4840"{namespaces} }}
{points}
[[task]]
mode = "subscribe"
"#
        )
    }

    #[test]
    fn device_normalizes_connection_and_points() {
        let text = device_config(
            NAMESPACES,
            &format!("{POINT}\nname = 'Temperature'\npath = 'Machine/Temperature'\nmode = 'poll'"),
        )
        .replace(
            "[[device]]",
            r#"[[device]]
name = "Boiler"
user = "operator"
password = "env:OPCUA_PASSWORD"
security_policy = "Basic256Sha256"
security_mode = "SignAndEncrypt"
cert = "client.pem"
key = "client.key"
trust_server = true
pki_dir = "pki"
timeout = "7s"
retry = 9
cache = "cache"
"#,
        );
        let config: AcquireConfig = toml::from_str(&text).unwrap();
        let connection = config.connection_args().unwrap();
        assert_eq!(connection.endpoint.as_deref(), Some("opc.tcp://host:4840"));
        assert_eq!(config.connection.name.as_deref(), Some("Boiler"));
        assert_eq!(connection.user.as_deref(), Some("operator"));
        assert_eq!(connection.password.as_deref(), Some("env:OPCUA_PASSWORD"));
        assert_eq!(
            connection.security_policy,
            SecurityPolicyArg::Basic256Sha256
        );
        assert_eq!(connection.security_mode, SecurityModeArg::SignAndEncrypt);
        assert_eq!(connection.cert, Some(PathBuf::from("client.pem")));
        assert_eq!(connection.key, Some(PathBuf::from("client.key")));
        assert_eq!(connection.pki_dir, Some(PathBuf::from("pki")));
        assert!(connection.trust_server);
        assert_eq!(connection.timeout, Duration::from_secs(7));
        assert_eq!(connection.retry, 9);
        assert_eq!(connection.cache, Some(PathBuf::from("cache")));
        let node = &config.selection.nodes[0];
        assert_eq!(node.id, "ns=4;i=123");
        assert_eq!(node.namespace.as_deref(), Some("urn:plant"));
        assert_eq!(node.name.as_deref(), Some("Temperature"));
        assert_eq!(node.path.as_deref(), Some("Machine/Temperature"));
        assert_eq!(node.mode, NodeMode::Poll);
    }

    #[test]
    fn device_optional_name_and_namespace_table_keep_literal_ids() {
        let config = AcquireConfig::from_toml(&device_config("", POINT)).unwrap();
        assert!(config.connection.name.is_none());
        let node = &config.selection.nodes[0];
        assert_eq!(node.id, "ns=4;i=123");
        assert!(node.name.is_none());
        assert!(node.namespace.is_none());
        assert_eq!(node.mode, NodeMode::Auto);
        let text = device_config("", POINT).replace(
            r#"protocol_address = { endpoint = "opc.tcp://host:4840" }"#,
            "[device.protocol_address]\nendpoint = 'opc.tcp://host:4840'",
        );
        assert!(text.contains("[device.protocol_address]"));
        assert!(AcquireConfig::from_toml(&text).is_ok());
    }

    #[test]
    fn device_rejects_mixed_legacy_fields_by_presence() {
        for prefix in [
            "[connection]\n",
            "[connection]\nname = 'legacy'\n",
            "[selection]\nnode_ids = []\n",
            "[selection]\nnode = []\n",
        ] {
            let error = AcquireConfig::from_toml(&format!("{prefix}{}", device_config("", POINT)))
                .unwrap_err();
            assert!(error.contains("cannot be combined"), "{error}");
        }
        let config = AcquireConfig::from_toml(&format!(
            "[selection]\ninclude = ['**/Temperature']\n{}",
            device_config("", POINT)
        ))
        .unwrap();
        assert_eq!(config.selection.include, ["**/Temperature"]);
        assert_eq!(config.selection.nodes.len(), 1);
    }

    #[test]
    fn device_requires_exactly_one_device_when_present() {
        let multiple = device_config("", POINT).replace(
            "[[task]]",
            "[[device]]\nprotocol_address = { endpoint = 'opc.tcp://other:4840' }\n[[task]]",
        );
        for text in ["device = []\n[[task]]\nmode = 'scan'\n", &multiple] {
            let error = AcquireConfig::from_toml(text).unwrap_err();
            assert!(error.contains("exactly one [[device]]"), "{error}");
        }
    }

    #[test]
    fn device_validates_namespace_table_without_compacting_or_deduplicating() {
        for namespaces in [
            ", namespaces = []",
            ", namespaces = {}",
            ", namespaces = ['urn:wrong-base', 'urn:plant']",
            ", namespaces = ['http://opcfoundation.org/UA/', '']",
            ", namespaces = ['http://opcfoundation.org/UA/', '  ']",
            ", namespaces = ['http://opcfoundation.org/UA/', 42]",
            ", namespaces = ['http://opcfoundation.org/UA/']",
        ] {
            assert!(
                AcquireConfig::from_toml(&device_config(namespaces, POINT)).is_err(),
                "accepted {namespaces}"
            );
        }
        let namespaces = r#", namespaces = [
            "http://opcfoundation.org/UA/", "urn:z", "urn:z", "urn:a", "urn:target"
        ]"#;
        let config = AcquireConfig::from_toml(&device_config(namespaces, POINT)).unwrap();
        assert_eq!(
            config.selection.nodes[0].namespace.as_deref(),
            Some("urn:target")
        );
    }

    #[test]
    fn device_validates_logical_keys_and_addresses() {
        for points in [
            POINT.replace("logical-temperature", ""),
            POINT.replace("logical-temperature", "  "),
            format!("{POINT}{POINT}"),
            POINT.replace("ns=4;i=123", "not-a-node"),
            POINT.replace("ns=4;i=123", "ns=65536;i=123"),
            POINT.replace("ns=4;i=123", "ns=4;i=not-numeric"),
            POINT.replace(
                "node_id = \"ns=4;i=123\"",
                "node_id = 'ns=4;i=123', namespace = 'urn:plant'",
            ),
        ] {
            assert!(AcquireConfig::from_toml(&device_config("", &points)).is_err());
        }
    }

    #[test]
    fn device_namespace_zero_stays_implicit_or_explicit() {
        for id in ["i=123", "ns=0;i=123"] {
            let config = AcquireConfig::from_toml(&device_config(
                NAMESPACES,
                &POINT.replace("ns=4;i=123", id),
            ))
            .unwrap();
            assert_eq!(config.selection.nodes[0].id, id);
            assert!(config.selection.nodes[0].namespace.is_none());
        }
    }

    #[test]
    fn legacy_validates_node_ids_and_namespace_zero_declarations() {
        for selection in [
            "[selection]\nnode_ids = ['bad-node']",
            "[[selection.node]]\nid = 'bad-node'",
            "[[selection.node]]\nid = 'i=123'\nnamespace = 'urn:wrong'",
            "[[selection.node]]\nid = 'ns=0;i=123'\nnamespace = 'urn:wrong'",
            "[[selection.node]]\nid = 'ns=2;i=123'\nnamespace = '  '",
        ] {
            assert!(
                AcquireConfig::from_toml(&format!("{selection}\n[[task]]\nmode = 'subscribe'"))
                    .is_err(),
                "accepted {selection}"
            );
        }
        let config = AcquireConfig::from_toml(
            r#"[[selection.node]]
id = "ns=4;s=Temperature"
namespace = "urn:plant"
[[selection.node]]
id = "i=123"
namespace = "http://opcfoundation.org/UA/"
[[task]]
mode = "subscribe"
"#,
        )
        .unwrap();
        assert_eq!(
            config.selection.nodes[0].namespace.as_deref(),
            Some("urn:plant")
        );
    }

    #[test]
    fn parses_all_task_modes() {
        let toml = r#"
[connection]
endpoint = "opc.tcp://host:4840"
trust_server = true
timeout = "5s"

[selection]
node_ids = ["ns=2;s=Foo"]
root = "i=85"
depth = "unlimited"
include = ["**/Temperature"]
node_class = ["Variable"]

[output]
format = "tedge"
topic = "te/device/main///m/opcua"

[[task]]
mode = "scan"
strategy = "dfs"
workers = 4

[[task]]
mode = "poll"
interval = "5s"

[[task]]
mode = "subscribe"
publishing_interval = "500ms"
sampling_interval = "250ms"
queue_size = 20

[[task]]
mode = "history"
start = "2024-01-01T00:00:00Z"
max_values = 1000
"#;
        let config = AcquireConfig::from_toml(toml).expect("config should parse");
        assert_eq!(config.tasks.len(), 4);
        assert_eq!(config.output.format, OutputFormat::Tedge);
        assert_eq!(config.selection.node_ids, vec!["ns=2;s=Foo".to_string()]);
        assert!(matches!(config.selection.depth(), Ok(Depth::Unlimited)));

        let args = config.connection_args().expect("connection args");
        assert!(args.trust_server);
        assert_eq!(args.timeout, Duration::from_secs(5));

        assert!(matches!(
            config.tasks[0],
            TaskConfig::Scan {
                workers: Some(4),
                ..
            }
        ));
        assert!(matches!(config.tasks[1], TaskConfig::Poll { .. }));
        assert!(matches!(config.tasks[2], TaskConfig::Subscribe { .. }));
        assert!(matches!(config.tasks[3], TaskConfig::History { .. }));
    }

    #[test]
    fn rejects_empty_tasks() {
        let err = AcquireConfig::from_toml("[connection]\nendpoint = \"opc.tcp://h:1\"\n")
            .expect_err("should reject config without tasks");
        assert!(err.contains("task"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_unknown_fields() {
        let toml = r#"
[selection]
node_ids = ["i=1"]
[[task]]
mode = "poll"
interval = "1s"
bogus = true
"#;
        assert!(AcquireConfig::from_toml(toml).is_err());
    }

    #[test]
    fn parses_per_node_mode_and_acquire_options() {
        let toml = r#"
[acquire]
default_mode = "subscribe"
poll_only = true

[selection]
[[selection.node]]
id = "ns=2;s=A"
mode = "poll"
[[selection.node]]
id = "ns=2;s=B"
mode = "subscribe"
[[selection.node]]
id = "ns=2;s=C"

[[task]]
mode = "poll"
interval = "2s"
"#;
        let config = AcquireConfig::from_toml(toml).expect("config should parse");
        assert_eq!(config.acquire.default_mode, Some(NodeMode::Subscribe));
        assert!(config.acquire.poll_only);
        assert_eq!(config.selection.nodes[0].mode, NodeMode::Poll);
        assert_eq!(config.selection.nodes[1].mode, NodeMode::Subscribe);
        // Omitted mode defaults to auto.
        assert_eq!(config.selection.nodes[2].mode, NodeMode::Auto);
        // Explicit default wins over inference.
        assert_eq!(config.effective_default_mode(), NodeMode::Subscribe);
        assert_eq!(config.poll_interval(), Duration::from_secs(2));
    }

    #[test]
    fn effective_default_mode_inferred_from_tasks() {
        let poll_only = r#"
[selection]
node_ids = ["i=1"]
[[task]]
mode = "poll"
interval = "1s"
"#;
        assert_eq!(
            AcquireConfig::from_toml(poll_only)
                .unwrap()
                .effective_default_mode(),
            NodeMode::Poll
        );

        let sub_only = r#"
[selection]
node_ids = ["i=1"]
[[task]]
mode = "subscribe"
"#;
        assert_eq!(
            AcquireConfig::from_toml(sub_only)
                .unwrap()
                .effective_default_mode(),
            NodeMode::Subscribe
        );

        let both = r#"
[selection]
node_ids = ["i=1"]
[[task]]
mode = "poll"
interval = "1s"
[[task]]
mode = "subscribe"
"#;
        assert_eq!(
            AcquireConfig::from_toml(both)
                .unwrap()
                .effective_default_mode(),
            NodeMode::Auto
        );
    }

    #[test]
    fn poll_interval_falls_back_to_default() {
        // A subscribe-only config still yields the default poll interval for
        // any nodes routed to polling.
        let toml = r#"
[selection]
node_ids = ["i=1"]
[[task]]
mode = "subscribe"
"#;
        let config = AcquireConfig::from_toml(toml).unwrap();
        assert_eq!(config.poll_interval(), DEFAULT_POLL_INTERVAL);
    }
}
