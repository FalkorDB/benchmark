# Query Explanations & Samples
This document describes the benchmark query catalog and highlights the phase-1 additions introduced to improve Cypher coverage without requiring dataset fixture changes.

## Dataset assumptions
- Primary label: `:User`
- Primary relationship: `:Friend`
- Common properties used by queries: `id`, `age`, and `bench_capacity`

## Core workload groups
- CRUD and point updates:
  - `single_vertex_read`
  - `single_vertex_write`
  - `single_vertex_update`
  - `single_edge_write`
  - `single_edge_update`
- Expansion and aggregation:
  - `aggregate_expansion_*`
  - `aggregate_age*`
  - `aggregate_count_users`
- Pattern/index/join probes:
  - `pattern_*`
  - `vertex_on_*`
  - `value_join`, `value_join_cnt`
  - `order_by_age`, `unwind_rows`
- Traversal and path probes:
  - `shortest_path`, `shortest_path_with_filter`
  - `var_len_friends`
  - `optional_friend`, `call_subquery`
  - `id_seek`, `id_range_scan`
- Algorithm probes:
  - `algo_pagerank_summary`
  - `algo_max_flow_single_pair`
  - `algo_msf_summary`
  - `algo_harmonic_summary`

## Phase-1 additions
The following queries were added as phase-1 coverage and are now part of the generated workload catalog.

### Write-clause coverage
- `merge_user_insert_path`: MERGE create-path coverage using `ON CREATE`.
- `merge_user_upsert_existing`: MERGE match-path coverage using `ON MATCH`.
- `merge_friend_edge_upsert`: relationship MERGE with create/match updates.
- `detach_delete_user`: `DETACH DELETE` clause coverage.
- `remove_user_property_and_label`: `REMOVE` property and label coverage.
- `foreach_loop_mutation`: `FOREACH` mutation-loop coverage.

### Composition and traversal depth coverage
- `union_all_ids`: `UNION ALL` semantics.
- `union_distinct_ids`: `UNION` (distinct) semantics.
- `all_shortest_paths_len`: `allShortestPaths` planning/runtime coverage with vendor-specific forms.
- `var_len_with_edge_where_filter`: variable-length traversal with edge-filter predicates.
- `exact_5_hop_traverse_count`: fixed 5-hop traversal depth.
- `exact_6_hop_traverse_count`: fixed 6-hop traversal depth.

### Optimizer and function/value coverage
- `count_users_plain`: node count reduction path.
- `count_friend_edges_plain`: relationship count reduction path.
- `indexed_or_predicate`: OR-based index utilization path.
- `indexed_in_list_predicate`: IN-list index utilization path.
- `entity_path_introspection`: entity/path functions (`labels`, `type`, `properties`, `nodes`, `relationships`, `length`).

## Sample Cypher snippets (phase-1)
```cypher
MERGE (u:User {id: $id})
ON CREATE SET u.created_at = timestamp(), u.age = $age
RETURN u.id
```

```cypher
MATCH (u:User {id: $id})
FOREACH (x IN [1,2,3] | SET u.loop_counter = x)
RETURN u.loop_counter
```

```cypher
MATCH (s:User {id: $id})-[:Friend*6..6]->(t:User)
RETURN count(t) AS cnt
```

```cypher
MATCH p=(a:User {id: $id})-[r:Friend]->(b:User)
RETURN labels(a), type(r), properties(a), nodes(p), relationships(p), length(p)
LIMIT 1
```

## Falkor-specific notes
- `all_shortest_paths_len` uses a Falkor-safe form with `WITH s, t` before `allShortestPaths(...)`.
- `algo_max_flow_single_pair` uses one relationship type in Falkor config:
  - obtains a single value from `db.relationshipTypes()`
  - passes `relationshipTypes: [relationshipType]`
