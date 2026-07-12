#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"

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
#  QUERIES_COUNT (default: 20000)
#  WRITE_RATIO (default: 0.0)
#  FALKOR_QUERY_TIMEOUT_MS (default: 900000)
#  ENABLE_ALGO_PAGERANK (default: 1)
#  ENABLE_ALGO_MAX_FLOW (default: 1)
#  ENABLE_ALGO_MSF (default: 1)
#  ENABLE_ALGO_HARMONIC (default: 1)
#  IN_SCRIPT_QUERY_PROFILE (default: empty; set in this file to force profile)
#    baseline          - baseline query set
#    extended-core     - baseline + extended core queries
#    fixture-dependent - extended-core + fixture/index-dependent queries
#  QUERY_PROFILE (env fallback when IN_SCRIPT_QUERY_PROFILE is empty; default: baseline)
#
# Results:
#  RESULTS_DIR (default: Results-YYMMDD-HH:MM)
#    Passed to `benchmark run --results-dir` so all engines write into the same run folder.

FALKOR_ENDPOINT=${FALKOR_ENDPOINT:-falkor://127.0.0.1:6379}
# Secondary FalkorDB endpoint for version comparison (e.g. rust-based)
FALKOR_ENDPOINT_2=${FALKOR_ENDPOINT_2:-falkor://127.0.0.1:6800}
# Suffix/name for version comparison results folders (metadata)
FALKOR_NAME=${FALKOR_NAME:-"falkordb-c"}
FALKOR_2_NAME=${FALKOR_2_NAME:-"falkordb-rs"}
NEO4J_ENDPOINT=${NEO4J_ENDPOINT:-neo4j://127.0.0.1:7687}
NEO4J_USER=${NEO4J_USER:-"neo4j"}
NEO4J_PASSWORD=${NEO4J_PASSWORD:-"six666six"}
MEMGRAPH_ENDPOINT=${MEMGRAPH_ENDPOINT:-"bolt://127.0.0.1:17687"}
MEMGRAPH_USER=${MEMGRAPH_USER:-"memgraph"}
MEMGRAPH_PASSWORD=${MEMGRAPH_PASSWORD:-"six666six"}

# Vendor toggles: set to 1 to enable, 0 to disable
RUN_FALKOR=${RUN_FALKOR:-1}
# Set to 1 to run comparison against the secondary FalkorDB version
RUN_FALKOR_2=${RUN_FALKOR_2:-1}
RUN_NEO4J=${RUN_NEO4J:-0}
RUN_MEMGRAPH=${RUN_MEMGRAPH:-0}

BATCH_SIZE=${BATCH_SIZE:-5000}
PARALLEL=${PARALLEL:-10}
MPS=${MPS:-3000}
QUERIES_FILE=${QUERIES_FILE:-"medium-readonly"}
QUERIES_COUNT=${QUERIES_COUNT:-2500}
WRITE_RATIO=${WRITE_RATIO:-0.05}
FALKOR_QUERY_TIMEOUT_MS=${FALKOR_QUERY_TIMEOUT_MS:-900000}
ENABLE_ALGO_PAGERANK=${ENABLE_ALGO_PAGERANK:-0}
ENABLE_ALGO_MAX_FLOW=${ENABLE_ALGO_MAX_FLOW:-0}
ENABLE_ALGO_MSF=${ENABLE_ALGO_MSF:-0}
ENABLE_ALGO_HARMONIC=${ENABLE_ALGO_HARMONIC:-0}
# Optional in-script query profile override.
# Set this value directly in the script to force a profile for every run.
# Leave empty ("") to keep env-based behavior (QUERY_PROFILE env var, else baseline).
IN_SCRIPT_QUERY_PROFILE="fixture-dependent"
if [[ -n "$IN_SCRIPT_QUERY_PROFILE" ]]; then
  QUERY_PROFILE="$IN_SCRIPT_QUERY_PROFILE"
else
  QUERY_PROFILE=${QUERY_PROFILE:-baseline}
fi

case "$QUERY_PROFILE" in
  baseline|extended-core|fixture-dependent) ;;
  *)
    echo "Invalid QUERY_PROFILE '$QUERY_PROFILE'. Valid options: baseline, extended-core, fixture-dependent." >&2
    exit 1
    ;;
esac

# Derive per-vendor query file names so each engine can use vendor-optimized queries.
QUERIES_FILE_BASE="${QUERIES_FILE}"
FALKOR_QUERIES_FILE="${QUERIES_FILE_BASE}-falkor"
NEO4J_QUERIES_FILE="${QUERIES_FILE_BASE}-neo4j"
MEMGRAPH_QUERIES_FILE="${QUERIES_FILE_BASE}-memgraph"

# Use a single shared results directory for all vendors so `benchmark aggregate` can
# generate neo4j-vs-falkordb and memgraph-vs-falkordb UI summaries from one run.
RESULTS_DIR=${RESULTS_DIR:-"Results-$(date +%y%m%d-%H:%M)"}

normalize_bool() {
  case "$1" in
    1|true|TRUE|True|yes|YES|Yes|on|ON|On) echo "true" ;;
    0|false|FALSE|False|no|NO|No|off|OFF|Off) echo "false" ;;
    *)
      echo "Invalid boolean value '$1' for $2 (expected 1/0 or true/false)" >&2
      exit 1
      ;;
  esac
}

