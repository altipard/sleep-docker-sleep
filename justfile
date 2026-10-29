# https://just.systems — `just` lists all recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

export CARGO_TERM_COLOR := "always"

bin := "target/release/sds"
config := env("CONFIG", "sds.toml")
smoke_name := "sds-smoke"
smoke_port := "18081"
smoke_admin := "127.0.0.1:9901"

# Lint, test with coverage, audit dependencies and build (what CI runs on a merge request)
default: lint coverage audit build

# Debug build
build:
    cargo build

# Optimised release build
release:
    cargo build --release --locked

# Install the release binary to /usr/local/bin (needs sudo)
install: release
    sudo install -m 755 {{bin}} /usr/local/bin/sds
    sds --version

# Unit and CLI tests
test:
    cargo test --locked

# Line coverage with an LCOV report in target/coverage/lcov.info
coverage:
    cargo llvm-cov --version >/dev/null 2>&1 || cargo install cargo-llvm-cov --locked
    mkdir -p target/coverage
    cargo llvm-cov --locked --all-features --workspace --lcov --output-path target/coverage/lcov.info

# Format check and clippy with warnings as errors
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings

# Format the code
fmt:
    cargo fmt --all

# Dependency vulnerability audit (installs cargo-audit on demand)
audit:
    cargo audit --version >/dev/null 2>&1 || cargo install cargo-audit --locked
    cargo audit

# Check commit messages on this branch against Conventional Commits
commits:
    cz check --rev-range "$(git merge-base origin/main HEAD 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD"

# Run the daemon with the terminal dashboard (CONFIG=path to override)
run:
    cargo run -- run --config {{config}}

# Run the daemon with logs only
run-headless:
    cargo run -- run --headless --config {{config}}

# Attach the dashboard to a running daemon
top:
    cargo run --quiet -- top --config {{config}}

# Validate config, Docker access and discovered groups
check:
    cargo run --quiet -- check --config {{config}}

# Show groups of the running daemon (extra args are passed through)
status *args:
    cargo run --quiet -- status --config {{config}} "$@"

# Write a commented sds.toml into the project directory
init:
    test -f {{config}} || cargo run --quiet -- init --path {{config}}

# Build the handbook with mdBook (installs it on demand)
docs:
    mdbook --version >/dev/null 2>&1 || cargo install mdbook --locked
    mdbook build docs

# Serve the handbook with live reload
docs-serve:
    mdbook --version >/dev/null 2>&1 || cargo install mdbook --locked
    mdbook serve docs --open

# Build the sds container image
docker-build:
    docker build -t sleep-docker-sleep:dev .

# Lint the Dockerfile with hadolint
docker-lint:
    docker run --rm -i hadolint/hadolint < Dockerfile

# End-to-end check against real Docker (labelled nginx, authenticated wake, idle stop)
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    SDS=target/debug/sds
    CFG=target/smoke.toml
    cleanup() {
        kill "${PID:-}" 2>/dev/null || true
        docker rm -f {{smoke_name}} >/dev/null 2>&1 || true
    }
    trap cleanup EXIT
    docker rm -f {{smoke_name}} >/dev/null 2>&1 || true
    docker run -d --name {{smoke_name}} \
        -l sds.enable=true -l sds.group=smoke -l sds.idle_timeout=20s \
        -l sds.start_grace=1s -l sds.health=http://127.0.0.1:{{smoke_port}}/ \
        -p 127.0.0.1:{{smoke_port}}:80 nginx:alpine >/dev/null
    printf '[admin]\nbind = "{{smoke_admin}}"\ntoken = "smoke-secret"\n' > "$CFG"
    "$SDS" run --headless --config "$CFG" > target/smoke.log 2>&1 &
    PID=$!
    sleep 3
    "$SDS" status --config "$CFG"
    "$SDS" sleep smoke --config "$CFG"; sleep 3
    test "$(docker inspect -f '{{{{.State.Status}}' {{smoke_name}})" = exited
    test "$(curl -s -o /dev/null -w '%{http_code}' 'http://{{smoke_admin}}/_sds/check/smoke?token=smoke-secret')" = 401
    test "$(curl -s -o /dev/null -w '%{http_code}' http://{{smoke_admin}}/_sds/wait/smoke)" = 401
    ! "$SDS" config --config "$CFG" | grep -q smoke-secret
    test "$(curl -s -o /dev/null -w '%{http_code}' -H 'X-SDS-Token: smoke-secret' http://{{smoke_admin}}/_sds/check/smoke)" = 200
    test "$(docker inspect -f '{{{{.State.Status}}' {{smoke_name}})" = running
    test "$(curl -s -o /dev/null -w '%{http_code}' -H 'X-SDS-Token: smoke-secret' -H 'Accept: text/html' http://{{smoke_admin}}/_sds/wait/smoke)" = 200
    echo "waiting for idle stop (20s)…"; sleep 25
    test "$(docker inspect -f '{{{{.State.Status}}' {{smoke_name}})" = exited
    echo "smoke test passed"

# Remove build artefacts
clean:
    cargo clean
    rm -rf docs/book
