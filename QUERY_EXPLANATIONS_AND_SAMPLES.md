# Query Explanations & Full Query Reference
This document references the complete query universe produced by `UsersQueriesRepository` in `src/queries_repository.rs`, including baseline, `extended-core` (a.k.a. `extended_core`), and `fixture-dependent` profile additions.

## Dataset assumptions
- Primary label: `:User`
- Primary relationship: `:Friend`
- Common properties used by queries: `id`, `age`, and `bench_capacity`

## Coverage mode options and inclusion rules
- `baseline`
  - always includes the full baseline core + phase-1 set
  - includes algorithm queries when their `--enable-algo-*` flags are enabled
- `extended-core`
  - baseline + `temporal_spatial_roundtrip` for FalkorDB and Neo4j
  - Memgraph does not currently add `temporal_spatial_roundtrip`
- `fixture-dependent`
  - extended-core + `vector_query_nodes_smoke`, `fulltext_query_nodes_smoke`, `fulltext_query_relationships_smoke`

## Full query list by inclusion group
### Baseline core + phase-1 queries (always possible in every profile)
- `single_vertex_read` (read): point lookup by `User.id`.
- `single_vertex_write` (write): create a single `User` node.
- `single_vertex_update` (write): update one user’s `rpc_social_credit`.
- `single_edge_update` (write): update one existing `Friend` edge.
- `single_edge_write` (write): create/merge a `Friend` edge between two users.
- `aggregate_expansion_1` (read): 1-hop expansion from a seed user.
- `aggregate_expansion_1_with_filter` (read): 1-hop expansion with `age >= 18`.
- `aggregate_expansion_2` (read): 2-hop expansion (`DISTINCT`).
- `aggregate_expansion_2_with_filter` (read): 2-hop expansion with `age >= 18`.
- `aggregate_expansion_3` (read): 3-hop expansion (`DISTINCT`).
- `aggregate_expansion_3_with_filter` (read): 3-hop expansion with `age >= 18`.
- `aggregate_expansion_4` (read): 4-hop expansion (`DISTINCT`).
- `aggregate_expansion_4_with_filter` (read): 4-hop expansion with `age >= 18`.
- `aggregate_age` (read): average age over all users.
- `aggregate_age_distinct` (read): count distinct age values.
- `aggregate_age_filtered` (read): average age for users where `age >= 18`.
- `aggregate_count_users` (read): total user count.
- `aggregate_age_min_max_avg` (read): min/max/avg age in one query.
- `neighbours_2` (read): 2-hop neighbor IDs.
- `neighbours_2_with_filter` (read): 2-hop neighbors filtered by age.
- `neighbours_2_with_data` (read): 2-hop neighbors returning full node records.
- `neighbours_2_with_data_and_filter` (read): 2-hop neighbors with node data + age filter.
- `shortest_path` (read): shortest path length between source and target.
- `shortest_path_with_filter` (read): shortest path length with non-empty path filter.
- `pattern_cycle` (read): 3-node cycle pattern.
- `pattern_long` (read): longer fixed path pattern.
- `pattern_short` (read): shorter fixed path pattern.
- `vertex_on_label_property` (read): label+property lookup (`:User {id: ...}`).
- `vertex_on_label_property_index` (read): same shape, intended for index-path benchmarking.
- `vertex_on_property` (read): property lookup without label predicate.
- `value_join` (read): value-based join on `age`.
- `value_join_cnt` (read): count variant of the value join.
- `order_by_age` (read): full sort by age and id.
- `unwind_rows` (read): row fan-out using `UNWIND`.
- `var_len_friends` (read): variable-length traversal (`*1..2`).
- `optional_friend` (read): optional expansion from a seed user.
- `call_subquery` (read): correlated `CALL { ... }` subquery.
- `id_seek` (read): internal node-id point lookup.
- `id_range_scan` (read): internal node-id range scan.
- `merge_user_insert_path` (write): `MERGE` insert path with `ON CREATE`.
- `merge_user_upsert_existing` (write): `MERGE` upsert with `ON MATCH` updates.
- `merge_friend_edge_upsert` (write): relationship `MERGE` upsert on `Friend`.
- `detach_delete_user` (write): `DETACH DELETE` coverage.
- `remove_user_property_and_label` (write): `REMOVE` property and label.
- `foreach_loop_mutation` (write): write mutation loop via `FOREACH`.
- `union_all_ids` (read): `UNION ALL` composition.
- `union_distinct_ids` (read): `UNION` (distinct) composition.
- `all_shortest_paths_len` (read): `allShortestPaths` / BFS coverage.
- `var_len_with_edge_where_filter` (read): variable-length traversal with edge filtering.
- `exact_5_hop_traverse_count` (read): exact 5-hop traversal count.
- `exact_6_hop_traverse_count` (read): exact 6-hop traversal count.
- `count_users_plain` (read): plain user count.
- `count_friend_edges_plain` (read): plain edge count.
- `indexed_or_predicate` (read): OR predicate index shape.
- `indexed_in_list_predicate` (read): `IN [...]` predicate index shape.
- `entity_path_introspection` (read): path/entity introspection (`labels`, `type`, `properties`, `nodes`, `relationships`, `length`).

