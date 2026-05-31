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
        .unwrap_or_else(|| panic!("no Temperature measurement emitted:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr)));
    let value: Value = serde_json::from_str(line).expect("valid tedge JSON line");
    assert!(value.get("time").is_some(), "missing time field: {line}");
    assert!(value.get("Temperature").is_some(), "missing measurement: {line}");

    let _ = std::fs::remove_dir_all(&dir);
}
