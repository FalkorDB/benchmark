import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export default async function FalkorDBCompare() {
  const dataUrl = "/summaries/falkordb_vs_falkordb.json";
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
        comparisonVendors={["falkordb-c", "falkordb-rs", "falkordb1", "falkordb2", "r6g.xl", "r7g.xl", "r8g.xl", "r6i.xl", "r7i.xl"]}
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb-c", "falkordb-rs", "falkordb1", "falkordb2", "r6g.xl", "r7g.xl", "r8g.xl", "r6i.xl", "r7i.xl"],
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="FalkorDB version comparison"
      />
    </main>
  );
}
