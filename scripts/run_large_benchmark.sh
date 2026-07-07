#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"

# Large dataset benchmark runner.
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
#  BATCH_SIZE   (default: 5000)
#  PARALLEL    (default: 20)
#  MPS         (default: 7500)
#  QUERIES_FILE  (default: large-readonly)
#  QUERIES_COUNT (default: 20000)
#  WRITE_RATIO   (default: 0.03)
#  FALKOR_QUERY_TIMEOUT_MS (default: 900000)
#  ENABLE_ALGO_PAGERANK (default: 0)
#  ENABLE_ALGO_MAX_FLOW (default: 0)
#  ENABLE_ALGO_MSF (default: 0)
#  ENABLE_ALGO_HARMONIC (default: 0)
#  FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN (default: 1)
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
NEO4J_PASSWORD=${NEO4J_PASSWORD:-"neo4jpass"}
MEMGRAPH_ENDPOINT=${MEMGRAPH_ENDPOINT:-"bolt://127.0.0.1:17687"}
MEMGRAPH_USER=${MEMGRAPH_USER:-"memgraph"}
MEMGRAPH_PASSWORD=${MEMGRAPH_PASSWORD:-"six666six"}
# Docker container running Memgraph; can be overridden via MEMGRAPH_CONTAINER_ID env var.
MEMGRAPH_CONTAINER_ID=${MEMGRAPH_CONTAINER_ID:-"da0b0f388531"}

# Vendor toggles: set to 1 to enable, 0 to disable
RUN_FALKOR=${RUN_FALKOR:-1}
# Set to 1 to run comparison against the secondary FalkorDB version
RUN_FALKOR_2=${RUN_FALKOR_2:-1}
RUN_NEO4J=${RUN_NEO4J:-0}
# Enable Memgraph by default for large benchmark comparisons (can be overridden via env).
RUN_MEMGRAPH=${RUN_MEMGRAPH:-0}

BATCH_SIZE=${BATCH_SIZE:-10000}
PARALLEL=${PARALLEL:-10}
MPS=${MPS:-200}
QUERIES_FILE=${QUERIES_FILE:-"large-readonly"}
QUERIES_COUNT=${QUERIES_COUNT:-20000}
WRITE_RATIO=${WRITE_RATIO:-0.0}
FALKOR_QUERY_TIMEOUT_MS=${FALKOR_QUERY_TIMEOUT_MS:-900000}
FALKOR_READY_MAX_RETRIES=${FALKOR_READY_MAX_RETRIES:-30}
FALKOR_READY_RETRY_DELAY_SECS=${FALKOR_READY_RETRY_DELAY_SECS:-2}
FALKOR_CLEAR_MAX_RETRIES=${FALKOR_CLEAR_MAX_RETRIES:-30}
FALKOR_CLEAR_RETRY_DELAY_SECS=${FALKOR_CLEAR_RETRY_DELAY_SECS:-1}
ENABLE_ALGO_PAGERANK=${ENABLE_ALGO_PAGERANK:-0}
ENABLE_ALGO_MAX_FLOW=${ENABLE_ALGO_MAX_FLOW:-0}
ENABLE_ALGO_MSF=${ENABLE_ALGO_MSF:-0}
ENABLE_ALGO_HARMONIC=${ENABLE_ALGO_HARMONIC:-0}
FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN=${FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN:-1}

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

clear_falkor_graph() {
  local label="$1"
  local host="$2"
  local port="$3"
  wait_for_falkor_endpoint_ready "$label" "$host" "$port"

  echo "==> Clearing ${label} graph (falkor) on port $port"
  redis-cli -h "$host" -p "$port" GRAPH.DELETE falkor >/dev/null 2>&1 || true
  # Also ensure no leftover non-graph key with the same name remains
  redis-cli -h "$host" -p "$port" DEL falkor >/dev/null 2>&1 || true
  wait_for_falkor_graph_cleared "$label" "$host" "$port"
}

