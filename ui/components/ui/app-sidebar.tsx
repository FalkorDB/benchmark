"use client";

import * as React from "react";
import { NavMain } from "@/components/ui/SidebarNavigation";
import { SidebarBrand } from "@/components/ui/SidebarBrand";
import {
  Sidebar,
  SidebarContent,
  SidebarHeader,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { sidebarConfig, userBrandInfo } from "@/app/data/sideBarData";

type Platforms = Record<
  string,
  {
    cpu: string;
    ram: string;
    storage: string;
  }
>;

export function AppSidebar({
  selectedOptions,
  handleSideBarSelection,
  platform,
  hideHardware,
  allowedVendors,
  throughputOptions,
  queryOptions,
  datasetSummary,
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSideBarSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
  hideHardware?: boolean;
  allowedVendors?: string[];
  throughputOptions?: Array<string | number>;
  queryOptions?: string[];
  datasetSummary?: {
    nodes: number;
    edges: number;
    readQueries: number;
    writeQueries: number;
    startedAtEpochSecs?: number;
  } | null;
}) {
  const filteredSidebarItems = React.useMemo(() => {
    const allowed = (allowedVendors ?? []).map((v) => v.toLowerCase());
    const throughputs = (throughputOptions ?? []).map((t) => String(t));
    const queries = queryOptions ?? [];
    const staticQueryLabels = new Map(
      (
        sidebarConfig.sidebarData.find((group) => group.title === "Queries")
          ?.options ?? []
      ).map((option) => [option.id, option.label])
    );

    const labelForVendor = (id: string) => {
      const k = (id ?? "").toString();
      const lower = k.toLowerCase();
      if (lower === "falkordb" || lower === "falkor") return "FalkorDB";
      if (lower === "falkordb1" || lower === "falkordb-c") return "FalkorDB (Standard)";
      if (lower === "falkordb2" || lower === "falkordb-rs") return "FalkorDB (Rust)";
      if (lower === "neo4j") return "Neo4j";
      if (lower === "memgraph") return "Memgraph";
      if (lower === "intel") return "Intel";
      if (lower === "graviton") return "Graviton";
      // Generic fallback: Title Case
      return lower.replace(/(^|\s|[-_])([a-z])/g, (_, p1, p2) => `${p1}${p2.toUpperCase()}`);
    };
    const labelForQuery = (id: string) => {
      const staticLabel = staticQueryLabels.get(id);
      if (staticLabel) return staticLabel;

      return id
        .split("_")
        .map((token) => {
          if (!token) return token;
          if (token.length <= 3) return token.toUpperCase();
          return token.charAt(0).toUpperCase() + token.slice(1);
        })
        .join(" ");
    };

    return sidebarConfig.sidebarData
      .filter((group) => !(hideHardware && group.title === "Hardware"))
      .map((group) => {
      if (group.title === "Vendors" && allowed.length) {
        return {
          ...group,
          options: allowed.map((id) => ({ id, label: labelForVendor(id) })),
        };
      }

      if (group.title === "Throughput" && throughputs.length) {
        return {
          ...group,
          options: throughputs.map((t) => ({ id: t, label: t })),
        };
      }

      if (group.title === "Queries" && queries.length) {
        return {
          ...group,
          options: queries.map((id) => ({ id, label: labelForQuery(id) })),
        };
      }

      return group;
    });
  }, [allowedVendors, throughputOptions, queryOptions, hideHardware]);

  return (
    <Sidebar
      collapsible="icon"
      {...props}
      className="flex flex-col h-screen-dynamic"
    >
      <SidebarHeader className="mt-20">
        <SidebarBrand Brand={userBrandInfo?.brand} />
        <SidebarTrigger className="ml-auto" />
      </SidebarHeader>
      <SidebarContent className="pb-20">
        <NavMain
          items={filteredSidebarItems ?? []}
          selectedOptions={selectedOptions}
          handleSideBarSelection={handleSideBarSelection}
          platform={platform}
          hideHardware={hideHardware}
          datasetSummary={datasetSummary}
        />
      </SidebarContent>
    </Sidebar>
  );
}
