#!/usr/bin/env bash
set -euo pipefail

# Medium dataset benchmark runner.
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
#  MPS      (default: 7500)
#  QUERIES_FILE (default: medium-readonly)
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
MEMGRAPH_USER=${MEMGRAPH_USER:-"memgraph"}
MEMGRAPH_PASSWORD=${MEMGRAPH_PASSWORD:-"six666six"}

# Vendor toggles: set to 1 to enable, 0 to disable
RUN_FALKOR=${RUN_FALKOR:-1}
RUN_NEO4J=${RUN_NEO4J:-1}
RUN_MEMGRAPH=${RUN_MEMGRAPH:-0}

BATCH_SIZE=${BATCH_SIZE:-5000}
PARALLEL=${PARALLEL:-10}
MPS=${MPS:-5000}
QUERIES_FILE=${QUERIES_FILE:-"medium-readonly"}
QUERIES_COUNT=${QUERIES_COUNT:-100000}
WRITE_RATIO=${WRITE_RATIO:-0.0}

# Derive per-vendor query file names so each engine can use vendor-optimized queries.
QUERIES_FILE_BASE="${QUERIES_FILE}"
FALKOR_QUERIES_FILE="${QUERIES_FILE_BASE}-falkor"
NEO4J_QUERIES_FILE="${QUERIES_FILE_BASE}-neo4j"
MEMGRAPH_QUERIES_FILE="${QUERIES_FILE_BASE}-memgraph"

# Use a single shared results directory for all vendors so `benchmark aggregate` can
# generate neo4j-vs-falkordb and memgraph-vs-falkordb UI summaries from one run.
RESULTS_DIR=${RESULTS_DIR:-"Results-$(date +%y%m%d-%H:%M)"}

# Prompt for secrets if not set (only for enabled vendors).
if [[ "${RUN_NEO4J}" == "1" && -z "${NEO4J_PASSWORD:-}" ]]; then
  read -r -s -p "Neo4j password for user '${NEO4J_USER}': " NEO4J_PASSWORD
  echo
fi

# MEMGRAPH_PASSWORD defaults to MEMGRAPH_USER above; only prompt if user explicitly cleared it
# and Memgraph is enabled.
if [[ "${RUN_MEMGRAPH}" == "1" && -z "${MEMGRAPH_PASSWORD:-}" && -n "${MEMGRAPH_USER}" ]]; then
  read -r -s -p "Memgraph password for user '${MEMGRAPH_USER}': " MEMGRAPH_PASSWORD
  echo
fi

export NEO4J_PASSWORD
export MEMGRAPH_PASSWORD

# The benchmark binary now supports credentials via env vars when endpoint URL omits them.
export NEO4J_USER
export MEMGRAPH_USER

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "==> Verifying Neo4j login"
  cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j "RETURN 1 AS ok" >/dev/null

  echo "==> Clearing Neo4j database (neo4j)"
  # Drop known constraints used in earlier experiments (best-effort)
  cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j \
    "DROP CONSTRAINT movie_title IF EXISTS; DROP CONSTRAINT person_name IF EXISTS;" >/dev/null
  # Wipe all data
  cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j \
    "MATCH (n) DETACH DELETE n;" >/dev/null
fi

if [[ "${RUN_FALKOR}" == "1" ]]; then
  echo "==> Deleting FalkorDB graph (falkor)"
  if ! command -v redis-cli >/dev/null 2>&1; then
    echo "redis-cli not found (required to wipe FalkorDB graph)." >&2
    exit 1
  fi
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

# Delete the entire FalkorDB graph key; ignore failures if the graph doesn't exist yet
if [[ "${RUN_FALKOR}" == "1" ]]; then
  redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" GRAPH.DELETE falkor >/dev/null 2>&1 || true
  # Also ensure no leftover non-graph key with the same name remains
  redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" DEL falkor >/dev/null 2>&1 || true
fi

# NOTE: Newer Neo4j cypher-shell versions send `CALL db.ping()` on connect.
# Memgraph doesn't implement that procedure, so we avoid using cypher-shell against Memgraph.
# Instead, we clear Memgraph via the benchmark client during load (see --force below).