wait_for_falkor_endpoint_ready() {
  local label="$1"
  local host="$2"
  local port="$3"

  for ((attempt=1; attempt<=FALKOR_READY_MAX_RETRIES; attempt++)); do
    if redis-cli -h "$host" -p "$port" PING >/dev/null 2>&1; then
      if [[ "$attempt" -gt 1 ]]; then
        echo "  - ${label} endpoint became ready on ${host}:${port} after ${attempt} attempt(s)"
      fi
      return 0
    fi

    if [[ "$attempt" -lt "$FALKOR_READY_MAX_RETRIES" ]]; then
      echo "  - ${label} endpoint not ready on ${host}:${port} (attempt ${attempt}/${FALKOR_READY_MAX_RETRIES}); retrying in ${FALKOR_READY_RETRY_DELAY_SECS}s..."
      sleep "$FALKOR_READY_RETRY_DELAY_SECS"
    fi
  done

  echo "❌ ${label} endpoint is not reachable on ${host}:${port} after ${FALKOR_READY_MAX_RETRIES} attempts." >&2
  exit 1
}

wait_for_falkor_graph_cleared() {
  local label="$1"
  local host="$2"
  local port="$3"
  local exists

  for ((attempt=1; attempt<=FALKOR_CLEAR_MAX_RETRIES; attempt++)); do
    exists=$(redis-cli --raw -h "$host" -p "$port" EXISTS falkor 2>/dev/null || echo "1")
    if [[ "$exists" == "0" ]]; then
      if [[ "$attempt" -gt 1 ]]; then
        echo "  - ${label} graph key 'falkor' cleared after ${attempt} attempt(s)"
      fi
      return 0
    fi

    if [[ "$attempt" -lt "$FALKOR_CLEAR_MAX_RETRIES" ]]; then
      echo "  - Waiting for ${label} graph key 'falkor' to clear (attempt ${attempt}/${FALKOR_CLEAR_MAX_RETRIES}); retrying in ${FALKOR_CLEAR_RETRY_DELAY_SECS}s..."
      sleep "$FALKOR_CLEAR_RETRY_DELAY_SECS"
    fi
  done

  echo "❌ ${label} graph key 'falkor' still exists after ${FALKOR_CLEAR_MAX_RETRIES} attempts." >&2
  redis-cli -h "$host" -p "$port" TYPE falkor 2>/dev/null || true
  exit 1
}

ENABLE_ALGO_PAGERANK_BOOL=$(normalize_bool "$ENABLE_ALGO_PAGERANK" "ENABLE_ALGO_PAGERANK")
ENABLE_ALGO_MAX_FLOW_BOOL=$(normalize_bool "$ENABLE_ALGO_MAX_FLOW" "ENABLE_ALGO_MAX_FLOW")
ENABLE_ALGO_MSF_BOOL=$(normalize_bool "$ENABLE_ALGO_MSF" "ENABLE_ALGO_MSF")
ENABLE_ALGO_HARMONIC_BOOL=$(normalize_bool "$ENABLE_ALGO_HARMONIC" "ENABLE_ALGO_HARMONIC")
FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN_BOOL=$(normalize_bool "$FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN" "FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN")
ALGO_QUERY_ARGS=(
  --enable-algo-pagerank "$ENABLE_ALGO_PAGERANK_BOOL"
  --enable-algo-max-flow "$ENABLE_ALGO_MAX_FLOW_BOOL"
  --enable-algo-msf "$ENABLE_ALGO_MSF_BOOL"
  --enable-algo-harmonic "$ENABLE_ALGO_HARMONIC_BOOL"
)

# Maximum number of data rows per Memgraph LOAD CSV chunk (header row not counted).
MEMGRAPH_CHUNK_SIZE=${MEMGRAPH_CHUNK_SIZE:-1000000}

# Helper to run a single non-interactive mgconsole query inside the Memgraph container
mgconsole_exec() {
  local query="$1"
  docker exec -i "$MEMGRAPH_CONTAINER_ID" mgconsole \
    --host=127.0.0.1 \
    --port=7687 \
    --username="$MEMGRAPH_USER" \
    --password="$MEMGRAPH_PASSWORD" \
    --no_history \
    --use_ssl=false \
    <<<"$query"
}

