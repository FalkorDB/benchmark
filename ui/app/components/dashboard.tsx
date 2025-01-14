"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import FooterComponent from "./footer";
import React, { useEffect, useState } from "react";
import { BenchmarkData } from "../types/benchmark";
import { useToast } from "@/hooks/use-toast";
import HorizontalBarChart from "./HorizontalBarChart";
import VerticalBarChart from "./VerticalBarChart";

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
        const response = await fetch("/resutData.json");

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
      const deadlineInMs = unit === "min" ? value * 60 * 1000 : value;
      return {
        vendor: item.vendor,
        deadline: deadlineInMs,
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
    cpu: item.result["cpu-usage"],
  }));

  return (
    <SidebarProvider className="h-full">
      <AppSidebar
        selectedOptions={selectedOptions}
        handleSelection={handleSelection}
        platform={data?.platforms}
      />
      <SidebarInset className="pt-2 flex flex-col h-full">
        <div className="flex flex-1 flex-col gap-2 overflow-hidden">
          <div className="flex flex-col h-full w-full gap-2 px-2">
            <div className="flex-1 w-full bg-muted/50 rounded-xl p-4">
              <VerticalBarChart
                data={latencyData}
                title="Vendor Latency Metrics"
                subtitle="P50, P95, and P99 Latency Comparison (less is better)"
                xAxisTitle="Vendors"
              />
            </div>
            <div className="flex w-full h-1/4 gap-2">
              <div className="flex-1 bg-muted/50 rounded-xl p-4">
                <HorizontalBarChart
                  data={throughputData}
                  dataKey="actualMessagesPerSecond"
                  chartLabel="Messages Per Second"
                  title="Throughput"
                  subtitle="Performance Metrics (more is better)"
                  yAxisTitle="Vendors"
                />
              </div>
              <div className="flex-1 bg-muted/50 rounded-xl p-4">
                <HorizontalBarChart
                  data={deadlineData}
                  dataKey="deadline"
                  chartLabel="Deadline Offset (ms)"
                  title="Deadline Offset Analysis"
                  subtitle="Offset Comparison (less is better)"
                  yAxisTitle="Vendors"
                />
              </div>
            </div>

            <div className="flex w-full h-1/4 gap-2">
              <div className="flex-1 bg-muted/50 rounded-xl p-4">
                <HorizontalBarChart
                  data={memoryData}
                  dataKey="memory"
                  chartLabel="Memory Utilization (MB)"
                  title="Memory Usage"
                  subtitle="Memory Allocation (less is better)"
                  yAxisTitle="Memory Slots"
                />
              </div>
              <div className="flex-1 bg-muted/50 rounded-xl p-4">
                <HorizontalBarChart
                  data={cpuData}
                  dataKey="cpu"
                  chartLabel="CPU Utilization (%)"
                  title="CPU Usage"
                  subtitle="Core Utilization (less is better)"
                  yAxisTitle="Cores"
                />
              </div>
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
