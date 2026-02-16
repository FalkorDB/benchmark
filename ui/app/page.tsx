
import DashBoard from "./components/dashboard";
import { Header } from "./components/header";

export default function Home() {
  return (
    <main className="h-screen flex flex-col">
      <Header />
      <DashBoard comparisonVendors={["falkordb", "neo4j"]} />
    </main>
  );
}