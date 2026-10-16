//! End-to-end tests for the tedge-dot interop: connecting via a tedge-dot
//! connector config and writing a point library from a scan.

mod common;

use std::path::Path;

use common::{run_cli, run_cli_ok, TEST_NS, TEST_PASSWORD, TEST_USER};

/// Browse the test server's Factory subtree as a point library into `file`.
fn export(endpoint: &str, file: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "browse",
        endpoint,
        "--depth",
        "unlimited",
        "--include",
        "Factory/**",
        "--output",
        "tedge-dot",
        "--file",
        file.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    run_cli(&args)
}

fn read_toml(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .unwrap()
        .parse()
        .expect("library is valid TOML")
}

fn points(doc: &toml::Table) -> Vec<toml::Table> {
    doc["point"]
        .as_array()
        .expect("[[point]]")
        .iter()
        .map(|p| p.as_table().unwrap().clone())
        .collect()
}

fn point<'a>(points: &'a [toml::Table], id: &str) -> &'a toml::Table {
    points
        .iter()
        .find(|p| p["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("no point '{id}' in {points:#?}"))
}

fn assert_ok(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn browse_writes_a_point_library() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("acme.toml");
    let out = export(&endpoint, &file, &["--library-type", "acme-motor"]);
    assert_ok(&out);

    let doc = read_toml(&file);
    let library = doc["library"].as_table().unwrap();
    assert_eq!(library["protocol"].as_str(), Some("opcua"));
    assert_eq!(library["type"].as_str(), Some("acme-motor"));
    let namespaces: Vec<&str> = library["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(namespaces[0], "http://opcfoundation.org/UA/");
    let ns = namespaces
        .iter()
        .position(|u| *u == TEST_NS)
        .expect("test namespace in the table");

    let pts = points(&doc);
    assert_eq!(pts.len(), common::MOTOR_VARIABLES.len(), "{pts:#?}");
    let temperature = point(&pts, "temperature");
    assert_eq!(temperature["datatype"].as_str(), Some("float64"));
    assert_eq!(temperature["name"].as_str(), Some("Temperature"));
    assert_eq!(
        temperature["address"]["node_id"].as_str(),
        Some(format!("ns={ns};s=Temperature").as_str())
    );
    assert_eq!(point(&pts, "speed")["datatype"].as_str(), Some("float32"));
    assert_eq!(point(&pts, "running")["datatype"].as_str(), Some("bool"));
    assert_eq!(point(&pts, "label")["datatype"].as_str(), Some("string"));
    assert_eq!(point(&pts, "counter")["datatype"].as_str(), Some("int32"));
    // Folders are not points and are not listed as skipped.
    assert!(!std::fs::read_to_string(&file).unwrap().contains("Skipped"));
}

#[test]
fn library_goes_to_stdout_without_file() {
    let endpoint = common::start();
    let stdout = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--include",
        "Factory/**",
        "--output",
        "tedge-dot",
    ]);
    let doc: toml::Table = stdout.parse().expect("stdout is a TOML library");
    assert_eq!(doc["library"]["protocol"].as_str(), Some("opcua"));
    assert!(!points(&doc).is_empty());
}

#[test]
fn export_merges_into_an_existing_library() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("acme.toml");

    // First a library with only Temperature, then a hand edit.
    assert_ok(&export(
        &endpoint,
        &file,
        &["--include", "**/Temperature", "--type", "Variable"],
    ));
    let first = std::fs::read_to_string(&file).unwrap();
    let edited = first.replace(
        "id = \"temperature\"",
        "id = \"boiler_temp\"\ntransform = { multiplier = 0.1 } # hand edit",
    );
    std::fs::write(&file, &edited).unwrap();

    // A second, wider export keeps the edit and adds the rest.
    let out = export(&endpoint, &file, &[]);
    assert_ok(&out);
    let merged = std::fs::read_to_string(&file).unwrap();
    assert!(
        merged.starts_with(&edited),
        "edited part changed:\n{merged}"
    );
    let pts = points(&read_toml(&file));
    assert_eq!(pts.len(), common::MOTOR_VARIABLES.len());
    assert!(pts.iter().all(|p| p["id"].as_str() != Some("temperature")));
}

