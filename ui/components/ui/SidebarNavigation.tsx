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
                    <HoverCardContent className="bg-gray-100 text-gray-800 p-4 rounded-md shadow-lg max-w-sm">
                      <p className="text-sm font-medium">
                        <strong>Query Operations</strong>:
                        <br />
                        <strong>4L</strong> → Expands 4 levels deep in the
                        graph.
                        <br />
                        <strong>3L</strong> → Expands 3 levels deep.
                        <br />
                        <strong>2L</strong> → Expands 2 levels deep.
                        <br />
                        <strong>1L</strong> → Expands 1 level deep.
                        <br />
                        <strong>(Filtered)</strong> → Applies filters to limit
                        results.
                        <br />
                        <strong>Write Edge</strong> → Adds a relationship
                        between nodes.
                        <br />
                        <strong>Write Vertex</strong> → Adds a new node.
                        <br />
                        <strong>Read Vertex</strong> → Retrieves a single node.
                      </p>
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