### Optional algorithm queries (enabled by default, can be toggled off)
- `algo_pagerank_summary` (read): page-rank score sample.
- `algo_max_flow_single_pair` (read): max-flow between two users using `bench_capacity`.
- `algo_msf_summary` (read): spanning-forest style edge/weight summary.
- `algo_harmonic_summary` (read): harmonic centrality summary stats.

### Extended-core additions
- `temporal_spatial_roundtrip` (read): temporal + spatial scalar-function roundtrip.
  - Added for FalkorDB and Neo4j when profile is `extended-core` or `fixture-dependent`.
  - Not currently generated for Memgraph.

### Fixture-dependent additions
- `vector_query_nodes_smoke` (read): vector-index smoke query over users.
- `fulltext_query_nodes_smoke` (read): node fulltext-index smoke query.
- `fulltext_query_relationships_smoke` (read): relationship fulltext-index smoke query.

## Vendor-specific notes
- `shortest_path`, `shortest_path_with_filter`, and `all_shortest_paths_len` use vendor-specific query text.
- `aggregate_count_users` uses FalkorDB’s `db.meta.stats()` path for the Falkor flavor.
- `temporal_spatial_roundtrip` uses:
  - Neo4j: `point.distance(...)`
  - FalkorDB: `distance(...)`
- Fixture-dependent smoke queries use vendor-specific procedures/index names:
  - FalkorDB: `db.idx.vector.*`, `db.idx.fulltext.*`
  - Neo4j: `db.index.vector.*`, `db.index.fulltext.*`
  - Memgraph: `vector_search.*`, `text_search.*`
- `algo_max_flow_single_pair` in Falkor obtains one relationship type from `db.relationshipTypes()` and passes `relationshipTypes: [relationshipType]`.
## Actual Cypher templates (complete)
This section contains actual query templates for every supported query ID in `src/queries_repository.rs`.

