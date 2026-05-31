# Testing guide

`opcua-scan` treats testing as a first-class quality concern. Tests are
organized into four tiers, from fast and hermetic to heavier and
interoperability-focused.

| Tier | Location | What it covers | Needs network/Docker |
| ---- | -------- | -------------- | -------------------- |
| Unit | `#[cfg(test)]` modules in `src/` | Pure logic: filters, value/variant conversion, namespace table, data-type/status handling, config & profile parsing, cache | No |
| Integration (in-process) | `tests/cli.rs`, `tests/services.rs`, `tests/security.rs` | End-to-end CLI against an in-process OPC UA server: Discovery, Browse (+ continuation), Read, Subscriptions, security handshakes, auth, exit codes | No |
| Interoperability | `tests/interop.rs` + `tests/interop/` | The CLI against real third-party servers (open62541, node-opcua, Eclipse Milo, OPC Foundation reference server, Microsoft OPC-PLC, public demo) | Yes |
| Property-based | `proptest` blocks (e.g. `src/filters/pattern.rs`) | Invariants such as "a matching path is never pruned during descent" | No |

## Running tests locally

```sh
# Everything hermetic (unit + in-process integration). No network required.
cargo test
# or
just test

# Just the unit tests (this is a binary crate, so use --bin, not --lib):
cargo test --bin opcua-scan
just test-unit

# Just the in-process integration tests:
just test-integration
```

The in-process server is started **once per test binary** and shared across the
tests in that binary (the address space is read-only). This keeps resource use
low and avoids the timing flakiness that comes from spawning many concurrent
OPC UA servers with cryptographic handshakes.

### Pointing integration tests at an external server

Set `OPCUA_TEST_ENDPOINT` to skip the in-process server and run the CLI
integration tests against your own server instead (it must expose a comparable
address space for the assertions to hold):

```sh
OPCUA_TEST_ENDPOINT=opc.tcp://127.0.0.1:4840/ cargo test --test services
```

## Running the interoperability suite

Interop tests are `#[ignore]`d and additionally no-op unless
`OPCUA_INTEROP_ENDPOINTS` is set, so they never run during `cargo test`.

1. Start the reference servers (Docker required):

   ```sh
   just servers-up
   # equivalently:
   docker compose -f tests/interop/docker-compose.yml up -d --build
   ```

2. Run the suite:

   ```sh
   just test-interop
   # equivalently:
   OPCUA_INTEROP_ENDPOINTS="opcplc=opc.tcp://127.0.0.1:50000/,node-opcua=opc.tcp://127.0.0.1:4841/" \
       cargo test --test interop -- --ignored --nocapture
   ```

3. Tear the servers down:

   ```sh
   just servers-down
   ```

`OPCUA_INTEROP_ENDPOINTS` is a comma-separated list of `name=endpoint` pairs.
Add as many servers as you like — the tests iterate over all of them and label
failures with the server name.

### Spinning up OPC UA test servers

`tests/interop/docker-compose.yml` ships two ready-to-run implementations:

- **opcplc** — Microsoft OPC-PLC (a .NET server) on `opc.tcp://127.0.0.1:50000/`.
- **node-opcua** — a Node.js server built from `tests/interop/node-opcua/`
  on `opc.tcp://127.0.0.1:4841/`.

The compose file also contains commented, opt-in service stubs for
**open62541**, **Eclipse Milo** and the **OPC Foundation reference server** —
supply a built image or build context and uncomment them to widen the matrix.

You can also test against the public demo server without Docker:

```sh
OPCUA_INTEROP_ENDPOINTS="demo=opc.tcp://opcua.demo-this.com:51210/UA/SampleServer" \
    cargo test --test interop -- --ignored --nocapture
```

## Debugging failures

- Add `--nocapture` to see test stdout/stderr: `cargo test -- --nocapture`.
- Raise log verbosity for the CLI under test via `RUST_LOG`
  (the harness sets `RUST_LOG=error` by default):

  ```sh
  RUST_LOG=debug cargo test --test services failing_test_name -- --nocapture
  ```

- Integration-test helpers (`run_cli_ok`) print both stdout and stderr on
  failure, including the process exit code.
- Prefer the `common::wait_for(timeout, || …)` helper over fixed `sleep`s when
  waiting for asynchronous conditions — it polls with a short interval and
  fails with a clear timeout message.
- For interop failures, the nightly workflow uploads `docker compose logs` as a
  build artifact (`interop-server-logs`); locally use `just servers-logs`.

## Adding new test cases

### A new unit test

Add it to the `#[cfg(test)] mod tests` block of the relevant module. Keep it
pure and deterministic. For invariants over many inputs, prefer a `proptest!`
block (see `src/filters/pattern.rs`).

### A new in-process integration case

Add a `#[test]` to `tests/services.rs` (services/CLI contracts) or
`tests/security.rs` (security/auth/cert behavior). Reuse the `common` helpers:

- `common::start()` — shared in-process server endpoint.
- `common::run_cli(args)` / `common::run_cli_ok(args)` — invoke the binary.
- `common::json_records(stdout)` — parse a JSON-array result.
- `common::wait_for(timeout, closure)` — deterministic async wait.

If you need a new node shape (data type, access level, historizing, etc.), add
it in `populate()` in `tests/common/mod.rs`. The fixture's leaf variable names
are listed in `common::MOTOR_VARIABLES`; update that constant when you add one.

### A new interoperability case

Add a `#[test]` to `tests/interop.rs`. Mark it `#[ignore]` and start it with
`endpoints_or_skip("test_name")` so it stays hermetic by default and iterates
over every configured server. Keep assertions **capability-tolerant**: not every
server supports every optional service (history, every security policy), so skip
and record rather than hard-failing when a server legitimately lacks a feature.
