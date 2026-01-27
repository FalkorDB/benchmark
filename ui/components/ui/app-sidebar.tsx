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
  allowedVendors,
  throughputOptions,
  datasetSummary,
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSideBarSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
  allowedVendors?: string[];
  throughputOptions?: Array<string | number>;
  datasetSummary?: {
    nodes: number;
    edges: number;
    readQueries: number;
    writeQueries: number;
  } | null;
}) {
  const filteredSidebarItems = React.useMemo(() => {
    const allowed = (allowedVendors ?? []).map((v) => v.toLowerCase());
    const throughputs = (throughputOptions ?? []).map((t) => String(t));

    const labelForVendor = (id: string) => {
      const k = (id ?? "").toString();
      const lower = k.toLowerCase();
      if (lower === "falkordb" || lower === "falkor") return "FalkorDB";
      if (lower === "neo4j") return "Neo4j";
      if (lower === "memgraph") return "Memgraph";
      if (lower === "intel") return "Intel";
      if (lower === "graviton") return "Graviton";
      // Generic fallback: Title Case
      return lower.replace(/(^|\s|[-_])([a-z])/g, (_, p1, p2) => `${p1}${p2.toUpperCase()}`);
    };

    return sidebarConfig.sidebarData.map((group) => {
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

      return group;
    });
  }, [allowedVendors, throughputOptions]);

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
          datasetSummary={datasetSummary}
        />
      </SidebarContent>
    </Sidebar>
  );
}
