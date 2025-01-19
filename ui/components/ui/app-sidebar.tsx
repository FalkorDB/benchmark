"use client";

import * as React from "react";
import { NavMain } from "@/components/ui/nav-main";
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
  handleSelection,
  platform,
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSelection: (groupTitle: string, optionId: string) => void;
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
          handleSelection={handleSelection}
          platform={platform}
        />
      </SidebarContent>
    </Sidebar>
  );
}
