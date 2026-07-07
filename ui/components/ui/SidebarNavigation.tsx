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
    cypher: "MATCH (n:User {id: $id})\nRETURN n"
  },
  {
    name: "Write Vertex (Create)",
    id: "single_vertex_write",
    description: "Creates a single User node.",
    cypher: "CREATE (n:User {id: $id})\nRETURN n"
  },
  {
    name: "Write Vertex (Update)",
    id: "single_vertex_update",
    description: "Updates a User property for a single vertex.",
    cypher: "MATCH (n:User {id: $id})\nSET n.rpc_social_credit = $rpc_social_credit\nRETURN n"
  },
  {
    name: "Write Edge (Update)",
    id: "single_edge_update",
    description: "Updates one existing Friend edge selected by random order.",
    cypher: "MATCH (n:User)-[e:Friend]->(m:User)\nWITH e ORDER BY rand() LIMIT 1\nSET e.color = $color\nRETURN e"
  },
  {
    name: "Write Edge (Create)",
    id: "single_edge_write",
    description: "Creates a Friend edge between two users.",
    cypher: "MATCH (n:User {id: $from}), (m:User {id: $to})\nWITH n, m\nCREATE (n)-[e:Friend]->(m)\nRETURN e"
  },
  {
    name: "Expand 1L",
    id: "aggregate_expansion_1",
    description: "1-hop expansion from a seed user.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nRETURN n.id"
  },
  {
    name: "Expand 1L (Filtered)",
    id: "aggregate_expansion_1_with_filter",
    description: "1-hop expansion with destination age filter.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nWHERE n.age >= 18\nRETURN n.id"
  },
  {
    name: "Expand 2L",
    id: "aggregate_expansion_2",
    description: "2-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 2L (Filtered)",
    id: "aggregate_expansion_2_with_filter",
    description: "2-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 3L",
    id: "aggregate_expansion_3",
    description: "3-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 3L (Filtered)",
    id: "aggregate_expansion_3_with_filter",
    description: "3-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 4L",
    id: "aggregate_expansion_4",
    description: "4-hop expansion and distinct destination IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 4L (Filtered)",
    id: "aggregate_expansion_4_with_filter",
    description: "4-hop expansion with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Aggregate Age",
    id: "aggregate_age",
    description: "Average age across all User nodes.",
    cypher: "MATCH (n:User)\nRETURN avg(n.age) AS avg_age"
  },
  {
    name: "Aggregate Age Distinct",
    id: "aggregate_age_distinct",
    description: "Count distinct age values in User nodes.",
    cypher: "MATCH (n:User)\nRETURN count(DISTINCT n.age) AS distinct_ages"
  },
  {
    name: "Aggregate Age (Filtered)",
    id: "aggregate_age_filtered",
    description: "Average age for users aged 18+.",
    cypher: "MATCH (n:User)\nWHERE n.age >= 18\nRETURN avg(n.age) AS avg_age"
  },
  {
    name: "Aggregate Count Users",
    id: "aggregate_count_users",
    description: "Total user count (uses db.meta.stats() optimization in FalkorDB).",
    cypher: "// FalkorDB:\nCALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt\n\n// Neo4j / Memgraph:\nMATCH (n:User) RETURN count(n) AS cnt"
  },
  {
    name: "Aggregate Age Min/Max/Avg",
    id: "aggregate_age_min_max_avg",
    description: "Returns min, max, and average age in one query.",
    cypher: "MATCH (n:User)\nRETURN min(n.age) AS min_age, max(n.age) AS max_age, avg(n.age) AS avg_age"
  },
  {
    name: "Neighbours 2L",
    id: "neighbours_2",
    description: "Returns 2-hop neighbor IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN n.id"
  },
  {
    name: "Neighbours 2L (Filtered)",
    id: "neighbours_2_with_filter",
    description: "Returns 2-hop neighbor IDs filtered by age.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN n.id"
  },
  {
    name: "Neighbours 2L (Data)",
    id: "neighbours_2_with_data",
    description: "Returns 2-hop full node payloads.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN n"
  },
  {
    name: "Neighbours 2L (Data + Filter)",
    id: "neighbours_2_with_data_and_filter",
    description: "Returns 2-hop node payloads with age filter.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN n"
  },
  {
    name: "Shortest Path",
    id: "shortest_path",
    description: "Computes shortest path length between two users.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nWITH shortestPath((s)-[*]->(t)) AS p\nRETURN length(p)\n\n// Neo4j:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nMATCH p = shortestPath((s)-[*]->(t))\nRETURN length(p)\n\n// Memgraph:\nMATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to})\nRETURN length(p)"
  },
  {
    name: "Shortest Path (Filtered)",
    id: "shortest_path_with_filter",
    description: "Shortest path length, excluding empty paths.",
    cypher: "MATCH (s:User {id: $from}), (t:User {id: $to})\nWITH shortestPath((s)-[*]->(t)) AS p\nWHERE length(p) > 0\nRETURN length(p)"
  },
  {
    name: "Pattern Cycle",
    id: "pattern_cycle",
    description: "Finds 3-node cycles anchored at the seed user.",
    cypher: "MATCH (a:User {id: $id})-->(b:User)-->(c:User)-->(a)\nRETURN a.id, b.id, c.id"
  },
  {
    name: "Pattern Long",
    id: "pattern_long",
    description: "Longer pattern expansion (4 hops).",
    cypher: "MATCH (a:User {id: $id})-->()-->()-->()-->(b:User)\nRETURN a.id, b.id"
  },
  {
    name: "Pattern Short",
    id: "pattern_short",
    description: "Short pattern expansion (2 hops).",
    cypher: "MATCH (a:User {id: $id})-->()-->(b:User)\nRETURN a.id, b.id"
  },
  {
    name: "Vertex on Label + Property",
    id: "vertex_on_label_property",
    description: "Lookup by label and property predicate.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n"
  },
  {
    name: "Vertex on Label + Property (Indexed)",
    id: "vertex_on_label_property_index",
    description: "Same predicate, intended for index-path benchmarking.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n"
  },
  {
    name: "Vertex on Property",
    id: "vertex_on_property",
    description: "Lookup by property without label restriction.",
    cypher: "MATCH (n {id: $id})\nRETURN n"
  },
  {
    name: "Value Join",
    id: "value_join",
    description: "Joins users on matching age against a seeded user.",
    cypher: "MATCH (a:User {id: $id}), (b:User)\nWHERE a.age = b.age\nRETURN b.id"
  },
  {
    name: "Value Join Count",
    id: "value_join_cnt",
    description: "Counts matches for value-join shape.",
    cypher: "MATCH (a:User {id: $id}), (b:User)\nWHERE a.age = b.age\nRETURN count(b)"
  },
  {
    name: "Order by Age",
    id: "order_by_age",
    description: "Full sort over users by age then id.",
    cypher: "MATCH (n:User)\nRETURN n.id, n.age\nORDER BY n.age, n.id"
  },
  {
    name: "Unwind Rows",
    id: "unwind_rows",
    description: "UNWIND fan-out from row-local values.",
    cypher: "MATCH (n:User {id: $id})\nUNWIND [n.id, n.id + 1, n.id + 2] AS x\nRETURN x"
  },
  {
    name: "Variable Length Friends",
    id: "var_len_friends",
    description: "Variable-length expansion (1..2 hops).",
    cypher: "MATCH (a:User {id: $id})-[*1..2]->(b:User)\nRETURN b.id"
  },
  {
    name: "Optional Friend",
    id: "optional_friend",
    description: "OPTIONAL MATCH expansion from seeded user.",
    cypher: "MATCH (a:User {id: $id})\nOPTIONAL MATCH (a)-->(b:User)\nRETURN a.id, b.id"
  },
  {
    name: "Call Subquery",
    id: "call_subquery",
    description: "Correlated subquery using CALL { ... }.",
    cypher: "MATCH (a:User {id: $id})\nCALL {\n  WITH a\n  MATCH (a)-->(b:User)\n  RETURN b.id AS bid\n}\nRETURN bid"
  },
  {
    name: "MERGE User (Insert Path)",
    id: "merge_user_insert_path",
    description: "MERGE branch that creates a new User when id does not exist.",
    cypher: "MERGE (u:User {id: $id})\nON CREATE SET u.created_at = timestamp(), u.age = $age\nRETURN u.id"
  },
  {
    name: "MERGE User (Upsert Existing)",
    id: "merge_user_upsert_existing",
    description: "MERGE branch that updates an existing User via ON MATCH.",
    cypher: "MERGE (u:User {id: $id})\nON CREATE SET u.created_at = timestamp()\nON MATCH SET u.age = $age, u.last_seen = timestamp()\nRETURN u.id"
  },
  {
    name: "MERGE Friend Edge (Upsert)",
    id: "merge_friend_edge_upsert",
    description: "MERGE on relationship pattern with ON CREATE/ON MATCH updates.",
    cypher: "MATCH (a:User {id: $from}), (b:User {id: $to})\nMERGE (a)-[r:Friend]->(b)\nON CREATE SET r.since = date()\nON MATCH SET r.touch = date()\nRETURN id(r)"
  },
  {
    name: "Detach Delete User",
    id: "detach_delete_user",
    description: "Deletes a user and all incident relationships.",
    cypher: "MATCH (u:User {id: $id})\nDETACH DELETE u"
  },
  {
    name: "Remove Property and Label",
    id: "remove_user_property_and_label",
    description: "Exercises REMOVE on both property and label targets.",
    cypher: "MATCH (u:User {id: $id})\nREMOVE u.rpc_social_credit, u:TemporaryLabel\nRETURN u.id"
  },
  {
    name: "FOREACH Loop Mutation",
    id: "foreach_loop_mutation",
    description: "Uses FOREACH to apply repeated SET mutations in one query.",
    cypher: "MATCH (u:User {id: $id})\nFOREACH (x IN [1,2,3] | SET u.loop_counter = x)\nRETURN u.loop_counter"
  },
  {
    name: "UNION ALL IDs",
    id: "union_all_ids",
    description: "UNION ALL composition without deduplication.",
    cypher: "MATCH (u:User {id: $id})\nRETURN u.id AS uid\nUNION ALL\nMATCH (v:User) WHERE v.id < 10\nRETURN v.id AS uid"
  },
  {
    name: "UNION Distinct IDs",
    id: "union_distinct_ids",
    description: "UNION composition with distinct semantics.",
    cypher: "MATCH (u:User {id: $id})\nRETURN u.id AS uid\nUNION\nMATCH (v:User {id: $id})\nRETURN v.id AS uid"
  },
  {
    name: "All Shortest Paths Length",
    id: "all_shortest_paths_len",
    description: "allShortestPaths coverage with vendor-specific syntax.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nWITH s, t\nMATCH p = allShortestPaths((s)-[:Friend*1..4]->(t))\nRETURN length(p)\n\n// Neo4j:\nMATCH (s:User {id: $from}), (t:User {id: $to})\nMATCH p = allShortestPaths((s)-[:Friend*1..4]->(t))\nRETURN length(p)\n\n// Memgraph:\nMATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to})\nRETURN length(p)"
  },
  {
    name: "Var-Length with Edge Filter",
    id: "var_len_with_edge_where_filter",
    description: "Variable-length traversal with edge property filtering.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User)\nWHERE r.bench_capacity >= $min_capacity\nRETURN count(t)\n\n// Neo4j / Memgraph:\nMATCH (s:User {id: $id})-[r:Friend*1..3]->(t:User)\nWHERE all(rel IN r WHERE rel.bench_capacity >= $min_capacity)\nRETURN count(t)"
  },
  {
    name: "Exact 5-Hop Traverse Count",
    id: "exact_5_hop_traverse_count",
    description: "Fixed-depth 5-hop traversal count for deeper expansion profiling.",
    cypher: "MATCH (s:User {id: $id})-[:Friend*5..5]->(t:User)\nRETURN count(t) AS cnt"
  },
  {
    name: "Exact 6-Hop Traverse Count",
    id: "exact_6_hop_traverse_count",
    description: "Fixed-depth 6-hop traversal count for depth scaling analysis.",
    cypher: "MATCH (s:User {id: $id})-[:Friend*6..6]->(t:User)\nRETURN count(t) AS cnt"
  },
  {
    name: "Count Users (Plain)",
    id: "count_users_plain",
    description: "Simple node count used for count-reduction optimizer paths.",
    cypher: "MATCH (u:User)\nRETURN count(u) AS cnt"
  },
  {
    name: "Count Friend Edges (Plain)",
    id: "count_friend_edges_plain",
    description: "Simple edge count used for relationship count-reduction paths.",
    cypher: "MATCH ()-[r:Friend]->()\nRETURN count(r) AS cnt"
  },
  {
    name: "Indexed OR Predicate",
    id: "indexed_or_predicate",
    description: "Predicate shape intended to trigger OR index utilization.",
    cypher: "MATCH (u:User)\nWHERE u.id = $id1 OR u.id = $id2\nRETURN u.id"
  },
  {
    name: "Indexed IN-List Predicate",
    id: "indexed_in_list_predicate",
    description: "IN-list predicate shape intended to trigger index utilization.",
    cypher: "MATCH (u:User)\nWHERE u.id IN [$id1, $id2, $id3, $id4]\nRETURN u.id"
  },
  {
    name: "Entity and Path Introspection",
    id: "entity_path_introspection",
    description: "Covers labels/type/properties and path decomposition functions.",
    cypher: "MATCH p=(a:User {id: $id})-[r:Friend]->(b:User)\nRETURN labels(a), type(r), properties(a), nodes(p), relationships(p), length(p)\nLIMIT 1"
  },
  {
    name: "ID Seek (Columnar)",
    id: "id_seek",
    description: "Internal id point lookup (columnar/id-path coverage).",
    cypher: "MATCH (n)\nWHERE id(n) = $id\nRETURN n.id"
  },
  {
    name: "ID Range Scan (Columnar)",
    id: "id_range_scan",
    description: "Internal id range scan for columnar fan-out behavior.",
    cypher: "MATCH (n)\nWHERE id(n) >= $start AND id(n) < $end\nRETURN n.id"
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
                            <pre className="bg-gray-900 text-gray-100 p-2 rounded text-[10px] font-mono overflow-x-auto whitespace-pre leading-normal">
                              {q.cypher}
                            </pre>
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
                className={`flex gap-3 ${
                  group.layout === "row" ? "flex-row" : "flex-col"
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
                      className={`flex items-center gap-2 w-full ${
                        group.title === "Queries"
                          ? "text-sm flex-wrap justify-center"
                          : ""
                      }`}
                    >
                      <button
                        onClick={() => handleSideBarSelection(group.title, option.id)}
                        className={`font-fira px-4 py-1 rounded-lg border text-center w-full ${getButtonClasses()}`}
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
