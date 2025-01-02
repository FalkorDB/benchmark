"use client";

import * as React from "react";
import { NavMain } from "@/components/ui/nav-main";
import { NavUser } from "@/components/ui/nav-user";
import { TeamSwitcher } from "@/components/ui/team-switcher";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarRail,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import {
  GalleryVerticalEnd,
  Layers,
  Cpu,
  Send,
  Users,
} from "lucide-react";

const data2: {
  sidebarData: {
    title: string;
    description?: string;
    layout?: "row" | "col";
    icon: React.ElementType;
    options: { id: string; label: string }[];
  }[];
} = {
  sidebarData: [
    {
      title: "Vendors",
      description: "",
      layout: "row",
      icon: Layers,
      options: [
        { id: "falkordb", label: "FalkorDB" },
        { id: "neo4j", label: "Neo4j" },
      ],
    },
    {
      title: "Hardware",
      description: "",
      layout: "col",
      icon: Cpu,
      options: [
        { id: "macbook", label: "Machine: MacBook" },
        { id: "linux", label: "Machine: Linux" },
      ],
    },
    {
      title: "Throughput",
      description: "Message request/second",
      layout: "col",
      icon: Send,
      options: [
        { id: "2600", label: "2600 messages/second" },
        { id: "400", label: "400 messages/second" },
      ],
    },
    {
      title: "Clients",
      description: "Number of parallel queries",
      layout: "row",
      icon: Users,
      options: [
        { id: "10", label: "10" },
        { id: "20", label: "20" },
        { id: "100", label: "100" },
      ],
    },
  ],
};

const data = {
  user: {
    name: "FalkorDB User",
    email: "info@falkordb.com",
    avatar: "/avatars/shadcn.jpg",
  },
  teams: [
    {
      name: "FalkorDB",
      logo: GalleryVerticalEnd,
      plan: "Enterprise",
    },
  ],
};

export function AppSidebar({
  selectedOptions,
  handleSelection,
  ...props
}: React.ComponentProps<typeof Sidebar> & {
  selectedOptions: Record<string, string[]>;
  handleSelection: (groupTitle: string, optionId: string) => void;
}) {
  return (
    <Sidebar collapsible="icon" {...props} className="pt-dynamic">
      <SidebarHeader>
        <TeamSwitcher teams={data.teams} />
        <SidebarTrigger className="ml-auto" />
      </SidebarHeader>
      <SidebarContent>
        <NavMain
          items={data2.sidebarData}
          selectedOptions={selectedOptions}
          handleSelection={handleSelection}
        />
      </SidebarContent>
      <SidebarFooter>
        <NavUser user={data.user} />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  );
}
