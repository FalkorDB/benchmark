"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import FooterComponent from "./footer";
import React, { useEffect, useState } from "react";
import { BenchmarkData } from "../types/benchmark";
import { useToast } from "@/hooks/use-toast";
import HorizontalBarChart from "./HorizontalBarChart";
import VerticalBarChart from "./VerticalBarChart";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";

export default function DashBoard() {
  const [data, setData] = useState<BenchmarkData | null>(null);
  const { toast } = useToast();
  // eslint-disable-next-line
  const [filteredResults, setFilteredResults] = useState<any[]>([]);
  const [selectedOptions, setSelectedOptions] = React.useState<
    Record<string, string[]>
  >({
    Vendors: ["falkordb", "neo4j"],
    Clients: ["20"],
    Throughput: ["500"],
    Hardware: ["linux"],
  });

  useEffect(() => {
    const fetchData = async () => {
      try {
        const response = await fetch("/resultData.json");

        if (!response.ok) {
          throw new Error(`HTTP error! status: ${response.status}`);
        }
        const jsonData = await response.json();
        setData(jsonData);
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : "An unknown error occurred";
        toast({
          title: "Error fetching data",
          description: errorMessage || "An unknown error occurred",
          variant: "destructive",
        });
      }
    };

    fetchData();
    // eslint-disable-next-line
  }, []);

  const handleSelection = (groupTitle: string, optionId: string) => {
    setSelectedOptions((prev) => {
      const groupSelections = prev[groupTitle] || [];

      if (groupTitle === "Vendors") {
        if (optionId === "falkordb" || optionId === "neo4j") {
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

  useEffect(() => {
    if (!data || !data.runs) return;

    const results = data.runs.filter((run) => {
      const isHardwareMatch =
        run.platform &&
        selectedOptions.Hardware.some((hardware) =>
          run.platform.toLowerCase().includes(hardware.toLowerCase())
        );

      const isVendorMatch =
        run.vendor &&
        selectedOptions.Vendors.some(
          (vendor) => vendor.toLowerCase() === run.vendor.toLowerCase()
        );
      const isClientMatch =
        run.clients !== undefined &&
        selectedOptions.Clients.some(
          (client) => client === String(run.clients)
        );
      const isThroughputMatch =
        run["target-messages-per-second"] !== undefined &&
        selectedOptions.Throughput.some(
          (throughput) =>
            throughput === String(run["target-messages-per-second"])
        );

      return (
        isVendorMatch && isClientMatch && isThroughputMatch && isHardwareMatch
      );
    });

    setFilteredResults(results);
  }, [data, selectedOptions]);

  const latencyData = filteredResults.map((item) => {
    const convertToMilliseconds = (value: string): number => {
      const match = value.match(/([\d.,]+)([a-zA-Z]+)/);
      if (match) {
        const numericValue = parseFloat(match[1].replace(",", "."));
        const unit = match[2].toLowerCase();
        return unit === "s" ? numericValue * 1000 : numericValue;
      }
      return 0;
    };

    return {
      vendor: item.vendor,
      p50: convertToMilliseconds(item.result.latency.p50),
      p95: convertToMilliseconds(item.result.latency.p95),
      p99: convertToMilliseconds(item.result.latency.p99),
    };
  });

  const throughputData = filteredResults.map((item) => ({
    vendor: item.vendor,
    actualMessagesPerSecond: item.result["actual-messages-per-second"],
  }));

  const deadlineData = filteredResults.map((item) => {
    const deadlineValue = item.result["deadline-offset"];
    const match = deadlineValue.match(/([\d.]+)([a-zA-Z]+)/);
    if (match) {
      const value = parseFloat(match[1]);
      const unit = match[2].toLowerCase();
      const deadlineInMin = unit === "ms" ? value / (60 * 1000) : value;
      return {
        vendor: item.vendor,
        deadline: deadlineInMin.toFixed(2),
      };
    }

    return {
      vendor: item.vendor,
      deadline: 0,
    };
  });

  const memoryData = filteredResults.map((item) => {
    const memoryValue = item.result["ram-usage"];
    const match = memoryValue.match(/([\d.]+)([a-zA-Z]+)/);
    if (match) {
      const value = parseFloat(match[1]);
      const unit = match[2].toUpperCase();
      const memoryInMB = unit === "GB" ? value * 1024 : value;
      return {
        vendor: item.vendor,
        memory: memoryInMB,
      };
    }

    return {
      vendor: item.vendor,
      memory: 0,
    };
  });

  const cpuData = filteredResults.map((item) => ({
    vendor: item.vendor,
    cpu: item.result["cpu-usage"]
      ? (item.result["cpu-usage"] * 100).toFixed(2)
      : "0.00",
  }));


  //saving data to window.allChartData
  /* eslint-disable */
  if (typeof window !== "undefined") {
    (window as any).allChartData = (window as any).allChartData || [];
    const addOrReplaceChartData = (key: string, value: any) => {
      const chartDataArray = (window as any).allChartData;
      const existingIndex = chartDataArray.findIndex((entry: any) => entry.key === key);
      if (existingIndex !== -1) {
        chartDataArray.splice(existingIndex, 1);
      }
      chartDataArray.push({ key, value });
    };

    addOrReplaceChartData("throughputData", throughputData);
    addOrReplaceChartData("deadlineData", deadlineData);
    addOrReplaceChartData("memoryData", memoryData);
    addOrReplaceChartData("cpuData", cpuData);
    addOrReplaceChartData("latencyData", latencyData);
  }
  
  
  return (
    <SidebarProvider className="h-screen w-screen overflow-hidden">
      <div className="flex h-full w-full">
        <AppSidebar
          selectedOptions={selectedOptions}
          handleSelection={handleSelection}
          platform={data?.platforms}
        />
        <SidebarInset className="flex-grow h-full min-h-0">
          <div className="grid h-full grid-cols-2 grid-rows-[2fr,1fr,1fr,50px] gap-2 p-1">
            <div className="col-span-2 bg-muted/50 rounded-xl p-4 min-h-0">
              <VerticalBarChart
                data={latencyData}
                title="Vendor Latency Metrics"
                subTitle="P50, P95, and P99 Latency Comparison (less is better)"
                xAxisTitle="Vendors"
              />
            </div>
            <div className="bg-muted/50 rounded-xl p-4 min-h-0">
              <HorizontalBarChart
                data={throughputData}
                dataKey="actualMessagesPerSecond"
                chartLabel="Messages Per Second"
                title="Throughput"
                subTitle="Performance Metrics (more is better)"
                yAxisTitle="Vendors"
                unit=""
              />
            </div>
            <div className="bg-muted/50 rounded-xl p-4 min-h-0 relative">
              <HoverCard>
                <HoverCardTrigger>
                  <span
                    className="absolute top-4 left-4 w-5 h-5 flex items-center justify-center bg-gray-400 text-white rounded-full text-xs font-bold cursor-pointer shadow-md z-10"
                    title="More info"
                  >
                    i
                  </span>
                </HoverCardTrigger>
                <HoverCardContent className="bg-gray-100 text-gray-800 p-4 rounded-md shadow-lg max-w-sm">
                  <p className="text-sm font-medium">
                    <strong>Deadline Offset Analysis</strong> Comparison of the
                    time delays (deadlines) between different vendors to
                    evaluate their performance and responsiveness.
                  </p>
                </HoverCardContent>
              </HoverCard>
              <HorizontalBarChart
                data={deadlineData}
                dataKey="deadline"
                chartLabel="Deadline Offset (min)"
                title="Deadline Offset Analysis"
                subTitle="Offset Comparison (less is better)"
                yAxisTitle="Vendors"
                unit="min"
              />
            </div>
            <div className="bg-muted/50 rounded-xl p-4 min-h-0">
              <HorizontalBarChart
                data={memoryData}
                dataKey="memory"
                chartLabel="Memory Utilization (MB)"
                title="Memory Usage"
                subTitle="Memory Allocation (less is better)"
                yAxisTitle="Vendors"
                unit="mb"
              />
            </div>
            <div className="bg-muted/50 rounded-xl p-4 min-h-0">
              <HorizontalBarChart
                data={cpuData}
                dataKey="cpu"
                chartLabel="CPU Utilization (%)"
                title="CPU Usage"
                subTitle="Core Utilization (less is better)"
                yAxisTitle="Vendors"
                unit="%"
              />
            </div>
            <div className="col-span-2 bg-muted/50 rounded-xl flex items-center justify-center h-[50px]">
              <FooterComponent />
            </div>
          </div>
        </SidebarInset>
      </div>
    </SidebarProvider>
  );
}
