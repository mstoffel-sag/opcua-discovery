# Component: CLI (`src/cli/`, `src/main.rs`)

> The command surface and per-command orchestration. Evidence: **[Confirmed]** unless noted.

## Purpose
Define every subcommand, flag, default, and value enum (clap derive), and orchestrate each command's
run sequence. Shared argument groups (`ConnectionArgs`, `TraversalArgs`, `FilterArgs`, `OutputArgs`)
are reused across subcommands so flags behave consistently.

## Top-level (`src/cli/mod.rs`)

```
Cli { verbose: u8 (-v..), quiet: bool (-q), profile: Option<String> (--profile NAME|PATH), command }
Command = Connect | Browse | Search | Sample | Acquire
```

### `ConnectionArgs`
| Field | Flag | Default | Notes |
| --- | --- | --- | --- |
| `endpoint` | positional `ENDPOINT` | — | must start with `opc.tcp://` |
| `user` | `--user` | — | |
| `password` | `--password` | — | `env:VAR` or secure prompt; requires `--user`; never logged |
| `cert` | `--cert FILE` | — | client cert (PEM/DER) |
| `key` | `--key FILE` | — | requires `--cert` |
| `security_policy` | `--security-policy` | `None` | `None`/`Basic256Sha256`/`Aes128Sha256RsaOaep`/`Aes256Sha256RsaPss` |
| `security_mode` | `--security-mode` | `None` | `None`/`Sign`/`SignAndEncrypt` |
| `trust_server` | `--trust-server` | false | INSECURE; lab only |
| `pki_dir` | `--pki-dir DIR` | — | own cert/key/trusted |
| `timeout` | `--timeout` | `10s` | humantime |
| `retry` | `--retry` | `3` | transient retries |
| `cache` | `--cache DIR` | — | namespace-table cache |

### `TraversalArgs` (browse/search/sample)
| Field | Flag | Default |
| --- | --- | --- |
| `depth` | `--depth N\|unlimited` | `3` (root=0; `-1`/`unlimited` = no limit) |
| `root` | `--root NODEID` | `i=85` (Objects) |
| `strategy` | `--strategy bfs\|dfs` | `bfs` |
| `workers` | `--workers N` | `8` |
| `page_size` | `--page-size N` | `200` |
| `max_children` | `--max-children N` | none |

### `FilterArgs` (browse/search/sample)
`include`/`include_regex`/`exclude`/`exclude_regex` (repeatable), `include_ns` (URIs), `ns_uri`,
`ns_index`, `node_class` (`--type`, csv), `datatype` (csv: builtin name/NodeId/URI), `readable`,
`writable`, `historizing`, `subscribable`.

### `OutputArgs`
`format` (`--format tree|table|json`, default `tree`), `output` (`--output json|ndjson|csv`),
`file` (`--file`), `preview_values`, `tui`, `watch` (DURATION).

### Helpers
- `parse_duration` (humantime), `parse_depth` (`unlimited`/`-1` → `Unlimited`).
- `load_namespaces(session, connection, endpoint)` — cache-aware namespace read.
- Enums: `Depth { Limited(u32), Unlimited }` (`allows_children_at`), `Strategy`, `Format`,
  `Encoding`, `NodeClassArg`, `SecurityPolicyArg`, `SecurityModeArg`.

## Commands

### `connect` (`src/cli/connect.rs`)
`ConnectArgs { connection }`. Connect → read `ServerInfo` → read namespace table → print metadata,
namespaces, endpoints (+ security) → disconnect.

### `browse` (`src/cli/browse.rs`) — primary
`BrowseArgs { connection, traversal, filters, output, validate_subscription, config }`.
Optionally pre-seed from `--config` (acquire.toml) → connect → load namespaces →
compile filters → if `--tui` launch explorer (early return) → else watch-loop: run `Engine`, stream
`NodeRecord`s, optionally validate subscriptions, render/export; repeat on `--watch`.
Config loading accepts preferred `[[device]]` / `[[device.point]]` and legacy
`[connection]` / `[[selection.node]]` / `[selection].node_ids`. Device connection/security
fields are direct fields; endpoint and ordered namespace URIs are in `protocol_address`.
Preselection resolves point addresses by URI to live indexes, never by logical point id.
Top-level `[selection]` discovery filters keep their existing meaning.
`--tui` or `--config` always reads the active session's NamespaceTable, bypassing
the disk namespace cache; other browse invocations retain their existing cache behavior.
Shared URI arrays are ordered config-local indexes: never sort, compact, or deduplicate
them. When `namespaces` is absent, preferred and legacy addresses keep literal numeric
indexes (subject to any legacy per-node URI mapping). Implicit/explicit namespace 0
always denotes the fixed standard OPC UA URI.

### `search` (`src/cli/search.rs`)
`SearchArgs { connection, query, traversal, filters, output, limit (50) }`. Require query → scan →
fuzzy score on name/path/description → keep top-N → render/export.

### `sample` (`src/cli/sample.rs`)
`SampleArgs { connection, traversal, filters, duration (10s) }`. Force `subscribable=true` → scan →
collect `(path, NodeId)` → `subscription::run_sample` prints live `path = value` for duration.

### `acquire` (`src/cli/acquire.rs`)
`AcquireArgs { config, config_dir, duration, poll_only }`. Load one file or a sorted directory of
configs → apply `--poll-only` override (can only turn on) → `acquire::run(...)` per config. See
[acquire.md](acquire.md).
Each preferred config contains **exactly one device**. For multiple devices, place one file
per device in `--config-dir`; multi-device files are not supported.
Acquisition always reads the active session's NamespaceTable rather than a potentially
stale disk namespace cache before resolving addresses.

```sh
opcua-scan acquire --config examples/acquire-demo.toml --duration 3s
opcua-scan acquire --config-dir ./devices --poll-only
opcua-scan browse --config test.toml --tui
```

`[[task]]`, `[output]`, `[acquire]`, and discovery filters are unchanged. Logical point
ids are config identity only, not output names. This is not a new engine, an MQTT
publisher, or full tedge-dot compatibility; poll/subscribe/auto and cached paths remain
tool-specific. Acquisition resolves namespaces once per run; client session retries do
not add runner-level namespace refresh/remapping after reconnect.

## Behavioral notes
- `main.rs` merges profile, installs signal handler, dispatches, maps errors→exit codes, and exits
  `4` if cancelled after a clean return.
- `--quiet` conflicts with `--verbose`. `--password` requires `--user`; `--key` requires `--cert`.

## Extension points
- New subcommand → add a `Command` variant + `src/cli/<name>.rs` with `run()` + wire in `main.rs`.
- New shared flag → add to the relevant `*Args` group so all commands inherit it.