### Baseline core + phase-1 templates (shared across vendors)
```cypher
// single_vertex_read
MATCH (n:User {id : $id}) RETURN n

// single_vertex_write
CREATE (n:User {id : $id}) RETURN n

// single_vertex_update
MATCH (n:User {id: $id}) SET n.rpc_social_credit = $rpc_social_credit RETURN n

// single_edge_update
MATCH (n:User)-[e:Friend]->(m:User) WITH n, m, e ORDER BY rand() LIMIT 1 SET e.color = $color, e.bench_capacity = coalesce(e.bench_capacity, 1 + ((n.id * 31 + m.id * 17) % 20)) RETURN e

// single_edge_write
MATCH (n:User {id: $from}), (m:User {id: $to}) MERGE (n)-[e:Friend]->(m) ON CREATE SET e.bench_capacity = 1 + ((n.id * 31 + m.id * 17) % 20) ON MATCH SET e.bench_capacity = coalesce(e.bench_capacity, 1 + ((n.id * 31 + m.id * 17) % 20)), e.touch = date() RETURN e

// aggregate_expansion_1
MATCH (s:User {id: $id})-->(n:User) RETURN n.id

// aggregate_expansion_1_with_filter
MATCH (s:User {id: $id})-->(n:User) WHERE n.age >= 18 RETURN n.id

// aggregate_expansion_2
MATCH (s:User {id: $id})-->()-->(n:User) RETURN DISTINCT n.id

// aggregate_expansion_2_with_filter
MATCH (s:User {id: $id})-->()-->(n:User) WHERE n.age >= 18 RETURN DISTINCT n.id

// aggregate_expansion_3
MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id

// aggregate_expansion_3_with_filter
MATCH (s:User {id: $id})-->()-->()-->(n:User) WHERE n.age >= 18 RETURN DISTINCT n.id

// aggregate_expansion_4
MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id

// aggregate_expansion_4_with_filter
MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) WHERE n.age >= 18 RETURN DISTINCT n.id

// aggregate_age
MATCH (n:User) RETURN avg(n.age) AS avg_age

// aggregate_age_distinct
MATCH (n:User) RETURN count(DISTINCT n.age) AS distinct_ages

// aggregate_age_filtered
MATCH (n:User) WHERE n.age >= 18 RETURN avg(n.age) AS avg_age

// aggregate_age_min_max_avg
MATCH (n:User) RETURN min(n.age) AS min_age, max(n.age) AS max_age, avg(n.age) AS avg_age

// neighbours_2
MATCH (s:User {id: $id})-->()-->(n:User) RETURN n.id

// neighbours_2_with_filter
MATCH (s:User {id: $id})-->()-->(n:User) WHERE n.age >= 18 RETURN n.id

// neighbours_2_with_data
MATCH (s:User {id: $id})-->()-->(n:User) RETURN n

// neighbours_2_with_data_and_filter
MATCH (s:User {id: $id})-->()-->(n:User) WHERE n.age >= 18 RETURN n

// pattern_cycle
MATCH (a:User {id: $id})-->(b:User)-->(c:User)-->(a) RETURN a.id, b.id, c.id

// pattern_long
MATCH (a:User {id: $id})-->()-->()-->()-->(b:User) RETURN a.id, b.id

// pattern_short
MATCH (a:User {id: $id})-->()-->(b:User) RETURN a.id, b.id

// vertex_on_label_property
MATCH (n:User {id: $id}) RETURN n

// vertex_on_label_property_index
MATCH (n:User {id: $id}) RETURN n

// vertex_on_property
MATCH (n {id: $id}) RETURN n

// value_join
MATCH (a:User {id: $id}), (b:User) WHERE a.age = b.age RETURN b.id

// value_join_cnt
MATCH (a:User {id: $id}), (b:User) WHERE a.age = b.age RETURN count(b)

// order_by_age
MATCH (n:User) RETURN n.id, n.age ORDER BY n.age, n.id

// unwind_rows
MATCH (n:User {id: $id}) UNWIND [n.id, n.id + 1, n.id + 2] AS x RETURN x

// var_len_friends
MATCH (a:User {id: $id})-[*1..2]->(b:User) RETURN b.id

// optional_friend
MATCH (a:User {id: $id}) OPTIONAL MATCH (a)-->(b:User) RETURN a.id, b.id

// call_subquery
MATCH (a:User {id: $id}) CALL { WITH a MATCH (a)-->(b:User) RETURN b.id AS bid } RETURN bid

// id_seek
MATCH (n) WHERE id(n) = $id RETURN n.id

// id_range_scan
MATCH (n) WHERE id(n) >= $start AND id(n) < $end RETURN n.id

// merge_user_insert_path
MERGE (u:User {id: $id}) ON CREATE SET u.created_at = timestamp(), u.age = $age RETURN u.id

// merge_user_upsert_existing
MERGE (u:User {id: $id}) ON CREATE SET u.created_at = timestamp() ON MATCH SET u.age = $age, u.last_seen = timestamp() RETURN u.id

// merge_friend_edge_upsert
MATCH (a:User {id: $from}), (b:User {id: $to}) MERGE (a)-[r:Friend]->(b) ON CREATE SET r.since = date(), r.bench_capacity = 1 + ((a.id * 31 + b.id * 17) % 20) ON MATCH SET r.touch = date(), r.bench_capacity = coalesce(r.bench_capacity, 1 + ((a.id * 31 + b.id * 17) % 20)) RETURN id(r)

// detach_delete_user
MATCH (u:User {id: $id}) DETACH DELETE u

// remove_user_property_and_label
MATCH (u:User {id: $id}) REMOVE u.rpc_social_credit, u:TemporaryLabel RETURN u.id

// foreach_loop_mutation
MATCH (u:User {id: $id}) FOREACH (x IN [1,2,3] | SET u.loop_counter = x) RETURN u.loop_counter

// union_all_ids
MATCH (u:User {id: $id}) RETURN u.id AS uid UNION ALL MATCH (v:User) WHERE v.id < 10 RETURN v.id AS uid

// union_distinct_ids
MATCH (u:User {id: $id}) RETURN u.id AS uid UNION MATCH (v:User {id: $id}) RETURN v.id AS uid

// exact_5_hop_traverse_count
MATCH (s:User {id: $id})-[:Friend*5..5]->(t:User) RETURN count(t) AS cnt

// exact_6_hop_traverse_count
MATCH (s:User {id: $id})-[:Friend*6..6]->(t:User) RETURN count(t) AS cnt

// count_users_plain
MATCH (u:User) RETURN count(u) AS cnt

// count_friend_edges_plain
MATCH ()-[r:Friend]->() RETURN count(r) AS cnt

// indexed_or_predicate
MATCH (u:User) WHERE u.id = $id1 OR u.id = $id2 RETURN u.id

// indexed_in_list_predicate
MATCH (u:User) WHERE u.id IN [$id1, $id2, $id3, $id4] RETURN u.id

// entity_path_introspection
MATCH p=(a:User {id: $id})-[r:Friend]->(b:User) RETURN labels(a), type(r), properties(a), nodes(p), relationships(p), length(p) LIMIT 1
```

