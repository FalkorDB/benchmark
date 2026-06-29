import DashBoard from "../components/dashboard";
import { Header } from "../components/header";

export default function MemgraphVsFalkor() {
  return (
    <main className="min-h-screen md:h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl="/summaries/memgraph_vs_falkordb.json"
        comparisonVendors={["falkordb", "memgraph"]}
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb", "memgraph"],
        }}
      />
    </main>
  );
}
