"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import {
  SidebarInset,
  SidebarProvider,
} from "@/components/ui/sidebar";
import MetricsComponent from "../components/metricsComponet";
import FooterComponent from "../components/footer";
import React, { useEffect, useState } from "react";
import ResultsComponent from "../components/ResultComponent";

export default function SideBar() {
    const [data, setData] = useState<any>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedOptions, setSelectedOptions] = React.useState<Record<string, string[]>>({
    Vendors: ["falkordb"],
    Clients: ["10"],
    Throughput: ["400"],
    Hardware: ["linux"],
  });

  useEffect(() => {
    const fetchData = async () => {
      try {
        
        const result = await fetch(`/api/benchmark`, {
          method: "GET",
        });

        if (!result.ok) {
          throw new Error(`Failed to fetch data: ${result.statusText}`);
        }

        const json = await result.json();
        console.log(json);
        
        setData(json.result.data);
      } catch (err: any) {
        setError(err.message || "An unknown error occurred");
      }
    };

    fetchData();
    console.log(data);
    
  }, []);

  const handleSelection = (groupTitle: string, optionId: string) => {
    setSelectedOptions((prev) => {
      const groupSelections = prev[groupTitle] || [];

      if (groupTitle === "Vendors") {
        if (optionId === "falkordb") {
          return prev;
        }
        const updatedSelections = groupSelections.includes(optionId)
          ? groupSelections.filter((id) => id !== optionId)
          : [...groupSelections, optionId];
        return {
          ...prev,
          [groupTitle]: updatedSelections,
        };
      }
      return {
        ...prev,
        [groupTitle]: [optionId],
      };
    });
  };

  return (
    <SidebarProvider className="h-full">
      <AppSidebar
        selectedOptions={selectedOptions}
        handleSelection={handleSelection}
      />
      <SidebarInset className="pt-5 flex flex-col h-full">
        <div className="flex flex-1 flex-col gap-4 overflow-hidden">
          <div className="flex flex-1 flex-row gap-4 overflow-hidden">
            <div className="w-1/4 md:w-1/3 lg:w-1/4 min-w-[100px] rounded-xl bg-muted/50">
              <MetricsComponent />
            </div>
            <div className="w-3/4 md:w-2/3 lg:w-3/4 min-w-[200px] rounded-xl bg-muted/50">
              <div className="h-full p-4"><ResultsComponent/></div>
            </div>
          </div>
          <div className="h-14 w-full rounded-xl bg-muted/50 p-0 flex-shrink-0">
            <div className="h-full flex items-center justify-center">
              <FooterComponent />
            </div>
          </div>
        </div>
      </SidebarInset>
    </SidebarProvider>
  );
}
