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
  handleSelection,
  platform,
}: {
  items: {
    title: string;
    description?: string;
    layout?: "row" | "col";
    icon: React.ElementType;
    options: { id: string; label: string }[];
  }[];
  selectedOptions: Record<string, string[]>;
  handleSelection: (groupTitle: string, optionId: string) => void;
  platform?: Platforms;
}) {
  const { state } = useSidebar();

  return (
    <SidebarMenu>
      {items.map((group) => (
        <SidebarMenuItem
          key={group.title}
          className={`font-space mt-2 ${
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
              <h2 className="text-lg font-semibold">{group.title}</h2>
            )}
          </SidebarMenuButton>

          {state !== "collapsed" && (
            <div className="pl-8 pr-4 mt-2">
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
                {group.options.map((option, index) => (
                  <div
                    key={option.id}
                    className="flex items-center gap-2 w-full"
                  >
                    <button
                      onClick={() => handleSelection(group.title, option.id)}
                      className={`font-fira px-4 py-2 rounded-md border text-center w-full ${
                        selectedOptions[group.title]?.includes(option.id)
                          ? option.id === "falkordb"
                            ? "bg-[#F5F4FF] text-[#FF66B3] border-[#FF66B3]"
                            : option.id === "neo4j"
                            ? "bg-[#F5F4FF] text-[#0B6190] border-[#0B6190]"
                            : "bg-[#F5F4FF] text-[#7466FF] border-[#7466FF]"
                          : "bg-gray-100 text-gray-800 border-transparent"
                      }`}
                    >
                      {option.label}
                    </button>
                    {group.title === "Hardware" &&
                      platform &&
                      platform[index] && (
                        <HoverCard>
                          <HoverCardTrigger>
                            <span className="inline-block w-4 h-4 bg-blue-200 text-blue-600 rounded-full text-center text-xs font-bold cursor-pointer">
                              i
                            </span>
                          </HoverCardTrigger>
                          <HoverCardContent className="bg-gray-50 text-gray-900 p-4 rounded-lg shadow-md max-w-sm border border-gray-200">
                            <div className="space-y-2">
                              <p className="text-sm">
                                <strong className="font-semibold text-gray-700">
                                  CPU:
                                </strong>{" "}
                                {platform[index].cpu}
                              </p>
                              <p className="text-sm">
                                <strong className="font-semibold text-gray-700">
                                  RAM:
                                </strong>{" "}
                                {platform[index].ram}
                              </p>
                              <p className="text-sm">
                                <strong className="font-semibold text-gray-700">
                                  Storage:
                                </strong>{" "}
                                {platform[index].storage}
                              </p>
                            </div>
                          </HoverCardContent>
                        </HoverCard>
                      )}
                  </div>
                ))}
              </div>
            </div>
          )}
        </SidebarMenuItem>
      ))}
    </SidebarMenu>
  );
}
