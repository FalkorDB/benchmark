"use client";

import * as React from "react";
import {
  SidebarMenu,
  SidebarMenuButton,
} from "@/components/ui/sidebar";
import Image from "next/image";
import icon from "../../public/favicon.svg";

export function SidebarBrand({
  teams,
}: {
  teams: {
    name: string;
    plan: string;
  }[];
}) {
  const [activeTeam] = React.useState(teams[0]);

  return (
    <SidebarMenu>
      <SidebarMenuButton
        size="lg"
        className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
      >
        <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
          <Image src={icon} alt="FalkorDB" width={130} height={25} />
        </div>
        <div className="grid flex-1 text-left text-sm leading-tight">
          <span className="truncate font-semibold">{activeTeam.name}</span>
          <span className="truncate text-xs">{activeTeam.plan}</span>
        </div>
      </SidebarMenuButton>
    </SidebarMenu>
  );
}
