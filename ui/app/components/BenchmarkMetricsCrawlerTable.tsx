import { BenchmarkData, Run } from "@/app/types/benchmark";

type BenchmarkMetricsCrawlerTableProps = {
  data: BenchmarkData | null;
  dataUrl: string;
  title: string;
};

const formatNumber = (value: number | string | undefined) => {
  if (value === undefined || value === null) return "";
  if (typeof value === "number") return Number.isFinite(value) ? value.toLocaleString() : "";
  return value;
};

const formatTimestamp = (epochSeconds?: number) => {
  if (!epochSeconds || !Number.isFinite(epochSeconds)) return "";
  const date = new Date(epochSeconds * 1000);
  if (Number.isNaN(date.getTime())) return "";
  return date.toISOString();
};

const getQueryCounts = (run: Run): Array<{ query: string; count: number | null }> => {
  if (!run?.result) return [];
  const byQuery = run.result.operations?.["by-query"];
  if (byQuery) {
    return Object.entries(byQuery)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([query, count]) => ({ query, count }));
  }

  const histogramQueryNames = Object.keys(run.result.histogram_for_type ?? {}).sort();
  return histogramQueryNames.map((query) => ({ query, count: null }));
};

export default function BenchmarkMetricsCrawlerTable({
  data,
  dataUrl,
  title,
}: BenchmarkMetricsCrawlerTableProps) {
  const runs = data?.runs ?? [];
  if (runs.length === 0) return null;

  return (
    <section className="sr-only" aria-label={`${title} crawlable benchmark metrics`}>
      <h2>{title} benchmark metrics</h2>
      <p>Data source: {dataUrl}</p>
      <table>
        <caption>Core run metrics</caption>
        <thead>
          <tr>
            <th>Vendor</th>
            <th>Platform</th>
            <th>Clients</th>
            <th>Target MPS</th>
            <th>Actual MPS</th>
            <th>Latency p50</th>
            <th>Latency p95</th>
            <th>Latency p99</th>
            <th>Avg latency (ms)</th>
            <th>CPU usage</th>
            <th>RAM usage</th>
            <th>Errors</th>
            <th>Successful requests</th>
            <th>Read/Write ratio</th>
            <th>Started at</th>
          </tr>
        </thead>
        <tbody>
          {runs.map((run, index) => {
            if (!run?.result) return null;
            return (
              <tr key={`${run.vendor}-${run.platform}-${index}`}>
                <td>{run.vendor}</td>
                <td>{run.platform}</td>
                <td>{formatNumber(run.clients)}</td>
                <td>{formatNumber(run["target-messages-per-second"])}</td>
                <td>{formatNumber(run.result["actual-messages-per-second"])}</td>
                <td>{run.result.latency?.p50 ?? ""}</td>
                <td>{run.result.latency?.p95 ?? ""}</td>
                <td>{run.result.latency?.p99 ?? ""}</td>
                <td>{formatNumber(run.result["avg-latency-ms"])}</td>
                <td>{formatNumber(run.result["cpu-usage"])}</td>
                <td>{run.result["ram-usage"] ?? ""}</td>
                <td>{formatNumber(run.result.errors)}</td>
                <td>{formatNumber(run.result["successful-requests"])}</td>
                <td>{formatNumber(run["read-write-ratio"])}</td>
                <td>{formatTimestamp(run["started-at-epoch-secs"])}</td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <table>
        <caption>Per-query operation coverage</caption>
        <thead>
          <tr>
            <th>Vendor</th>
            <th>Query ID</th>
            <th>Operation count</th>
          </tr>
        </thead>
        <tbody>
          {runs.flatMap((run, runIndex) => {
            if (!run?.result) return [];
            return getQueryCounts(run).map(({ query, count }) => (
              <tr key={`${run.vendor}-${runIndex}-${query}`}>
                <td>{run.vendor}</td>
                <td>{query}</td>
                <td>{count === null ? "n/a" : formatNumber(count)}</td>
              </tr>
            ));
          })}
        </tbody>
      </table>
    </section>
  );
}
