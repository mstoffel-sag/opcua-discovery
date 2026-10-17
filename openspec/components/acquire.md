# Component: Acquire (`src/acquire/`)

> Config-driven data acquisition: schema, node selection, sinks, orchestration, and config
> generation. Evidence: **[Confirmed]** unless noted.

## Overview
`acquire` connects once, resolves a node selection from a TOML config, partitions nodes into poll vs
subscribe by **effective mode**, and runs scan/poll/subscribe/history tasks concurrently into one
`Sink`. Reconstructed browse paths are cached back into the config file.

---

## Config schema (`config.rs`) — `AcquireConfig` (`#[serde(deny_unknown_fields)]`)

Preferred tables: exactly one `[[device]]`, zero or more `[[device.point]]`, optional
`[acquire]`, `[selection]` (discovery filters), `[output]`, and one or more `[[task]]`
(**≥1 required**). Use `acquire --config-dir DIR` for multiple device files, not multiple
devices in one file. Legacy connection and explicit-selection tables remain readable.

### `[[device]]` — preferred connection/source
- Optional `name` becomes `meta.source.name`.
- `protocol_address = { endpoint = "...", namespaces = ["http://opcfoundation.org/UA/", ...] }`.
  `namespaces` is the full ordered config-local URI table, not a compact list of selected URIs.
- Connection/security options live **directly on the device**: `user`, `password` (`env:VAR`),
  `cert`, `key`, `security_policy`, `security_mode`, `trust_server`, `pki_dir`, `timeout`,
  `retry`, `cache`. Do not nest these in `protocol_address` or a device security table.
- `trust_server` remains opt-in and insecure; this schema adds no security guarantees.

### `[[device.point]]` — preferred explicit selection
- `id`: required unique logical config key (for example `boiler-output`), **not** an OPC UA
  NodeId and **not** an output measurement name.
- `address = { node_id = "ns=4;i=1244" }`: OPC UA address with a config-local namespace index.
  `address.namespace` is unsupported; preferred namespace URIs belong only in
  `protocol_address.namespaces`. Legacy `[[selection.node]].namespace` remains supported.
- Optional `name`, `mode` (`auto` / `subscribe` / `poll`), and cached `path` retain the
  legacy meanings. Omitted `name` retains the previous browse-name/NodeId fallback;
  logical ids must not leak into output names. Existing path-based output keys are unchanged.

#### Namespace safety
Before use, a point's configured numeric namespace index selects a URI from the device's
shared array; that URI is looked up in the live server table and the NodeId is remapped
to the live index. With an array supplied, an absent config slot or absent live URI
fails safely: never reuse the configured numeric index as a fallback. Without a
`namespaces` table, both preferred and legacy addresses retain literal indexes;
legacy per-node `namespace` URI mappings still apply when supplied. Implicit namespace
0 (`i=...`) and explicit `ns=0` are fixed to `http://opcfoundation.org/UA/`;
array slot 0 must have that URI. Include all preceding slots, even unused ones.
Never sort, compact, or deduplicate an array. An ns0-only file may use just
`["http://opcfoundation.org/UA/"]`.

Acquisition always reads the active session's NamespaceTable, bypassing the disk
namespace cache. `browse --tui` and `browse --config` do the same for safe preselection,
remapping, and config generation; other browse cache behavior is unchanged.

The fixture `test.toml` uses the full nine-entry table recorded for its endpoint in
`tmp/opc_tcp___opcua_demo_this_com_51210_UA_SampleServer.namespaces.json`; its namespace
4/5 selections retain the Boiler and Boiler/Instance URIs at those slots.

### `[connection]` — legacy `ConnectionConfig`
`endpoint`, `name` (→ `meta.source.name`), `user`, `password` (`env:VAR`), `cert`, `key`,
`security_policy`, `security_mode`, `trust_server` (def false), `pki_dir`, `timeout` (humantime),
`retry`, `cache`. Methods `to_args()`, `apply_to(&mut ConnectionArgs)` (CLI precedence).

### `[acquire]` — `AcquireOptions`
- `default_mode: Option<NodeMode>` — applied to nodes without a mode; when unset, inferred from tasks.
- `poll_only: bool` (def false) — force all nodes to poll.
- **`NodeMode`** (`lowercase`): `auto` (default) | `subscribe` | `poll`.