### Baseline core + phase-1 templates (vendor-specific)
```cypher
// aggregate_count_users (FalkorDB)
CALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt

// aggregate_count_users (Neo4j, Memgraph)
MATCH (n:User) RETURN count(n) AS cnt

// shortest_path (FalkorDB)
MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p RETURN length(p)

// shortest_path (Neo4j)
MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) RETURN length(p)

// shortest_path (Memgraph)
MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) RETURN length(p)

// shortest_path_with_filter (FalkorDB)
MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p WHERE length(p) > 0 RETURN length(p)

// shortest_path_with_filter (Neo4j)
MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) WHERE length(p) > 0 RETURN length(p)

// shortest_path_with_filter (Memgraph)
MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) WHERE length(p) > 0 RETURN length(p)

// all_shortest_paths_len (FalkorDB)
MATCH (s:User {id: $from}), (t:User {id: $to}) WITH s, t MATCH p = allShortestPaths((s)-[:Friend*1..4]->(t)) RETURN length(p)

// all_shortest_paths_len (Neo4j)
MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = allShortestPaths((s)-[:Friend*1..4]->(t)) RETURN length(p)

// all_shortest_paths_len (Memgraph)
MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) RETURN length(p)

// var_len_with_edge_where_filter (FalkorDB)
MATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User) WHERE r.bench_capacity >= $min_capacity RETURN count(t)

// var_len_with_edge_where_filter (Neo4j, Memgraph)
MATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User) WHERE all(rel IN r WHERE rel.bench_capacity >= $min_capacity) RETURN count(t)
```

### Optional algorithm templates (vendor-specific)
```cypher
// algo_pagerank_summary (FalkorDB)
CALL algo.pageRank('User', null) YIELD node, score RETURN score LIMIT 1

// algo_pagerank_summary (Neo4j)
CALL gds.pageRank.stream('benchmark_algo_graph') YIELD nodeId, score RETURN score LIMIT 1

// algo_pagerank_summary (Memgraph)
CALL pagerank.get() YIELD node, rank RETURN rank AS score LIMIT 1

// algo_max_flow_single_pair (FalkorDB)
MATCH (s:User {id: $source_id}), (t:User {id: $target_id})
CALL db.relationshipTypes() YIELD relationshipType
WITH s, t, relationshipType
ORDER BY relationshipType
LIMIT 1
CALL algo.maxFlow({
  sourceNodes: [s],
  targetNodes: [t],
  relationshipTypes: [relationshipType],
  capacityProperty: 'bench_capacity'
})
YIELD maxFlow
RETURN coalesce(toFloat(maxFlow), 0.0) AS max_flow

// algo_max_flow_single_pair (Neo4j)
MATCH (s:User {id: $source_id}), (t:User {id: $target_id})
CALL gds.maxFlow.stats('benchmark_algo_graph', {
  sourceNodes: [s],
  targetNodes: [t],
  capacityProperty: 'bench_capacity'
})
YIELD maxFlow
RETURN coalesce(toFloat(maxFlow), 0.0) AS max_flow

// algo_max_flow_single_pair (Memgraph)
MATCH (s:User {id: $source_id}), (t:User {id: $target_id})
CALL max_flow.get_flow(s, t, 'bench_capacity')
YIELD max_flow
RETURN coalesce(toFloat(max_flow), 0.0) AS max_flow

// algo_msf_summary (FalkorDB)
CALL algo.MSF({
  weightAttribute: 'bench_capacity'
})
YIELD edges
RETURN
  size(edges) AS edge_count,
  reduce(total = 0.0, edge IN edges | total + coalesce(toFloat(edge.bench_capacity), 0.0)) AS total_weight

// algo_msf_summary (Neo4j)
MATCH (source:User {id: $source_id})
CALL gds.spanningTree.stats('benchmark_algo_graph', {
  sourceNode: source,
  relationshipWeightProperty: 'bench_capacity'
})
YIELD effectiveNodeCount, totalWeight
RETURN
  CASE WHEN effectiveNodeCount > 0 THEN effectiveNodeCount - 1 ELSE 0 END AS edge_count,
  coalesce(totalWeight, 0.0) AS total_weight

// algo_msf_summary (Memgraph)
CALL igraphalg.spanning_tree('bench_capacity', false)
YIELD tree
RETURN
  size(tree) AS edge_count,
  0.0 AS total_weight

// algo_harmonic_summary (FalkorDB)
CALL algo.HarmonicCentrality()
YIELD node, score
RETURN count(node) AS node_count, avg(score) AS avg_score, max(score) AS max_score

// algo_harmonic_summary (Neo4j)
CALL gds.closeness.harmonic.stream('benchmark_algo_graph')
YIELD nodeId, score
RETURN count(nodeId) AS node_count, avg(score) AS avg_score, max(score) AS max_score

// algo_harmonic_summary (Memgraph)
CALL nxalg.harmonic_centrality()
YIELD node, harmonic_centrality
RETURN
  count(node) AS node_count,
  avg(harmonic_centrality) AS avg_score,
  max(harmonic_centrality) AS max_score
```

### Extended-core template
```cypher
// temporal_spatial_roundtrip (FalkorDB)
RETURN
  date('2024-01-01') AS d,
  localtime('12:30:00') AS t,
  duration('P2DT3H') AS dur,
  distance(
    point({latitude: 32.1, longitude: 34.8}),
    point({latitude: 32.2, longitude: 34.9})
  ) AS dist

// temporal_spatial_roundtrip (Neo4j)
RETURN
  date('2024-01-01') AS d,
  localtime('12:30:00') AS t,
  duration('P2DT3H') AS dur,
  point.distance(
    point({latitude: 32.1, longitude: 34.8}),
    point({latitude: 32.2, longitude: 34.9})
  ) AS dist
```

