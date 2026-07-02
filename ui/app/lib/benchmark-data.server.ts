import "server-only";
import { promises as fs } from "fs";
import path from "path";
import { BenchmarkData } from "@/app/types/benchmark";

export type RunsManifest = Record<string, { filename: string; timestamp: number }[]>;

const publicDir = path.join(process.cwd(), "public");
const summariesDir = path.join(publicDir, "summaries");

const toSummaryFilePath = (dataUrl: string): string | null => {
  const normalized = dataUrl.startsWith("/") ? dataUrl.slice(1) : dataUrl;
  if (!normalized.startsWith("summaries/")) return null;

  const fullPath = path.join(publicDir, normalized);
  const relativeToSummaries = path.relative(summariesDir, fullPath);
  if (
    relativeToSummaries.startsWith("..") ||
    path.isAbsolute(relativeToSummaries)
  ) {
    return null;
  }

  return fullPath;
};

const readJsonFile = async <T>(filePath: string): Promise<T | null> => {
  try {
    const raw = await fs.readFile(filePath, "utf8");
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
};

export const loadBenchmarkSummary = async (
  dataUrl: string
): Promise<BenchmarkData | null> => {
  const filePath = toSummaryFilePath(dataUrl);
  if (!filePath) return null;

  const data = await readJsonFile<BenchmarkData>(filePath);
  if (!data || !Array.isArray(data.runs)) return null;
  return data;
};

export const loadRunsManifest = async (): Promise<RunsManifest> => {
  const filePath = path.join(summariesDir, "manifest.json");
  const data = await readJsonFile<RunsManifest>(filePath);
  if (!data || typeof data !== "object") return {};
  return data;
};