# Split a CSV into multiple chunk files with the header repeated in each chunk.
# Args: input_csv, output_prefix (e.g. /path/User.chunk), chunk_size
chunk_csv_with_header() {
  local input_csv="$1"
  local output_prefix="$2"
  local chunk_size="$3"

  if [[ ! -f "$input_csv" ]]; then
    echo "  - chunk_csv_with_header: input file $input_csv not found" >&2
    return 1
  fi

  local header
  header=$(head -n1 "$input_csv")

  # Remove any old chunks for this prefix.
  rm -f "${output_prefix}".part_* 2>/dev/null || true

  local part_prefix="${output_prefix}.part_"

  # Skip header and split the data rows into fixed-size chunks.
  tail -n +2 "$input_csv" | split -l "$chunk_size" - "$part_prefix"

  local idx=0
  for tmp in "${part_prefix}"*; do
    [[ -e "$tmp" ]] || continue
    local out="${output_prefix}.${idx}.csv"
    # Prepend header to each chunk.
    printf '%s\n' "$header" > "$out"
    cat "$tmp" >> "$out"
    rm "$tmp"
    idx=$((idx+1))
  done

  echo "  - Created ${idx} chunk file(s) for $(basename "$input_csv") (up to ${chunk_size} records per chunk)"
}

# Prompt for secrets if not set, only for enabled vendors.
if [[ "${RUN_NEO4J}" == "1" && -z "${NEO4J_PASSWORD:-}" ]]; then
  read -r -s -p "Neo4j password for user '${NEO4J_USER}': " NEO4J_PASSWORD
  echo
fi

if [[ "${RUN_MEMGRAPH}" == "1" && -z "${MEMGRAPH_PASSWORD:-}" && -n "${MEMGRAPH_USER}" ]]; then
  read -r -s -p "Memgraph password for user '${MEMGRAPH_USER}': " MEMGRAPH_PASSWORD
  echo
fi