### Fixture-dependent templates (vendor-specific)
```cypher
// vector_query_nodes_smoke (FalkorDB)
CALL db.idx.vector.queryNodes('User', 'embedding', 10, vecf32([0.1, 0.2, 0.3]))
YIELD node, score
RETURN id(node), score
LIMIT 10

// vector_query_nodes_smoke (Neo4j)
CALL db.index.vector.queryNodes('bench_user_embedding_idx', 10, [0.1, 0.2, 0.3])
YIELD node, score
RETURN id(node), score
LIMIT 10

// vector_query_nodes_smoke (Memgraph)
CALL vector_search.search('bench_user_embedding_idx', 10, [0.1, 0.2, 0.3])
YIELD node, similarity
RETURN id(node), similarity AS score
LIMIT 10

// fulltext_query_nodes_smoke (FalkorDB)
CALL db.idx.fulltext.queryNodes('User', 'fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10

// fulltext_query_nodes_smoke (Neo4j)
CALL db.index.fulltext.queryNodes('bench_user_ft_idx', 'fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10

// fulltext_query_nodes_smoke (Memgraph)
CALL text_search.search('bench_user_ft_idx', 'data.ft_text:fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10

// fulltext_query_relationships_smoke (FalkorDB)
CALL db.idx.fulltext.queryRelationships('Friend', 'fixture_blue')
YIELD relationship, score
RETURN id(relationship), score
LIMIT 10

// fulltext_query_relationships_smoke (Neo4j)
CALL db.index.fulltext.queryRelationships('bench_friend_ft_idx', 'fixture_blue')
YIELD relationship, score
RETURN id(relationship), score
LIMIT 10

// fulltext_query_relationships_smoke (Memgraph)
CALL text_search.search_edges('bench_friend_ft_idx', 'data.ft_text:fixture_blue')
YIELD edge, score
RETURN id(edge), score
LIMIT 10
```

## Postgres support
Postgres is modeled as a plain relational schema (`users`, `friend_edges` tables), not Apache
AGE, so it has its own SQL query catalog in `src/postgres_queries_repository.rs` rather than
reusing `src/queries_repository.rs`. There is no `Flavour` concept (single SQL dialect).

### Supported coverage profiles
- `baseline` (default): the full set of baseline/phase-1 families that have a SQL translation.
- `extended-core`: baseline + `exact_5_hop_traverse_count`, `exact_6_hop_traverse_count`, `temporal_spatial_roundtrip`.
- `fixture-dependent`: **not supported**. `--vendor postgres --query-profile fixture-dependent` is
  rejected at startup because Postgres has no vector/fulltext index equivalent for the smoke queries.

### Families excluded from every Postgres profile
These have no SQL equivalent and are always omitted from the Postgres catalog, regardless of profile:
- `algo_pagerank_summary`, `algo_max_flow_single_pair`, `algo_msf_summary`, `algo_harmonic_summary` (no built-in graph-algorithm procedures)
- `vector_query_nodes_smoke`, `fulltext_query_nodes_smoke`, `fulltext_query_relationships_smoke` (no vector/fulltext index setup)
- `entity_path_introspection` (no `labels`/`type`/`nodes`/`relationships`/`length` path-introspection equivalent)

### Postgres-only families
These are implemented for Postgres via recursive CTEs but have no equivalent in the planned
MongoDB `$graphLookup`-based engine, so they are Postgres-only for cross-engine parity purposes:
`shortest_path`, `shortest_path_with_filter`, `all_shortest_paths_len`, `pattern_cycle`.

