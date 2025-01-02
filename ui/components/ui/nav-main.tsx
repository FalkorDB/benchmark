"use client";

import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";

export function NavMain({
  items,
  selectedOptions,
  handleSelection,
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
}) {
  const { state } = useSidebar(); // Get the sidebar state (collapsed/expanded)

  return (
    <SidebarMenu>
      {items.map((group) => (
        <SidebarMenuItem
          key={group.title}
          className={`mt-6 ${
            state === "collapsed" ? "flex justify-center" : ""
          }`}
        >
          <SidebarMenuButton
            className={`flex items-center gap-3 pl-4 ${
              state === "collapsed" ? "justify-center" : ""
            }`}
          >
            {/* Icon always visible */}
            <group.icon
              className={`w-6 h-6 ${state === "collapsed" ? "mx-auto" : ""}`}
            />
            {/* Title only visible when sidebar is expanded */}
            {state !== "collapsed" && (
              <h2 className="text-lg font-semibold">{group.title}</h2>
            )}
          </SidebarMenuButton>

          {state !== "collapsed" && (
            <div className="pl-8 pr-4 mt-2">
              {/* Added pr-4 for padding-right */}
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
                {group.options.map((option) => (
                  <button
                    key={option.id}
                    onClick={() => handleSelection(group.title, option.id)}
                    className={`px-4 py-2 text-left rounded-md border text-center ${
                      selectedOptions[group.title]?.includes(option.id)
                        ? "bg-[#F5F4FF] text-[#7466FF] border-[#7466FF]"
                        : "bg-gray-100 text-gray-800 border-transparent"
                    }`}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>
          )}
        </SidebarMenuItem>
      ))}
    </SidebarMenu>
  );
}