set_falkor_query_timeout() {
  local label="$1"
  local host="$2"
  local port="$3"
  local result

  if ! result=$(redis-cli -h "$host" -p "$port" GRAPH.CONFIG SET TIMEOUT "$FALKOR_QUERY_TIMEOUT_MS" 2>&1); then
    echo "  - Warning: failed to set ${label} query timeout to ${FALKOR_QUERY_TIMEOUT_MS}ms: ${result}" >&2
    return 0
  fi

  echo "  - ${label} query timeout set to ${FALKOR_QUERY_TIMEOUT_MS}ms"
}
ENABLE_ALGO_PAGERANK_BOOL=$(normalize_bool "$ENABLE_ALGO_PAGERANK" "ENABLE_ALGO_PAGERANK")
ENABLE_ALGO_MAX_FLOW_BOOL=$(normalize_bool "$ENABLE_ALGO_MAX_FLOW" "ENABLE_ALGO_MAX_FLOW")
ENABLE_ALGO_MSF_BOOL=$(normalize_bool "$ENABLE_ALGO_MSF" "ENABLE_ALGO_MSF")
ENABLE_ALGO_HARMONIC_BOOL=$(normalize_bool "$ENABLE_ALGO_HARMONIC" "ENABLE_ALGO_HARMONIC")
ALGO_QUERY_ARGS=(
  --enable-algo-pagerank "$ENABLE_ALGO_PAGERANK_BOOL"
  --enable-algo-max-flow "$ENABLE_ALGO_MAX_FLOW_BOOL"
  --enable-algo-msf "$ENABLE_ALGO_MSF_BOOL"
  --enable-algo-harmonic "$ENABLE_ALGO_HARMONIC_BOOL"
)

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
export FALKOR_QUERY_TIMEOUT_MS

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

if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
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

if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  FALKOR_2_HOSTPORT="${FALKOR_ENDPOINT_2#falkor://}"
  if [[ "$FALKOR_2_HOSTPORT" == *:* ]]; then
    FALKOR_2_HOST="${FALKOR_2_HOSTPORT%%:*}"
    FALKOR_2_PORT="${FALKOR_2_HOSTPORT##*:}"
  else
    FALKOR_2_HOST="$FALKOR_2_HOSTPORT"
    FALKOR_2_PORT=3800
  fi
fi

# Delete the entire FalkorDB graph key; ignore failures if the graph doesn't exist yet
if [[ "${RUN_FALKOR}" == "1" ]]; then
  echo "==> Clearing FalkorDB graph (falkor) on port $FALKOR_PORT"
  redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" GRAPH.DELETE falkor >/dev/null 2>&1 || true
  # Also ensure no leftover non-graph key with the same name remains
  redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" DEL falkor >/dev/null 2>&1 || true
fi

