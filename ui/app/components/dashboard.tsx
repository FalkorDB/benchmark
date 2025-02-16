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
  const [latencyStats, setLatencyStats] = useState({
    p50: { minValue: 0, maxValue: 0, ratio: 0 },
    p95: { minValue: 0, maxValue: 0, ratio: 0 },
    p99: { minValue: 0, maxValue: 0, ratio: 0 },
  });
  const [filteredUnrealistic, setFilteredUnrealistic] = useState<
    { vendor: string; histogram: number[] }[]
  >([]);
  const [selectedOptions, setSelectedOptions] = React.useState<
    Record<string, string[]>
  >({
    "Workload Type": ["concurrent"],
    Vendors: ["falkordb", "neo4j"],
    Clients: ["40"],
    Throughput: ["2500"],
    Hardware: ["intel"],
    Queries: ["aggregate_expansion_4_with_filter"],
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

  // filter unrealstic data
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

  // filter realstic data
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

  const latencyDataForRealistic = useMemo(() => {
    const convertToMilliseconds = (value: string): number => {
      const match = value.match(/([\d.,]+)([a-zA-Z]+)/);
      if (match) {
        const numericValue = parseFloat(match[1].replace(",", "."));
        const unit = match[2].toLowerCase();
        return unit === "s" ? numericValue * 1000 : numericValue;
      }
      return 0;
    };

    const data = filteredResults.map((item) => ({
      vendor: item.vendor,
      p50: convertToMilliseconds(item.result.latency.p50),
      p95: convertToMilliseconds(item.result.latency.p95),
      p99: convertToMilliseconds(item.result.latency.p99),
    }));

    // Compute min, max, and ratio for each percentile with rounding
    const computeStats = (key: keyof (typeof data)[0]) => {
      const values = data.map((d) => d[key]);
      const minValue = Math.round(Math.min(...values));
      const maxValue = Math.round(Math.max(...values));
      const ratio =
        minValue !== 0 ? Math.round((maxValue / minValue) * 100) / 100 : 0;

      return { minValue, maxValue, ratio };
    };

    setLatencyStats({
      p50: computeStats("p50"),
      p95: computeStats("p95"),
      p99: computeStats("p99"),
    });

    return data;
  }, [filteredResults]);

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
            borderRadius: 8,
          },
          {
            label: `${vendor} P95`,
            data: [0, p95, 0],
            backgroundColor: getBarColor(vendor),
            stack: `${index}`,
            borderRadius: 8,
          },
          {
            label: `${vendor} P99`,
            data: [0, 0, p99],
            backgroundColor: getBarColor(vendor),
            stack: `${index}`,
            borderRadius: 8,
          },
        ]
      ),
    };
  }, [latencyDataForRealistic, getBarColor]);

  const throughputData = filteredResults.map((item) => ({
    vendor: item.vendor,
    actualMessagesPerSecond: item.result["actual-messages-per-second"],
  }));

  const maxThroughput = Math.max(
    ...throughputData.map((item) => item.actualMessagesPerSecond)
  );
  const minThroughput = Math.min(
    ...throughputData.map((item) => item.actualMessagesPerSecond)
  );
  const throughputRatio =
    minThroughput !== 0 ? Math.round(maxThroughput / minThroughput) : 0;

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

  const maxMemoryUsage = Math.max(...memoryData.map((item) => item.memory));
  const minMemoryUsage = Math.min(...memoryData.map((item) => item.memory));
  const memoryUsageRatio =
    minMemoryUsage !== 0 ? Math.round(maxMemoryUsage / minMemoryUsage) : 0;

  useEffect(() => {
    setGridKey((prevKey) => prevKey + 1);
  }, [selectedOptions["Workload Type"]]);

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
              selectedOptions["Workload Type"]?.includes("concurrent")
                ? "grid-rows-[2fr,1.5fr,50px]"
                : "grid-rows-[2fr,50px]"
            } gap-2 p-1`}
          >
            <div
              className="col-span-2 bg-muted/50 rounded-xl p-4 min-h-0 w-full flex flex-col items-center justify-between"
              id="latency-chart"
            >
              <h2 className="text-2xl font-bold text-center">LATENCY</h2>
              <p className="pb-1 text-gray-600 text-center">
                (LOWER IS BETTER)
              </p>
              <div className="pt-1 w-full border-b border-gray-400"></div>
              {selectedOptions["Workload Type"]?.includes("concurrent") && (
                <p className="text-lg font-semibold text-center mb-2">
                  Superior Latency:{" "}
                  <span className="text-purple-600 font-bold">
                    {latencyStats ? Math.round(latencyStats.p99.ratio) : ""}x
                  </span>{" "}
                  faster at P99
                </p>
              )}

              <div className="w-full flex-grow flex items-center justify-center min-h-0">
                <div className="w-full h-full">
                {latencyStats.p99.ratio > 0 &&
                  <VerticalBarChart
                    chartData={
                      selectedOptions["Workload Type"]?.includes("concurrent")
                        ? chartDataForRealistic
                        : chartDataForUnrealistic
                    }
                    chartId={
                      selectedOptions["Workload Type"]?.includes("concurrent")
                        ? "concurrent"
                        : "single"
                    }
                    unit="ms"
                    latencyStats={latencyStats}
                  />}
                </div>
              </div>
            </div>

            {selectedOptions["Workload Type"]?.includes("concurrent") && (
              <>
                <div
                  className="bg-muted/50 rounded-xl p-4 min-h-0 w-full flex flex-col items-center justify-between"
                  id="throughput-chart"
                >
                  <h2 className="text-2xl font-bold text-center">THROUGHPUT</h2>
                  <p className="text-gray-600 text-center">
                    (HIGHER IS BETTER)
                  </p>
                  <div className="pb-1 w-full border-b border-gray-400"></div>
                  <p className="pt-1 text-lg font-semibold text-center">
                    Execute{" "}
                    <span className="text-purple-600 font-bold">
                      {throughputRatio ? throughputRatio : ""}x
                    </span>{" "}
                    more queries with the same hardware
                  </p>
                  <div className="w-full flex-grow flex items-center justify-center min-h-0">
                    <div className="w-full h-full">
                      <HorizontalBarChart
                        data={throughputData}
                        dataKey="actualMessagesPerSecond"
                        chartLabel="Messages Per Second"
                        ratio={throughputRatio}
                        maxValue={maxThroughput}
                        minValue={minThroughput}
                        unit="mb"
                      />
                    </div>
                  </div>
                </div>

                <div
                  className="bg-muted/50 rounded-xl p-4 min-h-0 w-full flex flex-col items-center justify-between"
                  id="memory-chart"
                >
                  <h2 className="text-2xl font-bold text-center">
                    MEMORY USAGE
                  </h2>
                  <p className="text-gray-600 text-center">(LOWER IS BETTER)</p>
                  <div className="pb-1 w-full border-b border-gray-400"></div>
                  <p className="pt-1 text-lg font-semibold text-center">
                    <span className="text-purple-600 font-bold">
                      {memoryUsageRatio ? memoryUsageRatio : ""}x
                    </span>{" "}
                    Better performance, lower overall costs
                  </p>
                  <div className="w-full flex-grow flex items-center justify-center min-h-0">
                    <div className="w-full h-full">
                      <HorizontalBarChart
                        data={memoryData}
                        dataKey="memory"
                        chartLabel="Memory Utilization (MB)"
                        ratio={memoryUsageRatio}
                        maxValue={maxMemoryUsage}
                        minValue={minMemoryUsage}
                        unit="mb"
                      />
                    </div>
                  </div>
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