### Postgres SQL templates (baseline)
```sql
-- single_vertex_read
SELECT * FROM users WHERE id = $1

-- single_vertex_write
INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING RETURNING id

-- single_vertex_update
UPDATE users SET rpc_social_credit = $2 WHERE id = $1 RETURNING id

-- single_edge_update
UPDATE friend_edges SET color = $1,
  bench_capacity = COALESCE(bench_capacity, 1 + ((src_id * 31 + dst_id * 17) % 20))
WHERE (src_id, dst_id) = (
  SELECT src_id, dst_id FROM friend_edges ORDER BY random() LIMIT 1
) RETURNING src_id, dst_id

-- single_edge_write
INSERT INTO friend_edges (src_id, dst_id, bench_capacity)
VALUES ($1, $2, 1 + (($1 * 31 + $2 * 17) % 20))
ON CONFLICT (src_id, dst_id) DO UPDATE SET
  bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20)),
  touch = CURRENT_DATE
RETURNING src_id, dst_id

-- aggregate_expansion_1
SELECT dst_id AS id FROM friend_edges WHERE src_id = $1

-- aggregate_expansion_1_with_filter
SELECT fe.dst_id AS id FROM friend_edges fe
JOIN users u ON u.id = fe.dst_id
WHERE fe.src_id = $1 AND u.age >= 18

-- aggregate_expansion_2 / neighbours_2 (identical shape)
SELECT DISTINCT fe2.dst_id AS id FROM friend_edges fe1
JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id
WHERE fe1.src_id = $1

-- aggregate_expansion_2_with_filter / neighbours_2_with_filter
SELECT fe2.dst_id AS id FROM friend_edges fe1
JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id
JOIN users u ON u.id = fe2.dst_id
WHERE fe1.src_id = $1 AND u.age >= 18

-- aggregate_expansion_3 (aggregate_expansion_4 extends with one more join)
SELECT DISTINCT fe3.dst_id AS id FROM friend_edges fe1
JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id
JOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id
WHERE fe1.src_id = $1

-- aggregate_age / aggregate_age_filtered / aggregate_age_distinct / aggregate_age_min_max_avg
SELECT avg(age) AS avg_age FROM users
SELECT avg(age) AS avg_age FROM users WHERE age >= 18
SELECT count(DISTINCT age) AS distinct_ages FROM users
SELECT min(age) AS min_age, max(age) AS max_age, avg(age) AS avg_age FROM users

-- aggregate_count_users / count_users_plain
SELECT count(*) AS cnt FROM users

-- neighbours_2_with_data / neighbours_2_with_data_and_filter
SELECT u.* FROM friend_edges fe1
JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id
JOIN users u ON u.id = fe2.dst_id
WHERE fe1.src_id = $1 [AND u.age >= 18]

-- shortest_path (Postgres-only)
-- Bounded BFS via recursive CTE; UNION dedupes (id, depth) so the frontier
-- doesn't grow combinatorially across reconverging paths.
WITH RECURSIVE bfs(id, depth) AS (
  SELECT $1::int, 0
  UNION
  SELECT fe.dst_id, bfs.depth + 1
  FROM bfs JOIN friend_edges fe ON fe.src_id = bfs.id
  WHERE bfs.depth < 15
)
SELECT min(depth) AS length FROM bfs WHERE id = $2

-- shortest_path_with_filter (Postgres-only)
-- Same as shortest_path, with HAVING min(depth) > 0

-- pattern_cycle (Postgres-only)
SELECT e1.src_id AS a_id, e1.dst_id AS b_id, e2.dst_id AS c_id
FROM friend_edges e1
JOIN friend_edges e2 ON e2.src_id = e1.dst_id
JOIN friend_edges e3 ON e3.src_id = e2.dst_id AND e3.dst_id = e1.src_id
WHERE e1.src_id = $1

-- pattern_long / pattern_short
SELECT $1::int AS a_id, e4.dst_id AS b_id FROM friend_edges e1
JOIN friend_edges e2 ON e2.src_id = e1.dst_id
JOIN friend_edges e3 ON e3.src_id = e2.dst_id
JOIN friend_edges e4 ON e4.src_id = e3.dst_id
WHERE e1.src_id = $1

-- vertex_on_label_property / vertex_on_label_property_index / vertex_on_property / id_seek
SELECT * FROM users WHERE id = $1

-- value_join / value_join_cnt
SELECT b.id FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1
SELECT count(b.id) AS cnt FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1

-- order_by_age
SELECT id, age FROM users ORDER BY age, id

-- unwind_rows
SELECT x FROM users u
CROSS JOIN LATERAL (VALUES (u.id), (u.id + 1), (u.id + 2)) AS t(x)
WHERE u.id = $1

-- var_len_friends
WITH RECURSIVE vlf(id, depth) AS (
  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1
  UNION
  SELECT fe.dst_id, vlf.depth + 1
  FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id
  WHERE vlf.depth < 2
)
SELECT DISTINCT id FROM vlf

-- optional_friend
SELECT t.a AS a_id, fe.dst_id AS b_id
FROM (SELECT $1::int AS a) t
LEFT JOIN friend_edges fe ON fe.src_id = t.a

-- call_subquery
SELECT sub.bid FROM (SELECT $1::int AS a) t,
LATERAL (SELECT dst_id AS bid FROM friend_edges WHERE src_id = t.a) sub

-- id_range_scan
SELECT id FROM users WHERE id >= $1 AND id < $2

-- merge_user_insert_path
INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2)
ON CONFLICT (id) DO NOTHING RETURNING id

-- merge_user_upsert_existing
INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2)
ON CONFLICT (id) DO UPDATE SET age = EXCLUDED.age, last_seen = now()
RETURNING id

-- merge_friend_edge_upsert
INSERT INTO friend_edges (src_id, dst_id, since, bench_capacity)
VALUES ($1, $2, CURRENT_DATE, 1 + (($1 * 31 + $2 * 17) % 20))
ON CONFLICT (src_id, dst_id) DO UPDATE SET
  touch = CURRENT_DATE,
  bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20))
RETURNING src_id, dst_id

-- detach_delete_user
-- friend_edges has ON DELETE CASCADE on both FKs, matching DETACH DELETE semantics.
DELETE FROM users WHERE id = $1

-- remove_user_property_and_label
-- Postgres has no label concept; this drops only the property-removal semantics.
UPDATE users SET rpc_social_credit = NULL WHERE id = $1 RETURNING id

-- foreach_loop_mutation
-- Approximated as a single terminal assignment (equivalent end state to the
-- Cypher FOREACH (x IN [1,2,3] | SET u.loop_counter = x)).
UPDATE users SET loop_counter = 3 WHERE id = $1 RETURNING loop_counter

-- union_all_ids
SELECT id AS uid FROM users WHERE id = $1
UNION ALL SELECT id AS uid FROM users WHERE id < 10

-- union_distinct_ids
SELECT id AS uid FROM users WHERE id = $1
UNION SELECT id AS uid FROM users WHERE id = $1

-- all_shortest_paths_len (Postgres-only)
-- Bounded (depth <= 4) path-array recursive CTE with explicit cycle avoidance.
WITH RECURSIVE paths(id, depth, path) AS (
  SELECT $1::int, 0, ARRAY[$1::int]
  UNION ALL
  SELECT fe.dst_id, p.depth + 1, p.path || fe.dst_id
  FROM paths p JOIN friend_edges fe ON fe.src_id = p.id
  WHERE p.depth < 4 AND NOT (fe.dst_id = ANY(p.path))
)
SELECT min(depth) AS length FROM paths WHERE id = $2

-- var_len_with_edge_where_filter
WITH RECURSIVE vlf(id, depth) AS (
  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 AND bench_capacity >= $2
  UNION
  SELECT fe.dst_id, vlf.depth + 1
  FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id
  WHERE vlf.depth < 3 AND fe.bench_capacity >= $2
)
SELECT count(DISTINCT id) AS cnt FROM vlf

-- count_friend_edges_plain
SELECT count(*) AS cnt FROM friend_edges

-- indexed_or_predicate
SELECT id FROM users WHERE id = $1 OR id = $2

-- indexed_in_list_predicate
SELECT id FROM users WHERE id IN ($1, $2, $3, $4)
```