if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  echo "==> Clearing FalkorDB (secondary) graph on port $FALKOR_2_PORT"
  redis-cli -h "$FALKOR_2_HOST" -p "$FALKOR_2_PORT" GRAPH.DELETE falkor >/dev/null 2>&1 || true
  redis-cli -h "$FALKOR_2_HOST" -p "$FALKOR_2_PORT" DEL falkor >/dev/null 2>&1 || true
fi

# NOTE: Newer Neo4j cypher-shell versions send `CALL db.ping()` on connect.
# Memgraph doesn't implement that procedure, so we avoid using cypher-shell against Memgraph.
# Instead, we clear Memgraph via the benchmark client during load (see --force below).

if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
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

  # Generate FRIEND.csv (edges) if missing or if it is in the legacy 2-column format.
  if [[ ! -f "$CSV_DIR/FRIEND.csv" ]] || [[ "$(head -n 1 "$CSV_DIR/FRIEND.csv" 2>/dev/null)" != "src_id,dst_id,bench_capacity" ]]; then
    echo "  - Generating FRIEND.csv (with bench_capacity) from $(basename "$PCH_FILE")"
    (cd "$CSV_DIR" && {
      echo 'src_id,dst_id,bench_capacity'
      perl -ne 'if (/^MATCH \(n:User {id: (\d+)}\), \(m:User {id: (\d+)}\) CREATE \(n\)-\[e: Friend\]->\(m\);$/) { my ($src, $dst) = ($1, $2); my $cap = 1 + (($src * 31 + $dst * 17) % 20); print "$src,$dst,$cap\n"; }' "$(basename "$PCH_FILE")"
    } > FRIEND.csv)
  fi
fi

if [[ "${RUN_FALKOR}" == "1" ]]; then
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
    -R Friend "$CSV_DIR/FRIEND.csv" \
    -j INTEGER -s -i User:id -i User:age \
    -c 128 -b 16 -t 16
fi

if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  echo "==> Bulk-loading medium Pokec dataset into FalkorDB (secondary) via falkordb-bulk-loader"
  REDIS_URL_2="redis://$FALKOR_2_HOST:$FALKOR_2_PORT"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "❌ python3 not found; required to run falkordb-bulk-loader" >&2
    exit 1
  fi

  PYTHONPATH="../falkordb-bulk-loader" python3 ../falkordb-bulk-loader/falkordb_bulk_loader/bulk_insert.py falkor \
    -u "$REDIS_URL_2" \
    -n "$CSV_DIR/User.csv" \
    -R Friend "$CSV_DIR/FRIEND.csv" \
    -j INTEGER -s -i User:id -i User:age \
    -c 128 -b 16 -t 16
fi

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "==> Loading medium dataset into Neo4j"
  cargo run --release --bin benchmark -- load --vendor neo4j --size medium --endpoint "$NEO4J_ENDPOINT" -b "$BATCH_SIZE" --query-profile "$QUERY_PROFILE"
fi

if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "==> Loading medium dataset into Memgraph (UNWIND loader)"
  # --force clears the external Memgraph instance before loading
  cargo run --release --bin benchmark -- load --vendor memgraph --size medium --endpoint "$MEMGRAPH_ENDPOINT" -b "$BATCH_SIZE" --force --query-profile "$QUERY_PROFILE"
fi
if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
  echo "==> Configuring FalkorDB query timeout (${FALKOR_QUERY_TIMEOUT_MS}ms)"
fi
if [[ "${RUN_FALKOR}" == "1" ]]; then
  set_falkor_query_timeout "FalkorDB" "$FALKOR_HOST" "$FALKOR_PORT"
fi
if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  set_falkor_query_timeout "FalkorDB (secondary)" "$FALKOR_2_HOST" "$FALKOR_2_PORT"
fi

