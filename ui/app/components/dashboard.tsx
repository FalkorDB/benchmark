"use client";

import { AppSidebar } from "@/components/ui/app-sidebar";
import { SidebarInset, SidebarProvider, useSidebar } from "@/components/ui/sidebar";
import { SlidersHorizontal } from "lucide-react";
import FooterComponent from "./footer";
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { BenchmarkData, Run } from "../types/benchmark";
import { useToast } from "@/hooks/use-toast";
import HorizontalBarChart from "./HorizontalBarChart";
import VerticalBarChart from "./VerticalBarChart";
import MemoryBarChart from "./MemoryBarChart";
type RunsManifest = Record<string, { filename: string; timestamp: number }[]>;

const filterDataByVendors = (
  data: BenchmarkData,
  allowedVendors: string[] | null
): BenchmarkData => {
  if (!allowedVendors?.length) return data;
  return {
    ...data,
    runs: (data.runs ?? []).filter((r) =>
      allowedVendors.includes(r.vendor?.toLowerCase())
    ),
    unrealstic: (data.unrealstic ?? []).filter((u) =>
      allowedVendors.includes(u.vendor?.toLowerCase())
    ),
  };
};

const hasValidRunResult = (run: unknown): run is Run => {
  if (!run || typeof run !== "object") return false;
  const candidate = run as Partial<Run>;
  return (
    typeof candidate.vendor === "string" &&
    candidate.vendor.length > 0 &&
    Boolean(candidate.result && typeof candidate.result === "object")
  );
};

type DashboardProps = {
  dataUrl?: string;
  initialSelectedOptions?: Partial<Record<string, string[]>>;
  initialData?: BenchmarkData | null;
  initialManifest?: RunsManifest;
  /**
   * If provided, the dashboard will only show these vendors in the UI (and will
   * ignore any other vendors present in the data file).
   */
  comparisonVendors?: string[];
  /**
   * Hides hardware controls/indicators and ignores hardware filtering when true.
   */
  hideHardware?: boolean;
};

const DEFAULT_SELECTED_OPTIONS: Record<string, string[]> = {
  "Workload Type": ["concurrent"],
  Vendors: ["falkordb", "neo4j"],
  Clients: ["40"],
  Throughput: ["2500"],
  Hardware: ["arm"],
  Queries: ["aggregate_expansion_4_with_filter"],
};

function MobileFiltersBar() {
  const { toggleSidebar } = useSidebar();
  return (
    <button
      type="button"
      onClick={toggleSidebar}
      aria-label="Open filters and options"
      className="md:hidden sticky top-0 z-30 flex w-full items-center gap-2 border-b border-gray-200/60 bg-[#F7F3EF] px-4 py-3 text-left font-space active:bg-gray-200/60"
    >
      <SlidersHorizontal className="size-5 shrink-0" />
      <span className="text-sm font-semibold">Filters &amp; Options</span>
      <span className="ml-auto text-xs font-medium text-gray-500">Tap to open</span>
    </button>
  );
}

