import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export const dynamic = "force-dynamic";

/** All engines present in a multi-run summary (up to 5). */
const MULTI_ENGINE_VENDORS = [
  "falkordb",
  "falkordb-c",
  "falkordb-rs",
  "falkordb1",
  "falkordb2",
  "neo4j",
  "memgraph",
  "postgres",
  "mongo",
  "tigergraph",
];

export default async function MultiEngineCompare() {
  const dataUrl = "/summaries/multi_engine_compare.json";
  const [initialData, initialManifest] = await Promise.all([
    loadBenchmarkSummary(dataUrl),
    loadRunsManifest(),
  ]);

  // Prefer vendors actually present in the summary so charts stay dense.
  const present = Array.from(
    new Set(
      (initialData?.runs ?? [])
        .map((r) => r.vendor?.toString().toLowerCase())
        .filter(Boolean)
    )
  );
  const comparisonVendors = present.length
    ? present.slice(0, 5)
    : MULTI_ENGINE_VENDORS.slice(0, 5);

  return (
    <main className="min-h-screen md:h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl={dataUrl}
        initialData={initialData}
        initialManifest={initialManifest}
        comparisonVendors={comparisonVendors}
        hideHardware
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: comparisonVendors,
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="Multi-engine comparison (up to 5)"
      />
    </main>
  );
}
