//! Security, authentication and certificate-trust integration tests.
//!
//! The in-process server (see [`common`]) exposes three endpoints — `None`,
//! `Basic256Sha256`/`Sign` and `Basic256Sha256`/`SignAndEncrypt` — and accepts
//! both anonymous and username/password identities. These tests exercise the
//! SecureChannel handshake and Session activation across security modes and
//! identity tokens, plus the server-certificate trust decision.

mod common;

use common::{run_cli, run_cli_ok, TEST_NS, TEST_PASSWORD, TEST_USER};

#[test]
fn sign_handshake_succeeds_when_server_trusted() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "Sign",
        "--trust-server",
    ]);
    assert!(
        out.contains(TEST_NS),
        "missing namespace in metadata:\n{out}"
    );
}

#[test]
fn sign_and_encrypt_handshake_succeeds_when_server_trusted() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
    ]);
    assert!(out.contains("Endpoints"), "missing endpoints:\n{out}");
}

#[test]
fn browse_works_over_encrypted_channel() {
    let endpoint = common::start();
    // End-to-end discovery over a SignAndEncrypt channel: prove that real
    // service calls (Browse + Read) succeed through the secure transport, not
    // just the handshake.
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--depth",
        "unlimited",
        "--output",
        "json",
    ]);
    let recs = common::json_records(out.as_bytes());
    assert!(
        recs.iter()
            .any(|r| r.get("browse_name").and_then(|v| v.as_str()) == Some("Temperature")),
        "Temperature not discovered over encrypted channel: {recs:?}"
    );
}

#[test]
fn secure_connection_without_trust_is_rejected() {
    let endpoint = common::start();
    // Without `--trust-server`, the (self-signed) server certificate is not in
    // the client trust list, so a secure handshake must fail rather than
    // silently connecting insecurely.
    let out = run_cli(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--timeout",
        "5s",
    ]);
    assert!(
        !out.status.success(),
        "secure connect unexpectedly succeeded without trusting the server cert"
    );
}

#[test]
fn username_password_authentication_succeeds() {
    let endpoint = common::start();
    // Username/password tokens are only safe over an encrypted channel, which
    // is how the tool is expected to be used.
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--user",
        TEST_USER,
        "--password",
        TEST_PASSWORD,
    ]);
    assert!(
        out.contains(TEST_NS),
        "missing namespace in metadata:\n{out}"
    );
}

#[test]
fn wrong_password_is_rejected() {
    let endpoint = common::start();
    let out = run_cli(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--user",
        TEST_USER,
        "--password",
        "definitely-the-wrong-password",
        "--timeout",
        "5s",
    ]);
    assert!(
        !out.status.success(),
        "connect succeeded with an incorrect password"
    );
}
