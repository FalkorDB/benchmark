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

## Reference source
- Canonical query definitions: `src/queries_repository.rs`
- CLI profile/toggle options: `src/cli.rs` (`--query-profile` and `--enable-algo-*`)