if [[ "${RUN_FALKOR}" == "1" ]]; then
  echo "==> Preparing CSVs for FalkorDB bulk loader (User and FRIEND) [medium dataset]"
  CSV_DIR="cache/neo4j/users/medium"
  PCH_FILE="$CSV_DIR/pokec_medium_import.cypher"
  if [[ ! -f "$PCH_FILE" ]]; then
    echo "❌ Expected $PCH_FILE to exist; run the medium dataset load once to download/decompress the dataset (e.g., via 'cargo run --release --bin benchmark -- load --vendor neo4j --size medium')." >&2
    exit 1
  fi

  mkdir -p "$CSV_DIR"

  # Generate User.csv (nodes) if missing
  if [[ ! -f "$CSV_DIR/User.csv" ]]; then
    echo "  - Generating User.csv from $(basename "$PCH_FILE")"
    (cd "$CSV_DIR" && {
      echo 'id,completion_percentage,gender,age'
      perl -ne 'if (/^CREATE \(:User \{(.*)\}\);$/) { my $s=$1; my %f; for my $p (split /,\s*/, $s) { my ($k,$v)=split /:\s*/, $p,2; $v =~ s/^"//; $v =~ s/"$//; $f{$k}=$v; } print "$f{id},$f{completion_percentage},$f{gender},$f{age}\n" if defined $f{id}; }' "$(basename "$PCH_FILE")"
    } > User.csv)
  fi

  # Generate FRIEND.csv (edges) if missing
  if [[ ! -f "$CSV_DIR/FRIEND.csv" ]]; then
    echo "  - Generating FRIEND.csv from $(basename "$PCH_FILE")"
    (cd "$CSV_DIR" && {
      echo 'src_id,dst_id'
      perl -ne 'if (/^MATCH \(n:User {id: (\d+)}\), \(m:User {id: (\d+)}\) CREATE \(n\)-\[e: Friend\]->\(m\);$/) { print "$1,$2\n"; }' "$(basename "$PCH_FILE")"
    } > FRIEND.csv)
  fi

  echo "==> Bulk-loading medium Pokec dataset into FalkorDB via falkordb-bulk-loader"
  REDIS_URL="redis://$FALKOR_HOST:$FALKOR_PORT"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "❌ python3 not found; required to run falkordb-bulk-loader" >&2
    exit 1
  fi

  # Use similar (but slightly conservative) batch/concurrency settings as the large benchmark.
  PYTHONPATH="../falkordb-bulk-loader" python3 ../falkordb-bulk-loader/falkordb_bulk_loader/bulk_insert.py falkor \
    -u "$REDIS_URL" \
    -n "$CSV_DIR/User.csv" \
    -r "$CSV_DIR/FRIEND.csv" \
    -j INTEGER -s -i User:id -i User:age \
    -c 128 -b 16 -t 16
fi

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "==> Loading medium dataset into Neo4j"
  cargo run --release --bin benchmark -- load --vendor neo4j --size medium --endpoint "$NEO4J_ENDPOINT" -b "$BATCH_SIZE"
fi

if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "==> Loading medium dataset into Memgraph (UNWIND loader)"
  # --force clears the external Memgraph instance before loading
  cargo run --release --bin benchmark -- load --vendor memgraph --size medium --endpoint "$MEMGRAPH_ENDPOINT" -b "$BATCH_SIZE" --force
fi

echo "==> Generating vendor-specific query files (base=${QUERIES_FILE_BASE}, dataset=medium, count=${QUERIES_COUNT}, write_ratio=${WRITE_RATIO})"
# Always regenerate so each vendor gets the latest query catalog + stable q_id fields.
if [[ "${RUN_FALKOR}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor falkor   --dataset medium --size "$QUERIES_COUNT" --name "$FALKOR_QUERIES_FILE"   --write-ratio "$WRITE_RATIO"
fi
if [[ "${RUN_NEO4J}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor neo4j   --dataset medium --size "$QUERIES_COUNT" --name "$NEO4J_QUERIES_FILE"   --write-ratio "$WRITE_RATIO"
fi
if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor memgraph --dataset medium --size "$QUERIES_COUNT" --name "$MEMGRAPH_QUERIES_FILE" --write-ratio "$WRITE_RATIO"
fi

echo "==> Running ${QUERIES_FILE} workload (parallel=${PARALLEL}, mps=${MPS})"
echo "==> Writing detailed run results to: ${RESULTS_DIR}/<vendor>/"

if [[ "${RUN_FALKOR}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor falkor   --name "$FALKOR_QUERIES_FILE"   --parallel "$PARALLEL" --mps "$MPS" --endpoint "$FALKOR_ENDPOINT"   --results-dir "$RESULTS_DIR"
fi
if [[ "${RUN_NEO4J}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor neo4j   --name "$NEO4J_QUERIES_FILE"   --parallel "$PARALLEL" --mps "$MPS" --endpoint "$NEO4J_ENDPOINT"   --results-dir "$RESULTS_DIR"
fi
if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor memgraph --name "$MEMGRAPH_QUERIES_FILE" --parallel "$PARALLEL" --mps "$MPS" --endpoint "$MEMGRAPH_ENDPOINT" --results-dir "$RESULTS_DIR"
fi

echo "==> Aggregating UI summaries to ui/public/summaries"
cargo run --release --bin benchmark -- aggregate --results-dir "$RESULTS_DIR"

echo "==> Done"
