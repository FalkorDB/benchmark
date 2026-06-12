import DashBoard from "../components/dashboard";
import { Header } from "../components/header";

export default function FalkorDBCompare() {
  return (
    <main className="h-screen flex flex-col">
      <Header />
      <DashBoard
        dataUrl="/summaries/falkordb_vs_falkordb.json"
        comparisonVendors={["falkordb-c", "falkordb-rs", "falkordb1", "falkordb2", "r6g.xl", "r7g.xl", "r8g.xl", "r6i.xl", "r7i.xl"]}
        initialSelectedOptions={{
          "Workload Type": ["concurrent"],
          Vendors: ["falkordb-c", "falkordb-rs", "falkordb1", "falkordb2", "r6g.xl", "r7g.xl", "r8g.xl", "r6i.xl", "r7i.xl"],
        }}
      />
    </main>
  );
}
