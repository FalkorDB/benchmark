# Justfile — dev-cycle automation for the FalkorDB benchmark.
#
# Run `just` (or `just --list`) to see every available recipe. Every check CI performs is a
# recipe here, so CI and local runs use the exact same command (see .github/workflows/ and
# .github/copilot-instructions.md).
#
# This repo has two parts, each with its own recipes:
#   * the Rust benchmark binary  -> `fmt`, `clippy`, `build`, `test`, `run`, …
#   * the Next.js dashboard in ui/ -> `ui-*`

set shell := ["bash", "-uc"]
set positional-arguments

# --- Configuration (override on the CLI, e.g. `just PORT=3005 ui-dev`) --------

# Port for the UI dev server (`ui-dev`). The Playwright smoke test always targets 3000
# (see ui/tests/config/urls.json), so `ui-smoke` pins its own server to 3000.
PORT := env_var_or_default("PORT", "3000")

# Default recipe: list everything.
default:
    @just --list

# === Rust: format ============================================================

# Format all Rust code in place.
fmt:
    cargo fmt --all

# Check Rust formatting without modifying files.
fmt-check:
    cargo fmt --all --check

# === Rust: lint ==============================================================

# `--package benchmark` selects this crate explicitly, exactly as the CI invocation does.
# Strict clippy over all targets/features, warnings denied (matches the `clippy` CI gate).
clippy:
    cargo clippy --package benchmark --all-targets --all-features -- -D warnings

# === Rust: build =============================================================

# Build every target with all features (matches the `build` CI gate).
build:
    cargo build --verbose --all-targets --all-features

# Build the optimized release binary.
build-release:
    cargo build --release

# Build the API docs.
doc:
    cargo doc --no-deps

# === Rust: test ==============================================================

# Run the unit + integration test suite (matches the `test` CI gate).
test:
    cargo test --verbose

# Run a single test by name filter, e.g. `just test-one query_builder`.
test-one *args:
    cargo test "$@"

# === Docs: validate Markdown =================================================
# Validate the prose Markdown docs (readme, copilot-instructions, …). `just doc` (above) builds
# the Rust API docs; these recipes check the docs' links and embedded examples. Rust fenced
# examples are compiled as doctests by `just test` (see src/doc_examples.rs), so they are not
# repeated here.

# Every Markdown doc check (matches the `Docs validation` workflow): links + shell examples.
doc-check: doc-links doc-shell

# `--offline` skips network requests, so only relative and same-file anchor links are verified
# (external URLs are excluded) — no network-flaky CI. Needs `lychee` (CI installs it via
# taiki-e/install-action, exactly like `just`).
# Offline broken-link + anchor check (lychee) over every tracked *.md except vendor/.
doc-links:
    #!/usr/bin/env bash
    set -euo pipefail
    git ls-files '*.md' | grep -v '^vendor/' \
        | xargs lychee --offline --include-fragments --no-progress

# Syntax-check (`bash -n`, nothing is executed) the bash/sh fenced examples in the Markdown docs.
doc-shell:
    ./scripts/check_doc_shell.sh

# === Coverage ================================================================

# Generate Codecov JSON coverage for the benchmark crate (matches the `coverage` CI job).
# Runs unit tests AND the `#[ignore]`d integration tests (`--include-ignored`), so the
# server-backed code paths are measured — this needs a reachable FalkorDB (see `coverage-local`,
# or the coverage CI job's FalkorDB service). FALKORDB_HOST/FALKORDB_PORT select it (default
# 127.0.0.1:6379).
coverage:
    cargo llvm-cov --package benchmark --all-features --codecov --output-path codecov.json -- --include-ignored

# Spin up a Docker FalkorDB, collect coverage, then tear it down (no manual server needed).
coverage-local:
    #!/usr/bin/env bash
    set -euo pipefail
    docker rm -f falkordb-cov >/dev/null 2>&1 || true
    docker run -d --name falkordb-cov -p 6379:6379 falkordb/falkordb:latest >/dev/null
    trap 'docker rm -f falkordb-cov >/dev/null 2>&1 || true' EXIT
    for i in $(seq 1 30); do
        if docker exec falkordb-cov redis-cli ping >/dev/null 2>&1; then break; fi
        sleep 1
    done
    just coverage

# Generate an HTML coverage report and open it in a browser (needs a reachable FalkorDB too).
coverage-html:
    cargo llvm-cov --package benchmark --all-features --html --open -- --include-ignored

# === Rust: run ===============================================================

# Run the benchmark binary, forwarding args, e.g. `just run -- --help` or `just run load ...`.
run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Drop an optional leading `--` so both `just run --help` and `just run -- --help` forward
    # the flags to the binary (not to cargo).
    if [ "${1:-}" = "--" ]; then shift; fi
    cargo run --bin benchmark -- "$@"