#[test]
fn empty_result_does_not_create_the_file() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.toml");
    let out = export(&endpoint, &file, &["--type", "Object"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no node could be written"), "{stderr}");
    assert!(!file.exists());
}

#[test]
fn existing_non_library_file_is_left_alone() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("export.json");
    std::fs::write(&file, "[]\n").unwrap();
    let out = export(&endpoint, &file, &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not an OPC UA point library"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[]\n");
}

/// A tedge-dot config for the test server with a username and a password
/// file next to it, plus a second device so `--device` is needed. The device
/// uses an encrypted channel; `security` replaces those protocol_address keys.
fn write_config(dir: &Path, endpoint: &str, password: &str) -> std::path::PathBuf {
    write_config_with(
        dir,
        endpoint,
        password,
        r#"security_policy = "Basic256Sha256", trust_any_server_certificate = true"#,
    )
}

fn write_config_with(
    dir: &Path,
    endpoint: &str,
    password: &str,
    security: &str,
) -> std::path::PathBuf {
    std::fs::write(dir.join("plc.pw"), format!("{password}\n")).unwrap();
    let config = dir.join("opcua.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[connector]
protocol = "opcua"

[connection]
request_timeout_s = 5

[[device]]
name = "plc-7"
type = "acme-motor"
protocol_address = {{ endpoint = "{endpoint}", user = "{TEST_USER}", password_file = "plc.pw", {security} }}

[[device]]
name = "other"
protocol_address = {{ endpoint = "opc.tcp://127.0.0.1:1/" }}
"#
        ),
    )
    .unwrap();
    config
}

#[test]
fn connect_via_tedge_dot_config() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &endpoint, TEST_PASSWORD);
    // Run from elsewhere: the password file resolves against the config's directory.
    let stdout = run_cli_ok(&[
        "connect",
        "--tedge-dot-config",
        config.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(stdout.contains(TEST_NS), "{stdout}");
}

#[test]
fn tedge_dot_config_password_is_used() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &endpoint, "wrong-password");
    let out = run_cli(&[
        "connect",
        "--tedge-dot-config",
        config.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(!out.status.success(), "a wrong password must not connect");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("wrong-password"));
}

#[test]
fn several_devices_need_device() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &endpoint, TEST_PASSWORD);
    let out = run_cli(&["connect", "--tedge-dot-config", config.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--device") && stderr.contains("plc-7, other"),
        "{stderr}"
    );
}

