# Task runner for opcua-scan. Install `just` (https://github.com/casey/just),
# then run e.g. `just test` or `just servers-up`.

compose := "tests/interop/docker-compose.yml"
# Default endpoints used by `just test-interop`; override on the command line:
#   just interop_endpoints='opcplc=opc.tcp://127.0.0.1:50000/' test-interop
interop_endpoints := "opcplc=opc.tcp://127.0.0.1:50000/,node-opcua=opc.tcp://127.0.0.1:4841/"

# Show the available recipes.
default:
    @just --list

# Run the full hermetic test suite (unit + in-process integration).
test:
    cargo test --locked

# Run only the in-crate unit tests (pure logic).
test-unit:
    cargo test --bin opcua-scan --locked

# Run only the in-process integration tests (CLI, services, security).
test-integration:
    cargo test --test cli --test services --test security --locked

# Run the cross-implementation interoperability suite against running servers.
# Bring servers up first with `just servers-up`.
test-interop:
    OPCUA_INTEROP_ENDPOINTS="{{interop_endpoints}}" \
        cargo test --test interop --locked -- --ignored --nocapture

# Lint with clippy, treating warnings as errors (matches CI).
lint:
    cargo clippy --all-targets -- -D warnings

# Format the code.
fmt:
    cargo fmt --all

# Check formatting without modifying files (matches CI).
fmt-check:
    cargo fmt --all -- --check

# Generate an HTML coverage report (requires `cargo install cargo-llvm-cov`).
coverage:
    cargo llvm-cov --all-targets --locked --html
    @echo "Open target/llvm-cov/html/index.html"

# Bring up the containerized reference OPC UA servers.
servers-up:
    docker compose -f {{compose}} up -d --build

# Tail the reference server logs.
servers-logs:
    docker compose -f {{compose}} logs -f

# Tear the reference servers down (and remove volumes).
servers-down:
    docker compose -f {{compose}} down -v

# Everything CI runs on a pull request, in order.
ci: fmt-check lint test
