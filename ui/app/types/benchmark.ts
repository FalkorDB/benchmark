export interface Latency {
  p50: string;
  p95: string;
  p99: string;
}

export interface Result {
  "deadline-offset": string;
  "actual-messages-per-second": number;
  latency: Latency;
  "cpu-usage": number;
  "ram-usage": number;
  errors: number;
  "successful-requests": number;
}

export interface Run {
  vendor: string;
  "read-write-ratio": number;
  "clients": number;
  platform: string;
  "target-messages-per-second": number;
  edges: number;
  relationships: number;
  result: Result;
}

interface UnRealsticData {
  vendor: string;
  histogram_for_type: Record<string, number[]>;
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
  platforms: Platforms;
}

export interface ApiResponse {
  result: {
    data: BenchmarkData;
  };
}

export interface BenchmarkData {
  runs: Run[];
  unrealstic: UnRealsticData[];
  platforms: Platforms;
}