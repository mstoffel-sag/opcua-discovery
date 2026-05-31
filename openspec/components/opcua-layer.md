# Component: OPC UA Layer (`src/opcua/`)

> Thin async wrapper over `async-opcua 0.18` providing exactly what the rest of the crate needs.
> Evidence: **[Confirmed]** unless noted.

## Files & responsibilities

### `client.rs` — session lifecycle
- **`Connection`** `{ session: Arc<Session>, event_loop: JoinHandle<StatusCode>, endpoint, client }`.
  - `connect(args: &ConnectionArgs) -> Result<Connection>`: validate `opc.tcp://` scheme; build a
    `Client` (`build_client`); resolve identity token (X.509 `new_x509_path` / username
    `new_user_name` / `Anonymous`); construct `EndpointDescription` from policy+mode; connect to
    matching endpoint; spawn event loop; wait for connection with timeout (min 5s).
  - `session()`, `endpoint()`, `server_info() -> ServerInfo`, `disconnect()`.
- **`ServerInfo`** `{ application_uri, product_uri, application_name, endpoints: Vec<EndpointSummary> }`.
- **`EndpointSummary`** `{ url, security_policy, security_mode, security_level, user_tokens }`.
- Helpers: `build_client` (app identity, PKI dir, `create_sample_keypair(true)`, `trust_server`,
  retry), `resolve_identity`, `resolve_password` (`env:VAR` or `rpassword` prompt),
  `validate_endpoint`, `security_policy`/`security_mode` mappers, `shorten_policy_uri`.

### `browse.rs` — reference browsing
- `browse_children(session, node_id, page_size) -> Vec<ReferenceDescription>`: one-shot browse of
  forward `HierarchicalReferences` (`include_subtypes: true`), follows continuation points. Used by
  the TUI for lazy expansion.
- `browse_properties(session, node_ids) -> Vec<Vec<ReferenceDescription>>`: batch forward
  `HasProperty` browse (one vector per input node; no continuation — property sets are tiny).
- `reconstruct_browse_path(session, node_id, leaf) -> Option<String>`: walk inverse
  `HierarchicalReferences` up to stop nodes (Objects `i=85` / Root `i=84`), bounded to `MAX_DEPTH=32`,
  returning a `/`-separated path excluding the stop folder.

### `attributes.rs` — batch attribute reads
- Constants `CURRENT_READ = 0x01`, `CURRENT_WRITE = 0x02` (AccessLevel bits).
- **`NodeAttributes`** `{ node_class, display_name, browse_name, description, data_type, value_rank,
  access_level, user_access_level, historizing }` (all `Option`) + `is_readable`, `is_writable`,
  `is_unreadable_variable`, `is_historizing`.
- `read_node_attributes(session, node_ids) -> Vec<NodeAttributes>`: batch-reads a fixed 9-attribute
  set per node in **one** service call; defensively resizes to match input length.

### `value.rs` — value & type rendering
- **`ValueSnapshot`** `{ value: Option<String> }`; **`ValueReading`** `{ value: serde_json::Value,
  source_time, server_time, status }` + `from_data_value`, `best_time`.
- `variant_to_json` preserves **native** numeric/bool JSON types (not quoted) for thin-edge
  downstream; handles strings, byte strings (base64), datetimes (RFC3339), arrays (recursive), and
  extension objects.
- `extension_object_to_json` decodes `Range → {low, high}` and `EUInformation`; `range_from_data_value`,
  `eu_label_from_data_value` extract EU range/units.
- `read_values` (string snapshots), `read_readings` (with both timestamps), `read_data_values`
  (raw `DataValue` for structured decoding).

### `namespaces.rs` — namespace table
- **`NamespaceTable`** `{ uris: Vec<String>, by_uri: HashMap<String,u16> }`.
  - `read(session)` reads `Server_NamespaceArray` (`i=2255`); `from_uris`; `uri(index)`,
    `index(uri)`, `uris()`, `display(index)` (URI for ns>0, empty for ns0, `ns={index}` if unknown).

### `history.rs` — historical reads
- **`HistoryQuery`** `{ start: DateTime, end: DateTime, max_values: u32 }`.
- `read_raw(session, node_id, query) -> Vec<ValueReading>`: `ReadRawModifiedDetails`
  (`is_read_modified: false`), `TimestampsToReturn::Both`, follows continuation points until empty.
- `parse_time(s) -> DateTime` (RFC3339).

### `mod.rs` — class mapping
- `node_class_of(NodeClassArg) -> NodeClass`, `node_class_name(NodeClass) -> &str`.

## Invariants
- Attribute/value/property reads are **batched** (N nodes per service call).
- All operations are async over a cheaply-cloneable `Arc<Session>`.
- Browsing and history follow continuation points to completion.

## Extension points
- New attribute → extend the fixed list in `read_node_attributes` + `NodeAttributes`.
- New extension-object decoding → extend `extension_object_to_json`.
- New auth method → extend `resolve_identity`.
