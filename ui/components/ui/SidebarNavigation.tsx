"use client";

import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import { HardwareInfo } from "@/app/components/HardwareInfo";
import { Layers } from "lucide-react";

type Platforms = Record<
  string,
  {
    cpu: string;
    ram: string;
    storage: string;
  }
>;

const QUERY_DESCRIPTIONS = [
  {
    name: "Read Vertex",
    id: "single_vertex_read",
    description: "Point read by user id.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n",
    postgres: "SELECT * FROM users WHERE id = $1",
    mongo: "db.users.find({ _id: id })"
  },
  {
    name: "Write Vertex (Create)",
    id: "single_vertex_write",
    description: "Creates a single User node.",
    cypher: "CREATE (n:User {id: $id})\nRETURN n",
    postgres: "INSERT INTO users (id) VALUES ($1)\nON CONFLICT (id) DO NOTHING\nRETURNING id",
    mongo: "db.users.updateOne(\n  { _id: id },\n  { $setOnInsert: { created_at: now() } },\n  { upsert: true }\n)"
  },
  {
    name: "Write Vertex (Update)",
    id: "single_vertex_update",
    description: "Updates a User property for a single vertex.",
    cypher: "MATCH (n:User {id: $id})\nSET n.rpc_social_credit = $rpc_social_credit\nRETURN n",
    postgres: "UPDATE users SET rpc_social_credit = $2\nWHERE id = $1\nRETURNING id",
    mongo: "db.users.updateOne(\n  { _id: id },\n  { $set: { rpc_social_credit: value } }\n)"
  },
  {
    name: "Write Edge (Update)",
    id: "single_edge_update",
    description: "Updates one existing Friend edge selected by random order.",
    cypher: "MATCH (n:User)-[e:Friend]->(m:User)\nWITH e ORDER BY rand() LIMIT 1\nSET e.color = $color\nRETURN e",
    postgres: "UPDATE friend_edges SET color = $1,\n  bench_capacity = COALESCE(bench_capacity, 1 + ((src_id * 31 + dst_id * 17) % 20))\nWHERE (src_id, dst_id) = (\n  SELECT src_id, dst_id FROM friend_edges ORDER BY random() LIMIT 1\n)\nRETURNING src_id, dst_id",
    mongo: "// $sample + $merge is the standard Mongo idiom for updating a randomly chosen doc.\ndb.friend_edges.aggregate([\n  { $sample: { size: 1 } },\n  { $set: { color: value } },\n  { $merge: { into: \"friend_edges\", whenMatched: \"merge\", whenNotMatched: \"discard\" } },\n])"
  },
  {
    name: "Write Edge (Create)",
    id: "single_edge_write",
    description: "Creates a Friend edge between two users.",
    cypher: "MATCH (n:User {id: $from}), (m:User {id: $to})\nWITH n, m\nCREATE (n)-[e:Friend]->(m)\nRETURN e",
    postgres: "INSERT INTO friend_edges (src_id, dst_id, bench_capacity)\nVALUES ($1, $2, 1 + (($1 * 31 + $2 * 17) % 20))\nON CONFLICT (src_id, dst_id) DO UPDATE SET\n  bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20)),\n  touch = CURRENT_DATE\nRETURNING src_id, dst_id",
    mongo: "db.friend_edges.updateOne(\n  { src: from, dst: to },\n  {\n    $setOnInsert: { bench_capacity: capacity },\n    $set: { touch: now() },\n  },\n  { upsert: true }\n)"
  },
  {
    name: "Expand 1L",
    id: "aggregate_expansion_1",
    description: "1-hop expansion from a seed user.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nRETURN n.id",
    postgres: "SELECT dst_id AS id FROM friend_edges WHERE src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: {\n      from: \"friend_edges\", startWith: \"$_id\",\n      connectFromField: \"dst\", connectToField: \"src\",\n      as: \"reachable\", maxDepth: 0, depthField: \"depth\",\n  } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 0 } },\n  { $project: { _id: \"$reachable.dst\" } },\n])"
  },
  {
    name: "Expand 1L (Filtered)",
    id: "aggregate_expansion_1_with_filter",
    description: "1-hop expansion with destination age filter.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nWHERE n.age >= 18\nRETURN n.id",
    postgres: "SELECT fe.dst_id AS id FROM friend_edges fe\nJOIN users u ON u.id = fe.dst_id\nWHERE fe.src_id = $1 AND u.age >= 18",
    mongo: "// Same $graphLookup as Expand 1L, then joins back to users for the age filter:\ndb.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 0, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 0 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $project: { _id: \"$u._id\" } },\n])"
  },
  {
    name: "Expand 2L",
    id: "aggregate_expansion_2",
    description: "2-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe2.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nWHERE fe1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $project: { _id: \"$reachable.dst\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Expand 2L (Filtered)",
    id: "aggregate_expansion_2_with_filter",
    description: "2-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe2.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN users u ON u.id = fe2.dst_id\nWHERE fe1.src_id = $1 AND u.age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $project: { _id: \"$u._id\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Expand 3L",
    id: "aggregate_expansion_3",
    description: "3-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe3.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id\nWHERE fe1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 2, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 2 } },\n  { $project: { _id: \"$reachable.dst\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Expand 3L (Filtered)",
    id: "aggregate_expansion_3_with_filter",
    description: "3-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe3.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id\nJOIN users u ON u.id = fe3.dst_id\nWHERE fe1.src_id = $1 AND u.age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 2, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 2 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $project: { _id: \"$u._id\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Expand 4L",
    id: "aggregate_expansion_4",
    description: "4-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe4.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id\nJOIN friend_edges fe4 ON fe4.src_id = fe3.dst_id\nWHERE fe1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 3, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 3 } },\n  { $project: { _id: \"$reachable.dst\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Expand 4L (Filtered)",
    id: "aggregate_expansion_4_with_filter",
    description: "4-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id",
    postgres: "SELECT DISTINCT fe4.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id\nJOIN friend_edges fe4 ON fe4.src_id = fe3.dst_id\nJOIN users u ON u.id = fe4.dst_id\nWHERE fe1.src_id = $1 AND u.age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 3, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 3 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $project: { _id: \"$u._id\" } },\n  { $group: { _id: \"$_id\" } }, // DISTINCT\n])"
  },
  {
    name: "Aggregate Age",
    id: "aggregate_age",
    description: "Average age across all User nodes.",
    cypher: "MATCH (n:User)\nRETURN avg(n.age) AS avg_age",
    postgres: "SELECT avg(age) AS avg_age FROM users",
    mongo: "db.users.aggregate([\n  { $group: { _id: null, avg_age: { $avg: \"$age\" } } },\n])"
  },
  {
    name: "Aggregate Age Distinct",
    id: "aggregate_age_distinct",
    description: "Count distinct age values in User nodes.",
    cypher: "MATCH (n:User)\nRETURN count(DISTINCT n.age) AS distinct_ages",
    postgres: "SELECT count(DISTINCT age) AS distinct_ages FROM users",
    mongo: "db.users.aggregate([\n  { $group: { _id: \"$age\" } },\n  { $count: \"distinct_ages\" },\n])"
  },
  {
    name: "Aggregate Age (Filtered)",
    id: "aggregate_age_filtered",
    description: "Average age for users aged 18+.",
    cypher: "MATCH (n:User)\nWHERE n.age >= 18\nRETURN avg(n.age) AS avg_age",
    postgres: "SELECT avg(age) AS avg_age FROM users WHERE age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { age: { $gte: 18 } } },\n  { $group: { _id: null, avg_age: { $avg: \"$age\" } } },\n])"
  },
  {
    name: "Aggregate Count Users",
    id: "aggregate_count_users",
    description: "Total user count (uses db.meta.stats() optimization in FalkorDB).",
    cypher: "// FalkorDB:\nCALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt\n\n// Neo4j / Memgraph:\nMATCH (n:User) RETURN count(n) AS cnt",
    postgres: "SELECT count(*) AS cnt FROM users",
    mongo: "db.users.aggregate([ { $count: \"cnt\" } ])"
  },
  {
    name: "Aggregate Age Min/Max/Avg",
    id: "aggregate_age_min_max_avg",
    description: "Returns min, max, and average age in one query.",
    cypher: "MATCH (n:User)\nRETURN min(n.age) AS min_age, max(n.age) AS max_age, avg(n.age) AS avg_age",
    postgres: "SELECT min(age) AS min_age, max(age) AS max_age, avg(age) AS avg_age FROM users",
    mongo: "db.users.aggregate([\n  { $group: {\n      _id: null,\n      min_age: { $min: \"$age\" },\n      max_age: { $max: \"$age\" },\n      avg_age: { $avg: \"$age\" },\n  } },\n])"
  },
  {
    name: "Neighbours 2L",
    id: "neighbours_2",
    description: "Returns 2-hop neighbor IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN n.id",
    postgres: "SELECT fe2.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nWHERE fe1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $project: { _id: \"$reachable.dst\" } },\n])"
  },
  {
    name: "Neighbours 2L (Filtered)",
    id: "neighbours_2_with_filter",
    description: "Returns 2-hop neighbor IDs filtered by age.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN n.id",
    postgres: "SELECT fe2.dst_id AS id FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN users u ON u.id = fe2.dst_id\nWHERE fe1.src_id = $1 AND u.age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $project: { _id: \"$u._id\" } },\n])"
  },
  {
    name: "Neighbours 2L (Data)",
    id: "neighbours_2_with_data",
    description: "Returns 2-hop full node payloads.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN n",
    postgres: "SELECT u.* FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN users u ON u.id = fe2.dst_id\nWHERE fe1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $replaceRoot: { newRoot: \"$u\" } },\n])"
  },
  {
    name: "Neighbours 2L (Data + Filter)",
    id: "neighbours_2_with_data_and_filter",
    description: "Returns 2-hop node payloads with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN n",
    postgres: "SELECT u.* FROM friend_edges fe1\nJOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id\nJOIN users u ON u.id = fe2.dst_id\nWHERE fe1.src_id = $1 AND u.age >= 18",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $lookup: { from: \"users\", localField: \"reachable.dst\", foreignField: \"_id\", as: \"u\" } },\n  { $unwind: \"$u\" },\n  { $match: { \"u.age\": { $gte: 18 } } },\n  { $replaceRoot: { newRoot: \"$u\" } },\n])"
  },
  {
    name: "Shortest Path",
    id: "shortest_path",
    description: "Computes shortest path length between two users.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nWITH shortestPath((s)-[*]->(t)) AS p\nRETURN length(p)\n\n// Neo4j:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nMATCH p = shortestPath((s)-[*]->(t))\nRETURN length(p)\n\n// Memgraph:\nMATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to})\nRETURN length(p)",
    postgres: "-- Postgres-only: bounded BFS via recursive CTE. UNION (not UNION ALL) dedupes\n-- (id, depth) pairs so the frontier doesn't grow combinatorially.\nWITH RECURSIVE bfs(id, depth) AS (\n  SELECT $1::int, 0\n  UNION\n  SELECT fe.dst_id, bfs.depth + 1\n  FROM bfs JOIN friend_edges fe ON fe.src_id = bfs.id\n  WHERE bfs.depth < 15\n)\nSELECT min(depth) AS length FROM bfs WHERE id = $2"
  },
  {
    name: "Shortest Path (Filtered)",
    id: "shortest_path_with_filter",
    description: "Shortest path length, excluding empty paths.",
    cypher: "MATCH (s:User {id: $from}), (t:User {id: $to})\nWITH shortestPath((s)-[*]->(t)) AS p\nWHERE length(p) > 0\nRETURN length(p)",
    postgres: "-- Postgres-only (same bounded BFS as Shortest Path, filtered to non-empty paths).\nWITH RECURSIVE bfs(id, depth) AS (\n  SELECT $1::int, 0\n  UNION\n  SELECT fe.dst_id, bfs.depth + 1\n  FROM bfs JOIN friend_edges fe ON fe.src_id = bfs.id\n  WHERE bfs.depth < 15\n)\nSELECT min(depth) AS length FROM bfs WHERE id = $2 HAVING min(depth) > 0"
  },
  {
    name: "Pattern Cycle",
    id: "pattern_cycle",
    description: "Finds 3-node cycles anchored at the seed user.",
    cypher: "MATCH (a:User {id: $id})-->(b:User)-->(c:User)-->(a)\nRETURN a.id, b.id, c.id",
    postgres: "-- Postgres-only: $graphLookup can't verify a cycle's intermediate nodes, so this\n-- family has no Mongo equivalent.\nSELECT e1.src_id AS a_id, e1.dst_id AS b_id, e2.dst_id AS c_id\nFROM friend_edges e1\nJOIN friend_edges e2 ON e2.src_id = e1.dst_id\nJOIN friend_edges e3 ON e3.src_id = e2.dst_id AND e3.dst_id = e1.src_id\nWHERE e1.src_id = $1"
  },
  {
    name: "Pattern Long",
    id: "pattern_long",
    description: "Longer pattern expansion (4 hops).",
    cypher: "MATCH (a:User {id: $id})-->()-->()-->()-->(b:User)\nRETURN a.id, b.id",
    postgres: "SELECT $1::int AS a_id, e4.dst_id AS b_id\nFROM friend_edges e1\nJOIN friend_edges e2 ON e2.src_id = e1.dst_id\nJOIN friend_edges e3 ON e3.src_id = e2.dst_id\nJOIN friend_edges e4 ON e4.src_id = e3.dst_id\nWHERE e1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 3, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 3 } },\n  { $project: { a_id: seed, b_id: \"$reachable.dst\" } },\n])"
  },
  {
    name: "Pattern Short",
    id: "pattern_short",
    description: "Short pattern expansion (2 hops).",
    cypher: "MATCH (a:User {id: $id})-->()-->(b:User)\nRETURN a.id, b.id",
    postgres: "SELECT $1::int AS a_id, e2.dst_id AS b_id\nFROM friend_edges e1\nJOIN friend_edges e2 ON e2.src_id = e1.dst_id\nWHERE e1.src_id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 1 } },\n  { $project: { a_id: seed, b_id: \"$reachable.dst\" } },\n])"
  },
  {
    name: "Vertex on Label + Property",
    id: "vertex_on_label_property",
    description: "Lookup by label and property predicate.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n",
    postgres: "SELECT * FROM users WHERE id = $1",
    mongo: "db.users.find({ _id: id })"
  },
  {
    name: "Vertex on Label + Property (Indexed)",
    id: "vertex_on_label_property_index",
    description: "Same predicate, intended for index-path benchmarking.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n",
    postgres: "SELECT * FROM users WHERE id = $1",
    mongo: "db.users.find({ _id: id })"
  },
  {
    name: "Vertex on Property",
    id: "vertex_on_property",
    description: "Lookup by property without label restriction.",
    cypher: "MATCH (n {id: $id})\nRETURN n",
    postgres: "SELECT * FROM users WHERE id = $1",
    mongo: "db.users.find({ _id: id })"
  },
  {
    name: "Value Join",
    id: "value_join",
    description: "Joins users on matching age against a seeded user.",
    cypher: "MATCH (a:User {id: $id}), (b:User)\nWHERE a.age = b.age\nRETURN b.id",
    postgres: "SELECT b.id FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $lookup: {\n      from: \"users\", let: { age: \"$age\" },\n      pipeline: [ { $match: { $expr: { $eq: [\"$age\", \"$$age\"] } } } ],\n      as: \"matches\",\n  } },\n  { $unwind: \"$matches\" },\n  { $project: { _id: \"$matches._id\" } },\n])"
  },
  {
    name: "Value Join Count",
    id: "value_join_cnt",
    description: "Counts matches for value-join shape.",
    cypher: "MATCH (a:User {id: $id}), (b:User)\nWHERE a.age = b.age\nRETURN count(b)",
    postgres: "SELECT count(b.id) AS cnt FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $lookup: {\n      from: \"users\", let: { age: \"$age\" },\n      pipeline: [ { $match: { $expr: { $eq: [\"$age\", \"$$age\"] } } } ],\n      as: \"matches\",\n  } },\n  { $project: { cnt: { $size: \"$matches\" } } },\n])"
  },
  {
    name: "Order by Age",
    id: "order_by_age",
    description: "Full sort over users by age then id.",
    cypher: "MATCH (n:User)\nRETURN n.id, n.age\nORDER BY n.age, n.id",
    postgres: "SELECT id, age FROM users ORDER BY age, id",
    mongo: "db.users.aggregate([\n  { $sort: { age: 1, _id: 1 } },\n  { $project: { _id: 1, age: 1 } },\n])"
  },
  {
    name: "Unwind Rows",
    id: "unwind_rows",
    description: "UNWIND fan-out from row-local values.",
    cypher: "MATCH (n:User {id: $id})\nUNWIND [n.id, n.id + 1, n.id + 2] AS x\nRETURN x",
    postgres: "SELECT x FROM users u\nCROSS JOIN LATERAL (VALUES (u.id), (u.id + 1), (u.id + 2)) AS t(x)\nWHERE u.id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $project: { vals: [seed, seed + 1, seed + 2] } },\n  { $unwind: \"$vals\" },\n])"
  },
  {
    name: "Variable Length Friends",
    id: "var_len_friends",
    description: "Variable-length expansion (1..2 hops).",
    cypher: "MATCH (a:User {id: $id})-[*1..2]->(b:User)\nRETURN b.id",
    postgres: "WITH RECURSIVE vlf(id, depth) AS (\n  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1\n  UNION\n  SELECT fe.dst_id, vlf.depth + 1\n  FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id\n  WHERE vlf.depth < 2\n)\nSELECT DISTINCT id FROM vlf",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 1, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $group: { _id: \"$reachable.dst\" } },\n])"
  },
  {
    name: "Optional Friend",
    id: "optional_friend",
    description: "OPTIONAL MATCH expansion from seeded user.",
    cypher: "MATCH (a:User {id: $id})\nOPTIONAL MATCH (a)-->(b:User)\nRETURN a.id, b.id",
    postgres: "SELECT t.a AS a_id, fe.dst_id AS b_id\nFROM (SELECT $1::int AS a) t\nLEFT JOIN friend_edges fe ON fe.src_id = t.a",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $lookup: { from: \"friend_edges\", localField: \"_id\", foreignField: \"src\", as: \"edges\" } },\n  { $unwind: { path: \"$edges\", preserveNullAndEmptyArrays: true } },\n  { $project: { a_id: \"$_id\", b_id: \"$edges.dst\" } },\n])"
  },
  {
    name: "Call Subquery",
    id: "call_subquery",
    description: "Correlated subquery using CALL { ... }.",
    cypher: "MATCH (a:User {id: $id})\nCALL {\n  WITH a\n  MATCH (a)-->(b:User)\n  RETURN b.id AS bid\n}\nRETURN bid",
    postgres: "SELECT sub.bid FROM (SELECT $1::int AS a) t,\nLATERAL (SELECT dst_id AS bid FROM friend_edges WHERE src_id = t.a) sub",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $lookup: { from: \"friend_edges\", localField: \"_id\", foreignField: \"src\", as: \"sub\" } },\n  { $unwind: \"$sub\" },\n  { $project: { bid: \"$sub.dst\" } },\n])"
  },
  {
    name: "MERGE User (Insert Path)",
    id: "merge_user_insert_path",
    description: "MERGE branch that creates a new User when id does not exist.",
    cypher: "MERGE (u:User {id: $id})\nON CREATE SET u.created_at = timestamp(), u.age = $age\nRETURN u.id",
    postgres: "INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2)\nON CONFLICT (id) DO NOTHING\nRETURNING id",
    mongo: "db.users.updateOne(\n  { _id: insertId },\n  { $setOnInsert: { created_at: now(), age: value } },\n  { upsert: true }\n)"
  },
  {
    name: "MERGE User (Upsert Existing)",
    id: "merge_user_upsert_existing",
    description: "MERGE branch that updates an existing User via ON MATCH.",
    cypher: "MERGE (u:User {id: $id})\nON CREATE SET u.created_at = timestamp()\nON MATCH SET u.age = $age, u.last_seen = timestamp()\nRETURN u.id",
    postgres: "INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2)\nON CONFLICT (id) DO UPDATE SET age = EXCLUDED.age, last_seen = now()\nRETURNING id",
    mongo: "db.users.updateOne(\n  { _id: id },\n  {\n    $set: { age: value, last_seen: now() },\n    $setOnInsert: { created_at: now() },\n  },\n  { upsert: true }\n)"
  },
  {
    name: "MERGE Friend Edge (Upsert)",
    id: "merge_friend_edge_upsert",
    description: "MERGE on relationship pattern with ON CREATE/ON MATCH updates.",
    cypher: "MATCH (a:User {id: $from}), (b:User {id: $to})\nMERGE (a)-[r:Friend]->(b)\nON CREATE SET r.since = date()\nON MATCH SET r.touch = date()\nRETURN id(r)",
    postgres: "INSERT INTO friend_edges (src_id, dst_id, since, bench_capacity)\nVALUES ($1, $2, CURRENT_DATE, 1 + (($1 * 31 + $2 * 17) % 20))\nON CONFLICT (src_id, dst_id) DO UPDATE SET\n  touch = CURRENT_DATE,\n  bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20))\nRETURNING src_id, dst_id",
    mongo: "db.friend_edges.updateOne(\n  { src: from, dst: to },\n  {\n    $setOnInsert: { since: now(), bench_capacity: capacity },\n    $set: { touch: now() },\n  },\n  { upsert: true }\n)"
  },
  {
    name: "Detach Delete User",
    id: "detach_delete_user",
    description: "Deletes a user and all incident relationships.",
    cypher: "MATCH (u:User {id: $id})\nDETACH DELETE u",
    postgres: "-- friend_edges has ON DELETE CASCADE on both FKs, matching DETACH DELETE semantics.\nDELETE FROM users WHERE id = $1",
    mongo: "// Mongo has no FK/cascade concept: only the user document is removed; any\n// friend_edges referencing it are left in place (documented limitation).\ndb.users.deleteOne({ _id: id })"
  },
  {
    name: "Remove Property and Label",
    id: "remove_user_property_and_label",
    description: "Exercises REMOVE on both property and label targets.",
    cypher: "MATCH (u:User {id: $id})\nREMOVE u.rpc_social_credit, u:TemporaryLabel\nRETURN u.id",
    postgres: "-- Postgres has no label concept; this drops only the property-removal semantics.\nUPDATE users SET rpc_social_credit = NULL WHERE id = $1 RETURNING id",
    mongo: "// Mongo has no label concept; this drops only the property-removal semantics.\ndb.users.updateOne({ _id: id }, { $unset: { rpc_social_credit: \"\" } })"
  },
  {
    name: "FOREACH Loop Mutation",
    id: "foreach_loop_mutation",
    description: "Uses FOREACH to apply repeated SET mutations in one query.",
    cypher: "MATCH (u:User {id: $id})\nFOREACH (x IN [1,2,3] | SET u.loop_counter = x)\nRETURN u.loop_counter",
    postgres: "-- Approximated as a single terminal assignment (equivalent end state to\n-- FOREACH (x IN [1,2,3] | SET u.loop_counter = x)).\nUPDATE users SET loop_counter = 3 WHERE id = $1 RETURNING loop_counter",
    mongo: "// Same terminal-assignment approximation as Postgres.\ndb.users.updateOne({ _id: id }, { $set: { loop_counter: 3 } })"
  },
  {
    name: "UNION ALL IDs",
    id: "union_all_ids",
    description: "UNION ALL composition without deduplication.",
    cypher: "MATCH (u:User {id: $id})\nRETURN u.id AS uid\nUNION ALL\nMATCH (v:User) WHERE v.id < 10\nRETURN v.id AS uid",
    postgres: "SELECT id AS uid FROM users WHERE id = $1\nUNION ALL SELECT id AS uid FROM users WHERE id < 10",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $project: { uid: \"$_id\" } },\n  { $unionWith: { coll: \"users\", pipeline: [\n      { $match: { _id: { $lt: 10 } } },\n      { $project: { uid: \"$_id\" } },\n  ] } },\n])"
  },
  {
    name: "UNION Distinct IDs",
    id: "union_distinct_ids",
    description: "UNION composition with distinct semantics.",
    cypher: "MATCH (u:User {id: $id})\nRETURN u.id AS uid\nUNION\nMATCH (v:User {id: $id})\nRETURN v.id AS uid",
    postgres: "SELECT id AS uid FROM users WHERE id = $1\nUNION SELECT id AS uid FROM users WHERE id = $1",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $project: { uid: \"$_id\" } },\n  { $unionWith: { coll: \"users\", pipeline: [\n      { $match: { _id: seed } },\n      { $project: { uid: \"$_id\" } },\n  ] } },\n  { $group: { _id: \"$uid\" } },\n])"
  },
  {
    name: "All Shortest Paths Length",
    id: "all_shortest_paths_len",
    description: "allShortestPaths coverage with vendor-specific syntax.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nWITH s, t\nMATCH p = allShortestPaths((s)-[:Friend*1..4]->(t))\nRETURN length(p)\n\n// Neo4j:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nMATCH p = allShortestPaths((s)-[:Friend*1..4]->(t))\nRETURN length(p)\n\n// Memgraph:\nMATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to})\nRETURN length(p)",
    postgres: "-- Postgres-only: bounded (depth <= 4) path-array recursive CTE with explicit\n-- cycle-avoidance, approximating allShortestPaths. No Mongo equivalent since\n-- $graphLookup can't enumerate distinct paths.\nWITH RECURSIVE paths(id, depth, path) AS (\n  SELECT $1::int, 0, ARRAY[$1::int]\n  UNION ALL\n  SELECT fe.dst_id, p.depth + 1, p.path || fe.dst_id\n  FROM paths p JOIN friend_edges fe ON fe.src_id = p.id\n  WHERE p.depth < 4 AND NOT (fe.dst_id = ANY(p.path))\n)\nSELECT min(depth) AS length FROM paths WHERE id = $2"
  },
  {
    name: "Var-Length with Edge Filter",
    id: "var_len_with_edge_where_filter",
    description: "Variable-length traversal with edge property filtering.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User)\nWHERE r.bench_capacity >= $min_capacity\nRETURN count(t)\n\n// Neo4j / Memgraph:\nMATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User)\nWHERE all(rel IN r WHERE rel.bench_capacity >= $min_capacity)\nRETURN count(t)",
    postgres: "WITH RECURSIVE vlf(id, depth) AS (\n  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 AND bench_capacity >= $2\n  UNION\n  SELECT fe.dst_id, vlf.depth + 1\n  FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id\n  WHERE vlf.depth < 3 AND fe.bench_capacity >= $2\n)\nSELECT count(DISTINCT id) AS cnt FROM vlf",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: {\n      ..., maxDepth: 2, depthField: \"depth\",\n      restrictSearchWithMatch: { bench_capacity: { $gte: minCapacity } },\n  } },\n  { $unwind: \"$reachable\" },\n  { $group: { _id: \"$reachable.dst\" } },\n  { $count: \"cnt\" },\n])"
  },
  {
    name: "Exact 5-Hop Traverse Count",
    id: "exact_5_hop_traverse_count",
    description: "Fixed-depth 5-hop traversal count for deeper expansion profiling.",
    cypher: "MATCH (s:User {id: $id})-[:Friend*5..5]->(t:User)\nRETURN count(t) AS cnt",
    postgres: "WITH RECURSIVE hops(id, depth) AS (\n  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1\n  UNION\n  SELECT fe.dst_id, hops.depth + 1\n  FROM hops JOIN friend_edges fe ON fe.src_id = hops.id\n  WHERE hops.depth < 5\n)\nSELECT count(*) AS cnt FROM hops WHERE depth = 5",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 4, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 4 } },\n  { $count: \"cnt\" },\n])"
  },
  {
    name: "Exact 6-Hop Traverse Count",
    id: "exact_6_hop_traverse_count",
    description: "Fixed-depth 6-hop traversal count for depth scaling analysis.",
    cypher: "MATCH (s:User {id: $id})-[:Friend*6..6]->(t:User)\nRETURN count(t) AS cnt",
    postgres: "WITH RECURSIVE hops(id, depth) AS (\n  SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1\n  UNION\n  SELECT fe.dst_id, hops.depth + 1\n  FROM hops JOIN friend_edges fe ON fe.src_id = hops.id\n  WHERE hops.depth < 6\n)\nSELECT count(*) AS cnt FROM hops WHERE depth = 6",
    mongo: "db.users.aggregate([\n  { $match: { _id: seed } },\n  { $graphLookup: { ..., maxDepth: 5, depthField: \"depth\" } },\n  { $unwind: \"$reachable\" },\n  { $match: { \"reachable.depth\": 5 } },\n  { $count: \"cnt\" },\n])"
  },
  {
    name: "Count Users (Plain)",
    id: "count_users_plain",
    description: "Simple node count used for count-reduction optimizer paths.",
    cypher: "MATCH (u:User)\nRETURN count(u) AS cnt",
    postgres: "SELECT count(*) AS cnt FROM users",
    mongo: "db.users.aggregate([ { $count: \"cnt\" } ])"
  },
  {
    name: "Count Friend Edges (Plain)",
    id: "count_friend_edges_plain",
    description: "Simple edge count used for relationship count-reduction paths.",
    cypher: "MATCH ()-[r:Friend]->()\nRETURN count(r) AS cnt",
    postgres: "SELECT count(*) AS cnt FROM friend_edges",
    mongo: "db.friend_edges.aggregate([ { $count: \"cnt\" } ])"
  },
  {
    name: "Indexed OR Predicate",
    id: "indexed_or_predicate",
    description: "Predicate shape intended to trigger OR index utilization.",
    cypher: "MATCH (u:User)\nWHERE u.id = $id1 OR u.id = $id2\nRETURN u.id",
    postgres: "SELECT id FROM users WHERE id = $1 OR id = $2",
    mongo: "db.users.find({ $or: [ { _id: id1 }, { _id: id2 } ] })"
  },
  {
    name: "Indexed IN-List Predicate",
    id: "indexed_in_list_predicate",
    description: "IN-list predicate shape intended to trigger index utilization.",
    cypher: "MATCH (u:User)\nWHERE u.id IN [$id1, $id2, $id3, $id4]\nRETURN u.id",
    postgres: "SELECT id FROM users WHERE id IN ($1, $2, $3, $4)",
    mongo: "db.users.find({ _id: { $in: [id1, id2, id3, id4] } })"
  },
  {
    name: "Entity and Path Introspection",
    id: "entity_path_introspection",
    description: "Covers labels/type/properties and path decomposition functions.",
    cypher: "MATCH p=(a:User {id: $id})-[r:Friend]->(b:User)\nRETURN labels(a), type(r), properties(a), nodes(p), relationships(p), length(p)\nLIMIT 1"
  },
  {
    name: "Temporal + Spatial Roundtrip",
    id: "temporal_spatial_roundtrip",
    description: "Extended-core scalar/function sanity query (available on FalkorDB and Neo4j profiles).",
    cypher: `RETURN
  date('2024-01-01') AS d,
  localtime('12:30:00') AS t,
  duration('P2DT3H') AS dur,
  distance(point({latitude: 32.1, longitude: 34.8}), point({latitude: 32.2, longitude: 34.9})) AS dist`,
    postgres: "-- No PostGIS dependency; distance via the spherical law of cosines.\nSELECT\n  DATE '2024-01-01' AS d,\n  TIME '12:30:00' AS t,\n  INTERVAL '2 days 3 hours' AS dur,\n  (6371000 * acos(\n    cos(radians(32.1)) * cos(radians(32.2)) * cos(radians(34.9) - radians(34.8))\n    + sin(radians(32.1)) * sin(radians(32.2))\n  )) AS dist",
    mongo: "// Requires MongoDB 6.0+ for $documents; distance via the spherical law of cosines\n// using $sin/$cos/$acos/$degreesToRadians (MongoDB 4.2+).\ndb.users.aggregate([\n  { $documents: [ {} ] },\n  { $project: {\n      d: { $dateFromString: { dateString: \"2024-01-01\" } },\n      dur_hours: 51,\n      dist: { $multiply: [ 6371000, { $acos: { $add: [\n        { $multiply: [ { $cos: {\"$degreesToRadians\": 32.1} }, { $cos: {\"$degreesToRadians\": 32.2} }, { $cos: {\"$subtract\": [34.9, 34.8]} } ] },\n        { $multiply: [ { $sin: {\"$degreesToRadians\": 32.1} }, { $sin: {\"$degreesToRadians\": 32.2} } ] },\n      ] } } ] },\n  } },\n])"
  },
  {
    name: "Vector Query Nodes (Smoke)",
    id: "vector_query_nodes_smoke",
    description: "Fixture-dependent vector-search smoke query with vendor-specific procedures/index names.",
    cypher: `// FalkorDB:
CALL db.idx.vector.queryNodes('User', 'embedding', 10, vecf32([0.1, 0.2, 0.3]))
YIELD node, score
RETURN id(node), score
LIMIT 10

// Neo4j:
CALL db.index.vector.queryNodes('bench_user_embedding_idx', 10, [0.1, 0.2, 0.3])
YIELD node, score
RETURN id(node), score
LIMIT 10

// Memgraph:
CALL vector_search.search('bench_user_embedding_idx', 10, [0.1, 0.2, 0.3])
YIELD node, similarity
RETURN id(node), similarity AS score
LIMIT 10`
  },
  {
    name: "Fulltext Query Nodes (Smoke)",
    id: "fulltext_query_nodes_smoke",
    description: "Fixture-dependent node fulltext smoke query with vendor-specific procedures/index names.",
    cypher: `// FalkorDB:
CALL db.idx.fulltext.queryNodes('User', 'fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10

// Neo4j:
CALL db.index.fulltext.queryNodes('bench_user_ft_idx', 'fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10

// Memgraph:
CALL text_search.search('bench_user_ft_idx', 'data.ft_text:fixture_alice')
YIELD node, score
RETURN id(node), score
LIMIT 10`
  },
  {
    name: "Fulltext Query Relationships (Smoke)",
    id: "fulltext_query_relationships_smoke",
    description: "Fixture-dependent relationship fulltext smoke query with vendor-specific procedures/index names.",
    cypher: `// FalkorDB:
CALL db.idx.fulltext.queryRelationships('Friend', 'fixture_blue')
YIELD relationship, score
RETURN id(relationship), score
LIMIT 10

// Neo4j:
CALL db.index.fulltext.queryRelationships('bench_friend_ft_idx', 'fixture_blue')
YIELD relationship, score
RETURN id(relationship), score
LIMIT 10

// Memgraph:
CALL text_search.search_edges('bench_friend_ft_idx', 'data.ft_text:fixture_blue')
YIELD edge, score
RETURN id(edge), score
LIMIT 10`
  },
  {
    name: "ID Seek (Columnar)",
    id: "id_seek",
    description: "Internal id point lookup (columnar/id-path coverage).",
    cypher: "MATCH (n)\nWHERE id(n) = $id\nRETURN n.id",
    postgres: "SELECT id FROM users WHERE id = $1",
    mongo: "db.users.find({ _id: id })"
  },
  {
    name: "ID Range Scan (Columnar)",
    id: "id_range_scan",
    description: "Internal id range scan for columnar fan-out behavior.",
    cypher: "MATCH (n)\nWHERE id(n) >= $start AND id(n) < $end\nRETURN n.id",
    postgres: "SELECT id FROM users WHERE id >= $1 AND id < $2",
    mongo: "db.users.find({ _id: { $gte: start, $lt: end } })"
  },
  {
    name: "Algorithm: PageRank Summary",
    id: "algo_pagerank_summary",
    description: "Runs PageRank and returns one representative score.",
    cypher: "// FalkorDB:\nCALL algo.pageRank('User', null)\nYIELD node, score\nRETURN score\nLIMIT 1\n\n// Neo4j:\nCALL gds.pageRank.stream('benchmark_algo_graph')\nYIELD nodeId, score\nRETURN score\nLIMIT 1\n\n// Memgraph:\nCALL pagerank.get()\nYIELD node, rank\nRETURN rank AS score\nLIMIT 1"
  },
  {
    name: "Algorithm: Max Flow (Single Pair)",
    id: "algo_max_flow_single_pair",
    description: "Computes max-flow between source and target users with bench_capacity.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $source_id}), (t:User {id: $target_id})\nCALL db.relationshipTypes() YIELD relationshipType\nWITH s, t, relationshipType ORDER BY relationshipType LIMIT 1\nCALL algo.maxFlow({ sourceNodes: [s], targetNodes: [t], relationshipTypes: [relationshipType], capacityProperty: 'bench_capacity' })\nYIELD maxFlow\nRETURN coalesce(toFloat(maxFlow), 0.0) AS max_flow"
  },
  {
    name: "Algorithm: MSF Summary",
    id: "algo_msf_summary",
    description: "Runs minimum spanning forest style summary and returns edge/weight stats.",
    cypher: "// FalkorDB:\nCALL algo.MSF({ weightAttribute: 'bench_capacity' })\nYIELD edges\nRETURN size(edges) AS edge_count,\nreduce(total = 0.0, edge IN edges | total + coalesce(toFloat(edge.bench_capacity), 0.0)) AS total_weight"
  },
  {
    name: "Algorithm: Harmonic Summary",
    id: "algo_harmonic_summary",
    description: "Computes harmonic centrality summary statistics.",
    cypher: "// FalkorDB:\nCALL algo.HarmonicCentrality()\nYIELD node, score\nRETURN count(node) AS node_count, avg(score) AS avg_score, max(score) AS max_score"
  }
];

export function NavMain({
  items,
  selectedOptions,
  handleSideBarSelection,
  platform,
  hideHardware,
  datasetSummary,
}: {
  items: {
    title: string;
    description?: string;
    layout?: "row" | "col";
    icon: React.ElementType;
    options: { id: string; label: string }[];
  }[];
  selectedOptions: Record<string, string[]>;
  handleSideBarSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
  hideHardware?: boolean;
  datasetSummary?: {
    nodes: number;
    edges: number;
    readQueries: number;
    writeQueries: number;
    startedAtEpochSecs?: number;
    engineVersions?: Record<string, string>;
  } | null;
}) {
  const { state } = useSidebar();

  const isRealisticWorkloadOn =
    selectedOptions["Workload Type"]?.includes("concurrent");

  const filteredItems = items.filter((group) => {
    if (group.title === "Queries" && isRealisticWorkloadOn) return false;
    if (group.title === "Hardware" && hideHardware) return false;
    if (
      (group.title === "Clients" ||
        group.title === "Throughput" ||
        group.title === "Realistic Workload" ||
        group.title === "Hardware") &&
      !isRealisticWorkloadOn
    )
      return false;
    return true;
  });

  return (
    <SidebarMenu>
      {datasetSummary && (
        <SidebarMenuItem
          className={`font-space mt-2 mb-4${
            state === "collapsed" ? " flex justify-center" : ""
          }`}
        >
          <SidebarMenuButton
            size="lg"
            className={`flex items-start gap-3 pl-4 h-auto cursor-default ${
              state === "collapsed" ? "justify-center" : ""
            }`}
          >
            <Layers
              className={`w-6 h-6 ${state === "collapsed" ? "mx-auto" : ""}`}
            />
            {state !== "collapsed" && (
              <div className="flex flex-col">
                <h2 className="text-lg font-semibold mb-1">Dataset &amp; workload</h2>
                <div className="mt-0.5 flex flex-col gap-0.5 text-xs text-gray-700 font-medium">
                  <div className="flex justify-between gap-4">
                    <span className="text-gray-500">Nodes</span>
                    <span className="tabular-nums">
                      {datasetSummary.nodes.toLocaleString()}
                    </span>
                  </div>
                  <div className="flex justify-between gap-4">
                    <span className="text-gray-500">Edges</span>
                    <span className="tabular-nums">
                      {datasetSummary.edges.toLocaleString()}
                    </span>
                  </div>
                  <div className="flex justify-between gap-4 pt-1">
                    <span className="text-gray-500">Queries</span>
                    <span className="tabular-nums">
                      {(datasetSummary.readQueries + datasetSummary.writeQueries).toLocaleString()}
                    </span>
                  </div>
                  <div className="flex justify-between gap-4">
                    <span className="text-gray-500">Read / write</span>
                    <span className="tabular-nums">
                      {datasetSummary.readQueries.toLocaleString()} / {datasetSummary.writeQueries.toLocaleString()}
                    </span>
                  </div>
                  {datasetSummary.engineVersions && Object.keys(datasetSummary.engineVersions).length > 0 && (
                    <div className="pt-1">
                      <div className="text-gray-500 mb-0.5">Engine versions</div>
                      <div className="space-y-0.5">
                        {Object.entries(datasetSummary.engineVersions).map(([vendor, version]) => (
                          <div key={`${vendor}-${version}`} className="flex justify-between gap-2">
                            <span className="text-gray-500">{vendor}</span>
                            <span className="tabular-nums text-right">{version}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              </div>
            )}
          </SidebarMenuButton>
        </SidebarMenuItem>
      )}
      {filteredItems.map((group) => (
        <SidebarMenuItem
          key={group.title}
          className={`font-space mt-2 mb-4${
            state === "collapsed" ? "flex justify-center" : ""
          }`}
        >
          <SidebarMenuButton
            className={`flex items-center gap-3 pl-4 ${
              state === "collapsed" ? "justify-center" : ""
            }`}
          >
            <group.icon
              className={`w-6 h-6 ${state === "collapsed" ? "mx-auto" : ""}`}
            />
            {state !== "collapsed" && (
              <div className="flex items-center gap-2">
                <h2 className="text-lg font-semibold">{group.title}</h2>
                {group.title === "Queries" && (
                  <HoverCard>
                    <HoverCardTrigger>
                      <span
                        className="w-4 h-4 flex items-center justify-center bg-gray-400 text-white rounded-full text-xs font-bold cursor-pointer shadow-md"
                      >
                        i
                      </span>
                    </HoverCardTrigger>
                    <HoverCardContent className="bg-gray-100 text-gray-800 p-4 rounded-md shadow-xl w-[480px] max-w-[90vw] max-h-[450px] overflow-y-auto font-space">
                      <h3 className="text-sm font-bold border-b border-gray-200 pb-2 mb-3 text-gray-900">
                        Query Explanations &amp; Samples
                      </h3>
                      <div className="flex flex-col gap-4">
                        {[...QUERY_DESCRIPTIONS].sort((a, b) => a.name.localeCompare(b.name)).map((q) => (
                          <div key={q.id} className="text-xs border-b border-gray-200/60 pb-3 last:border-0 last:pb-0 text-left">
                            <div className="flex items-center justify-between mb-1 gap-2">
                              <span className="font-bold text-gray-900">{q.name}</span>
                              <span className="font-mono text-[10px] text-gray-500 bg-gray-200/50 px-1.5 py-0.5 rounded shrink-0">{q.id}</span>
                            </div>
                            <p className="text-gray-600 mb-1.5 leading-relaxed">{q.description}</p>
                            <p className="text-[10px] font-semibold text-gray-500 uppercase tracking-wide mb-0.5">Cypher</p>
                            <pre className="bg-gray-900 text-gray-100 p-2 rounded text-[10px] font-mono overflow-x-auto whitespace-pre leading-normal">
                              {q.cypher}
                            </pre>
                            {q.postgres && (
                              <>
                                <p className="text-[10px] font-semibold text-gray-500 uppercase tracking-wide mt-1.5 mb-0.5">Postgres</p>
                                <pre className="bg-gray-900 text-gray-100 p-2 rounded text-[10px] font-mono overflow-x-auto whitespace-pre leading-normal">
                                  {q.postgres}
                                </pre>
                              </>
                            )}
                            {q.mongo && (
                              <>
                                <p className="text-[10px] font-semibold text-gray-500 uppercase tracking-wide mt-1.5 mb-0.5">Mongo</p>
                                <pre className="bg-gray-900 text-gray-100 p-2 rounded text-[10px] font-mono overflow-x-auto whitespace-pre leading-normal">
                                  {q.mongo}
                                </pre>
                              </>
                            )}
                          </div>
                        ))}
                      </div>
                    </HoverCardContent>
                  </HoverCard>
                )}
              </div>
            )}
          </SidebarMenuButton>

          {state !== "collapsed" && (
            <div className="pl-4 pr-4 mt-2">
              {group.description && (
                <p className="text-sm text-gray-500 mb-3">
                  {group.description}
                </p>
              )}
              <div
                className={`gap-2 ${
                  group.layout === "row"
                    ? "grid grid-cols-2"
                    : "flex flex-col"
                }`}
              >
                {(group.title === "Queries"
                  ? [...group.options].sort((a, b) =>
                      a.label.localeCompare(b.label, undefined, {
                        numeric: true,
                        sensitivity: "base",
                      })
                    )
                  : group.options
                ).map((option, index) => {
                  const isSelected = selectedOptions[group.title]?.includes(
                    option.id
                  );
                  const getButtonClasses = () => {
                    if (isSelected) {
                      if (option.id === "falkordb")
                        return "bg-[#F5F4FF] text-FalkorDB border-FalkorDB";
                      if (option.id === "neo4j")
                        return "bg-[#F5F4FF] text-Neo4j border-Neo4j";
                      if (option.id === "memgraph")
                        return "bg-[#F5F4FF] text-Memgraph border-Memgraph";
                      return "bg-[#F5F4FF] text-[#7466FF] border-[#7466FF]";
                    }
                    return "bg-gray-100 text-gray-800 border-transparent";
                  };

                  return (
                    <div
                      key={option.id}
                      className={`flex min-w-0 items-center gap-2 w-full ${
                        group.title === "Queries"
                          ? "text-sm flex-wrap justify-center"
                          : ""
                      }`}
                    >
                      <button
                        onClick={() => handleSideBarSelection(group.title, option.id)}
                        className={`font-fira flex-1 min-w-0 px-3 py-1 rounded-lg border text-center whitespace-normal break-words leading-tight ${getButtonClasses()}`}
                      >
                        {option.label}
                      </button>
                      {group.title === "Hardware" &&
                        platform &&
                        platform[index] && (
                          <HardwareInfo
                            cpu={platform[index].cpu}
                            ram={platform[index].ram}
                            storage={platform[index].storage}
                          />
                        )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </SidebarMenuItem>
      ))}
    </SidebarMenu>
  );
}
