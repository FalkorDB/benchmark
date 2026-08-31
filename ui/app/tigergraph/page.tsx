import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export default async function TigerGraphVsFalkor() {
  const dataUrl = "/summaries/tigergraph_vs_falkordb.json";
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
        comparisonVendors={["falkordb", "tigergraph"]}
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb", "tigergraph"],
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="TigerGraph vs FalkorDB"
      />
    </main>
  );
}
