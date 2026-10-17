# Component: Interactive TUI (`src/tui/`)

> Opt-in `browse --tui` explorer: lazy browsing, marking, export, and acquire-config generation.
> Evidence: **[Confirmed]** (structure summarized; file is large).

## Entry
- **`TuiOptions`** `{ root: NodeId, page_size, export_file: Option<PathBuf>, strategy, workers,
  max_children, retry, preview_values, connection: GenConnection, acquire_file: PathBuf,
  selected_ids: HashSet<String>, selected_paths: HashSet<String>, source_config: Option<PathBuf> }`.
- `TuiOptions::run(session, namespaces, filters, options) -> Result<()>` — launches the loop; the
  terminal is always restored on exit, **including panic**.

## App state (`App`)
- `session, namespaces, filters` — live connection + metadata.
- `arena: Vec<Node>` — lazily-built tree; `visible: Vec<usize>` — flattened expanded chain.
- `selected: usize`, `list_state` — current selection.
- `filter_query: String`, `filter_mode: bool` — live path search (`/`).
- `marked: Vec<usize>` — logical marks (a mark = node + whole subtree).
- `mode: NodeMode` — acquisition mode applied to new marks (Auto/Subscribe/Poll).
- `vars_only: bool` — hide non-Variable containers.
- `job, job_cancel, pending_job` — background work (browse/export/collect).
- `Node { loaded, loading, marked, ... }` — lazy-load flags.

## Key bindings
| Key | Action |
| --- | --- |
| `↑/↓`, `k/j` | Move selection |
| `→/Enter/l` | Expand (lazy browse children on demand) |
| `←/h` | Collapse / parent |
| `space` | Mark node/subtree (auto mode) |
| `S` / `P` | Mark to subscribe / poll |
| `V` | Toggle vars-only |
| `v` | Preview selected variable's value |
| `s` | Validate subscription for selected variable |
| `e` | Export loaded/marked nodes (uses `--file` or `opcua-scan-export.json`) |
| `a` | Generate/update `acquire` config from marked variables |
| `x` | Cancel in-progress job |
| `/` | Incremental path filter |
| `g` / `G` | Jump top / bottom |
| `q` / `Esc` / `Ctrl+C` | Quit |

## Main loop
Render (tree + detail pane + progress/footer) → select over terminal input, background job messages
(`BrowseDone`/`ExportDone`/`CollectDone`), and a periodic tick (spinner) → handle input → process
job results (attach children, refresh visible, reposition selection).

## Lazy browsing
On expand, if a node isn't `loaded`, spawn a non-blocking `browse_into_children` job
(`opcua::browse::browse_children`); attach results on completion. Marks are logical/instant; export
and acquire traverse into unmarked nodes only as needed.

## Export / Acquire jobs
- **Export:** recursively traverse each marked root, build `NodeRecord`s for Variables, write
  json/ndjson/csv.
- **Acquire:** collect readable Variables under marks, then either `update_selection_in_toml`
  (editing an existing `source_config`) or `generate_toml` (new config); if editing, prompt for
  `<poll|subscribe> <interval>`. See [acquire.md](acquire.md) `generate.rs`.
- **New config shape:** exactly one `[[device]]` (optional `name`), with security/connection
  fields such as `trust_server` directly on it and
  `protocol_address = { endpoint = "...", namespaces = [<full ordered URI table>] }`.
  Each `[[device.point]]` has a stable unique node-derived logical `id`,
  `address = { node_id = "ns=N;i=..." }`, and optional `name`, `mode`, `path`.
  Logical ids are not measurement names; omitted names retain the prior fallback.
- **Open/edit:** `browse --config FILE --tui` accepts preferred and legacy files. Match marks
  using resolved OPC UA addresses/paths, not logical ids. Device point addresses use
  config-local namespace indexes mapped through URIs to live indexes; missing mappings
  fail safely, with no numeric fallback when an array is supplied. Without `namespaces`,
  preferred and legacy addresses keep literal indexes (legacy per-node URI mapping still
  applies). Implicit/explicit namespace 0 is fixed to the standard OPC UA URI.
  Saving retained points preserves their logical ids,
  names, modes, and cached paths, and preserves unrelated task/output/acquire/filter settings.
  Never sort, compact, or deduplicate the shared namespace array; map new live nodes back
  into config-local slots. `browse --tui` and `browse --config` read the active session's
  NamespaceTable, bypassing potentially stale disk cache data for remapping/generation.
- **Scope:** multiple devices require separate files (`acquire --config-dir DIR`). Modes and
  cached paths are tool-specific extensions, not full tedge-dot compatibility; config
  generation adds no MQTT publishing or security guarantees.

## Integrations
Uses `opcua::browse` (lazy children), `opcua::value` (preview), `subscription` (validate),
`export` (`NodeRecord`), and `acquire::generate` (config emit).

## Extension points
- New key action → extend the input handler + (optionally) a new background job message.
- New marking semantics → extend `Node.marked` handling and the collect/export traversal.