### Postgres SQL templates (extended-core additions)
```sql
-- exact_5_hop_traverse_count (exact_6_hop_traverse_count uses depth 6)
WITH RECURSIVE hops(id, depth) AS (
  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1
  UNION
  SELECT fe.dst_id, hops.depth + 1
  FROM hops JOIN friend_edges fe ON fe.src_id = hops.id
  WHERE hops.depth < 5
)
SELECT count(*) AS cnt FROM hops WHERE depth = 5

-- temporal_spatial_roundtrip
-- No PostGIS dependency: distance is computed manually via the spherical law of cosines.
SELECT
  DATE '2024-01-01' AS d,
  TIME '12:30:00' AS t,
  INTERVAL '2 days 3 hours' AS dur,
  (6371000 * acos(
    cos(radians(32.1)) * cos(radians(32.2)) * cos(radians(34.9) - radians(34.8))
    + sin(radians(32.1)) * sin(radians(32.2))
  )) AS dist
```

## Mongo support
Mongo is modeled as two collections, `users` and `friend_edges` (edge documents with `src`/`dst`
fields), traversed with `$graphLookup`. Its own aggregation-pipeline catalog lives in
`src/mongo_queries_repository.rs` rather than reusing `src/queries_repository.rs`.

### Supported coverage profiles
- `baseline` (default): the full set of baseline/phase-1 families that have an aggregation-pipeline translation.
- `extended-core`: baseline + `exact_5_hop_traverse_count`, `exact_6_hop_traverse_count`, `temporal_spatial_roundtrip` (requires MongoDB 6.0+ for the `$documents` stage).
- `fixture-dependent`: **not supported**. `--vendor mongo --query-profile fixture-dependent` is
  rejected at startup because Mongo has no vector/fulltext index equivalent for the smoke queries.

### Families excluded from every Mongo profile
Same exclusions as Postgres (no aggregation-pipeline equivalent):
- `algo_pagerank_summary`, `algo_max_flow_single_pair`, `algo_msf_summary`, `algo_harmonic_summary`
- `vector_query_nodes_smoke`, `fulltext_query_nodes_smoke`, `fulltext_query_relationships_smoke`
- `entity_path_introspection`

### Additional families excluded (Postgres-only)
`$graphLookup` returns an unordered, deduplicated reachable set per document with a first-seen
`depthField` (BFS-like); this is usable for reachability/hop-count queries but cannot enumerate
distinct paths or verify a full cyclic pattern's intermediate nodes, so these remain Postgres-only:
`shortest_path`, `shortest_path_with_filter`, `all_shortest_paths_len`, `pattern_cycle`.

