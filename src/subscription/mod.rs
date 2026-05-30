//! Subscription validation and live sampling.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use opcua::client::{DataChangeCallback, Session};
use opcua::types::{MonitoredItemCreateRequest, NodeId, StatusCode, TimestampsToReturn};
use tracing::{debug, warn};

use crate::export::NodeRecord;

/// Validate that subscribable records can actually be monitored.
///
/// Creates a single temporary subscription, attempts to create a monitored item
/// for each subscribable node, records the per-node result, then cleans up.
pub async fn validate_records(
    session: &Arc<Session>,
    records: &mut [NodeRecord],
    cancel: Arc<AtomicBool>,
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }

    let candidates: Vec<(usize, NodeId)> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.subscribable)
        .filter_map(|(i, r)| NodeId::from_str(&r.node_id).ok().map(|id| (i, id)))
        .collect();

    if candidates.is_empty() {
        return;
    }

    let subscription_id = match session
        .create_subscription(
            Duration::from_millis(1000),
            10,
            30,
            0,
            0,
            true,
            DataChangeCallback::new(|_dv, _item| {}),
        )
        .await
    {
        Ok(id) => id,
        Err(status) => {
            warn!(%status, "could not create validation subscription");
            for (i, _) in &candidates {
                records[*i].validation_status = Some(format!("subscription failed: {status}"));
            }
            return;
        }
    };

    let requests: Vec<MonitoredItemCreateRequest> =
        candidates.iter().map(|(_, id)| id.clone().into()).collect();

    match session
        .create_monitored_items(subscription_id, TimestampsToReturn::Both, requests)
        .await
    {
        Ok(results) => {
            for ((i, _), result) in candidates.iter().zip(results.iter()) {
                let status = result.result.status_code;
                records[*i].validation_status = Some(if status.is_good() {
                    "ok".to_string()
                } else {
                    status.to_string()
                });
                if !status.is_good() {
                    records[*i].subscribable = false;
                }
            }
        }
        Err(status) => {
            warn!(%status, "create_monitored_items failed during validation");
            for (i, _) in &candidates {
                records[*i].validation_status = Some(status.to_string());
            }
        }
    }

    let _ = session.delete_subscription(subscription_id).await;
    debug!("validation subscription cleaned up");
}

/// Validate that a single node can be monitored. Returns `Ok(())` if a monitored
/// item was created successfully, otherwise an `Err` with a human-readable
/// reason. Cleans up the temporary subscription before returning.
pub async fn validate_one(
    session: &Arc<Session>,
    node_id: &NodeId,
) -> std::result::Result<(), String> {
    let subscription_id = session
        .create_subscription(
            Duration::from_millis(1000),
            10,
            30,
            0,
            0,
            true,
            DataChangeCallback::new(|_dv, _item| {}),
        )
        .await
        .map_err(|s| format!("subscription failed: {s}"))?;

    let request: MonitoredItemCreateRequest = node_id.clone().into();
    let result = session
        .create_monitored_items(subscription_id, TimestampsToReturn::Both, vec![request])
        .await
        .map_err(|s| s.to_string());

    let outcome = match result {
        Ok(items) => match items.first() {
            Some(item) if item.result.status_code.is_good() => Ok(()),
            Some(item) => Err(item.result.status_code.to_string()),
            None => Err("no result returned".to_string()),
        },
        Err(e) => Err(e),
    };

    let _ = session.delete_subscription(subscription_id).await;
    outcome
}

/// Subscribe to the given nodes and stream live value changes for `duration`,
/// printing `path = value` lines. Cleans up on completion or cancellation.
pub async fn run_sample(
    session: &Arc<Session>,
    nodes: Vec<(String, NodeId)>,
    duration: Duration,
    cancel: Arc<AtomicBool>,
) -> StatusCode {
    if nodes.is_empty() {
        eprintln!("no matching variables to sample");
        return StatusCode::Good;
    }

    // Map node id -> browse path for friendly output.
    let path_map: Arc<HashMap<NodeId, String>> = Arc::new(
        nodes
            .iter()
            .map(|(p, id)| (id.clone(), p.clone()))
            .collect(),
    );
    let cb_map = path_map.clone();

    let subscription_id = match session
        .create_subscription(
            Duration::from_millis(500),
            10,
            30,
            0,
            0,
            true,
            DataChangeCallback::new(move |dv, item| {
                let node_id = &item.item_to_monitor().node_id;
                let label = cb_map
                    .get(node_id)
                    .cloned()
                    .unwrap_or_else(|| node_id.to_string());
                let value = dv
                    .value
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<no value>".to_string());
                println!("{label} = {value}");
            }),
        )
        .await
    {
        Ok(id) => id,
        Err(status) => {
            warn!(%status, "could not create sample subscription");
            return status;
        }
    };

    let requests: Vec<MonitoredItemCreateRequest> =
        nodes.iter().map(|(_, id)| id.clone().into()).collect();

    if let Err(status) = session
        .create_monitored_items(subscription_id, TimestampsToReturn::Both, requests)
        .await
    {
        warn!(%status, "could not create monitored items");
        let _ = session.delete_subscription(subscription_id).await;
        return status;
    }

    // Wait for the duration or cancellation.
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = session.delete_subscription(subscription_id).await;
    StatusCode::Good
}
