//! Minimal, allocation-light parser for the Pokec dataset's `.cypher` import files.
//!
//! The import files are line based and consist of two phases:
//! - Node lines: `CREATE (:User {id: 1, age: 20, gender: "male", completion_percentage: 75});`
//! - Edge lines: `MATCH (n:User {id: X}), (m:User {id: Y}) CREATE (n)-[e: Friend]->(m);`
//!
//! This mirrors the inline parsing already used by `memgraph_client::execute_pokec_users_import_unwind`,
//! extracted into pure functions so it can be reused by the Postgres loader as well.

#[derive(Debug, Clone, PartialEq)]
pub struct NodeRecord {
    pub id: i32,
    pub completion_percentage: i32,
    pub gender: String,
    pub age: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRecord {
    pub src: i32,
    pub dst: i32,
}

fn extract_map(trimmed: &str) -> Option<&str> {
    let l = trimmed.find('{')?;
    let r = trimmed.rfind('}')?;
    if r > l {
        Some(&trimmed[l..=r])
    } else {
        None
    }
}

/// Parse a Cypher-style property map `{k: v, k2: "v2", ...}` into ordered key/value string pairs.
/// Values are returned as raw strings (quotes stripped for string literals); callers parse
/// further as needed.
fn parse_kv_pairs(map_str: &str) -> Vec<(String, String)> {
    let content = map_str.trim().trim_start_matches('{').trim_end_matches('}');
    let mut out = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let key_start = i;
        while i < chars.len() && chars[i] != ':' {
            i += 1;
        }
        let key = chars[key_start..i].iter().collect::<String>().trim().to_string();
        if i >= chars.len() {
            break;
        }
        i += 1; // skip ':'
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }

        let value: String;
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            let value_start = i;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            value = chars[value_start..i].iter().collect();
            i += 1; // skip closing quote
        } else {
            let value_start = i;
            while i < chars.len() && chars[i] != ',' && chars[i] != '}' {
                i += 1;
            }
            value = chars[value_start..i].iter().collect::<String>().trim().to_string();
        }

        if !key.is_empty() {
            out.push((key, value));
        }
    }

    out
}

/// Parse a `CREATE (:User {...})` line into a `NodeRecord`.
pub fn parse_node_line(trimmed: &str) -> Option<NodeRecord> {
    let map_str = extract_map(trimmed)?;
    let pairs = parse_kv_pairs(map_str);

    let mut id = None;
    let mut completion_percentage = 0i32;
    let mut gender = String::new();
    let mut age = 0i32;

    for (key, value) in pairs {
        match key.as_str() {
            "id" => id = value.parse::<i32>().ok(),
            "completion_percentage" => completion_percentage = value.parse::<i32>().unwrap_or(0),
            "gender" => gender = value,
            "age" => age = value.parse::<i32>().unwrap_or(0),
            _ => {}
        }
    }

    Some(NodeRecord {
        id: id?,
        completion_percentage,
        gender,
        age,
    })
}

/// Scan a line for up to `want` occurrences of `id:` followed by a decimal integer.
pub fn find_all_ids(
    trimmed: &str,
    want: usize,
) -> Vec<i32> {
    let mut ids = Vec::with_capacity(want);
    let mut rest = trimmed;

    while ids.len() < want {
        let Some(pos) = rest.find("id:") else {
            break;
        };
        rest = &rest[pos + 3..];
        let s = rest.trim_start();
        let mut end = 0usize;
        for (i, ch) in s.char_indices() {
            if !ch.is_ascii_digit() {
                end = i;
                break;
            }
        }
        let end = if end == 0 { s.len() } else { end };
        if let Ok(v) = s[..end].parse::<i32>() {
            ids.push(v);
        }
        rest = &s[end..];
    }

    ids
}

/// Parse a `MATCH (n:User {id: X}), (m:User {id: Y}) CREATE ...` line into an `EdgeRecord`.
pub fn parse_edge_line(trimmed: &str) -> Option<EdgeRecord> {
    let ids = find_all_ids(trimmed, 2);
    if ids.len() == 2 {
        Some(EdgeRecord {
            src: ids[0],
            dst: ids[1],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_line() {
        let line = r#"CREATE (:User {id: 1, age: 20, gender: "male", completion_percentage: 75});"#;
        let node = parse_node_line(line).unwrap();
        assert_eq!(node.id, 1);
        assert_eq!(node.age, 20);
        assert_eq!(node.gender, "male");
        assert_eq!(node.completion_percentage, 75);
    }

    #[test]
    fn parses_edge_line() {
        let line = "MATCH (n:User {id: 12}), (m:User {id: 34}) CREATE (n)-[e: Friend]->(m);";
        let edge = parse_edge_line(line).unwrap();
        assert_eq!(edge.src, 12);
        assert_eq!(edge.dst, 34);
    }
}