### Mongo aggregation-pipeline templates (baseline, representative subset)
```js
// single_vertex_read
db.users.findOne({ _id: id })

// single_vertex_write (approximated as MERGE ... ON CREATE, no-op if already present)
db.users.updateOne({ _id: id }, { $setOnInsert: { created_at: now } }, { upsert: true })

// single_vertex_update
db.users.updateOne({ _id: id }, { $set: { rpc_social_credit: value } })

// single_edge_update ($sample + $merge is the standard Mongo idiom for "update a random doc")
db.friend_edges.aggregate([
  { $sample: { size: 1 } },
  { $set: { color: value } },
  { $merge: { into: "friend_edges", whenMatched: "merge", whenNotMatched: "discard" } },
])

// single_edge_write
db.friend_edges.updateOne(
  { src: from, dst: to },
  { $setOnInsert: { bench_capacity: capacity }, $set: { touch: now } },
  { upsert: true }
)

// aggregate_expansion_N[_with_filter] / neighbours_2[_with_filter]
// Follow src -> dst edges via $graphLookup for (N-1) recursive hops, then filter to exactly
// that depth (depth 0 == direct out-edges of the seed).
db.users.aggregate([
  { $match: { _id: seed } },
  { $graphLookup: {
      from: "friend_edges", startWith: "$_id",
      connectFromField: "dst", connectToField: "src",
      as: "reachable", maxDepth: hops - 1, depthField: "depth",
  } },
  { $unwind: "$reachable" },
  { $match: { "reachable.depth": hops - 1 } },
  // with_filter variant joins back to users and filters age >= 18:
  { $lookup: { from: "users", localField: "reachable.dst", foreignField: "_id", as: "u" } },
  { $unwind: "$u" },
  { $match: { "u.age": { $gte: 18 } } },
  { $project: { _id: "$u._id" } },
])

// value_join / value_join_cnt
db.users.aggregate([
  { $match: { _id: seed } },
  { $lookup: {
      from: "users", let: { age: "$age" },
      pipeline: [ { $match: { $expr: { $eq: ["$age", "$$age"] } } } ],
      as: "matches",
  } },
  { $unwind: "$matches" },
  { $project: { _id: "$matches._id" } },
])

// union_all_ids / union_distinct_ids (via $unionWith; distinct adds a trailing $group)
db.users.aggregate([
  { $match: { _id: seed } },
  { $project: { uid: "$_id" } },
  { $unionWith: { coll: "users", pipeline: [
      { $match: { _id: { $lt: 10 } } },
      { $project: { uid: "$_id" } },
  ] } },
])

// var_len_with_edge_where_filter (restrictSearchWithMatch filters the traversal itself)
db.users.aggregate([
  { $match: { _id: seed } },
  { $graphLookup: {
      from: "friend_edges", startWith: "$_id",
      connectFromField: "dst", connectToField: "src",
      as: "reachable", maxDepth: 2, depthField: "depth",
      restrictSearchWithMatch: { bench_capacity: { $gte: min_capacity } },
  } },
  { $unwind: "$reachable" },
  { $group: { _id: "$reachable.dst" } },
  { $count: "cnt" },
])

// indexed_or_predicate / indexed_in_list_predicate
db.users.find({ $or: [ { _id: id1 }, { _id: id2 } ] })
db.users.find({ _id: { $in: [id1, id2, id3, id4] } })
```

### Mongo extended-core template
```js
// temporal_spatial_roundtrip
// No $geoNear/PostGIS dependency: distance via the spherical law of cosines using Mongo's
// trigonometry aggregation operators (MongoDB 4.2+); $documents (MongoDB 6.0+) seeds a single
// input row without touching a real collection.
db.users.aggregate([
  { $documents: [ {} ] },
  { $project: {
      d: { $dateFromString: { dateString: "2024-01-01" } },
      dur_hours: 51,
      dist: { $multiply: [ 6371000, { $acos: { $add: [
        { $multiply: [
            { $cos: { $degreesToRadians: 32.1 } },
            { $cos: { $degreesToRadians: 32.2 } },
            { $cos: { $subtract: [ { $degreesToRadians: 34.9 }, { $degreesToRadians: 34.8 } ] } },
        ] },
        { $multiply: [ { $sin: { $degreesToRadians: 32.1 } }, { $sin: { $degreesToRadians: 32.2 } } ] },
      ] } } ] },
  } },
])
```

### Mongo-specific limitations (documented, not fixed this phase)
- `detach_delete_user`: Mongo has no FK/cascade concept; unlike Postgres's `ON DELETE CASCADE`,
  deleting a user document does not remove `friend_edges` documents that reference it.
- `remove_user_property_and_label` / `single_vertex_write` / `foreach_loop_mutation`: same
  approximations as documented for Postgres (no label concept; single terminal assignment).

## Reference source
- Canonical query definitions: `src/queries_repository.rs`
- Postgres query definitions: `src/postgres_queries_repository.rs`
- Mongo query definitions: `src/mongo_queries_repository.rs`
- CLI profile/toggle options: `src/cli.rs` (`--query-profile` and `--enable-algo-*`)
