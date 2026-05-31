# Component: Config / Profiles & Cache (`src/config/`, `src/cache/`)

> Saved per-server defaults (YAML) and a best-effort namespace cache. Evidence: **[Confirmed]**.

## Profiles (`src/config/mod.rs`) — `Profile`
`#[serde(deny_unknown_fields, rename_all = "kebab-case")]` (YAML). Optional fields mirror the CLI:

- **Connection:** `endpoint`, `user`, `password` (`env:VAR`), `security-policy`, `security-mode`,
  `trust-server`, `pki-dir`, `timeout` (humantime), `retry`.
- **Traversal:** `depth` (number/`unlimited`), `root`, `strategy` (`bfs`/`dfs`), `workers`,
  `page-size`, `max-children`.
- **Filters:** `include`, `include-regex`, `exclude`, `exclude-regex`, `include-ns`, `ns-uri`,
  `ns-index`, `node-class`, `datatype`.

### Loading & merge
- `load(value) -> Result<Profile>`:
  1. If `value` is an existing file path → load it directly.
  2. Else treat as a name and look in `<config-dir>/opcua-scan/profiles/<name>.{yaml,yml}` then
     `~/.opcua-scan/profiles/<name>.{yaml,yml}`.
  3. Error (`Config`) if not found.
- `apply(connection, traversal, filters)` → `apply_connection` + `apply_traversal` + `apply_filters`.
  - **CLI always wins**: profile only fills `None`/empty-`Vec`/still-default-scalar fields.

### Example
```yaml
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
node-class: [Variable]
datatype: [Double]
```

## Cache (`src/cache/mod.rs`) — `Cache`
- Stores only the **namespace table** per endpoint (indexes are session-local, but the URI array is
  stable for a server). Not a full address-space mirror.
- `open(dir)`, `load_namespaces(endpoint) -> Option<Vec<String>>`, `store_namespaces(endpoint, uris)`.
- File: `<cache-dir>/<sanitized-endpoint>.namespaces.json` containing `{ endpoint, uris }`.
- Best-effort: read/write failures are logged at debug, never fatal; mismatched `endpoint` (hash
  collision/stale) is ignored.

## Extension points
- Add profile fields by mirroring the CLI arg + extending the relevant `apply_*`.
- Extend the cache to store more stable metadata (keep it best-effort and keyed by endpoint).
