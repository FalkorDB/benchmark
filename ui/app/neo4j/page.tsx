import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export default async function Neo4jVsFalkor() {
  const dataUrl = "/summaries/neo4j_vs_falkordb.json";
  const [initialData, initialManifest] = await Promise.all([
    loadBenchmarkSummary(dataUrl),
    loadRunsManifest(),
  ]);
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
