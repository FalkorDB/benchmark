export interface Latency {
  p50: string;
  p95: string;
  p99: string;
}

export interface LatencyHistogram {
  "buckets-ms": number[];
  "cumulative-counts": number[];
  count: number;
}

export interface OpsBreakdown {
  "by-query": Record<string, number>;
  "by-spawn": Record<string, number>;
}

export interface SpawnStats {
  min: number;
  max: number;
  p50: number;
  p95: number;
  "max-min-ratio": number;
  cv: number;
}

export interface Result {
  "deadline-offset": string;
  "actual-messages-per-second": number;
  latency: Latency;
  "avg-latency-ms"?: number;
  "latency-histogram"?: LatencyHistogram;
  "elapsed-ms"?: number;
  "cpu-usage": number;
  "ram-usage": string;
  "base-dataset-bytes"?: number;
  errors: number;
  "successful-requests": number;
  operations?: OpsBreakdown;
  "spawn-stats"?: SpawnStats;
  // Present in aggregated summaries / newer result formats
  histogram_for_type?: Record<string, number[]>;
}

export interface Run {
  vendor: string;
  "read-write-ratio": number;
  clients: number;
  platform: string;
  "target-messages-per-second": number;
  edges: number;
  relationships: number;
  result: Result;
}

export interface UnrealisticData {
  vendor: string;
  histogram_for_type: Record<string, number[]>;
  memory: string;
}

export interface PlatformDetails {
  cpu: string;
  ram: string;
  storage: string;
}

export interface Platforms {
  [key: string]: PlatformDetails;
}

export interface BenchmarkData {
  runs: Run[];
  unrealstic?: UnrealisticData[];
  // Historical files used different shapes here (array vs map), and summaries omit it.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  platforms?: any;
}

export interface ApiResponse {
  result: {
    data: BenchmarkData;
  };
}
