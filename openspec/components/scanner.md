# Component: Scanner Engine (`src/scanner/`)

> Concurrent address-space traversal orchestrator. Evidence: **[Confirmed]** unless noted.

## Purpose
Browse the address space efficiently with bounded concurrency, pagination, retry/backoff, cycle
protection, depth control, and per-branch truncation, streaming matched nodes as `NodeRecord`s.

## `engine.rs`

### `ScanConfig`
`{ root: NodeId, depth: Depth, strategy: Strategy, workers: usize, page_size: u32,
   max_children: Option<usize>, retry: u32, preview_values: bool }`.

### `Engine`
`{ session: Arc<Session>, config: Arc<ScanConfig>, filters: Arc<FilterSet>,
   namespaces: Arc<NamespaceTable>, stats: Arc<ScanStats>, cancel: Arc<AtomicBool> }`.
- `new(session, config, filters, namespaces, cancel)`, `stats()`,
  `run(sink: mpsc::Sender<NodeRecord>) -> Result<()>`.

### Worker pool (`Shared`)
`{ queue: Mutex<VecDeque<Pending>>, active: AtomicUsize, notify: Notify,
   cancel: Arc<AtomicBool>, visited: Mutex<HashSet<NodeId>>, dfs: bool }`.
- One Tokio task per `worker`; each pulls a `Pending { node_id, depth, ... }` from the shared queue.
- **BFS** = `pop_front`, **DFS** = `pop_back` (same queue, different end).
- Idle workers wait on `Notify`; `notify_waiters()` wakes them when work is enqueued or on cancel.
- A worker exits when the queue is empty and `active == 0`.

### Per-node expansion
1. `browse_children` — browse forward `HierarchicalReferences` (`include_subtypes: true`),
   following continuation points; truncate to `max_children` (count `truncated`).
2. `read_node_attributes` — batch-read all children's attributes in one service call.
3. For each child: `filters.path_decision(path)` → prune (count `pruned`) / decide descend +
   include; if included and `filters.attributes_match(...)` → emit `NodeRecord` (count `matched`,
   and `subscribable` when applicable); optionally read current value (`preview_values`).
4. Enqueue child for expansion iff `descend` and `config.depth.allows_children_at(child_depth)`.

### Resilience
- **Cycle protection:** each `NodeId` processed at most once (`visited`).
- **Retry/backoff:** `browse_with_retry` / `browse_next_with_retry` retry transient statuses
  (`BadTimeout`, `BadTooManyOperations`, `BadServerTooBusy`, `BadNoContinuationPoints`,
  `BadServerNotConnected`) with backoff `min(100ms << attempt.min(5), 2000ms)` up to `retry`.
- **Cancellation:** workers poll `cancel`; a closed sink also cancels (send error).

## `stats.rs`
- **`ScanStats`** (atomics): `visited, matched, pruned, errors, subscribable, truncated`, `start`.
  Methods: `add_visited`, `inc_matched`, `inc_subscribable`, `inc_pruned`, `inc_errors`,
  `inc_truncated`, `snapshot()`.
- **`StatsSnapshot`**: same counters + `elapsed: Duration`.

## `mod.rs`
- `resolve_root(&Option<String>) -> Result<NodeId>` — parse `--root` or default to `ObjectsFolder`
  (`i=85`).

## Invariants
- Exclude patterns prune (stop descent); include + predicates gate emission.
- Depth is per-node; root is depth 0.
- Memory stays bounded by streaming records through an `mpsc` channel rather than collecting all.

## Extension points
- New traversal order → extend `Strategy` + queue discipline.
- New per-node side effect (e.g. eager history) → extend `expand`.