# === Synthetic per-operation benchmark =======================================

# Needs a reachable FalkorDB, e.g. `docker run -d -p 6379:6379 falkordb/falkordb:latest`. Examples:
# `just synthetic-bench --graph demo --op match_by_index,expand_1_hop --samples 500` (or --all-reads),
# or generate a reproducible dataset first:
# `just synthetic-bench --graph bench --generate --nodes 100000 --edges 1000000 --all-reads`.
# Read primitives need a `:User {id}` / `:Friend` dataset in --graph (use --generate or a config
# file, `synthetic-bench.toml`; see the README catalog + `synthetic-bench.example.toml`).
# Run the synthetic per-operation latency probe (forwards args to `synthetic run`).
synthetic-bench *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Drop an optional leading `--` so both `just synthetic-bench --samples 5` and
    # `just synthetic-bench -- --samples 5` forward the flags to the probe (matches `just run`).
    if [ "${1:-}" = "--" ]; then shift; fi
    cargo run --release --bin benchmark -- synthetic run "$@"

# List the available synthetic operations.
synthetic-ops:
    cargo run --quiet --bin benchmark -- synthetic list-ops

# FALKORDB_HOST/PORT select the server (default 127.0.0.1:6379); these `#[ignore]`d tests need one.
# Run the synthetic integration test against a live FalkorDB.
synthetic-it:
    cargo test --test synthetic_probe -- --ignored --nocapture

# === UI (Next.js dashboard in ui/) ===========================================

# Install UI dependencies from the lockfile.
ui-install:
    cd ui && npm ci

# Lint the UI.
ui-lint:
    cd ui && npm run lint

# Production build of the UI. Do NOT run while `ui-dev` is live — both write ui/.next.
ui-build:
    cd ui && npm run build

# Start the UI dev server on {{PORT}}.
ui-dev:
    cd ui && PORT="{{PORT}}" npm run dev

# Run `just ui-install` first. Set the NEXT_PUBLIC_HUBSPOT_* env vars (as CI does) if the
# smoke path needs them. Mirrors the `playwright.yml` workflow.
# Playwright CI smoke test: start the dev server, wait for it, run the smoke spec, tear it down.
ui-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ui
    # The smoke spec targets http://localhost:3000 (ui/tests/config/urls.json), so pin the
    # dev server to 3000 regardless of any outer PORT.
    port=3000
    if command -v google-chrome >/dev/null 2>&1; then
        export PLAYWRIGHT_CHROMIUM_CHANNEL=chrome
        google-chrome --version
    elif command -v google-chrome-stable >/dev/null 2>&1; then
        export PLAYWRIGHT_CHROMIUM_CHANNEL=chrome
        google-chrome-stable --version
    else
        echo "System Chrome not found; installing Playwright Chromium headless shell fallback."
        npx playwright install --only-shell chromium
    fi
    PORT="$port" npm run dev &
    dev_pid=$!
    trap 'kill "$dev_pid" 2>/dev/null || true; wait "$dev_pid" 2>/dev/null || true' EXIT
    echo "Waiting for Next.js to be ready on http://localhost:${port}/ ..."
    ready=0
    for i in $(seq 1 120); do
        if curl -fsS --connect-timeout 2 --max-time 5 "http://localhost:${port}/" >/dev/null 2>&1; then
            echo "Next.js is up."
            ready=1
            break
        fi
        sleep 1
    done
    if [ "$ready" -ne 1 ]; then
        echo "Next.js did not become ready on http://localhost:${port}/ within 120s" >&2
        exit 1
    fi
    npx playwright test tests/tests/ci-smoke.spec.ts --project=chromium --retries=0 --workers=1 --reporter=dot

# === Benchmarks (helper-script wrappers) =====================================
# Thin pass-throughs to scripts/. Configure via env vars (see the README and each script's
# header), e.g. `RUN_NEO4J=0 QUERIES_COUNT=25000 just bench-small`.

# Run the small-dataset benchmark pipeline.
bench-small:
    ./scripts/run_small_benchmark.sh

# Run the medium-dataset benchmark pipeline.
bench-medium:
    ./scripts/run_medium_benchmark.sh

# Run the large-dataset benchmark pipeline.
bench-large:
    ./scripts/run_large_benchmark.sh

# === Aggregates ==============================================================

# Fast pre-commit loop for Rust: format, lint, build.
check: fmt clippy build

# Must be green before declaring a task done.
# Every Rust CI gate, in the same order CI runs them: build, clippy, test.
ci: build clippy test

# === Housekeeping ============================================================

# Remove Rust build artifacts, coverage output, and the UI build/test output.
clean:
    cargo clean
    rm -f codecov.json
    rm -rf ui/.next ui/playwright-report ui/test-results
