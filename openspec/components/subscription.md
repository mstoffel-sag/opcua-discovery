# Component: Subscription (`src/subscription/`)

> Monitored-item lifecycle for validation, live sampling, and acquire-mode subscriptions.
> Evidence: **[Confirmed]**.

## Constants
- `KEEP_ALIVE_COUNT = 10`, `LIFETIME_COUNT = 30` (3× keep-alive per OPC UA spec).

## `SubscriptionParams`
`{ publishing_interval: Duration (def 500ms), sampling_interval: Duration (def 250ms),
   queue_size: u32 (def 10) }`.

## Functions
- `validate_records(session, records: &mut [NodeRecord], cancel)` — for each `subscribable` record,
  try to create a temporary subscription + monitored item; write result into `validation_status`;
  on server-side failure set `subscribable = false`. Cleans up.
- `validate_one(session, node_id) -> Result<(), String>` — test-create a single monitored item.
- `run_sample(session, nodes: Vec<(String, NodeId)>, duration, cancel) -> StatusCode` — subscribe
  (publishing 500ms), print `path = value` on each data change, run for `duration` or until cancel.
- `run_subscription(session, nodes: Vec<SelectedNode>, params, sink: Arc<Sink>, cancel) -> StatusCode`
  — long-lived subscription for `acquire`; a `DataChangeCallback` emits `Sample`s into the `Sink`;
  stops on cancel.

## Used by
- `browse --validate-subscription` → `validate_records`.
- `sample` → `run_sample`.
- `acquire` subscribe task → `run_subscription`.

## Invariants
- Validation is best-effort and non-destructive (temporary items, cleaned up).
- A node that is `subscribable` by heuristic may still fail validation → demoted.

## Extension points
- Expose keep-alive/lifetime counts via config if servers need tuning.
