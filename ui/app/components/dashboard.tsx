"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import FooterComponent from "./footer";
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { BenchmarkData } from "../types/benchmark";
import { useToast } from "@/hooks/use-toast";
import HorizontalBarChart from "./HorizontalBarChart";
import VerticalBarChart from "./VerticalBarChart";

export default function DashBoard() {
  const [data, setData] = useState<BenchmarkData | null>(null);
  const { toast } = useToast();
  const [gridKey, setGridKey] = useState(0);
  // eslint-disable-next-line
  const [filteredResults, setFilteredResults] = useState<any[]>([]);
  const [filteredUnrealistic, setFilteredUnrealistic] = useState<
    { vendor: string; histogram: number[] }[]
  >([]);
  const [selectedOptions, setSelectedOptions] = React.useState<
    Record<string, string[]>
  >({
    Realistic: ["on"],
    Vendors: ["falkordb", "neo4j"],
    Clients: ["20"],
    Throughput: ["2500"],
    Hardware: ["intel"],
    Queries: ["aggregate_expansion_3"],
    "Realistic Workload": ["1"],
  });

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch("/resultData.json");
      if (!response.ok)
        throw new Error(`HTTP error! status: ${response.status}`);
      setData(await response.json());
    } catch (error) {
      toast({
        title: "Error fetching data",
        description: error instanceof Error ? error.message : "Unknown error",
        variant: "destructive",
      });
    }
  }, [toast]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  const handleSideBarSelection = (groupTitle: string, optionId: string) => {
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
    if (!data || !data.unrealstic || !selectedOptions.Queries) {
      setFilteredUnrealistic([]);
      return;
    }
    const selectedQuery = selectedOptions.Queries[0];

    setFilteredUnrealistic(
      data.unrealstic
        .map(({ vendor, histogram_for_type }) => ({
          vendor,
          histogram: histogram_for_type[selectedQuery] || [],
        }))
        .filter((entry) => entry.histogram.length > 0)
    );
  }, [data, selectedOptions.Queries]);

  useEffect(() => {
    if (!data || !data.runs) {
      setFilteredResults([]);
      return;
    }

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
        selectedOptions.Clients.includes(String(run.clients));

      const isThroughputMatch =
        run["target-messages-per-second"] !== undefined &&
        selectedOptions.Throughput.includes(
          String(run["target-messages-per-second"])
        );

      return (
        isVendorMatch && isClientMatch && isThroughputMatch && isHardwareMatch
      );
    });

    setFilteredResults(results);
  }, [data, selectedOptions]);

  const latencyDataForRealistic = filteredResults.map((item) => {
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

  const getBarColor = useCallback((vendor: string) => {

    return (
      getComputedStyle(document.documentElement)
        .getPropertyValue(`--${vendor}-color`)
        .trim() || "#191919"
    );
  }, []);

  const chartDataForUnrealistic = useMemo(() => {
    if (!filteredUnrealistic.length) return { labels: [], datasets: [] };

    const labels = [
      "P10",
      "P20",
      "P30",
      "P40",
      "P50",
      "P60",
      "P70",
      "P80",
      "P90",
      "P95",
      "P99",
    ];

    return {
      labels,
      datasets: filteredUnrealistic.map(({ vendor, histogram }) => ({
        label: vendor,
        data: histogram,
        backgroundColor: getBarColor(vendor),
        borderRadius: 8,
        barPercentage: 0.95,
        categoryPercentage: 0.8,
      })),
    };
  }, [filteredUnrealistic, getBarColor]);

  const chartDataForRealistic = useMemo(() => {
    return {
      labels: ["P50", "P95", "P99"],
      datasets: latencyDataForRealistic.flatMap(
        ({ vendor, p50, p95, p99 }, index) => [
          {
            label: `${vendor} P50`,
            data: [p50, 0, 0],
            backgroundColor: getBarColor(vendor),
            stack: `${index}`,
          },
          {
            label: `${vendor} P95`,
            data: [0, p95, 0],
            backgroundColor: getBarColor(vendor),
            stack: `${index}`,
          },
          {
            label: `${vendor} P99`,
            data: [0, 0, p99],
            backgroundColor: getBarColor(vendor),
            stack: `${index}`,
          },
        ]
      ),
    };
  }, [latencyDataForRealistic, getBarColor]);

  const throughputData = filteredResults.map((item) => ({
    vendor: item.vendor,
    actualMessagesPerSecond: item.result["actual-messages-per-second"],
  }));

  const memoryData = filteredResults.map((item) => {
    const memoryValue = item.result["ram-usage"] ?? "0MB";
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

  useEffect(() => {
    setGridKey((prevKey) => prevKey + 1);
  }, [selectedOptions["Realistic"]]);

  //saving data to window.allChartData
  /* eslint-disable */
  if (typeof window !== "undefined") {
    (window as any).allChartData = (window as any).allChartData || [];
    const addOrReplaceChartData = (key: string, value: any) => {
      const chartDataArray = (window as any).allChartData;
      const existingIndex = chartDataArray.findIndex(
        (entry: any) => entry.key === key
      );
      if (existingIndex !== -1) {
        chartDataArray.splice(existingIndex, 1);
      }
      chartDataArray.push({ key, value });
    };

    addOrReplaceChartData("throughputData", throughputData);
    addOrReplaceChartData("memoryData", memoryData);
    addOrReplaceChartData("latencyData", latencyDataForRealistic);
  }

  return (
    <SidebarProvider className="h-screen w-screen overflow-hidden">
      <div className="flex h-full w-full">
        <AppSidebar
          selectedOptions={selectedOptions}
          handleSideBarSelection={handleSideBarSelection}
          platform={data?.platforms}
        />
        <SidebarInset className="flex-grow h-full min-h-0">
          <div
            key={gridKey}
            className={`grid h-full grid-cols-2 ${
              selectedOptions["Realistic"]?.includes("on")
                ? "grid-rows-[2fr,1fr,50px]"
                : "grid-rows-[2fr,50px]"
            } gap-2 p-1`}
          >
            <div
              className="col-span-2 bg-muted/50 rounded-xl p-4 min-h-0"
              id="latency-chart"
            >
              <VerticalBarChart
                chartData={
                  selectedOptions["Realistic"]?.includes("on")
                    ? chartDataForRealistic
                    : chartDataForUnrealistic
                }
                chartId={
                  selectedOptions["Realistic"]?.includes("on") ? "1" : "2"
                }
                title="Vendor Latency Metrics"
                subTitle="P50, P95, and P99 Latency Comparison ( Less is better )"
                xAxisTitle="Vendors"
              />
            </div>
            {selectedOptions["Realistic"]?.includes("on") && (
              <>
                <div
                  className="bg-muted/50 rounded-xl p-4 min-h-0"
                  id="throughput-chart"
                >
                  <HorizontalBarChart
                    data={throughputData}
                    dataKey="actualMessagesPerSecond"
                    chartLabel="Messages Per Second"
                    title="Throughput"
                    subTitle="Performance Metrics ( More is better )"
                    yAxisTitle="Vendors"
                    unit=""
                  />
                </div>
                <div
                  className="bg-muted/50 rounded-xl p-4 min-h-0"
                  id="memory-chart"
                >
                  <HorizontalBarChart
                    data={memoryData}
                    dataKey="memory"
                    chartLabel="Memory Utilization (MB)"
                    title="Memory Usage"
                    subTitle="Memory Allocation ( Less is better )"
                    yAxisTitle="Vendors"
                    unit="mb"
                  />
                </div>
              </>
            )}
            <div className="col-span-2 bg-muted/50 rounded-xl flex items-center justify-center h-[50px]">
              <FooterComponent />
            </div>
          </div>
        </SidebarInset>
      </div>
    </SidebarProvider>
  );
}