export default function DashBoard({
  dataUrl = "/resultData.json",
  initialSelectedOptions,
  initialData = null,
  initialManifest,
  comparisonVendors,
  hideHardware = false,
}: DashboardProps) {
  const allowedVendors = useMemo(() => {
    const v = (comparisonVendors ?? [])
      .map((x) => x.toLowerCase())
      .filter(Boolean);
    return v.length ? v : null;
  }, [comparisonVendors]);
  const [activeUrl, setActiveUrl] = useState(dataUrl);
  const [data, setData] = useState<BenchmarkData | null>(() =>
    initialData ? filterDataByVendors(initialData, allowedVendors) : null
  );
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
  const [didInitFromData, setDidInitFromData] = useState(false);
  const [loadedDataUrl, setLoadedDataUrl] = useState<string | null>(() =>
    initialData ? dataUrl : null
  );

  const [manifest, setManifest] = useState<RunsManifest>(initialManifest ?? {});
  const validRuns = useMemo(
    () => (data?.runs ?? []).filter((run) => hasValidRunResult(run)),
    [data]
  );

  useEffect(() => {
    if (Object.keys(manifest).length > 0) return;
    const fetchManifest = async () => {
      try {
        const response = await fetch("/summaries/manifest.json");
        if (response.ok) {
          const json = await response.json();
          setManifest(json);
        }
      } catch (e) {
        console.error("Failed to load past runs manifest", e);
      }
    };
    fetchManifest();
  }, [manifest]);

  useEffect(() => {
    setActiveUrl(dataUrl);
    setData(initialData ? filterDataByVendors(initialData, allowedVendors) : null);
    setLoadedDataUrl(initialData ? dataUrl : null);
    setDidInitFromData(false);
  }, [dataUrl, initialData, allowedVendors]);

  const baseFileName = useMemo(() => {
    if (!dataUrl) return "";
    const parts = dataUrl.split("/");
    return parts[parts.length - 1];
  }, [dataUrl]);

  const pastRuns = useMemo(() => {
    return manifest[baseFileName] || [];
  }, [manifest, baseFileName]);

  const formatRunTimestamp = useCallback((timestamp: number) => {
    if (!Number.isFinite(timestamp)) return "unknown time";
    const date = new Date(timestamp * 1000);
    if (Number.isNaN(date.getTime())) return "unknown time";
    return date.toLocaleString();
  }, []);


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


  useEffect(() => {
    if (loadedDataUrl === activeUrl && data) return;
    let cancelled = false;
    const urlForRequest = activeUrl;

    const fetchData = async () => {
      try {
        const response = await fetch(urlForRequest);
        if (!response.ok)
          throw new Error(`HTTP error! status: ${response.status}`);

        const json = (await response.json()) as BenchmarkData;

        if (cancelled) return;

        setData(filterDataByVendors(json, allowedVendors));
        setLoadedDataUrl(urlForRequest);
      } catch (error) {
        if (cancelled) return;
        setLoadedDataUrl(null);
        toast({
          title: "Error fetching data",
          description: error instanceof Error ? error.message : "Unknown error",
          variant: "destructive",
        });
      }
    };

    fetchData();

    return () => {
      cancelled = true;
    };
  }, [activeUrl, allowedVendors, toast, loadedDataUrl, data]);

  const availableQueries = useMemo(() => {
    if (!data) return [];

    const queriesFromUnrealistic = (data.unrealstic ?? [])
      .flatMap((u) => Object.keys(u.histogram_for_type ?? {}))
      .filter(Boolean);

    const queriesFromRuns = validRuns
      .flatMap((r) => Object.keys(r.result?.histogram_for_type ?? {}))
      .filter(Boolean);

    return Array.from(
      new Set([...queriesFromUnrealistic, ...queriesFromRuns])
    ).sort();
  }, [data, validRuns]);

  // On first load of a summary file, auto-pick filters that match the data.
  useEffect(() => {
    if (didInitFromData || loadedDataUrl !== activeUrl || !validRuns.length) return;

    const vendors = allowedVendors?.length
      ? allowedVendors
      : Array.from(
          new Set(
            validRuns
              .map((r) => r.vendor?.toString().toLowerCase())
              .filter(Boolean)
          )
        );
    const clients = Array.from(
      new Set(validRuns.map((r) => String(r.clients)).filter(Boolean))
    );
    const throughputs = Array.from(
      new Set(
        validRuns
          .map((r) => String(r["target-messages-per-second"]))
          .filter(Boolean)
      )
    );
    const hardware = hideHardware
      ? []
      : Array.from(
          new Set(validRuns.map((r) => r.platform?.toLowerCase()).filter(Boolean))
        );

    const queries = availableQueries;

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

      // For vendor selection, default to showing *all* vendors present in the file.
      // This is important for aws-tests comparisons (two runs) and prevents auto-picking only the first vendor.
      if (vendors.length) {
        const current = next["Vendors"] ?? [];
        const hasMatch = current.some((c) => vendors.some((a) => a === c));
        if (!hasMatch) next["Vendors"] = vendors;
      }

      replaceIfNoMatch("Queries", queries);

      replaceIfNoMatch("Clients", clients);
      replaceIfNoMatch("Throughput", throughputs);
      
      if (!hideHardware) {
        // For hardware selection, default to showing *all* hardwares present in the file if no match.
        if (hardware.length) {
          const current = next["Hardware"] ?? [];
          const hasMatch = current.some((c) => hardware.some((a) => a === c));
          if (!hasMatch) {
            next["Hardware"] = hardware;
          }
        } else {
          replaceIfNoMatch("Hardware", hardware);
        }
      }

      return next;
    });

    setDidInitFromData(true);
  }, [validRuns, didInitFromData, loadedDataUrl, activeUrl, allowedVendors, availableQueries, hideHardware]);

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

      if (groupTitle === "Hardware") {
        const updatedSelections = groupSelections.includes(optionId)
          ? groupSelections.filter((id) => id !== optionId)
          : [...groupSelections, optionId];

        // Never allow an empty hardware selection.
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

    if (validRuns.length) {
      // Note: aggregated summaries store the histogram on runs[].result.histogram_for_type.
      setFilteredUnrealistic(
        validRuns
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
  }, [data, validRuns, selectedOptions.Queries]);

  // filter realstic data
  useEffect(() => {
    if (!validRuns.length) {
      setFilteredResults([]);
      return;
    }
    const results = validRuns.filter((run) => {
      const isHardwareMatch = hideHardware
        ? true
        : selectedOptions.Hardware?.length
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
    const shouldDeduplicateConcurrentPublicRuns =
      hideHardware &&
      selectedOptions["Workload Type"]?.includes("concurrent");

    if (shouldDeduplicateConcurrentPublicRuns) {
      const byVendor = new Map<string, Run>();
      for (const run of results) {
        const vendorKey = (run.vendor ?? "").toString().toLowerCase();
        if (!vendorKey || byVendor.has(vendorKey)) continue;
        byVendor.set(vendorKey, run);
      }
      setFilteredResults(Array.from(byVendor.values()));
      return;
    }

    setFilteredResults(results);
  }, [validRuns, selectedOptions, hideHardware]);

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
      p50: convertToMilliseconds(item.result?.latency?.p50 ?? "0ms"),
      p95: convertToMilliseconds(item.result?.latency?.p95 ?? "0ms"),
      p99: convertToMilliseconds(item.result?.latency?.p99 ?? "0ms"),
    }));

    const computeStats = (key: "p50" | "p95" | "p99") => {
      const values = data.map((d) => d[key]);
      if (!values.length) return { minValue: 0, maxValue: 0, ratio: 0 };
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
    // For aws-tests, the "vendor" is the instance type (e.g. r7i.2xlarge / r8g.2xlarge).
    const cssVar =
      key === "falkordb" || key === "falkor" || key === "falkordb1" || key.includes("falkordb1") || key.includes("standard") || key.includes("falkordb-c")
        ? "--FalkorDB-color"
        : key === "falkordb2" || key === "falkordb2" || key.includes("falkordb2") || key.includes("secondary") || key.includes("rust") || key.includes("falkordb-rs")
        ? "--FalkorDB2-color"
        : key === "neo4j"
        ? "--Neo4j-color"
        : key === "memgraph"
        ? "--Memgraph-color"
        : key === "intel" || key === "x86" || key.startsWith("r7i")
        ? "--Intel-color"
        : key === "graviton" || key === "arm" || key.startsWith("r8g")
        ? "--Graviton-color"
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

  const throughputData = filteredResults
    .map((item) => ({
      vendor: item.vendor,
      actualMessagesPerSecond: Number(
        item.result?.["actual-messages-per-second"] ?? 0
      ),
    }))
    .filter((item) => Number.isFinite(item.actualMessagesPerSecond));

  // Telemetry breakdown per run (single-workload per-query view)
  const telemetryBreakdownPerRun = useMemo(() => {
    const selectedQuery = selectedOptions.Queries?.[0];
    if (!selectedQuery) return [];

    // Use filteredResults so it respects vendor/hardware/throughput filters.
    const runs = filteredResults.length ? filteredResults : validRuns;

    return runs
      .map((r) => {
        const tb = r?.result?.telemetry_for_type?.[selectedQuery];
        if (!tb) return null;
        return {
          vendor: r.vendor,
          platform: r.platform,
          query: selectedQuery,
          waitMs: tb["wait-ms"],
          execMs: tb["exec-ms"],
          reportMs: tb["report-ms"],
        };
      })
      .filter(Boolean) as Array<{
      vendor: string;
      platform?: string;
      query: string;
      waitMs: number;
      execMs: number;
      reportMs: number;
    }>;
  }, [filteredResults, selectedOptions.Queries, validRuns]);

  const throughputValues = throughputData.map(
    (item) => item.actualMessagesPerSecond
  );
  const maxThroughput = throughputValues.length
    ? Math.max(...throughputValues)
    : 0;
  const minThroughput = throughputValues.length
    ? Math.min(...throughputValues)
    : 0;
  const throughputRatio =
    minThroughput !== 0 ? Math.round(maxThroughput / minThroughput) : 0;

  // Dataset & workload summary (nodes, edges, read/write queries)
  const datasetSummary = React.useMemo(() => {
    if (!validRuns.length) return null;

    const baseRun = validRuns[0];
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
        startedAtEpochSecs: baseRun["started-at-epoch-secs"],
      };
    }

    return { nodes, edges, readQueries, writeQueries, startedAtEpochSecs: baseRun["started-at-epoch-secs"] };
  }, [validRuns]);

  const parseMemory = (memory: string): number => {
    if (!memory) return 0;

    // Parse values like "1.37GB", "800MB", "512kb" and normalize to MB.
    const match = memory.match(/([\d.]+)\s*([a-zA-Z]+)?/);
    if (!match) return 0;

    const value = parseFloat(match[1]);
    if (!Number.isFinite(value)) return 0;

    const unit = (match[2] || "").toLowerCase();

    if (unit.startsWith("g")) {
      // GB -> MB
      return value * 1024;
    }
    if (unit.startsWith("k")) {
      // KB -> MB
      return value / 1024;
    }

    // Treat everything else ("m", "mb", unknown/empty) as MB
    return value;
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

  const singleMemoryValues = singleMemory
    .map((item) => item.memory)
    .filter((value) => Number.isFinite(value));
  const maxSingleMemory = singleMemoryValues.length
    ? Math.max(...singleMemoryValues)
    : 0;
  const minSingleMemory = singleMemoryValues.length
    ? Math.min(...singleMemoryValues)
    : 0;
  const singleMemoryRatio =
    minSingleMemory !== 0 ? Math.round(maxSingleMemory / minSingleMemory) : 0;

  const workloadType = selectedOptions["Workload Type"];
  useEffect(() => {
    setGridKey((prevKey) => prevKey + 1);
  }, [workloadType, activeUrl]);

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

  const hasConcurrentRunDetails = useMemo(() => {
    return concurrentRuns.some((r) => {
      const elapsedMs = Number(r?.result?.["elapsed-ms"] ?? 0);
      const avgLatency = Number(r?.result?.["avg-latency-ms"] ?? 0);
      const stats = r?.result?.["spawn-stats"];
      const ratio = Number(stats?.["max-min-ratio"] ?? 0);
      const cv = Number(stats?.cv ?? 0);

      return elapsedMs > 0 || avgLatency > 0 || ratio > 0 || cv > 0;
    });
  }, [concurrentRuns]);

  return (
    <SidebarProvider className="max-h-none md:max-h-svh h-auto md:h-screen w-full md:w-screen overflow-visible md:overflow-hidden">
      <div className="flex h-full w-full min-h-0">
        <AppSidebar
          selectedOptions={selectedOptions}
          handleSideBarSelection={handleSideBarSelection}
          platform={data?.platforms}
          hideHardware={hideHardware}
          allowedVendors={
            validRuns.length
              ? Array.from(
                  new Set(
                    validRuns
                      .map((r) => r.vendor?.toString().toLowerCase())
                      .filter(Boolean)
                  )
                )
              : undefined
          }
          throughputOptions={
            validRuns.length
              ? Array.from(
                  new Set(
                    validRuns
                      .map((r) => r["target-messages-per-second"])
                      .filter((v) => v !== undefined && v !== null)
                  )
                ).sort((a, b) => Number(a) - Number(b))
              : undefined
          }
          queryOptions={availableQueries.length ? availableQueries : undefined}
          datasetSummary={datasetSummary}
        />
        <SidebarInset className="flex-grow h-auto md:h-full min-h-0 max-h-none md:max-h-svh overflow-visible md:overflow-y-auto">
          <MobileFiltersBar />
          {!hideHardware && pastRuns.length > 0 && (
            <div className="bg-muted/30 border-b border-gray-200/40 p-4 flex flex-wrap items-center justify-between gap-4 font-space">
              <div className="flex flex-col">
                <span className="text-xs text-gray-500 font-medium">Select Run History</span>
                <h1 className="text-sm font-semibold text-gray-800">Viewing Benchmark Run</h1>
              </div>
              <div className="flex items-center gap-3 w-full md:w-auto">
                <select
                  value={activeUrl.replace("/summaries/", "")}
                  onChange={(e) => {
                    const val = e.target.value;
                    if (val === baseFileName) {
                      setActiveUrl(dataUrl);
                    } else {
                      setActiveUrl(`/summaries/${val}`);
                    }
                    setData(null);
                    setLoadedDataUrl(null);
                    setDidInitFromData(false);
                  }}
                  className="bg-white border border-gray-200/80 text-gray-800 text-sm rounded-lg px-3 py-2 font-fira shadow-sm focus:outline-none focus:ring-1 focus:ring-primary focus:border-primary cursor-pointer w-full md:w-auto md:min-w-[280px]"
                >
                  <option value={baseFileName}>Latest Run (Default)</option>
                  {pastRuns.map((run) => (
                    <option key={run.filename} value={run.filename}>
                      {`${run.filename} — ${formatRunTimestamp(run.timestamp)}`}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          )}
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

              {hasConcurrentRunDetails && (
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
              )}

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

              <div className="bg-muted/50 rounded-xl flex items-center justify-center h-auto md:h-[50px]">
                <FooterComponent />
              </div>
            </div>
          ) : (
            <div
              key={gridKey}
              className="grid w-full h-full min-w-0 grid-cols-1 md:grid-cols-[7fr_3fr] grid-rows-[auto_auto_auto] md:grid-rows-[2fr,50px] gap-2 p-1"
            >
              <div
                className="bg-muted/50 rounded-xl p-4 min-h-[360px] md:min-h-0 w-full flex flex-col min-w-0 items-center justify-between"
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
              <div className="bg-muted/50 rounded-xl p-4 min-h-[360px] md:min-h-0 w-full flex flex-col min-w-0 items-center justify-between">
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

                {!hideHardware && telemetryBreakdownPerRun.length > 0 && (
                  <div className="w-full pb-2">
                    <p className="text-xs text-gray-600 text-center font-fira pb-1">
                      Telemetry breakdown for {telemetryBreakdownPerRun[0].query} (wait / exec / report)
                    </p>
                    <div className="w-full overflow-x-auto">
                      <table className="w-full text-xs font-fira">
                        <thead>
                          <tr className="text-left text-gray-600">
                            <th className="py-1 pr-3">Run</th>
                            <th className="py-1 pr-3">HW</th>
                            <th className="py-1 pr-3">wait (ms)</th>
                            <th className="py-1 pr-3">exec (ms)</th>
                            <th className="py-1 pr-3">report (ms)</th>
                          </tr>
                        </thead>
                        <tbody>
                          {telemetryBreakdownPerRun.map((t) => (
                            <tr
                              key={`${t.vendor}-${t.platform ?? ""}`}
                              className="border-t border-gray-200/60"
                            >
                              <td className="py-1 pr-3 font-semibold">{t.vendor}</td>
                              <td className="py-1 pr-3">{t.platform ?? "—"}</td>
                              <td className="py-1 pr-3">{t.waitMs.toFixed(1)}</td>
                              <td className="py-1 pr-3">{t.execMs.toFixed(1)}</td>
                              <td className="py-1 pr-3">{t.reportMs.toFixed(1)}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </div>
                )}

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
              <div className="col-span-1 md:col-span-2 bg-muted/50 rounded-xl flex items-center justify-center h-auto md:h-[50px]">
                <FooterComponent />
              </div>
            </div>
          )}
        </SidebarInset>
      </div>
    </SidebarProvider>
  );
}
