
import { Header } from "./components/header";
import SideBar from "./dashboard/page";

export default function Home() {
  return (
    <main className="h-screen flex flex-col">
      <Header />
      <SideBar />
    </main>
  );
}