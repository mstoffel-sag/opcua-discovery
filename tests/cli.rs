//! End-to-end tests driving the compiled `opcua-scan` binary against an
//! in-process OPC UA server.

mod common;

use serde_json::Value;

/// Parse the stdout of a CLI run as a JSON array of records.
fn records(stdout: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(stdout);
    serde_json::from_str::<Vec<Value>>(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got error {e}; output was:\n{text}"))
}

#[test]
fn connect_prints_server_metadata() {
    let endpoint = common::start();
    let out = common::run_cli(&["connect", &endpoint]);
    assert!(
        out.status.success(),
        "connect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Endpoints"), "missing endpoints:\n{stdout}");
    assert!(
        stdout.contains(common::TEST_NS),
        "missing test namespace:\n{stdout}"
    );
}

#[test]
fn browse_discovers_known_variables() {
    let endpoint = common::start();
    let out = common::run_cli(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--output",
        "json",
    ]);
    assert!(
        out.status.success(),
        "browse failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recs = records(&out.stdout);
    let names: Vec<&str> = recs
        .iter()
        .filter_map(|r| r.get("browse_name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"Temperature"), "names: {names:?}");
    assert!(names.contains(&"Speed"), "names: {names:?}");
    assert!(names.contains(&"Motor"), "names: {names:?}");
}

#[test]
fn type_filter_returns_only_variables() {
    let endpoint = common::start();
    let out = common::run_cli(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--type",
        "Variable",
        "--output",
        "json",
    ]);
    assert!(
        out.status.success(),
        "type filter failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recs = records(&out.stdout);
    assert!(!recs.is_empty(), "expected some variables");
    for r in &recs {
        assert_eq!(
            r.get("node_class").and_then(Value::as_str),
            Some("Variable"),
            "non-variable in results: {r}"
        );
    }
}

#[test]
fn subscribable_filter_finds_variables() {
    let endpoint = common::start();
    let out = common::run_cli(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--subscribable",
        "--output",
        "json",
    ]);
    assert!(out.status.success());

    let recs = records(&out.stdout);
    assert!(
        recs.iter()
            .all(|r| r.get("subscribable").and_then(Value::as_bool) == Some(true)),
        "all results should be subscribable: {recs:?}"
    );
    assert!(recs.len() >= 2, "expected at least Temperature and Speed");
}

#[test]
fn search_finds_temperature() {
    let endpoint = common::start();
    let out = common::run_cli(&[
        "search",
        &endpoint,
        "Temp",
        "--depth",
        "unlimited",
        "--output",
        "json",
    ]);
    assert!(
        out.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recs = records(&out.stdout);
    assert!(
        recs.iter()
            .any(|r| { r.get("browse_name").and_then(Value::as_str) == Some("Temperature") }),
        "Temperature not found: {recs:?}"
    );
}

#[test]
fn export_csv_to_file() {
    let endpoint = common::start();
    let dir = std::env::temp_dir().join(format!("opcua-scan-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nodes.csv");

    let out = common::run_cli(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--output",
        "csv",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "csv export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let csv = std::fs::read_to_string(&file).expect("csv file written");
    assert!(csv.contains("browse_path"), "missing header:\n{csv}");
    assert!(csv.contains("Temperature"), "missing data:\n{csv}");
}

#[test]
fn invalid_endpoint_exits_with_usage_code() {
    let out = common::run_cli(&["connect", "http://not-opcua"]);
    assert!(!out.status.success());
    // Exit code 2 == invalid usage / arguments.
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn acquire_poll_emits_tedge_measurements() {
    let endpoint = common::start();
    let dir = std::env::temp_dir().join(format!("opcua-scan-acquire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pki = dir.join("pki");
    let config_path = dir.join("acquire.toml");
    let config = format!(
        r#"
[connection]
endpoint = "{endpoint}"
trust_server = true
pki_dir = "{pki}"

[selection]
root = "i=85"
depth = "unlimited"
include = ["**/Temperature"]
node_class = ["Variable"]

[output]
format = "tedge"

[[task]]
mode = "poll"
interval = "300ms"
"#,
        pki = pki.to_str().unwrap()
    );
    std::fs::write(&config_path, config).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_opcua-scan"))
        .args([
            "acquire",
            "--config",
            config_path.to_str().unwrap(),
            "--duration",
            "1500ms",
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("run acquire");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("Temperature"))
        .unwrap_or_else(|| {
            panic!(
                "no Temperature measurement emitted:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let value: Value = serde_json::from_str(line).expect("valid tedge JSON line");
    assert!(value.get("time").is_some(), "missing time field: {line}");
    // The measurement key is now the full browse path, which ends with the leaf name.
    let has_temperature_key = value
        .as_object()
        .map(|obj| obj.keys().any(|k| k.ends_with("Temperature")))
        .unwrap_or(false);
    assert!(
        has_temperature_key,
        "missing measurement keyed by browse path: {line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn acquire_poll_emits_fixed_schema_ndjson() {
    let endpoint = common::start();
    let dir = std::env::temp_dir().join(format!("opcua-scan-ndjson-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pki = dir.join("pki");
    let config_path = dir.join("acquire.toml");
    let config = format!(
        r#"
[connection]
endpoint = "{endpoint}"
trust_server = true
pki_dir = "{pki}"

[selection]
root = "i=85"
depth = "unlimited"
include = ["**/Temperature"]
node_class = ["Variable"]

[output]
format = "ndjson"

[[task]]
mode = "poll"
interval = "300ms"
"#,
        pki = pki.to_str().unwrap()
    );
    std::fs::write(&config_path, config).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_opcua-scan"))
        .args([
            "acquire",
            "--config",
            config_path.to_str().unwrap(),
            "--duration",
            "1500ms",
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("run acquire");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("Temperature"))
        .unwrap_or_else(|| {
            panic!(
                "no Temperature record emitted:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let value: Value = serde_json::from_str(line).expect("valid ndjson line");

    // Fixed schema: every top-level key is always present.
    for key in [
        "time", "key", "value", "dataType", "nodeId", "units", "status", "meta",
    ] {
        assert!(
            value.get(key).is_some(),
            "missing fixed key '{key}': {line}"
        );
    }
    assert_eq!(
        value
            .get("key")
            .and_then(Value::as_str)
            .map(|k| k.ends_with("Temperature")),
        Some(true),
        "expected key to be the full browse path ending in Temperature: {line}"
    );
    assert_eq!(
        value.get("dataType").and_then(Value::as_str),
        Some("float64"),
        "expected normalized dataType: {line}"
    );
    assert!(
        value.get("meta").map(Value::is_object).unwrap_or(false),
        "meta not an object: {line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn acquire_device_remaps_namespaces_and_caches_paths_by_address() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("device.toml");
    let cache = dir.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let cache_key: String = endpoint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // An earlier session's table must never drive current-session acquisition.
    std::fs::write(
        cache.join(format!("{cache_key}.namespaces.json")),
        serde_json::to_string(&serde_json::json!({
            "endpoint": endpoint,
            "uris": ["http://opcfoundation.org/UA/", common::TEST_NS]
        }))
        .unwrap(),
    )
    .unwrap();
    let config = format!(
        r#"# preserve this comment
[[device]]
name = "Local fixture"
trust_server = true
pki_dir = "{pki}"
cache = "{cache}"
protocol_address = {{ endpoint = "{endpoint}", namespaces = [
    "http://opcfoundation.org/UA/", "urn:config:one", "urn:config:two",
    "urn:config:three", "{namespace}"
] }}

[[device.point]]
id = "stable-temperature-key"
name = "Temperature alias"
address = {{ node_id = "ns=4;s=Temperature" }}
mode = "poll"

[output]
format = "ndjson"

[[task]]
mode = "poll"
interval = "100ms"
"#,
        pki = dir.path().join("pki").display(),
        cache = cache.display(),
        namespace = common::TEST_NS,
    );
    std::fs::write(&config_path, config).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_opcua-scan"))
        .args([
            "acquire",
            "--config",
            config_path.to_str().unwrap(),
            "--duration",
            "300ms",
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("run acquire");
    // Duration expiry uses the existing cancellation exit code.
    assert_eq!(
        out.status.code(),
        Some(4),
        "acquire failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let sample: Value = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["value"] == 25.0)
        .unwrap_or_else(|| panic!("no remapped Temperature sample: {stdout}"));
    assert_eq!(sample["nodeId"], "ns=2;s=Temperature");
    assert!(sample["key"].as_str().unwrap().ends_with("Temperature"));
    assert_eq!(sample["meta"]["source"]["name"], "Local fixture");
    let saved = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved.contains("# preserve this comment"));
    let saved: toml::Value = toml::from_str(&saved).unwrap();
    let point = &saved["device"][0]["point"][0];
    assert_eq!(point["id"].as_str(), Some("stable-temperature-key"));
    assert_eq!(
        point["address"]["node_id"].as_str(),
        Some("ns=4;s=Temperature")
    );
    assert!(point["path"].as_str().unwrap().ends_with("Temperature"));
}

#[test]
fn acquire_device_missing_namespace_fails_without_literal_index_fallback() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("missing.toml");
    let config = format!(
        r#"[[device]]
trust_server = true
pki_dir = "{pki}"
protocol_address = {{ endpoint = "{endpoint}", namespaces = [
    "http://opcfoundation.org/UA/", "urn:config:one", "urn:absent-on-server"
] }}
[[device.point]]
id = "temperature"
address = {{ node_id = "ns=2;s=Temperature" }}
[output]
format = "ndjson"
[[task]]
mode = "poll"
interval = "100ms"
"#,
        pki = dir.path().join("pki").display(),
    );
    std::fs::write(&config_path, config).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_opcua-scan"))
        .args([
            "acquire",
            "--config",
            config_path.to_str().unwrap(),
            "--duration",
            "100ms",
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("run acquire");
    assert!(!out.status.success(), "missing URI must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("urn:absent-on-server"), "{stderr}");
    assert!(stderr.contains("ns=2;s=Temperature"), "{stderr}");
    assert!(
        out.stdout.is_empty(),
        "must not sample the literal ns=2 node"
    );
}