export NEO4J_PASSWORD
export MEMGRAPH_PASSWORD
export NEO4J_USER
export MEMGRAPH_USER
export FALKOR_QUERY_TIMEOUT_MS

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "==> Clearing Neo4j database (neo4j)"
  echo "  - Stopping Neo4j to remove persistent data (large dataset requires fresh start)"
  neo4j stop >/dev/null 2>&1
  
  echo "  - Removing Neo4j data directory"
  NEO4J_DATA_DIR="/opt/homebrew/var/neo4j/data"
  if [[ -d "$NEO4J_DATA_DIR" ]]; then
    rm -rf "$NEO4J_DATA_DIR"/*
  fi
  
  echo "  - Setting initial password for Neo4j"
  neo4j-admin dbms set-initial-password "$NEO4J_PASSWORD" >/dev/null 2>&1
  
  echo "  - Starting Neo4j with clean database"
  neo4j start >/dev/null 2>&1
  
  # Wait for Neo4j to be ready
  echo "  - Waiting for Neo4j to be ready..."
  for i in {1..60}; do
    if cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j "RETURN 1 AS ok" >/dev/null 2>&1; then
      echo "  - Neo4j is ready"
      break
    fi
    if [[ $i -eq 60 ]]; then
      echo "❌ Neo4j failed to start after 60 seconds" >&2
      exit 1
    fi
    sleep 1
  done
fi

if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
  if ! command -v redis-cli >/dev/null 2>&1; then
    echo "redis-cli not found (required to delete FalkorDB graph)." >&2
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

# Clear primary FalkorDB graph key before primary loading.
# Secondary graph is cleared right before its own loading phase.
if [[ "${RUN_FALKOR}" == "1" ]]; then
  clear_falkor_graph "FalkorDB" "$FALKOR_HOST" "$FALKOR_PORT"
fi

# NOTE: Newer Neo4j cypher-shell versions send `CALL db.ping()` on connect.
# Memgraph doesn't implement that procedure, so we avoid using cypher-shell against Memgraph.
# Instead, we clear and bulk-load Memgraph via mgconsole + LOAD CSV inside the Docker container.

# Prepare CSVs for bulk loading (needed by FalkorDB and Memgraph)
if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" || "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "==> Preparing CSVs for bulk loader (User and FRIEND)"
  CSV_DIR="cache/neo4j/users/large"
  PCH_FILE="$CSV_DIR/pokec_large.setup.cypher"
  if [[ ! -f "$PCH_FILE" ]]; then
    echo "❌ Expected $PCH_FILE to exist; run the benchmark once to download/decompress the large dataset." >&2
    exit 1
  fi

  mkdir -p "$CSV_DIR"

  # Generate User.csv (nodes) if missing
  if [[ ! -f "$CSV_DIR/User.csv" ]]; then
    echo "  - Generating User.csv from pokec_large.setup.cypher"
    (cd "$CSV_DIR" && {
      echo 'id,completion_percentage,gender,age'
      perl -ne 'if (/^CREATE \(:User \{(.*)\}\);$/) { my $s=$1; my %f; for my $p (split /,\s*/, $s) { my ($k,$v)=split /:\s*/, $p,2; $v =~ s/^"//; $v =~ s/"$//; $f{$k}=$v; } print "$f{id},$f{completion_percentage},$f{gender},$f{age}\n" if defined $f{id}; }' "$(basename "$PCH_FILE")"
    } > User.csv)
  fi

  # Generate FRIEND.csv (edges) if missing or if it is in the legacy 2-column format.
  if [[ ! -f "$CSV_DIR/FRIEND.csv" ]] || [[ "$(head -n 1 "$CSV_DIR/FRIEND.csv" 2>/dev/null)" != "src_id,dst_id,bench_capacity" ]]; then
    echo "  - Generating FRIEND.csv (with bench_capacity) from pokec_large.setup.cypher"
    (cd "$CSV_DIR" && {
      echo 'src_id,dst_id,bench_capacity'
      perl -ne 'if (/^MATCH \(n:User {id: (\d+)}\), \(m:User {id: (\d+)}\) CREATE \(n\)-\[e: Friend\]->\(m\);$/) { my ($src, $dst) = ($1, $2); my $cap = 1 + (($src * 31 + $dst * 17) % 20); print "$src,$dst,$cap\n"; }' "$(basename "$PCH_FILE")"
    } > FRIEND.csv)
  fi
fi

if [[ "${RUN_FALKOR}" == "1" ]]; then
  # Bulk-load large Pokec dataset into FalkorDB using falkordb-bulk-loader
  echo "==> Bulk-loading large Pokec dataset into FalkorDB via falkordb-bulk-loader"
  REDIS_URL="redis://$FALKOR_HOST:$FALKOR_PORT"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "❌ python3 not found; required to run falkordb-bulk-loader" >&2
    exit 1
  fi

  # NOTE: Omit -e/--skip-invalid-edges so the loader stops on the first invalid edge
  # instead of silently skipping, which helps debug edge failures.
  # NOTE: Throttled batch sizes to reduce connection pressure: smaller token count
  # and buffer sizes mean smaller GRAPH.BULK requests and fewer connection retries.
  PYTHONPATH="../falkordb-bulk-loader" python3 "../falkordb-bulk-loader/falkordb_bulk_loader/bulk_insert.py" falkor \
    -u "$REDIS_URL" \
    -n "$CSV_DIR/User.csv" \
    -r "$CSV_DIR/FRIEND.csv" \
    -j INTEGER -s -i User:id -i User:age \
    -c 128 -b 16 -t 16
fi


