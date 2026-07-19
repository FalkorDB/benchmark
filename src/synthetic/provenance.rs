//! Server provenance: read what a client *can* know about the FalkorDB build it measured.
//!
//! FalkorDB does not expose a graph-module git SHA to clients, so we capture the module version
//! (`MODULE LIST` / `INFO modules`) and a handful of `INFO server` fields; the operator supplies
//! the immutable image identity via `--server-image`. See [`crate::synthetic::report::ServerInfo`].

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::synthetic::report::ServerInfo;
use tracing::warn;

/// Query `INFO server` + `MODULE LIST` over a raw redis connection and assemble a [`ServerInfo`].
///
/// `redis_url` is a `redis://host:port` string (convert a `falkor://` endpoint with
/// [`crate::falkor::falkor_driver::falkor_endpoint_to_redis_url`]). `server_image` is recorded
/// verbatim. Failure to read is non-fatal for the caller's decision, but returned as an error here
/// so the caller can log it and continue with a best-effort (partial) `ServerInfo`.
pub async fn collect(
    redis_url: &str,
    server_image: Option<String>,
) -> BenchmarkResult<ServerInfo> {
    let client = redis::Client::open(redis_url)
        .map_err(|e| OtherError(format!("provenance: bad redis url '{}': {}", redis_url, e)))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|e| OtherError(format!("provenance: connect failed: {}", e)))?;

    // Collect the two commands independently so a failure of one still preserves the other's fields.
    let mut info = ServerInfo {
        server_image,
        ..Default::default()
    };

    match redis::cmd("INFO")
        .arg("server")
        .query_async::<String>(&mut conn)
        .await
    {
        Ok(info_server) => {
            info.redis_version = info_field(&info_server, "redis_version");
            info.redis_build_id = info_field(&info_server, "redis_build_id");
            info.redis_git_sha1 = info_field(&info_server, "redis_git_sha1");
            info.run_id = info_field(&info_server, "run_id");
            info.os = info_field(&info_server, "os");
            info.arch_bits = info_field(&info_server, "arch_bits");
        }
        Err(e) => warn!("provenance: INFO server failed: {}", e),
    }

    match redis::cmd("MODULE")
        .arg("LIST")
        .query_async::<redis::Value>(&mut conn)
        .await
    {
        Ok(modules) => info.module_graph_ver = parse_graph_module_version(&modules),
        Err(e) => warn!("provenance: MODULE LIST failed: {}", e),
    }

    Ok(info)
}