### `[selection]` — discovery filters and legacy explicit selection
- `node_ids: Vec<String>` — legacy bare ids (index not remapped).
- `nodes: Vec<NodeSelector>` (`#[serde(rename = "node")]`) — rich entries.
- `root: Option<String>` (def `i=85`), `depth: Option<String>` (number/`unlimited`, def 3).
- Filters: `include`, `include_regex`, `exclude`, `exclude_regex`, `include_ns`, `node_class`,
  `datatype`.
- Methods: `depth() -> Depth`, `to_filter_args() -> FilterArgs`.

#### `[[selection.node]]` — legacy `NodeSelector`
`id` (e.g. `ns=4;i=1274`), `namespace` (URI; remaps `id`'s index to the server's current index for
that URI), `name` (measurement name), `path` (cached browse path; reconstructed + cached on first run
if absent), `mode: NodeMode` (def `auto`).

Keep legacy `id` semantics: it is the OPC UA NodeId, unlike the new point's logical
`id`. Do not mix preferred device syntax with legacy connection/explicit selections.
Discovery filters in `[selection]` remain top-level and work with either schema;
`[acquire]`, `[output]`, and `[[task]]` semantics are unchanged.

The device/point shape is a limited tedge-dot-style configuration surface, not full
tedge-dot compatibility. Per-point modes and cached paths are tool-specific extensions.
It neither replaces the acquisition engine/sinks nor adds an MQTT client.

### `[output]` — `OutputConfig`
- `format: OutputFormat` (`lowercase`): `tedge` (default) | `json` | `ndjson` | `csv`.
- `file: Option<PathBuf>` (else stdout), `topic: Option<String>` (MQTT topic **prefix** hint for
  downstream `tedge mqtt pub` — no MQTT client here **[Likely]**),
  `metadata: MetadataMode` (`nested` default | `off`).

### `[[task]]` — `TaskConfig` (`#[serde(tag = "mode", rename_all = "lowercase")]`)
| `mode` | Variant fields |
| --- | --- |
| `scan` | `strategy` (bfs/dfs), `workers?`, `page_size?`, `max_children?`, `preview_values` (def false) |
| `poll` | `interval: Duration` (**required**, humantime) |
| `subscribe` | `publishing_interval?`, `sampling_interval?`, `queue_size?` |
| `history` | `start: String` (RFC3339, **required**), `end?` (def now), `max_values?` |

### Derived logic on `AcquireConfig`
- `effective_default_mode()` — explicit if set; else inferred (poll-only tasks → poll; subscribe-only
  → subscribe; both/neither → auto).
- `poll_interval()` — first poll task's interval, else `DEFAULT_POLL_INTERVAL = 1s`.
- `subscribe_params()` — first subscribe task's params.
- `from_file` / `from_toml` — parse + validate (≥1 task; node selection required for non-scan tasks).

---

## Orchestration (`mod.rs`)
`run(config, duration: Option<Duration>, config_path: Option<&Path>, cancel) -> Result<()>`:
1. `Connection::connect` (once) → shared `Arc<Session>`.
2. Read the active session's NamespaceTable (bypass disk cache) for URI remapping.
3. Build `Sink` from `[output]`.
4. If poll/subscribe/history tasks exist → `selection::resolve` → `Resolved { nodes, path_updates }`;
   optionally `persist_node_paths` (write reconstructed paths back via `toml_edit`).
5. `partition_nodes(nodes, effective_default_mode, poll_only) -> (poll_nodes, subscribe_nodes)` using
   `effective_subscribe(node, default, poll_only)`:
   - `poll_only` → always poll.
   - explicit node mode wins; `auto` → subscribe iff `subscribable && server supports monitoring`,
     else poll.
6. Spawn a `JoinSet`: `run_scan` (scan tasks), `run_history` (history tasks), one `run_poll` loop,
   one `run_subscription` — all → `Sink`.
7. If `duration` set, a timer sets `cancel` to bound continuous tasks.
8. Await all → `Sink::finish()` → disconnect.

### Reconnection limitation
`mod.rs` connects once, loads namespaces once, and resolves points once before spawning
tasks. `opcua/client.rs` starts the underlying session event loop with configured session
retry limits and an initial retry delay; this is not a runner-level reconnect/remap loop.
The runner does not reload namespace tables, re-resolve point addresses, or guarantee
subscription recovery after a reconnect. Restart after server namespace-table changes.
URI remapping protects initial resolution, not continuous namespace changes during a run.

