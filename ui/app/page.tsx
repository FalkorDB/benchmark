
import { redirect } from "next/navigation";

export const dynamic = "force-dynamic";

// The public default landing page is the Neo4j vs FalkorDB comparison.
export default async function Home() {
  redirect("/neo4j");
}
