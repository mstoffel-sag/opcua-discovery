# opcua-scan

A fast, production-grade command-line tool for **scanning and discovering an OPC UA
namespace**. It connects to an OPC UA server, browses the address space efficiently,
and reports nodes with rich metadata, powerful filtering, subscribable detection, and
multiple export formats.

`opcua-scan` is built for real industrial environments where address spaces can contain
**10,000–500,000+ nodes**, so traversal is concurrent, cycle-safe, paginated, and
streamed to keep memory low and avoid overloading the server.

Built in Rust on top of [`async-opcua`](https://github.com/freeopcua/async-opcua).

---

## Features

- **Four commands**: `connect`, `browse` (primary), `search`, `sample`.
- **Concurrent traversal**: BFS or DFS with a bounded worker pool, continuation-point
  pagination, retry with backoff on transient failures, and cycle protection.
- **Depth control**: limit traversal depth or go `unlimited`.
- **Rich filters**: include/exclude by glob or regex on the browse path, node class,
  data type, namespace, access level (readable/writable/historizing), and a
  subscribable heuristic.
- **Subscribable detection** and optional **subscription validation** (actually creates a
  temporary monitored item to confirm a node can be monitored).
- **Value preview** for variables.
- **Output**: human-readable `tree` / `table` / `json`, and machine-readable
  `json` / `ndjson` / `csv` exports to stdout or a file.
- **Interactive TUI** (opt-in) for lazy, on-demand exploration.
- **Watch mode** to re-scan periodically.
- **Profiles** to save per-server defaults, and a best-effort **metadata cache**.
- **Secure by default**: the server certificate is only auto-trusted with
  `--trust-server`; secrets are never logged.

---

## Installation

```bash
# From the repository root
cargo build --release
# Binary at target/release/opcua-scan
```

Requires a recent stable Rust toolchain (edition 2021, rustc ≥ 1.80).

---

## Quick start

```bash
# Verify connectivity and print server metadata + endpoints
opcua-scan connect opc.tcp://localhost:4840

# Browse the first few levels and print a tree
opcua-scan browse opc.tcp://localhost:4840 --depth 3

# Find all subscribable variables and export to CSV
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited --subscribable \
  --output csv --file variables.csv
```

---

## Commands

### `connect`

Verify connectivity and print server application metadata, the namespace table, and all
available endpoints with their security policies.

```bash
opcua-scan connect opc.tcp://10.0.0.10:4840
```

### `browse` (primary)

Traverse the address space and report matching nodes.

```bash
# Tree view, depth 4
opcua-scan browse opc.tcp://localhost:4840 --depth 4

# Only Variables under a Factory path, as a table, with current values
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited \
  --include "Objects/Factory/**" \
  --type Variable \
  --preview-values \
  --format table

# Exclude a noisy subtree, keep only Double variables, validate subscriptions
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited \
  --exclude "**/Diagnostics/**" \
  --type Variable --datatype Double \
  --subscribable --validate-subscription \
  --output ndjson --file nodes.ndjson

# Depth-first traversal with more workers and a per-branch child cap
opcua-scan browse opc.tcp://localhost:4840 \
  --strategy dfs --workers 16 --max-children 5000 --depth unlimited

# Re-scan every 30 seconds until Ctrl+C
opcua-scan browse opc.tcp://localhost:4840 --depth 3 --watch 30s
```

### `search`

Fuzzy-search the address space for nodes whose name, path, or description matches a query.

```bash
opcua-scan search opc.tcp://localhost:4840 "temp" --depth unlimited
opcua-scan search opc.tcp://localhost:4840 "motor speed" --limit 20 --output json
```

### `sample`

Subscribe to matching variables and print live value updates for a duration.

```bash
# Sample every subscribable variable under a path for 15 seconds
opcua-scan sample opc.tcp://localhost:4840 \
  --include "Objects/Factory/Line1/**" \
  --depth unlimited --duration 15s
```

---

## Interactive TUI

Add `--tui` to `browse` to launch an interactive explorer that browses **lazily** —
children are fetched from the server only when you expand a node, keeping server load
minimal on huge address spaces.

```bash
opcua-scan browse opc.tcp://localhost:4840 --tui
```

Keys:

| Key | Action |
| --- | --- |
| `↑` / `↓` or `k` / `j` | Move selection |
| `→` / `Enter` / `l` | Expand (browse children on demand) |
| `←` / `h` | Collapse / go to parent |
| `v` | Preview the selected variable's value |
| `s` | Validate subscription for the selected variable |
| `e` | Export the loaded nodes (uses `--file`, or `opcua-scan-export.json`) |
| `/` | Incremental path filter |
| `g` / `G` | Jump to top / bottom |
| `q` / `Esc` | Quit |

The terminal is always restored on exit, even on panic.

---

## Filtering reference

| Flag | Meaning |
| --- | --- |
| `--include <GLOB>` | Keep branches whose browse path matches the glob (repeatable). |
| `--include-regex <RE>` | Same, using a regular expression. |
| `--exclude <GLOB>` | Prune branches matching the glob (repeatable). |
| `--exclude-regex <RE>` | Same, using a regular expression. |
| `--type <CLASS,...>` | Node classes: `Object`, `Variable`, `Method`, `ObjectType`, `VariableType`, `ReferenceType`, `DataType`, `View`. |
| `--datatype <TYPE,...>` | Variable data type by builtin name (e.g. `Double`), NodeId, or namespace URI. |
| `--include-ns <URI>` | Restrict to namespace URIs (repeatable). |
| `--ns-uri <URI>` / `--ns-index <N>` | Restrict to a single namespace. |
| `--readable` / `--writable` / `--historizing` | Access-level filters. |
| `--subscribable` | Only variables that look eligible for monitoring. |

Globs use `/` as the path separator and support `*` (within a segment) and `**`
(across segments). Browse paths use **namespace URIs**, not session-local numeric indexes.

---

## Traversal & output reference

| Flag | Default | Meaning |
| --- | --- | --- |
| `--depth <N\|unlimited>` | `3` | Maximum depth (root = 0). |
| `--root <NODEID>` | Objects (`i=85`) | Starting node. |
| `--strategy <bfs\|dfs>` | `bfs` | Traversal order. |
| `--workers <N>` | `8` | Concurrent browse workers. |
| `--page-size <N>` | `200` | References requested per browse call. |
| `--max-children <N>` | — | Cap children per branch; reports truncation. |
| `--format <tree\|table\|json>` | `tree` | Human-readable display. |
| `--output <json\|ndjson\|csv>` | — | Machine-readable export. |
| `--file <FILE>` | stdout | Export destination. |
| `--preview-values` | off | Read current values for matched variables. |
| `--watch <DURATION>` | — | Repeat the scan periodically. |

---

## Connection, security & authentication

| Flag | Meaning |
| --- | --- |
| `--user <USER>` | Username for user/password auth. |
| `--password <PASSWORD\|env:VAR>` | Password; use `env:VAR` to read from an env var, or omit to be prompted securely. Secrets are never logged. |
| `--cert <FILE>` / `--key <FILE>` | Client certificate and private key. |
| `--security-policy <POLICY>` | `None`, `Basic256Sha256`, `Aes128Sha256RsaOaep`, `Aes256Sha256RsaPss`. |
| `--security-mode <MODE>` | `None`, `Sign`, `SignAndEncrypt`. |
| `--trust-server` | Auto-trust the server certificate (**insecure**; lab use only). |
| `--pki-dir <DIR>` | Client PKI directory (own cert, key, trusted certs). |
| `--timeout <DURATION>` | Per-request timeout (default `10s`). |
| `--retry <N>` | Retries for transient failures (default `3`). |
| `--cache <DIR>` | Best-effort metadata cache directory. |

```bash
# User/password from an environment variable, signed & encrypted channel
export OPCUA_PW='s3cret'
opcua-scan browse opc.tcp://10.0.0.10:4840 \
  --user operator --password env:OPCUA_PW \
  --security-policy Basic256Sha256 --security-mode SignAndEncrypt \
  --depth 3
```

---

## Profiles

Save common settings for a server in a YAML profile, then select it with `--profile NAME`
(a name is resolved under `<config-dir>/opcua-scan/profiles/<name>.yaml` or
`~/.opcua-scan/profiles/<name>.yaml`) or pass a file path directly. **CLI flags always
override profile values.**

```yaml
# ~/.opcua-scan/profiles/plant.yaml
endpoint: opc.tcp://10.0.0.10:4840
user: operator
security-policy: Basic256Sha256
security-mode: SignAndEncrypt
timeout: 15s
retry: 5

depth: unlimited
strategy: bfs
workers: 12

include:
  - "Objects/Factory/**"
exclude:
  - "**/Diagnostics/**"
node-class:
  - Variable
datatype:
  - Double
```

```bash
opcua-scan browse --profile plant --subscribable --output csv --file plant.csv
```

---

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | General failure |
| `2` | Invalid usage / arguments |
| `3` | Connection failure |
| `4` | Cancelled (Ctrl+C) |

---

## Logging

Logging goes to **stderr** so it never pollutes machine-readable output on stdout.

- `-v` / `-vv` / `-vvv` increase verbosity (info / debug / trace).
- `-q` suppresses everything except errors.

The underlying OPC UA libraries are kept at `warn` unless you pass `-vvv`.

---

## Development

```bash
cargo fmt
cargo clippy --all-targets
cargo test
```

Integration tests spin up an **in-process OPC UA server** with a small known address
space (`Factory/Line1/Motor/{Temperature, Speed}`) and drive the compiled binary against
it. To run the tests against an **external** server instead, set:

```bash
export OPCUA_TEST_ENDPOINT=opc.tcp://localhost:4840
cargo test
```

---

## License

MPL-2.0