echo "==> Generating vendor-specific query files (base=${QUERIES_FILE_BASE}, dataset=medium, count=${QUERIES_COUNT}, write_ratio=${WRITE_RATIO}, profile=${QUERY_PROFILE})"
echo "==> Algorithm query toggles (pagerank=${ENABLE_ALGO_PAGERANK_BOOL}, max_flow=${ENABLE_ALGO_MAX_FLOW_BOOL}, msf=${ENABLE_ALGO_MSF_BOOL}, harmonic=${ENABLE_ALGO_HARMONIC_BOOL})"
# Always regenerate so each vendor gets the latest query catalog + stable q_id fields.
if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor falkor   --dataset medium --size "$QUERIES_COUNT" --name "$FALKOR_QUERIES_FILE"   --write-ratio "$WRITE_RATIO" --query-profile "$QUERY_PROFILE" "${ALGO_QUERY_ARGS[@]}"
fi
if [[ "${RUN_NEO4J}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor neo4j   --dataset medium --size "$QUERIES_COUNT" --name "$NEO4J_QUERIES_FILE"   --write-ratio "$WRITE_RATIO" --query-profile "$QUERY_PROFILE" "${ALGO_QUERY_ARGS[@]}"
fi
if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor memgraph --dataset medium --size "$QUERIES_COUNT" --name "$MEMGRAPH_QUERIES_FILE" --write-ratio "$WRITE_RATIO" --query-profile "$QUERY_PROFILE" "${ALGO_QUERY_ARGS[@]}"
fi

echo "==> Running ${QUERIES_FILE} workload (parallel=${PARALLEL}, mps=${MPS})"
echo "==> Writing detailed run results to: ${RESULTS_DIR}/<vendor>/"

if [[ "${RUN_FALKOR}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor falkor   --name "$FALKOR_QUERIES_FILE"   --parallel "$PARALLEL" --mps "$MPS" --endpoint "$FALKOR_ENDPOINT"   --results-dir "$RESULTS_DIR"

  # Store first falkor results in a subfolder using custom metadata name
  mkdir -p "$RESULTS_DIR/$FALKOR_NAME"
  mv "$RESULTS_DIR/falkor"/* "$RESULTS_DIR/$FALKOR_NAME/"
  rmdir "$RESULTS_DIR/falkor"
fi

if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  echo "==> Running workload against FalkorDB (secondary) on $FALKOR_ENDPOINT_2"
  cargo run --release --bin benchmark -- run --vendor falkor   --name "$FALKOR_QUERIES_FILE"   --parallel "$PARALLEL" --mps "$MPS" --endpoint "$FALKOR_ENDPOINT_2"   --results-dir "$RESULTS_DIR"

  # Store secondary falkor results in a subfolder using custom metadata name
  mkdir -p "$RESULTS_DIR/$FALKOR_2_NAME"
  mv "$RESULTS_DIR/falkor"/* "$RESULTS_DIR/$FALKOR_2_NAME/"
  rmdir "$RESULTS_DIR/falkor"
fi

if [[ "${RUN_NEO4J}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor neo4j   --name "$NEO4J_QUERIES_FILE"   --parallel "$PARALLEL" --mps "$MPS" --endpoint "$NEO4J_ENDPOINT"   --results-dir "$RESULTS_DIR"
fi
if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  cargo run --release --bin benchmark -- run --vendor memgraph --name "$MEMGRAPH_QUERIES_FILE" --parallel "$PARALLEL" --mps "$MPS" --endpoint "$MEMGRAPH_ENDPOINT" --results-dir "$RESULTS_DIR"
fi

if [[ "${RUN_FALKOR_2}" == "1" ]]; then
  echo "==> Aggregating comparison run into UI summary: $SCRIPT_DIR/../ui/public/summaries/falkordb_vs_falkordb.json"
  cargo run --release --bin benchmark -- aggregate-aws-tests --aws-tests-dir "$RESULTS_DIR" --out-path "$SCRIPT_DIR/../ui/public/summaries/falkordb_vs_falkordb.json"
else
  # If secondary run is not enabled, restore standard structure before normal aggregation
  if [[ "${RUN_FALKOR}" == "1" ]]; then
    mkdir -p "$RESULTS_DIR/falkor"
    mv "$RESULTS_DIR/$FALKOR_NAME"/* "$RESULTS_DIR/falkor/"
    rmdir "$RESULTS_DIR/$FALKOR_NAME"
  fi
  echo "==> Aggregating UI summaries to $SCRIPT_DIR/../ui/public/summaries"
  cargo run --release --bin benchmark -- aggregate --results-dir "$RESULTS_DIR" --out-dir "$SCRIPT_DIR/../ui/public/summaries"
fi

echo "==> Done"
