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
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSideBarSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
}) {
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
          items={sidebarConfig?.sidebarData}
          selectedOptions={selectedOptions}
          handleSideBarSelection={handleSideBarSelection}
          platform={platform}
        />
      </SidebarContent>
    </Sidebar>
  );
}