if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "==> Bulk-loading large Pokec dataset into Memgraph via LOAD CSV"

  MEMGRAPH_LOAD_START=$(date +%s)
  echo "  - Memgraph LOAD CSV start: $(date)"

  MEMGRAPH_CSV_DIR_IN_CONTAINER="/usr/lib/memgraph/pokec_large"

  echo "  - Preparing CSV directory inside Memgraph container ($MEMGRAPH_CONTAINER_ID)"
  docker exec -u root "$MEMGRAPH_CONTAINER_ID" mkdir -p "$MEMGRAPH_CSV_DIR_IN_CONTAINER"
  docker exec -u root "$MEMGRAPH_CONTAINER_ID" chown -R memgraph:memgraph "$MEMGRAPH_CSV_DIR_IN_CONTAINER"

  echo "  - Copying CSVs into Memgraph container"
  docker cp "$CSV_DIR/User.csv"   "$MEMGRAPH_CONTAINER_ID":"$MEMGRAPH_CSV_DIR_IN_CONTAINER/User.csv"
  docker cp "$CSV_DIR/FRIEND.csv" "$MEMGRAPH_CONTAINER_ID":"$MEMGRAPH_CSV_DIR_IN_CONTAINER/FRIEND.csv"

  # Show approximate CSV sizes so we have a sense of progress during long LOAD CSV runs.
  USER_LINES=$(wc -l < "$CSV_DIR/User.csv" || echo 0)
  FRIEND_LINES=$(wc -l < "$CSV_DIR/FRIEND.csv" || echo 0)
  echo "  - User.csv rows (including header):   ${USER_LINES}"
  echo "  - FRIEND.csv rows (including header): ${FRIEND_LINES}"

  echo "  - Chunking CSVs for Memgraph (up to ${MEMGRAPH_CHUNK_SIZE} records per chunk)"
  chunk_csv_with_header "$CSV_DIR/User.csv"   "$CSV_DIR/User.chunk"   "$MEMGRAPH_CHUNK_SIZE"
  chunk_csv_with_header "$CSV_DIR/FRIEND.csv" "$CSV_DIR/FRIEND.chunk" "$MEMGRAPH_CHUNK_SIZE"

  echo "  - Copying chunked CSVs into Memgraph container"
  for f in "$CSV_DIR"/User.chunk.*.csv; do
    [[ -e "$f" ]] || continue
    docker cp "$f" "$MEMGRAPH_CONTAINER_ID":"$MEMGRAPH_CSV_DIR_IN_CONTAINER/$(basename "$f")"
  done
  for f in "$CSV_DIR"/FRIEND.chunk.*.csv; do
    [[ -e "$f" ]] || continue
    docker cp "$f" "$MEMGRAPH_CONTAINER_ID":"$MEMGRAPH_CSV_DIR_IN_CONTAINER/$(basename "$f")"
  done

  echo "  - Clearing existing Memgraph data"
  mgconsole_exec "MATCH (n) DETACH DELETE n;"

  echo "  - Creating index on :User(id)"
  mgconsole_exec "CREATE INDEX ON :User(id);"

  echo "  - Loading User node chunks via LOAD CSV (up to ${MEMGRAPH_CHUNK_SIZE} records per chunk; this may take several minutes)..."
  USER_CHUNK_INDEX=0
  for f in "$CSV_DIR"/User.chunk.*.csv; do
    [[ -e "$f" ]] || continue
    chunk_file=$(basename "$f")
    echo "    * Loading User chunk ${USER_CHUNK_INDEX} (${chunk_file})..."
    mgconsole_exec "LOAD CSV FROM \"$MEMGRAPH_CSV_DIR_IN_CONTAINER/${chunk_file}\" WITH HEADER AS row
  CREATE (:User {id: ToInteger(row.id),
                 completion_percentage: ToInteger(row.completion_percentage),
                 gender: row.gender,
                 age: ToInteger(row.age)});"
    USER_CHUNK_INDEX=$((USER_CHUNK_INDEX+1))
  done
  echo "  - Finished loading User node chunks at: $(date) (total chunks: ${USER_CHUNK_INDEX})"

  echo "  - Loading Friend relationship chunks via LOAD CSV (up to ${MEMGRAPH_CHUNK_SIZE} records per chunk; this may also take several minutes)..."
  FRIEND_CHUNK_INDEX=0
  for f in "$CSV_DIR"/FRIEND.chunk.*.csv; do
    [[ -e "$f" ]] || continue
    chunk_file=$(basename "$f")
    echo "    * Loading FRIEND chunk ${FRIEND_CHUNK_INDEX} (${chunk_file})..."
    mgconsole_exec "LOAD CSV FROM \"$MEMGRAPH_CSV_DIR_IN_CONTAINER/${chunk_file}\" WITH HEADER AS row
  MATCH (a:User {id: ToInteger(row.src_id)}),
        (b:User {id: ToInteger(row.dst_id)})
  CREATE (a)-[:Friend]->(b);"
    FRIEND_CHUNK_INDEX=$((FRIEND_CHUNK_INDEX+1))
  done
  echo "  - Finished loading Friend relationship chunks at: $(date) (total chunks: ${FRIEND_CHUNK_INDEX})"

  MEMGRAPH_LOAD_END=$(date +%s)
  MEMGRAPH_LOAD_DURATION=$((MEMGRAPH_LOAD_END - MEMGRAPH_LOAD_START))
  echo "  - Memgraph LOAD CSV end: $(date)"
  echo "  - Memgraph LOAD CSV duration: ${MEMGRAPH_LOAD_DURATION}s"
