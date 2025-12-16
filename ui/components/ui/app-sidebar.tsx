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
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSideBarSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
  allowedVendors?: string[];
  throughputOptions?: Array<string | number>;
}) {
  const filteredSidebarItems = React.useMemo(() => {
    const allowed = (allowedVendors ?? []).map((v) => v.toLowerCase());
    const throughputs = (throughputOptions ?? []).map((t) => String(t));

    return sidebarConfig.sidebarData.map((group) => {
      if (group.title === "Vendors" && allowed.length) {
        return {
          ...group,
          options: group.options.filter((o) => allowed.includes(o.id.toLowerCase())),
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
        />
      </SidebarContent>
    </Sidebar>
  );
}
