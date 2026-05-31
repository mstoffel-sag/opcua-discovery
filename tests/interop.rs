//! Cross-implementation interoperability tests.
//!
//! These exercise the compiled `opcua-scan` binary against **real** external
//! OPC UA servers (open62541, node-opcua, Eclipse Milo, the OPC Foundation
//! reference server, Microsoft OPC-PLC, the public demo server, …) to catch
//! places where servers interpret the spec slightly differently.
//!
//! They are excluded from the default `cargo test` run in two ways, so the
//! normal suite stays hermetic and fast:
//!   1. Every test is marked `#[ignore]` (run them with `-- --ignored`).
//!   2. Even when run, each test no-ops unless `OPCUA_INTEROP_ENDPOINTS` lists
//!      one or more servers.
//!
//! `OPCUA_INTEROP_ENDPOINTS` is a comma-separated list of `name=endpoint`
//! pairs, e.g.:
//!
//! ```text
//! OPCUA_INTEROP_ENDPOINTS="open62541=opc.tcp://127.0.0.1:4840/,node-opcua=opc.tcp://127.0.0.1:4841/"
//! ```
//!
//! Bring the reference servers up with `just servers-up` (see
//! `tests/interop/docker-compose.yml`) before running:
//!
//! ```text
//! just servers-up
//! cargo test --test interop -- --ignored
//! ```

mod common;

use std::time::Duration;

use common::wait_for;

/// Parse the `OPCUA_INTEROP_ENDPOINTS` env var into `(name, endpoint)` pairs.
fn interop_endpoints() -> Vec<(String, String)> {
    match std::env::var("OPCUA_INTEROP_ENDPOINTS") {
        Ok(s) if !s.trim().is_empty() => s
            .split(',')
            .filter_map(|pair| {
                let (name, endpoint) = pair.split_once('=')?;
                let name = name.trim();
                let endpoint = endpoint.trim();
                if name.is_empty() || endpoint.is_empty() {
                    None
                } else {
                    Some((name.to_string(), endpoint.to_string()))
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Skip helper: returns the endpoint list, or prints a clear skip note and
/// returns an empty list so the test passes as a no-op.
fn endpoints_or_skip(test: &str) -> Vec<(String, String)> {
    let eps = interop_endpoints();
    if eps.is_empty() {
        eprintln!(
            "[skip] {test}: set OPCUA_INTEROP_ENDPOINTS=\"name=opc.tcp://host:port/,...\" \
             (e.g. via `just servers-up`) to run interoperability tests"
        );
    }
    eps
}

/// Run the CLI, retrying briefly so a server that is still finishing start-up
/// (common right after `docker compose up`) does not cause a spurious failure.
fn run_until_ok(args: &[&str]) -> String {
    wait_for(Duration::from_secs(30), || {
        let out = common::run_cli(args);
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

#[test]
#[ignore = "requires external OPC UA servers; run with --ignored and OPCUA_INTEROP_ENDPOINTS"]
fn interop_connect_reports_endpoints_and_namespaces() {
    for (name, endpoint) in endpoints_or_skip("interop_connect_reports_endpoints_and_namespaces") {
        let stdout = run_until_ok(&["connect", &endpoint, "--trust-server"]);
        assert!(
            stdout.contains("Endpoints"),
            "[{name}] connect did not report endpoints:\n{stdout}"
        );
        assert!(
            stdout.contains("Namespaces") || stdout.to_lowercase().contains("namespace"),
            "[{name}] connect did not report the namespace table:\n{stdout}"
        );
    }
}

#[test]
#[ignore = "requires external OPC UA servers; run with --ignored and OPCUA_INTEROP_ENDPOINTS"]
fn interop_browse_returns_nodes() {
    for (name, endpoint) in endpoints_or_skip("interop_browse_returns_nodes") {
        // Bounded depth keeps the test fast even on very large servers.
        let stdout = run_until_ok(&[
            "browse",
            &endpoint,
            "--trust-server",
            "--depth",
            "2",
            "--page-size",
            "50",
            "--output",
            "json",
        ]);
        let recs = common::json_records(stdout.as_bytes());
        assert!(
            !recs.is_empty(),
            "[{name}] browse returned no nodes from a standard address space"
        );
        // Every standard server exposes the Server object under Objects.
        let has_server = recs.iter().any(|r| {
            r.get("browse_path")
                .and_then(|v| v.as_str())
                .map(|p| p.contains("Server"))
                .unwrap_or(false)
        });
        assert!(
            has_server,
            "[{name}] standard `Server` node not found via Browse"
        );
    }
}

#[test]
#[ignore = "requires external OPC UA servers; run with --ignored and OPCUA_INTEROP_ENDPOINTS"]
fn interop_read_variable_values() {
    for (name, endpoint) in endpoints_or_skip("interop_read_variable_values") {
        // The ServerStatus CurrentTime variable (i=2258) exists on every
        // compliant server, so a value read should succeed everywhere.
        let stdout = run_until_ok(&[
            "browse",
            &endpoint,
            "--trust-server",
            "--depth",
            "unlimited",
            "--include",
            "**/ServerStatus/CurrentTime",
            "--preview-values",
            "--output",
            "json",
        ]);
        let recs = common::json_records(stdout.as_bytes());
        assert!(
            recs.iter()
                .any(|r| { r.get("browse_name").and_then(|v| v.as_str()) == Some("CurrentTime") }),
            "[{name}] could not locate ServerStatus/CurrentTime: {recs:?}"
        );
    }
}

#[test]
#[ignore = "requires external OPC UA servers; run with --ignored and OPCUA_INTEROP_ENDPOINTS"]
fn interop_subscription_smoke() {
    for (name, endpoint) in endpoints_or_skip("interop_subscription_smoke") {
        // Subscribe to the always-changing CurrentTime variable; the initial
        // value (and likely an update) should arrive within the window.
        let out = common::run_cli(&[
            "sample",
            &endpoint,
            "--trust-server",
            "--depth",
            "unlimited",
            "--include",
            "**/ServerStatus/CurrentTime",
            "--duration",
            "3s",
        ]);
        assert!(
            out.status.success(),
            "[{name}] sample failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("CurrentTime"),
            "[{name}] no subscription update received for CurrentTime:\n{stdout}"
        );
    }
}
