# Component: Filters (`src/filters/`)

> Compile CLI filter args into a composite filter applied during traversal. Evidence: **[Confirmed]**.

## `mod.rs` — `FilterSet`
`{ include: PatternSet, exclude: PatternSet, predicate: Predicate }`.
- `compile(filters: &FilterArgs, namespaces: &NamespaceTable) -> Result<FilterSet>`.
- `path_decision(path: &str) -> PathDecision` — path-only layer:
  - **`PathDecision { descend, path_included, pruned }`**.
  - `exclude.matches(path)` → `pruned = true`, `descend = false` (stop browsing this branch).
  - `include` empty → `path_included = true`; else `path_included = include.matches(path)` and
    `descend` may stay true if the path is `prefix_compatible` (could be an ancestor of a match).
- `attributes_match(node_id, attrs, namespaces) -> bool` — final emission gate (delegates to
  `Predicate`).

## `pattern.rs` — `PatternSet`
`{ globs: Option<GlobSet>, regexes: Vec<Regex>, literal_prefixes: Vec<String>, has_regex, empty }`.
- `compile(globs, regexes) -> Result<PatternSet>` (errors on invalid pattern → exit `2`).
- `matches(path)` — true if any glob/regex matches.
- `prefix_compatible(path)` — **conservative** ancestor check for safe branch pruning; returns
  `true` whenever any regex is present (regex prefixes can't be statically analyzed).
- Glob semantics: `/` separator, `*` within a segment, `**` across segments.

## `predicate.rs` — attribute predicates
- **`Predicate`** `{ node_classes: HashSet<i32>, has_node_class_filter, datatype: DataTypeMatcher,
  readable, writable, historizing, subscribable, ns_indexes: Option<HashSet<u16>> }`.
  - `compile(filters, namespaces)` — maps namespace URIs → indexes (errors if URI unknown).
  - `matches(node_id, attrs, namespaces)` — checks, in order: namespace index restriction → node
    class → datatype → access-level bits (readable/writable) → historizing → subscribable heuristic.
- **`DataTypeMatcher`** `{ names (lowercased builtin), node_ids, ns_uris, empty }` — matches a
  Variable's datatype by builtin name, explicit NodeId, or namespace URI.
- **`is_subscribable(attrs) -> bool`** = `node_class == Variable && attrs.is_readable()`. (No
  sampling-interval probe; confirm with `--validate-subscription`.)
- `builtin_datatype_name(node_id) -> Option<&str>` — ns0 builtin NodeId → name (1→Boolean … 11→Double,
  12→String, …).
- `canonical_data_type(builtin) -> Option<&str>` — cross-platform mapping (e.g. `Double→float64`,
  `String→string`, `Structure→None`).

## Invariants
- Path filtering and attribute filtering are separate layers; both must pass to emit.
- Browse paths use namespace **URIs**, so patterns are stable across sessions.

## Extension points
- New attribute filter → extend `FilterArgs` + `Predicate::compile`/`matches`.
- New datatype canonicalization → extend `canonical_data_type`.
