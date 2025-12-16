#!/usr/bin/env bash
set -euo pipefail

# Small dataset benchmark runner.
#
# This script never prints passwords. Provide credentials via env vars or it will prompt
# (prompt input is hidden).
#
# Required tools:
# - cargo
# - cypher-shell (for Neo4j + Memgraph wipe)
# - redis-cli (for FalkorDB graph wipe)
#
# Env vars (optional; defaults shown):
#   FALKOR_ENDPOINT   (default: falkor://127.0.0.1:6379)
#   NEO4J_ENDPOINT    (default: neo4j://127.0.0.1:7687)
#   NEO4J_USER        (default: neo4j)
#   NEO4J_PASSWORD    (no default; will prompt)
#   MEMGRAPH_ENDPOINT (default: bolt://127.0.0.1:17687)
#   MEMGRAPH_USER     (default: six666six)
#   MEMGRAPH_PASSWORD (default: same as MEMGRAPH_USER)
#
# Workload params:
#  BATCH_SIZE (default: 5000)
#  PARALLEL (default: 20)
#  MPS      (default: 2000)
#  QUERIES_FILE (default: small-readonly)
#  QUERIES_COUNT (default: 1000000)
#  WRITE_RATIO (default: 0.0)
#
# Results:
#  RESULTS_DIR (default: Results-YYMMDD-HH:MM)
#    Passed to `benchmark run --results-dir` so all engines write into the same run folder.

FALKOR_ENDPOINT=${FALKOR_ENDPOINT:-"falkor://127.0.0.1:6379"}
NEO4J_ENDPOINT=${NEO4J_ENDPOINT:-"neo4j://127.0.0.1:7687"}
NEO4J_USER=${NEO4J_USER:-"neo4j"}
NEO4J_PASSWORD=${NEO4J_PASSWORD:-"six666six"}
MEMGRAPH_ENDPOINT=${MEMGRAPH_ENDPOINT:-"bolt://127.0.0.1:17687"}
MEMGRAPH_USER=${MEMGRAPH_USER:-"six666six"}
MEMGRAPH_PASSWORD=${MEMGRAPH_PASSWORD:-"${MEMGRAPH_USER}"}

BATCH_SIZE=${BATCH_SIZE:-5000}
PARALLEL=${PARALLEL:-20}
MPS=${MPS:-7500}
QUERIES_FILE=${QUERIES_FILE:-"small-readonly"}
QUERIES_COUNT=${QUERIES_COUNT:-1000000}
WRITE_RATIO=${WRITE_RATIO:-0.0}

# Use a single shared results directory for all vendors so `benchmark aggregate` can
# generate neo4j-vs-falkordb and memgraph-vs-falkordb UI summaries from one run.
RESULTS_DIR=${RESULTS_DIR:-"Results-$(date +%y%m%d-%H:%M)"}

# Prompt for secrets if not set.
if [[ -z "${NEO4J_PASSWORD:-}" ]]; then
  read -r -s -p "Neo4j password for user '${NEO4J_USER}': " NEO4J_PASSWORD
  echo
fi

# MEMGRAPH_PASSWORD defaults to MEMGRAPH_USER above; only prompt if user explicitly cleared it.
if [[ -z "${MEMGRAPH_PASSWORD:-}" && -n "${MEMGRAPH_USER}" ]]; then
  read -r -s -p "Memgraph password for user '${MEMGRAPH_USER}': " MEMGRAPH_PASSWORD
  echo
fi

export NEO4J_PASSWORD
export MEMGRAPH_PASSWORD

# The benchmark binary now supports credentials via env vars when endpoint URL omits them.
export NEO4J_USER
export MEMGRAPH_USER

echo "==> Verifying Neo4j login"
cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j "RETURN 1 AS ok" >/dev/null

echo "==> Clearing Neo4j database (neo4j)"
# Drop known constraints used in earlier experiments (best-effort)
cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j \
  "DROP CONSTRAINT movie_title IF EXISTS; DROP CONSTRAINT person_name IF EXISTS;" >/dev/null
# Wipe all data
cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j \
  "MATCH (n) DETACH DELETE n;" >/dev/null

echo "==> Clearing FalkorDB graph (falkor)"
if ! command -v redis-cli >/dev/null 2>&1; then
  echo "redis-cli not found (required to wipe FalkorDB graph)." >&2
  exit 1
fi

# Extract host/port from FALKOR_ENDPOINT (default falkor://127.0.0.1:6379)
FALKOR_HOSTPORT="${FALKOR_ENDPOINT#falkor://}"
if [[ "$FALKOR_HOSTPORT" == *:* ]]; then
  FALKOR_HOST="${FALKOR_HOSTPORT%%:*}"
  FALKOR_PORT="${FALKOR_HOSTPORT##*:}"
else
  FALKOR_HOST="$FALKOR_HOSTPORT"
  FALKOR_PORT=6379
fi

# Ignore failures if the graph doesn't exist yet
redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" GRAPH.DELETE falkor >/dev/null 2>&1 || true

# NOTE: Newer Neo4j cypher-shell versions send `CALL db.ping()` on connect.
# Memgraph doesn't implement that procedure, so we avoid using cypher-shell against Memgraph.
# Instead, we clear Memgraph via the benchmark client during load (see --force below).

echo "==> Loading small dataset"
cargo run --release --bin benchmark -- load --vendor falkor --size small --endpoint "$FALKOR_ENDPOINT" -b "$BATCH_SIZE"
#cargo run --release --bin benchmark -- load --vendor neo4j --size small --endpoint "$NEO4J_ENDPOINT" -b "$BATCH_SIZE"
# --force clears the external Memgraph instance before loading
cargo run --release --bin benchmark -- load --vendor memgraph --size small --endpoint "$MEMGRAPH_ENDPOINT" -b "$BATCH_SIZE" --force

echo "==> Generating queries file: ${QUERIES_FILE} (dataset=small, count=${QUERIES_COUNT}, write_ratio=${WRITE_RATIO})"
# Always regenerate so the file contains the latest query catalog + stable q_id fields.
cargo run --release --bin benchmark -- generate-queries --dataset small --size "$QUERIES_COUNT" --name "$QUERIES_FILE" --write-ratio "$WRITE_RATIO"

echo "==> Running ${QUERIES_FILE} workload (parallel=${PARALLEL}, mps=${MPS})"
echo "==> Writing detailed run results to: ${RESULTS_DIR}/<vendor>/"

cargo run --release --bin benchmark -- run --vendor falkor --name "$QUERIES_FILE" --parallel "$PARALLEL" --mps "$MPS" --endpoint "$FALKOR_ENDPOINT" --results-dir "$RESULTS_DIR"
#cargo run --release --bin benchmark -- run --vendor neo4j --name "$QUERIES_FILE" --parallel "$PARALLEL" --mps "$MPS" --endpoint "$NEO4J_ENDPOINT" --results-dir "$RESULTS_DIR"
cargo run --release --bin benchmark -- run --vendor memgraph --name "$QUERIES_FILE" --parallel "$PARALLEL" --mps "$MPS" --endpoint "$MEMGRAPH_ENDPOINT" --results-dir "$RESULTS_DIR"

echo "==> Aggregating UI summaries to ui/public/summaries"
cargo run --release --bin benchmark -- aggregate --results-dir "$RESULTS_DIR"

echo "==> Done"
