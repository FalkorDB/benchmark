
import { redirect } from "next/navigation";
import DashBoard from "./components/dashboard";
import { Header } from "./components/header";
import BenchmarkMetricsCrawlerTable from "./components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "./lib/benchmark-data.server";

export const dynamic = "force-dynamic";

const PAIRWISE_DEFAULT_URL = "/summaries/neo4j_vs_falkordb.json";
const MULTI_ENGINE_URL = "/summaries/multi_engine_compare.json";

export default async function Home() {
  const [multiData, pairwiseData, initialManifest] = await Promise.all([
    loadBenchmarkSummary(MULTI_ENGINE_URL),
    loadBenchmarkSummary(PAIRWISE_DEFAULT_URL),
    loadRunsManifest(),
  ]);

  const multiVendors = Array.from(
    new Set(
      (multiData?.runs ?? [])
        .map((r) => r.vendor?.toString().toLowerCase())
        .filter(Boolean)
    )
  );

  // If at least 3 engines were run, multi-engine is the default view.
  if (multiVendors.length >= 3) {
    redirect("/multi-engine");
  }

  const dataUrl = PAIRWISE_DEFAULT_URL;
  const initialData = pairwiseData;
  return (
    <main className="min-h-screen md:h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl={dataUrl}
        initialData={initialData}
        initialManifest={initialManifest}
        comparisonVendors={["falkordb", "neo4j"]}
        hideHardware
        initialSelectedOptions={{
          "Workload Type": ["single"],
          Vendors: ["falkordb", "neo4j"],
          Queries: ["aggregate_expansion_4_with_filter"],
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="Neo4j vs FalkorDB"
      />
    </main>
  );
}