#[test]
fn library_type_defaults_to_the_device_type() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &endpoint, TEST_PASSWORD);
    let file = dir.path().join("lib.toml");
    let out = run_cli(&[
        "browse",
        "--tedge-dot-config",
        config.to_str().unwrap(),
        "--device",
        "plc-7",
        "--depth",
        "unlimited",
        "--include",
        "Factory/**",
        "--output",
        "tedge-dot",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert_ok(&out);
    assert_eq!(
        read_toml(&file)["library"]["type"].as_str(),
        Some("acme-motor")
    );
}

#[test]
fn plaintext_password_is_refused_like_tedge_dot() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with(
        dir.path(),
        &endpoint,
        TEST_PASSWORD,
        r#"security_policy = "None""#,
    );
    let out = run_cli(&[
        "connect",
        "--tedge-dot-config",
        config.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("plaintext password refused"), "{stderr}");
    assert!(!stderr.contains(TEST_PASSWORD));
}

#[test]
fn plaintext_password_with_opt_in_connects() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with(
        dir.path(),
        &endpoint,
        TEST_PASSWORD,
        r#"security_policy = "None", allow_plaintext_password = true"#,
    );
    let stdout = run_cli_ok(&[
        "connect",
        "--tedge-dot-config",
        config.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(stdout.contains(TEST_NS), "{stdout}");
}

#[test]
fn deprecated_policy_via_config() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let refused = write_config_with(
        dir.path(),
        &endpoint,
        TEST_PASSWORD,
        r#"security_policy = "Basic128Rsa15", trust_any_server_certificate = true"#,
    );
    let out = run_cli(&[
        "connect",
        "--tedge-dot-config",
        refused.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("allow_deprecated_security"));

    let allowed = write_config_with(
        dir.path(),
        &endpoint,
        TEST_PASSWORD,
        r#"security_policy = "Basic128Rsa15", security_mode = "sign", allow_deprecated_security = true, trust_any_server_certificate = true"#,
    );
    let stdout = run_cli_ok(&[
        "connect",
        "--tedge-dot-config",
        allowed.to_str().unwrap(),
        "--device",
        "plc-7",
    ]);
    assert!(stdout.contains(TEST_NS), "{stdout}");
}

#[test]
fn library_covers_structures_enums_derived_types_and_methods() {
    let endpoint = common::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plant.toml");
    let out = run_cli(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--include",
        "Plant/**",
        "--type",
        "Variable,Method",
        "--output",
        "tedge-dot",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert_ok(&out);
    let text = std::fs::read_to_string(&file).unwrap();
    let doc = read_toml(&file);
    let pts = points(&doc);

    // Structure: one point per mappable leaf field, all on the Status node.
    let speed = point(&pts, "speed");
    assert_eq!(speed["address"]["field"].as_str(), Some("Speed"));
    assert_eq!(speed["datatype"].as_str(), Some("float64"));
    assert!(speed["address"]["node_id"]
        .as_str()
        .unwrap()
        .ends_with(";s=Pump1.Status"));
    let current = point(&pts, "motor_current");
    assert_eq!(current["address"]["field"].as_str(), Some("Motor.Current"));
    assert_eq!(current["datatype"].as_str(), Some("float32"));
    assert!(current.get("access").is_none());
    // The State field and the State variable both want `state`: extended ids.
    let state_field = pts
        .iter()
        .find(|p| p["address"].get("field").and_then(|f| f.as_str()) == Some("State"))
        .expect("State field point");
    assert_eq!(state_field["datatype"].as_str(), Some("int32"));
    // History (array) and Blob (XmlElement) are listed as skipped.
    assert!(
        text.contains("Pump1/Status.History  (array field)"),
        "{text}"
    );
    assert!(text.contains("Pump1/Status.Blob"), "{text}");

    // Enumeration variable: int32 with labels, writable.
    let state = pts
        .iter()
        .find(|p| {
            p["address"]
                .get("node_id")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.ends_with(";s=Pump1.State"))
                && p["address"].get("field").is_none()
        })
        .expect("State variable point");
    assert_eq!(state["datatype"].as_str(), Some("int32"));
    assert_eq!(state["access"].as_str(), Some("read_write"));
    let cases = state["map"]["cases"].as_array().unwrap();
    let labels: Vec<&str> = cases.iter().map(|c| c["to"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Stopped", "Running", "Fault"]);
    assert_eq!(state["map"]["as"].as_str(), Some("string"));

    // Derived type: Percent → float64.
    assert_eq!(point(&pts, "level")["datatype"].as_str(), Some("float64"));

    // Methods.
    let multiply = point(&pts, "multiply");
    assert_eq!(multiply["access"].as_str(), Some("call_method"));
    assert!(multiply["address"]["object_id"]
        .as_str()
        .unwrap()
        .ends_with(";s=Machine"));
    let inputs: Vec<&str> = multiply["address"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(inputs, ["int32", "int32"]);
    assert!(point(&pts, "reset")["address"].get("inputs").is_none());
    assert!(pts.iter().all(|p| p["id"].as_str() != Some("locked")));
    assert!(text.contains("Machine/Locked  (not executable)"), "{text}");
    assert!(
        text.contains("Machine/Configure  (argument 'cfg'"),
        "{text}"
    );
    assert!(!text.contains("InputArguments"), "{text}");
}

#[test]
fn method_records_carry_their_object() {
    let endpoint = common::start();
    let stdout = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--include",
        "Plant/**",
        "--type",
        "Method",
        "--output",
        "json",
    ]);
    let records = common::json_records(stdout.as_bytes());
    let multiply = records
        .iter()
        .find(|r| r["browse_name"] == "Multiply")
        .expect("Multiply record");
    assert!(multiply["parent_node_id"]
        .as_str()
        .unwrap()
        .ends_with(";s=Machine"));
}
