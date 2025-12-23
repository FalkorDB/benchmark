"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import FooterComponent from "./footer";
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { BenchmarkData, Run } from "../types/benchmark";
import { useToast } from "@/hooks/use-toast";
import HorizontalBarChart from "./HorizontalBarChart";
import VerticalBarChart from "./VerticalBarChart";
import MemoryBarChart from "./MemoryBarChart";

type DashboardProps = {
  dataUrl?: string;
  initialSelectedOptions?: Partial<Record<string, string[]>>;
  /**
   * If provided, the dashboard will only show these vendors in the UI (and will
   * ignore any other vendors present in the data file).
   */
  comparisonVendors?: string[];
};

const DEFAULT_SELECTED_OPTIONS: Record<string, string[]> = {
  "Workload Type": ["single"],
  Vendors: ["falkordb", "neo4j"],
  Clients: ["40"],
  Throughput: ["2500"],
  Hardware: ["arm"],
  Queries: ["aggregate_expansion_4_with_filter"],
};

export default function DashBoard({
  dataUrl = "/resultData.json",
  initialSelectedOptions,
  comparisonVendors,
}: DashboardProps) {
  const [data, setData] = useState<BenchmarkData | null>(null);
  const { toast } = useToast();
  const [gridKey, setGridKey] = useState(0);
  const [p99SingleRatio, setP99SingleRatio] = useState<number | null>(null);
  const [filteredResults, setFilteredResults] = useState<Run[]>([]);
  const [latencyStats, setLatencyStats] = useState({
    p50: { minValue: 0, maxValue: 0, ratio: 0 },
    p95: { minValue: 0, maxValue: 0, ratio: 0 },
    p99: { minValue: 0, maxValue: 0, ratio: 0 },
  });
  const [filteredUnrealistic, setFilteredUnrealistic] = useState<
    {
      vendor: string;
      histogram: number[];
      memory: string;
      baseDatasetBytes?: number;
    }[]
  >([]);

  const allowedVendors = useMemo(() => {
    const v = (comparisonVendors ?? []).map((x) => x.toLowerCase()).filter(Boolean);
    return v.length ? v : null;
  }, [comparisonVendors]);

  const [selectedOptions, setSelectedOptions] = React.useState<
    Record<string, string[]>
  >(() => {
    const next: Record<string, string[]> = {
      ...DEFAULT_SELECTED_OPTIONS,
      ...(initialSelectedOptions ?? {}),
    } as Record<string, string[]>;

    // If this page compares a specific set of vendors, lock the vendor selection to those.
    if (allowedVendors?.length) {
      next["Vendors"] = allowedVendors;
    }

    return next;
  });

  const [didInitFromData, setDidInitFromData] = useState(false);

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch(dataUrl);
      if (!response.ok)
        throw new Error(`HTTP error! status: ${response.status}`);

      const json = (await response.json()) as BenchmarkData;

      if (allowedVendors?.length) {
        const filtered: BenchmarkData = {
          ...json,
          runs: (json.runs ?? []).filter((r) =>
            allowedVendors.includes(r.vendor?.toLowerCase())
          ),
          unrealstic: (json.unrealstic ?? []).filter((u) =>
            allowedVendors.includes(u.vendor?.toLowerCase())
          ),
        };
        setData(filtered);
      } else {
        setData(json);
      }
    } catch (error) {
      toast({
        title: "Error fetching data",
        description: error instanceof Error ? error.message : "Unknown error",
        variant: "destructive",
      });
    }
  }, [toast, dataUrl, allowedVendors]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  // On first load of a summary file, auto-pick filters that match the data.
  useEffect(() => {
    if (didInitFromData || !data?.runs?.length) return;

    const vendors = allowedVendors?.length
      ? allowedVendors
      : Array.from(
          new Set(
            data.runs
              .map((r) => r.vendor?.toString().toLowerCase())
              .filter(Boolean)
          )
        );
    const clients = Array.from(
      new Set(data.runs.map((r) => String(r.clients)).filter(Boolean))
    );
    const throughputs = Array.from(
      new Set(
        data.runs
          .map((r) => String(r["target-messages-per-second"]))
          .filter(Boolean)
      )
    );
    const hardware = Array.from(
      new Set(data.runs.map((r) => r.platform?.toLowerCase()).filter(Boolean))
    );

    setSelectedOptions((prev) => {
      const next = { ...prev };

      // Only overwrite if current selection doesn't intersect available values.
      const replaceIfNoMatch = (
        key: string,
        available: string[],
        normalize?: (v: string) => string
      ) => {
        const current = next[key] ?? [];
        const norm = normalize ?? ((v: string) => v);
        const hasMatch = current.some((c) =>
          available.some((a) => norm(a) === norm(c))
        );
        if (!hasMatch && available.length) {
          next[key] = [available[0]];
        }
      };

      replaceIfNoMatch("Vendors", vendors);
      replaceIfNoMatch("Clients", clients);
      replaceIfNoMatch("Throughput", throughputs);
      replaceIfNoMatch("Hardware", hardware);

      return next;
    });

    setDidInitFromData(true);
  }, [data, didInitFromData, allowedVendors]);

  const handleSideBarSelection = (groupTitle: string, optionId: string) => {
    setSelectedOptions((prev) => {
      const groupSelections = prev[groupTitle] || [];

      if (groupTitle === "Vendors") {
        // If this page is restricted to a specific vendor pair, ignore toggles outside it.
        if (
          allowedVendors?.length &&
          !allowedVendors.includes(optionId.toLowerCase())
        ) {
          return prev;
        }

        const updatedSelections = groupSelections.includes(optionId)
          ? groupSelections.filter((id) => id !== optionId)
          : [...groupSelections, optionId];

        // Never allow an empty vendor selection.
        if (updatedSelections.length === 0) {
          return prev;
        }

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

  // filter unrealistic (single-workload) data
  // Preferred source: data.unrealstic (legacy file format)
  // Fallback source: runs[].result.histogram_for_type (aggregated summaries)
  useEffect(() => {
    if (!data || !selectedOptions.Queries?.length) {
      setFilteredUnrealistic([]);
      return;
    }

    const selectedQuery = selectedOptions.Queries[0];

    if (data.unrealstic?.length) {
      setFilteredUnrealistic(
        data.unrealstic
          .map(({ vendor, histogram_for_type, memory }) => ({
            vendor,
            histogram: histogram_for_type[selectedQuery] || [],
            memory,
          }))
          .filter((entry) => entry.histogram.length > 0)
      );
      return;
    }

    if (data.runs?.length) {
      // Note: aggregated summaries store the histogram on runs[].result.histogram_for_type.
      setFilteredUnrealistic(
        data.runs
          .map((run: Run) => ({
            vendor: run.vendor,
            histogram: run?.result?.histogram_for_type?.[selectedQuery] || [],
            memory: run?.result?.["ram-usage"] ?? "",
            baseDatasetBytes: run?.result?.["base-dataset-bytes"],
          }))
          .filter((entry) => entry.histogram.length > 0)
      );
      return;
    }

    setFilteredUnrealistic([]);
  }, [data, selectedOptions.Queries]);

  // filter realstic data
  useEffect(() => {
    if (!data || !data.runs) {
      setFilteredResults([]);
      return;
    }

    const results = data.runs.filter((run) => {
      const isHardwareMatch = selectedOptions.Hardware?.length
        ? run.platform &&
          selectedOptions.Hardware.some((hardware) =>
            run.platform.toLowerCase().includes(hardware.toLowerCase())
          )
        : true;

      const isVendorMatch = selectedOptions.Vendors?.length
        ? run.vendor &&
          selectedOptions.Vendors.some(
            (vendor) => vendor.toLowerCase() === run.vendor.toLowerCase()
          )
        : true;

      const isClientMatch = selectedOptions.Clients?.length
        ? run.clients !== undefined &&
          selectedOptions.Clients.includes(String(run.clients))
        : true;

      const isThroughputMatch = selectedOptions.Throughput?.length
        ? run["target-messages-per-second"] !== undefined &&
          selectedOptions.Throughput.includes(
            String(run["target-messages-per-second"])
          )
        : true;

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

    type LatencyDatum = {
      vendor: string;
      p50: number;
      p95: number;
      p99: number;
    };

    const data: LatencyDatum[] = filteredResults.map((item) => ({
      vendor: item.vendor,
      p50: convertToMilliseconds(item.result.latency.p50),
      p95: convertToMilliseconds(item.result.latency.p95),
      p99: convertToMilliseconds(item.result.latency.p99),
    }));

    const computeStats = (key: "p50" | "p95" | "p99") => {
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
    const key = (vendor ?? "").toString().trim().toLowerCase();

    // Map vendor identifiers/names to the same CSS vars used by the MAX THROUGHPUT chart.
    const cssVar =
      key === "falkordb" || key === "falkor"
        ? "--FalkorDB-color"
        : key === "neo4j"
        ? "--Neo4j-color"
        : key === "memgraph"
        ? "--Memgraph-color"
        : "";

    if (!cssVar) return "#191919";

    return (
      getComputedStyle(document.documentElement)
        .getPropertyValue(cssVar)
        .trim() || "#191919"
    );
  }, []);

  const chartDataForUnrealistic = useMemo(() => {
    if (!filteredUnrealistic.length) {
      setP99SingleRatio(null);
      return { labels: [], datasets: [] };
    }

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

    const datasets = filteredUnrealistic.map(({ vendor, histogram }) => ({
      label: vendor,
      data: histogram,
      backgroundColor: getBarColor(vendor),
      hoverBackgroundColor: getBarColor(vendor),
      borderRadius: 8,
      barPercentage: 0.95,
      categoryPercentage: 0.9,
    }));

    if (filteredUnrealistic.length >= 2) {
      const p99Values = filteredUnrealistic
        .map(({ histogram }) => histogram[10])
        .sort((a, b) => b - a);

      if (p99Values.length >= 2 && p99Values[1] !== 0) {
        setP99SingleRatio(p99Values[0] / p99Values[1]);
      } else {
        setP99SingleRatio(null);
      }
    } else {
      setP99SingleRatio(null);
    }

    return { labels, datasets };
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
            hoverBackgroundColor: getBarColor(vendor),
            stack: `${index}`,
            borderRadius: 8,
          },
          {
            label: `${vendor} P95`,
            data: [0, p95, 0],
            backgroundColor: getBarColor(vendor),
            hoverBackgroundColor: getBarColor(vendor),
            stack: `${index}`,
            borderRadius: 8,
          },
          {
            label: `${vendor} P99`,
            data: [0, 0, p99],
            backgroundColor: getBarColor(vendor),
            hoverBackgroundColor: getBarColor(vendor),
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

  // Dataset & workload summary (nodes, edges, read/write queries)
  const datasetSummary = React.useMemo(() => {
    if (!data?.runs?.length) return null;

    const baseRun = data.runs[0];
    const nodes = baseRun.edges ?? 0;
    const edges = baseRun.relationships ?? 0;

    const opsByQuery = baseRun.result?.operations?.["by-query"] ?? {};
    const writeQueryNames = new Set([
      "single_vertex_update",
      "single_edge_update",
      "single_vertex_write",
      "single_edge_write",
      "write",
    ]);

    let readQueries = 0;
    let writeQueries = 0;

    for (const [name, count] of Object.entries(
      opsByQuery as Record<string, number>
    )) {
      if (writeQueryNames.has(name)) {
        writeQueries += count;
      } else {
        readQueries += count;
      }
    }

    // Fallback: if we don't have per-query breakdown, treat all successful requests as reads.
    if (readQueries === 0 && writeQueries === 0) {
      const total = baseRun.result?.["successful-requests"] ?? 0;
      return {
        nodes,
        edges,
        readQueries: typeof total === "number" ? total : 0,
        writeQueries: 0,
      };
    }

    return { nodes, edges, readQueries, writeQueries };
  }, [data]);

  const parseMemory = (memory: string): number => {
    const match = memory.match(/([\d.]+)/);
    return match ? parseFloat(match[1]) : 0;
  };

  const formatBytes = (bytes?: number) => {
    if (!bytes || bytes <= 0) return "";
    const mib = bytes / (1024 * 1024);
    if (mib >= 1024) return `${(mib / 1024).toFixed(2)}GB`;
    return `${mib.toFixed(1)}MB`;
  };

  const singleMemory = filteredUnrealistic.map(
    ({ vendor, memory, baseDatasetBytes }) => {
      const key = (vendor ?? "").toString().trim().toLowerCase();

      // For Memgraph and Neo4j, prefer the "base dataset" estimate (bytes) as the bar value.
      // This normalizes the chart to dataset footprint rather than process RSS.
      if (
        (key === "memgraph" || key === "neo4j") &&
        baseDatasetBytes &&
        baseDatasetBytes > 0
      ) {
        return {
          vendor,
          memory: baseDatasetBytes / (1024 * 1024),
        };
      }

      return {
        vendor,
        memory: parseMemory(memory),
      };
    }
  );

  const baseDatasetByVendor = filteredUnrealistic.reduce<Record<string, number>>(
    (acc, cur) => {
      if (cur.baseDatasetBytes) acc[cur.vendor] = cur.baseDatasetBytes;
      return acc;
    },
    {}
  );

  const maxSingleMemory = Math.max(...singleMemory.map((item) => item.memory));
  const minSingleMemory = Math.min(...singleMemory.map((item) => item.memory));
  const singleMemoryRatio =
    minSingleMemory !== 0 ? Math.round(maxSingleMemory / minSingleMemory) : 0;

  const workloadType = selectedOptions["Workload Type"];
  useEffect(() => {
    setGridKey((prevKey) => prevKey + 1);
  }, [workloadType]);

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
    // addOrReplaceChartData("memoryData", singleMemory);
    addOrReplaceChartData("latencyData", latencyDataForRealistic);
  }

  const isConcurrent = selectedOptions["Workload Type"]?.includes("concurrent");

  const formatDuration = (ms: number) => {
    if (!Number.isFinite(ms) || ms <= 0) return "0s";
    const totalSeconds = ms / 1000;
    if (totalSeconds < 60) return `${totalSeconds.toFixed(1)}s`;
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = Math.round(totalSeconds % 60);
    return `${minutes}m ${seconds}s`;
  };

  const concurrentRuns = useMemo(() => {
    const byVendor = new Map<string, any>();
    for (const r of filteredResults) {
      const v = (r.vendor ?? "").toString().toLowerCase();
      if (!v) continue;
      if (!byVendor.has(v)) byVendor.set(v, r);
    }
    return Array.from(byVendor.values());
  }, [filteredResults]);

  return (
    <SidebarProvider className="h-screen w-screen overflow-hidden">
      <div className="flex h-full w-full">
        <AppSidebar
          selectedOptions={selectedOptions}
          handleSideBarSelection={handleSideBarSelection}
          platform={data?.platforms}
          allowedVendors={
            data?.runs?.length
              ? Array.from(
                  new Set(
                    data.runs
                      .map((r) => r.vendor?.toString().toLowerCase())
                      .filter(Boolean)
                  )
                )
              : undefined
          }
          throughputOptions={
            data?.runs?.length
              ? Array.from(
                  new Set(
                    data.runs
                      .map((r) => r["target-messages-per-second"])
                      .filter((v) => v !== undefined && v !== null)
                  )
                ).sort((a, b) => Number(a) - Number(b))
              : undefined
          }
          datasetSummary={datasetSummary}
        />
        <SidebarInset className="flex-grow h-full min-h-0 overflow-y-auto">
          {isConcurrent ? (
            <div key={gridKey} className="flex flex-col w-full min-w-0 gap-2 p-1">
              <div className="bg-muted/50 rounded-xl p-4 w-full flex flex-col items-center justify-between min-h-[420px]">
                <h2 className="text-2xl font-bold text-center font-space">
                  LATENCY
                </h2>
                <p className="pb-1 text-gray-600 text-center font-fira">
                  (LOWER IS BETTER)
                </p>
                <p className="text-lg font-semibold text-center mb-2 font-fira">
                  Superior Latency:{" "}
                  <span className="text-[#FF66B3] font-bold">
                    {latencyStats ? `${Math.round(latencyStats.p99.ratio)}x` : ""}
                  </span>{" "}
                  faster at P99
                </p>
                <div className="w-full flex-grow min-h-0">
                  {latencyStats.p99.ratio > 0 && (
                    <VerticalBarChart
                      chartData={chartDataForRealistic}
                      chartId="concurrent"
                      unit="ms"
                      latencyStats={latencyStats}
                    />
                  )}
                </div>
              </div>

              <div className="bg-muted/50 rounded-xl p-4 w-full flex flex-col min-h-[180px]">
                <h2 className="text-2xl font-bold text-center font-space">
                  RUN DETAILS
                </h2>
                <p className="pb-2 text-gray-600 text-center font-fira">
                  Duration, mean latency, and worker fairness
                </p>
                <div className="w-full overflow-x-auto">
                  <table className="w-full text-sm font-fira">
                    <thead>
                      <tr className="text-left text-gray-600">
                        <th className="py-1 pr-4">Vendor</th>
                        <th className="py-1 pr-4">Duration</th>
                        <th className="py-1 pr-4">Avg latency</th>
                        <th className="py-1 pr-4">Worker imbalance</th>
                        <th className="py-1 pr-4">Worker CV</th>
                      </tr>
                    </thead>
                    <tbody>
                      {concurrentRuns.map((r) => {
                        const elapsedMs = Number(r?.result?.["elapsed-ms"] ?? 0);
                        const avgLatency = Number(r?.result?.["avg-latency-ms"] ?? 0);
                        const stats = r?.result?.["spawn-stats"];
                        const ratio = Number(stats?.["max-min-ratio"] ?? 0);
                        const cv = Number(stats?.cv ?? 0);

                        return (
                          <tr key={r.vendor} className="border-t border-gray-200/60">
                            <td className="py-2 pr-4 font-semibold">{r.vendor}</td>
                            <td className="py-2 pr-4">{formatDuration(elapsedMs)}</td>
                            <td className="py-2 pr-4">{avgLatency.toFixed(2)} ms</td>
                            <td className="py-2 pr-4">
                              {ratio > 0 ? `${ratio.toFixed(2)}x (max/min)` : "—"}
                            </td>
                            <td className="py-2 pr-4">{cv > 0 ? cv.toFixed(3) : "—"}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </div>

              <div
                className="bg-muted/50 rounded-xl p-4 w-full flex flex-col items-center justify-between min-h-[420px]"
                id="throughput-chart"
              >
                <h2 className="text-2xl font-bold text-center font-space">
                  MAX THROUGHPUT
                </h2>
                <p className="text-gray-600 text-center font-fira">
                  (HIGHER IS BETTER)
                </p>
                <p className="pt-1 text-lg font-semibold text-center font-fira">
                  Execute{" "}
                  <span className="text-[#FF66B3] font-bold">
                    {throughputRatio ? throughputRatio : ""}x
                  </span>{" "}
                  more queries with the same hardware
                </p>
                <div className="w-full flex-grow min-h-0">
                  <HorizontalBarChart
                    data={throughputData}
                    dataKey="actualMessagesPerSecond"
                    chartLabel="Queries Per Second"
                    ratio={throughputRatio}
                    maxValue={maxThroughput}
                    minValue={minThroughput}
                    unit=" qps"
                    getBarColor={getBarColor}
                  />
                </div>
              </div>

              <div className="bg-muted/50 rounded-xl flex items-center justify-center h-[50px]">
                <FooterComponent />
              </div>
            </div>
          ) : (
            <div
              key={gridKey}
              className="grid w-full h-full min-w-0 grid-cols-[7fr_3fr] grid-rows-[2fr,50px] gap-2 p-1"
            >
              <div
                className="bg-muted/50 rounded-xl p-4 min-h-0 w-full flex flex-col min-w-0 items-center justify-between"
                id="latency-chart"
              >
                <h2 className="text-2xl font-bold text-center font-space">
                  LATENCY
                </h2>
                <p className="pb-1 text-gray-600 text-center font-fira">
                  (LOWER IS BETTER)
                </p>
                <p className="text-lg font-semibold text-center mb-2 font-fira">
                  Superior Latency:{" "}
                  <span className="text-[#FF66B3] font-bold">
                    {p99SingleRatio ? `${Math.round(p99SingleRatio)}x` : ""}
                  </span>{" "}
                  faster at P99
                </p>
                <div className="w-full flex-grow flex items-center justify-center min-h-0">
                  <div className="w-full h-full">
                    {chartDataForUnrealistic.datasets.length > 0 && (
                      <VerticalBarChart
                        chartData={chartDataForUnrealistic}
                        chartId="single"
                        unit="ms"
                        latencyStats={latencyStats}
                      />
                    )}
                  </div>
                </div>
              </div>
              <div className="bg-muted/50 rounded-xl p-4 min-h-0 w-full flex flex-col min-w-0 items-center justify-between">
                <h2 className="text-2xl font-bold text-center font-space">
                  MEMORY USAGE
                </h2>
                <p className="text-gray-600 text-center font-fira">
                  (LOWER IS BETTER)
                </p>
                <p className="pt-1 text-lg font-semibold text-center font-fira pb-1">
                  <span className="text-[#FF66B3] font-bold">
                    {singleMemoryRatio ? singleMemoryRatio : ""}x
                  </span>{" "}
                  Better performance, lower overall costs
                </p>

                {Object.keys(baseDatasetByVendor).length > 0 && (
                  <div className="text-sm text-gray-600 text-center font-fira pb-2">
                    {Object.entries(baseDatasetByVendor).map(([vendor, bytes]) => (
                      <div key={vendor}>
                        {vendor} base dataset estimate: {formatBytes(bytes)}
                      </div>
                    ))}
                  </div>
                )}
                <div className="w-full flex-grow flex items-center justify-center min-h-0">
                  <div className="w-full h-full">
                    <MemoryBarChart
                      singleMemory={singleMemory}
                      ratio={singleMemoryRatio}
                      maxValue={maxSingleMemory}
                      minValue={minSingleMemory}
                      unit="MB"
                      getBarColor={getBarColor}
                    />
                  </div>
                </div>
              </div>
              <div className="col-span-2 bg-muted/50 rounded-xl flex items-center justify-center h-[50px]">
                <FooterComponent />
              </div>
            </div>
          )}
        </SidebarInset>
      </div>
    </SidebarProvider>
  );
}
