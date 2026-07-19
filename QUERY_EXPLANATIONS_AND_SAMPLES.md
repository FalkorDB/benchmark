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

## Reference source
- Canonical query definitions: `src/queries_repository.rs`
- CLI profile/toggle options: `src/cli.rs` (`--query-profile` and `--enable-algo-*`)
