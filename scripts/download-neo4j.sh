#!/usr/bin/env bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
DOWNLOADS="$SCRIPT_DIR/../downloads"
NEO4J_VERSION="2026.10.2"
NEO4J_DOWNLOAD_URL="https://dist.neo4j.org/neo4j-community-${NEO4J_VERSION}-unix.tar.gz"
NEO4J_DIR="$DOWNLOADS/neo4j_local"
NEO4J_DATA_DIR="$NEO4J_DIR/neo4j_data"
NEO4J_LOGS_DIR="$NEO4J_DIR/neo4j_logs"
mkdir -p "$DOWNLOADS"
rm -rf "$NEO4J_DIR"
mkdir -p "$NEO4J_DIR" "$NEO4J_DATA_DIR" "$NEO4J_LOGS_DIR"
curl -L "$NEO4J_DOWNLOAD_URL" | tar -xz -C "$NEO4J_DIR" --strip-components=1
echo "dbms.usage_report.enabled=false" >> "$NEO4J_DIR/conf/neo4j.conf"

# Neo4j 2025+ Community requires Java 21 (or 25). Prefer Homebrew openjdk@21.
if [[ -z "${JAVA_HOME:-}" ]] || ! "$JAVA_HOME/bin/java" -version 2>&1 | grep -qE 'version "(21|25)\.'; then
  if [[ -x /opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home/bin/java ]]; then
    export JAVA_HOME="/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home"
  elif command -v brew >/dev/null 2>&1 && [[ -x "$(brew --prefix openjdk@21 2>/dev/null)/libexec/openjdk.jdk/Contents/Home/bin/java" ]]; then
    export JAVA_HOME="$(brew --prefix openjdk@21)/libexec/openjdk.jdk/Contents/Home"
  else
    echo "❌ Neo4j ${NEO4J_VERSION} needs Java 21+. Install with: brew install openjdk@21" >&2
    exit 1
  fi
fi
export PATH="$JAVA_HOME/bin:$PATH"
cat > "$NEO4J_DIR/env.sh" <<EOF
export JAVA_HOME="$JAVA_HOME"
export PATH="\$JAVA_HOME/bin:\$PATH"
EOF

# Match scripts/run_medium_benchmark.sh + run_small_benchmark.sh default
NEO4J_PASSWORD="six666six"
echo "changing neo4j password for user neo4j to $NEO4J_PASSWORD"
echo "$NEO4J_DIR/bin/neo4j-admin dbms set-initial-password $NEO4J_PASSWORD"
$NEO4J_DIR/bin/neo4j-admin dbms set-initial-password $NEO4J_PASSWORD

echo "Installed Neo4j Community ${NEO4J_VERSION} -> $NEO4J_DIR"
echo "source $NEO4J_DIR/env.sh   # Java 21"
echo "run with: $NEO4J_DIR/bin/neo4j start"

