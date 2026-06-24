
import DashBoard from "./components/dashboard";
import { Header } from "./components/header";

export default function Home() {
  return (
    <main className="h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl="/summaries/neo4j_vs_falkordb.json"
        comparisonVendors={["falkordb", "neo4j"]}
        hideHardware
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb", "neo4j"],
        }}
      />
    </main>
  );
}