- `run_poll` — ticker → batch `read_readings(nodes)` → emit `Sample`s.
- `run_history` — `history::read_raw` per node over the time range → emit `Sample`s.
- Unit tests cover poll_only override, explicit-mode-wins, auto-follows-default, auto-uses-
  subscribability, and partition routing.

---

## Selection (`selection.rs`)
- **`SelectedNode`** `{ name, path, node_id, mode, subscribable, meta: SampleMeta }`.
- **`Resolved`** `{ nodes, path_updates: Vec<(config_id, reconstructed_path)> }`.
- `resolve(session, config, namespaces, endpoint, cancel)` stages:
  1. **Explicit** (`resolve_explicit`): resolve point `address.node_id` through the device URI
     array (or parse legacy `id` + optional `namespace`), read
     attributes, skip unreadable, resolve path (cached or `reconstruct_browse_path`), build
     `SampleMeta`; dedupe by `NodeId`.
  2. **Filtered** (`resolve_filtered`, if filters present): run `Engine` with `subscribable=true`;
     discovered nodes default to `auto`.
  3. **Enrich** (`enrich_metadata`): fold `EURange`/`EngineeringUnits` properties into parents and
     drop them as standalone nodes.
  4. **Stamp** `source_endpoint` / `source_name` on every node.

---

## Sink (`sink.rs`)
- **`Sample`** `{ name, node_id, value: serde_json::Value, time, status, meta: Arc<SampleMeta> }`.
- **`SampleMeta`** `{ data_type, engineering_units, browse_name, display_name, browse_path,
  namespace, eu_range: Option<(f64,f64)>, source_endpoint, source_name }` + `descriptive_json()`,
  `output_key(fallback)`.
- **`Sink`** `{ format, topic, metadata, state: Mutex<State> }`:
  - `new(&OutputConfig)`, `emit_sample`, `emit_record`, `finish`.
  - `tedge`/`ndjson` write immediately; `json`/`csv` buffer then flush on `finish`.
- Output shapes:
  - `tedge`: `{ "time", "<key>": <value>, "meta": {...} }` (key = browse path or name; `meta` omitted
    when `MetadataMode::Off`).
  - `ndjson`/`json`: stable record `{ time, key, value, dataType, nodeId, units, status, meta }`
    (off-mode trims to `time, key, value, nodeId, status`).
  - `csv`: union-of-keys header, metadata flattened, buffered.

---

## Config generation (`generate.rs`) — used by the TUI
- New TUI configs emit one `[[device]]`, direct device connection/security fields, the endpoint
  and full ordered namespace table in `protocol_address`, and `[[device.point]]` entries.
  Point ids are stable, unique, node-derived logical keys; OPC UA ids go in `address.node_id`.
- Generation retains optional names, cached paths, and per-point modes, emitting poll and/or
  subscribe tasks and the existing `[acquire]` policy as appropriate. Passwords are not written.
- Editing a device config preserves logical ids/names for retained points and maps live namespace
  indexes back into its config-local URI table. Do not compact/reorder the array without rewriting
  every affected point address.
- Legacy files remain supported when loading/editing. Selection updates preserve unrelated
  task/output/acquire/filter settings; path caching updates the matching point or legacy node.

---

## Example (working) config
```toml
[[device]]
name = "Factory"        # optional source name
trust_server = true
protocol_address = { endpoint = "opc.tcp://host:4840", namespaces = ["http://opcfoundation.org/UA/", "urn:example:factory"] }

[[device.point]]
id = "temperature"
address = { node_id = "ns=1;i=1001" }
name = "Temperature"
mode = "subscribe"

[acquire]
default_mode = "auto"   # auto | subscribe | poll

[output]
format = "tedge"

[[task]]
mode = "poll"
interval = "1s"
[[task]]
mode = "subscribe"
publishing_interval = "500ms"
```

`trust_server = true` is for lab use only. The namespace array above is a complete
two-slot example table, not the public demo server's table.

## Extension points
- New task type → add a `TaskConfig` variant + a `run_*` task + spawn wiring.
- New sink format → add an `OutputFormat` variant + `emit_*`/`finish` branches.
- New metadata field → extend `SampleMeta` + `descriptive_json`.
