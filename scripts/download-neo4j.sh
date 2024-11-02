#!/usr/bin/env bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
DOWNLOADS="$SCRIPT_DIR/../downloads"
NEO4J_VERSION="5.25.1"
NEO4J_DOWNLOAD_URL="https://dist.neo4j.org/neo4j-community-${NEO4J_VERSION}-unix.tar.gz"
NEO4J_DIR="$DOWNLOADS/neo4j_local"
NEO4J_DATA_DIR="$NEO4J_DIR/neo4j_data"
NEO4J_LOGS_DIR="$NEO4J_DIR/neo4j_logs"
mkdir -p "$DOWNLOADS"
rm -rf "$NEO4J_DIR"
mkdir -p "$NEO4J_DIR" "$NEO4J_DATA_DIR" "$NEO4J_LOGS_DIR"
curl -L "$NEO4J_DOWNLOAD_URL" | tar -xz -C "$NEO4J_DIR" --strip-components=1

NEO4J_PASSWORD="h6u4krd10"
echo "changing neo4j password for user neo4j to $NEO4J_PASSWORD"
echo "$NEO4J_DIR/bin/neo4j-admin dbms set-initial-password $NEO4J_PASSWORD"
$NEO4J_DIR/bin/neo4j-admin dbms set-initial-password $NEO4J_PASSWORD

echo "run with: $NEO4J_DIR/bin/neoj4 start"

