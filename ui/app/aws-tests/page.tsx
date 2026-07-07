import DashBoard from "../components/dashboard";
import { Header } from "../components/header";
import BenchmarkMetricsCrawlerTable from "../components/BenchmarkMetricsCrawlerTable";
import { loadBenchmarkSummary, loadRunsManifest } from "../lib/benchmark-data.server";

export default async function AwsTestsFalkorGravitonVsIntel() {
  const dataUrl = "/summaries/aws_tests_falkor_graviton_vs_intel.json";
  const [initialData, initialManifest] = await Promise.all([
    loadBenchmarkSummary(dataUrl),
    loadRunsManifest(),
  ]);
  return (
    <main className="h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl={dataUrl}
        initialData={initialData}
        initialManifest={initialManifest}
        // Do not hardcode comparisonVendors: aws-tests labels are instance types (e.g. r7i.2xlarge).
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Hardware: ["falkordb1", "falkordb2", "intel", "arm"],
        }}
      />
      <BenchmarkMetricsCrawlerTable
        data={initialData}
        dataUrl={dataUrl}
        title="AWS FalkorDB Graviton vs Intel"
      />
    </main>
  );
}
