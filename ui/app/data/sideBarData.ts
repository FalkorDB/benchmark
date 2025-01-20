import { Layers, Cpu, Send, Users} from "lucide-react";
export const sidebarConfig: {
    sidebarData: {
      title: string;
      description?: string;
      layout?: "row" | "col";
      icon: React.ElementType;
      options: { id: string; label: string }[];
    }[];
  } = {
    sidebarData: [
      {
        title: "Vendors",
        description: "",
        layout: "row",
        icon: Layers,
        options: [
          { id: "falkordb", label: "FalkorDB" },
          { id: "neo4j", label: "Neo4j" },
        ],
      },
      {
        title: "Hardware",
        description: "",
        layout: "col",
        icon: Cpu,
        options: [
          { id: "macbook", label: "MacBook" },
          { id: "linux", label: "Linux" },
        ],
      },
      {
        title: "Throughput",
        description: "Message request/second",
        layout: "col",
        icon: Send,
        options: [
          { id: "500", label: "500 m/s" },
          { id: "1000", label: "1000 m/s" },
          { id: "1500", label: "1500 m/s" },
          { id: "2000", label: "2000 m/s" },
          { id: "2500", label: "2500 m/s" },
        ],
      },
      {
        title: "Clients",
        description: "Number of parallel queries",
        layout: "row",
        icon: Users,
        options: [
          { id: "20", label: "20" },
          { id: "40", label: "40" },
        ],
      },
    ],
  };
  
  export const userBrandInfo = {
    user: {
      name: "FalkorDB User",
      email: "info@falkordb.com",
    },
    brand: [
      {
        name: "FalkorDB",
        plan: "Enterprise",
      },
    ],
  };