fi

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "==> Loading large dataset into Neo4j"
  cargo run --release --bin benchmark -- load --vendor neo4j --size large --endpoint "$NEO4J_ENDPOINT" -b "$BATCH_SIZE"
fi

if [[ "${RUN_FALKOR}" == "1" ]]; then
  echo "==> Configuring FalkorDB query timeout (${FALKOR_QUERY_TIMEOUT_MS}ms) before primary run"
  set_falkor_query_timeout "FalkorDB" "$FALKOR_HOST" "$FALKOR_PORT"
fi

if [[ "${RUN_NEO4J}" == "1" || "${RUN_FALKOR}" == "1" || "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "==> Validating database contents before running queries"
fi

if [[ "${RUN_NEO4J}" == "1" ]]; then
  echo "  - Checking Neo4j data..."
  NODE_COUNT=$(cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j --format plain "MATCH (n:User) RETURN count(n) AS count" 2>/dev/null | tail -n1 | tr -d '"' || echo "0")
  REL_COUNT=$(cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" -d neo4j --format plain "MATCH ()-[r:Friend]->() RETURN count(r) AS count" 2>/dev/null | tail -n1 | tr -d '"' || echo "0")
  # Ensure variables are set (default to 0 if empty)
  NODE_COUNT=${NODE_COUNT:-0}
  REL_COUNT=${REL_COUNT:-0}
  echo "    Neo4j: ${NODE_COUNT} User nodes, ${REL_COUNT} Friend relationships"
  if [[ "$NODE_COUNT" -eq 0 ]]; then
    echo "❌ Neo4j has no User nodes loaded. Cannot proceed with benchmark." >&2
    exit 1
  fi
fi

if [[ "${RUN_FALKOR}" == "1" ]]; then
  echo "  - Checking FalkorDB data..."
  # Extract host/port if not already set
  if [[ -z "${FALKOR_HOST:-}" ]]; then
    FALKOR_HOSTPORT="${FALKOR_ENDPOINT#falkor://}"
    if [[ "$FALKOR_HOSTPORT" == *:* ]]; then
      FALKOR_HOST="${FALKOR_HOSTPORT%%:*}"
      FALKOR_PORT="${FALKOR_HOSTPORT##*:}"
    else
      FALKOR_HOST="$FALKOR_HOSTPORT"
      FALKOR_PORT=6379
    fi
  fi
  # Use db.meta.stats() for efficient metadata retrieval without full graph scan
  # The output is a Redis RESP array where:
  # - Line 8: labels (map like {User: count})
  # - Line 9: relTypes (map like {FRIEND: count})
  # - Line 10: relCount (integer)
  # - Line 11: nodeCount (integer)
  FALKOR_STATS=$(redis-cli -h "$FALKOR_HOST" -p "$FALKOR_PORT" GRAPH.QUERY falkor "CALL db.meta.stats()" 2>/dev/null || echo "")
  # Extract nodeCount from line 11
  FALKOR_NODE_COUNT=$(echo "$FALKOR_STATS" | sed -n '11p' | grep -oE '[0-9]+' || echo "0")
  # Extract relCount from line 10
  FALKOR_REL_COUNT=$(echo "$FALKOR_STATS" | sed -n '10p' | grep -oE '[0-9]+' || echo "0")
  # Ensure variables are set (default to 0 if empty)
  FALKOR_NODE_COUNT=${FALKOR_NODE_COUNT:-0}
  FALKOR_REL_COUNT=${FALKOR_REL_COUNT:-0}
  echo "    FalkorDB: ${FALKOR_NODE_COUNT} User nodes, ${FALKOR_REL_COUNT} Friend relationships (via db.meta.stats)"
  if [[ "$FALKOR_NODE_COUNT" -eq 0 ]]; then
    echo "❌ FalkorDB has no User nodes loaded. Cannot proceed with benchmark." >&2
    exit 1
  fi
fi


if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  echo "  - Checking Memgraph data..."
  MEMGRAPH_NODE_COUNT=$(mgconsole_exec "MATCH (n:User) RETURN count(n) AS count;" 2>/dev/null | grep -oE '[0-9]+' | tail -n1 || echo "0")
  MEMGRAPH_REL_COUNT=$(mgconsole_exec "MATCH ()-[r:Friend]->() RETURN count(r) AS count;" 2>/dev/null | grep -oE '[0-9]+' | tail -n1 || echo "0")
  # Ensure variables are set (default to 0 if empty)
  MEMGRAPH_NODE_COUNT=${MEMGRAPH_NODE_COUNT:-0}
  MEMGRAPH_REL_COUNT=${MEMGRAPH_REL_COUNT:-0}
  echo "    Memgraph: ${MEMGRAPH_NODE_COUNT} User nodes, ${MEMGRAPH_REL_COUNT} Friend relationships"
  if [[ "$MEMGRAPH_NODE_COUNT" -eq 0 ]]; then
    echo "❌ Memgraph has no User nodes loaded. Cannot proceed with benchmark." >&2
    exit 1
  fi
fi

echo "==> Generating vendor-specific query files (base=${QUERIES_FILE_BASE}, dataset=large, count=${QUERIES_COUNT}, write_ratio=${WRITE_RATIO})"
echo "==> Algorithm query toggles (pagerank=${ENABLE_ALGO_PAGERANK_BOOL}, max_flow=${ENABLE_ALGO_MAX_FLOW_BOOL}, msf=${ENABLE_ALGO_MSF_BOOL}, harmonic=${ENABLE_ALGO_HARMONIC_BOOL})"
# Always regenerate so each selected vendor gets the latest query catalog + stable q_id fields.
if [[ "${RUN_FALKOR}" == "1" || "${RUN_FALKOR_2}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor falkor   --dataset large --size "$QUERIES_COUNT" --name "$FALKOR_QUERIES_FILE"   --write-ratio "$WRITE_RATIO" "${ALGO_QUERY_ARGS[@]}"
fi
if [[ "${RUN_NEO4J}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor neo4j   --dataset large --size "$QUERIES_COUNT" --name "$NEO4J_QUERIES_FILE"   --write-ratio "$WRITE_RATIO" "${ALGO_QUERY_ARGS[@]}"
fi
if [[ "${RUN_MEMGRAPH}" == "1" ]]; then
  cargo run --release --bin benchmark -- generate-queries --vendor memgraph --dataset large --size "$QUERIES_COUNT" --name "$MEMGRAPH_QUERIES_FILE" --write-ratio "$WRITE_RATIO" "${ALGO_QUERY_ARGS[@]}"
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
  if [[ "${RUN_FALKOR}" == "1" && "$FREE_PRIMARY_FALKOR_BEFORE_SECOND_RUN_BOOL" == "true" ]]; then
    echo "==> Releasing primary FalkorDB graph memory before secondary loading"
    clear_falkor_graph "FalkorDB" "$FALKOR_HOST" "$FALKOR_PORT"
  fi

  clear_falkor_graph "FalkorDB (secondary)" "$FALKOR_2_HOST" "$FALKOR_2_PORT"

  # Bulk-load large Pokec dataset into secondary FalkorDB only after primary run finishes.
  echo "==> Bulk-loading large Pokec dataset into FalkorDB (secondary) via falkordb-bulk-loader"
  REDIS_URL_2="redis://$FALKOR_2_HOST:$FALKOR_2_PORT"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "❌ python3 not found; required to run falkordb-bulk-loader" >&2
    exit 1
  fi

  PYTHONPATH="../falkordb-bulk-loader" python3 "../falkordb-bulk-loader/falkordb_bulk_loader/bulk_insert.py" falkor \
    -u "$REDIS_URL_2" \
    -n "$CSV_DIR/User.csv" \
    -r "$CSV_DIR/FRIEND.csv" \
    -j INTEGER -s -i User:id -i User:age \
    -c 128 -b 16 -t 16

  echo "==> Configuring FalkorDB (secondary) query timeout (${FALKOR_QUERY_TIMEOUT_MS}ms)"
  set_falkor_query_timeout "FalkorDB (secondary)" "$FALKOR_2_HOST" "$FALKOR_2_PORT"

  echo "==> Validating FalkorDB (secondary) contents before secondary run"
  FALKOR_2_STATS_MAX_RETRIES=${FALKOR_2_STATS_MAX_RETRIES:-10}
  FALKOR_2_STATS_RETRY_DELAY_SECS=${FALKOR_2_STATS_RETRY_DELAY_SECS:-2}
  FALKOR_2_STATS=""
  FALKOR_2_NODE_COUNT=0
  FALKOR_2_REL_COUNT=0

  for ((attempt=1; attempt<=FALKOR_2_STATS_MAX_RETRIES; attempt++)); do
    # Preserve stderr in logs (do not redirect it away); tolerate transient command failures.
    FALKOR_2_STATS=$(redis-cli -h "$FALKOR_2_HOST" -p "$FALKOR_2_PORT" GRAPH.QUERY falkor "CALL db.meta.stats()" || true)
    FALKOR_2_NODE_COUNT=$(echo "$FALKOR_2_STATS" | sed -n '11p' | grep -oE '[0-9]+' || true)
    FALKOR_2_REL_COUNT=$(echo "$FALKOR_2_STATS" | sed -n '10p' | grep -oE '[0-9]+' || true)
    FALKOR_2_NODE_COUNT=${FALKOR_2_NODE_COUNT:-0}
    FALKOR_2_REL_COUNT=${FALKOR_2_REL_COUNT:-0}

    if [[ "$FALKOR_2_NODE_COUNT" -gt 0 ]]; then
      break
    fi

    if [[ "$attempt" -lt "$FALKOR_2_STATS_MAX_RETRIES" ]]; then
      echo "    FalkorDB (secondary) stats not ready yet (attempt ${attempt}/${FALKOR_2_STATS_MAX_RETRIES}); retrying in ${FALKOR_2_STATS_RETRY_DELAY_SECS}s..."
      sleep "$FALKOR_2_STATS_RETRY_DELAY_SECS"
    fi
  done
  echo "    FalkorDB (secondary): ${FALKOR_2_NODE_COUNT} User nodes, ${FALKOR_2_REL_COUNT} Friend relationships (via db.meta.stats)"
  if [[ "$FALKOR_2_NODE_COUNT" -eq 0 ]]; then
    if [[ -n "$FALKOR_2_STATS" ]]; then
      echo "    Last db.meta.stats output from secondary:" >&2
      echo "$FALKOR_2_STATS" >&2
    fi
    echo "❌ FalkorDB (secondary) has no User nodes loaded. Cannot proceed with benchmark." >&2
    exit 1
  fi
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
