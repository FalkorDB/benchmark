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

# Run a single test by name filter, e.g. `just test-one aggregator`.
test-one *args:
    cargo test "$@"

# === Rust: run ===============================================================

# Run the benchmark binary, forwarding args, e.g. `just run -- --help` or `just run load ...`.
run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Drop an optional leading `--` so both `just run --help` and `just run -- --help` forward
    # the flags to the binary (not to cargo).
    if [ "${1:-}" = "--" ]; then shift; fi
    cargo run --bin benchmark -- "$@"

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
    cd ui && PORT={{PORT}} npm run dev

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
        if curl -fsS "http://localhost:${port}/" >/dev/null 2>&1; then
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

# Remove Rust build artifacts and the UI build/test output.
clean:
    cargo clean
    rm -rf ui/.next ui/playwright-report ui/test-results
