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
    name: "Expand 4L",
    id: "aggregate_expansion_4",
    description: "Expands 4 levels deep in the graph and returns distinct user IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 4L (Filtered)",
    id: "aggregate_expansion_4_with_filter",
    description: "Expands 4 levels deep in the graph with an age filter (age >= 18) on the destination nodes.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 3L",
    id: "aggregate_expansion_3",
    description: "Expands 3 levels deep in the graph and returns distinct user IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 3L (Filtered)",
    id: "aggregate_expansion_3_with_filter",
    description: "Expands 3 levels deep in the graph with an age filter (age >= 18) on the destination nodes.",
    cypher: "MATCH (s:User {id: $id})-->()-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 2L",
    id: "aggregate_expansion_2",
    description: "Expands 2 levels deep in the graph and returns distinct user IDs.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 2L (Filtered)",
    id: "aggregate_expansion_2_with_filter",
    description: "Expands 2 levels deep in the graph with an age filter (age >= 18) on the destination nodes.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN DISTINCT n.id"
  },
  {
    name: "Expand 1L",
    id: "aggregate_expansion_1",
    description: "Expands 1 level deep in the graph and returns connected user IDs.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nRETURN n.id"
  },
  {
    name: "Expand 1L (Filtered)",
    id: "aggregate_expansion_1_with_filter",
    description: "Expands 1 level deep in the graph with an age filter (age >= 18) on connected user nodes.",
    cypher: "MATCH (s:User {id: $id})-->(n:User)\nWHERE n.age >= 18\nRETURN n.id"
  },
  {
    name: "Aggregate age (Filtered)",
    id: "aggregate_age_filtered",
    description: "Calculates the average age of all users aged 18 or older.",
    cypher: "MATCH (n:User)\nWHERE n.age >= 18\nRETURN avg(n.age) AS avg_age"
  },
  {
    name: "Count users",
    id: "aggregate_count_users",
    description: "Retrieves the total count of user nodes. Uses optimized stats in FalkorDB.",
    cypher: "// FalkorDB:\nCALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt\n\n// Neo4j/Memgraph:\nMATCH (n:User) RETURN count(n) AS cnt"
  },
  {
    name: "Neighbours 2L (data+filter)",
    id: "neighbours_2_with_data_and_filter",
    description: "Retrieves 2-hop neighbor nodes (with age >= 18) and returns full properties.",
    cypher: "MATCH (s:User {id: $id})-->()-->(n:User)\nWHERE n.age >= 18\nRETURN n"
  },
  {
    name: "Shortest path",
    id: "shortest_path",
    description: "Finds the shortest path between two users and returns its length.",
    cypher: "// FalkorDB:\nMATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p RETURN length(p)\n\n// Neo4j:\nMATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) RETURN length(p)\n\n// Memgraph:\nMATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) RETURN length(p)"
  },
  {
    name: "Write Edge",
    id: "single_edge_write",
    description: "Matches two users by ID and creates a Friend relationship between them.",
    cypher: "MATCH (n:User {id: $from}), (m:User {id: $to})\nWITH n, m\nCREATE (n)-[e:Friend]->(m)\nRETURN e"
  },
  {
    name: "Write Vertex",
    id: "single_vertex_write",
    description: "Creates a new User vertex with a specific ID.",
    cypher: "CREATE (n:User {id: $id})\nRETURN n"
  },
  {
    name: "Write General",
    id: "write",
    description: "Updates attributes of a User node.",
    cypher: "MATCH (n:User {id: $id})\nSET n.rpc_social_credit = $rpc_social_credit\nRETURN n"
  },
  {
    name: "Read Vertex",
    id: "single_vertex_read",
    description: "Retrieves a single User vertex by ID.",
    cypher: "MATCH (n:User {id: $id})\nRETURN n"
  }
];

export function NavMain({
  items,
  selectedOptions,
  handleSideBarSelection,
  platform,
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
                  {datasetSummary.startedAtEpochSecs && (
                    <div className="flex justify-between gap-4 pb-1 mb-1 border-b border-gray-200/40">
                      <span className="text-gray-500">Date &amp; time</span>
                      <span className="tabular-nums text-right">
                        {(() => {
                          const date = new Date(datasetSummary.startedAtEpochSecs * 1000);
                          const pad = (num: number) => String(num).padStart(2, "0");
                          const yyyy = date.getUTCFullYear();
                          const mm = pad(date.getUTCMonth() + 1);
                          const dd = pad(date.getUTCDate());
                          const hh = pad(date.getUTCHours());
                          const min = pad(date.getUTCMinutes());
                          const ss = pad(date.getUTCSeconds());
                          return `${yyyy}-${mm}-${dd} ${hh}:${min}:${ss} UTC`;
                        })()}
                      </span>
                    </div>
                  )}
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
                        {QUERY_DESCRIPTIONS.map((q) => (
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
                {group.options.map((option, index) => {
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