/// Extract a `key:value` field from a redis `INFO` text blob.
fn info_field(
    info: &str,
    key: &str,
) -> Option<String> {
    info.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Find the `graph` module's `ver` in a `MODULE LIST` reply.
///
/// The reply is an array of module entries. Depending on RESP2/RESP3 each entry is either a flat
/// array of `[key, value, key, value, …]` or a `Map` of `key → value`. We look for the entry whose
/// `name` is `graph` and return its `ver` as a `u64`.
fn parse_graph_module_version(value: &redis::Value) -> Option<u64> {
    let entries = match value {
        redis::Value::Array(entries) => entries,
        redis::Value::Map(pairs) => {
            // A single module reported directly as a map.
            return module_field_pairs(pairs, "graph");
        }
        _ => return None,
    };
    for entry in entries {
        let (name, ver) = match entry {
            redis::Value::Array(fields) => {
                let mut name = None;
                let mut ver = None;
                let mut i = 0;
                while i + 1 < fields.len() {
                    if let Some(k) = redis_value_as_string(&fields[i]) {
                        match k.as_str() {
                            "name" => name = redis_value_as_string(&fields[i + 1]),
                            "ver" => ver = redis_value_as_u64(&fields[i + 1]),
                            _ => {}
                        }
                    }
                    i += 2;
                }
                (name, ver)
            }
            redis::Value::Map(pairs) => {
                let mut name = None;
                let mut ver = None;
                for (k, v) in pairs {
                    match redis_value_as_string(k).as_deref() {
                        Some("name") => name = redis_value_as_string(v),
                        Some("ver") => ver = redis_value_as_u64(v),
                        _ => {}
                    }
                }
                (name, ver)
            }
            _ => (None, None),
        };
        if name.as_deref() == Some("graph") {
            return ver;
        }
    }
    None
}

/// Extract `ver` from a single module's RESP3 key/value map if its `name` matches.
fn module_field_pairs(
    pairs: &[(redis::Value, redis::Value)],
    want_name: &str,
) -> Option<u64> {
    let mut name = None;
    let mut ver = None;
    for (k, v) in pairs {
        match redis_value_as_string(k).as_deref() {
            Some("name") => name = redis_value_as_string(v),
            Some("ver") => ver = redis_value_as_u64(v),
            _ => {}
        }
    }
    if name.as_deref() == Some(want_name) {
        ver
    } else {
        None
    }
}

fn redis_value_as_string(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        redis::Value::VerbatimString { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn redis_value_as_u64(value: &redis::Value) -> Option<u64> {
    match value {
        redis::Value::Int(i) => u64::try_from(*i).ok(),
        other => redis_value_as_string(other).and_then(|s| s.parse::<u64>().ok()),
    }
}

/// Decode a numeric FalkorDB module version (`major*10000 + minor*100 + patch`) into a dotted
/// string, e.g. `42001` → `"4.20.1"`. Exposed for reporting/tests.
pub fn decode_module_version(ver: u64) -> String {
    let major = ver / 10_000;
    let minor = (ver / 100) % 100;
    let patch = ver % 100;
    format!("{}.{}.{}", major, minor, patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_field_parses_values() {
        let info = "# Server\r\nredis_version:8.6.3\r\nredis_build_id:313de0794c1dcd59\r\nrun_id:abc123\r\n";
        assert_eq!(info_field(info, "redis_version").as_deref(), Some("8.6.3"));
        assert_eq!(
            info_field(info, "redis_build_id").as_deref(),
            Some("313de0794c1dcd59")
        );
        assert_eq!(info_field(info, "run_id").as_deref(), Some("abc123"));
        assert_eq!(info_field(info, "missing"), None);
    }

    #[test]
    fn info_field_ignores_prefix_collisions() {
        // `redis_version` must not be matched by a lookup for `redis_ver`.
        let info = "redis_version:8.6.3\r\n";
        // exact key with colon is required, so a partial key returns None
        assert_eq!(info_field(info, "redis_versio"), None);
    }

    #[test]
    fn parses_graph_module_version() {
        let modules = redis::Value::Array(vec![
            redis::Value::Array(vec![
                redis::Value::BulkString(b"name".to_vec()),
                redis::Value::BulkString(b"graph".to_vec()),
                redis::Value::BulkString(b"ver".to_vec()),
                redis::Value::Int(42001),
            ]),
            redis::Value::Array(vec![
                redis::Value::BulkString(b"name".to_vec()),
                redis::Value::BulkString(b"vectorset".to_vec()),
                redis::Value::BulkString(b"ver".to_vec()),
                redis::Value::Int(1),
            ]),
        ]);
        assert_eq!(parse_graph_module_version(&modules), Some(42001));
    }

    #[test]
    fn graph_version_absent_when_no_graph_module() {
        let modules = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::BulkString(b"name".to_vec()),
            redis::Value::BulkString(b"vectorset".to_vec()),
            redis::Value::BulkString(b"ver".to_vec()),
            redis::Value::Int(1),
        ])]);
        assert_eq!(parse_graph_module_version(&modules), None);
    }

    #[test]
    fn parses_graph_module_version_resp3_map() {
        // RESP3 returns each module entry as a Map.
        let modules = redis::Value::Array(vec![redis::Value::Map(vec![
            (
                redis::Value::BulkString(b"name".to_vec()),
                redis::Value::BulkString(b"graph".to_vec()),
            ),
            (
                redis::Value::BulkString(b"ver".to_vec()),
                redis::Value::Int(42001),
            ),
        ])]);
        assert_eq!(parse_graph_module_version(&modules), Some(42001));
    }

    #[test]
    fn decode_version_examples() {
        assert_eq!(decode_module_version(42001), "4.20.1");
        assert_eq!(decode_module_version(999_999), "99.99.99");
        assert_eq!(decode_module_version(40200), "4.2.0");
    }
}
