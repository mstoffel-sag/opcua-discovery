//! Shared test fixtures: an in-process OPC UA server with a small, known
//! address space, plus helpers to invoke the compiled `opcua-scan` binary.
//!
//! The server exposes:
//!
//! ```text
//! Objects/Factory/Line1/Motor/Temperature  (Double, 25.0)
//! Objects/Factory/Line1/Motor/Speed         (Float, 1500.0)
//! ```
//!
//! Tests can also point at an external server by setting the
//! `OPCUA_TEST_ENDPOINT` environment variable, in which case the in-process
//! server is not started (the external server is expected to expose a similar
//! address space for the assertions to hold).

#![allow(dead_code)]

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use opcua::server::address_space::VariableBuilder;
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{simple_node_manager, SimpleNodeManager};
use opcua::server::{ServerBuilder, ServerEndpoint, ANONYMOUS_USER_TOKEN_ID};
use opcua::types::{DataTypeId, NodeId};

/// Namespace URI used by the test address space.
pub const TEST_NS: &str = "urn:opcua-scan-test";

/// Start (or locate) a test server and return its endpoint URL.
///
/// If `OPCUA_TEST_ENDPOINT` is set, that endpoint is returned and no in-process
/// server is started.
pub fn start() -> String {
    if let Ok(ep) = std::env::var("OPCUA_TEST_ENDPOINT") {
        return ep;
    }

    let port = free_port();
    let endpoint = format!("opc.tcp://127.0.0.1:{port}/");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime");
        rt.block_on(async move {
            run_server(port).await;
        });
    });

    wait_until_listening(port);
    endpoint
}

/// Build, populate and run the in-process server until the runtime ends.
async fn run_server(port: u16) {
    let pki = std::env::temp_dir().join(format!("opcua-scan-test-pki-{port}"));
    let (server, handle) = ServerBuilder::new()
        .application_name("opcua-scan-test-server")
        .application_uri("urn:opcua-scan-test-server")
        .host("127.0.0.1")
        .port(port)
        .discovery_urls(vec![format!("opc.tcp://127.0.0.1:{port}/")])
        .add_endpoint(
            "none",
            ServerEndpoint::new_none("/", &[ANONYMOUS_USER_TOKEN_ID.to_string()]),
        )
        .with_node_manager(simple_node_manager(
            NamespaceMetadata {
                namespace_uri: TEST_NS.to_owned(),
                ..Default::default()
            },
            "test",
        ))
        .create_sample_keypair(true)
        .trust_client_certs(true)
        .pki_dir(pki)
        .build()
        .expect("build test server");

    populate(&handle);

    let _ = server.run().await;
}

/// Populate the address space with the known factory hierarchy.
fn populate(handle: &opcua::server::ServerHandle) {
    let nm = handle
        .node_managers()
        .get_of_type::<SimpleNodeManager>()
        .expect("simple node manager");
    let ns = handle
        .get_namespace_index(TEST_NS)
        .expect("test namespace registered");

    let address_space = nm.address_space();
    let mut a = address_space.write();

    let factory = NodeId::new(ns, "Factory");
    let _ = a.add_folder(&factory, "Factory", "Factory", &NodeId::objects_folder_id());
    let line = NodeId::new(ns, "Line1");
    let _ = a.add_folder(&line, "Line1", "Line1", &factory);
    let motor = NodeId::new(ns, "Motor");
    let _ = a.add_folder(&motor, "Motor", "Motor", &line);

    VariableBuilder::new(
        &NodeId::new(ns, "Temperature"),
        "Temperature",
        "Temperature",
    )
    .data_type(DataTypeId::Double)
    .value(25.0f64)
    .organized_by(&motor)
    .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Speed"), "Speed", "Speed")
        .data_type(DataTypeId::Float)
        .value(1500.0f32)
        .organized_by(&motor)
        .insert(&mut *a);
}

/// Grab an unused TCP port by binding to port 0 and reading the assigned port.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Block until the server is accepting TCP connections (or time out).
fn wait_until_listening(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            // Give the OPC UA stack a brief moment after the socket is up.
            std::thread::sleep(Duration::from_millis(200));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("test server did not start listening on port {port}");
}

/// Run the compiled `opcua-scan` binary with the given arguments.
///
/// A throwaway PKI directory is used so the client keypair does not clutter the
/// working directory.
pub fn run_cli(args: &[&str]) -> Output {
    let pki = std::env::temp_dir().join(format!("opcua-scan-client-pki-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_opcua-scan"))
        .args(args)
        .arg("--pki-dir")
        .arg(&pki)
        .env("RUST_LOG", "error")
        .output()
        .expect("run opcua-scan binary")
}
