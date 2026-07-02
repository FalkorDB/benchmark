import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export default async function MemgraphVsFalkor() {
  const dataUrl = "/summaries/memgraph_vs_falkordb.json";
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
        comparisonVendors={["falkordb", "memgraph"]}
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb", "memgraph"],
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="Memgraph vs FalkorDB"
      />
    </main>
  );
}
