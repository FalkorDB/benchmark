import {
  Layers,
  Cpu,
  Users,
  Activity,
  BarChart,
  Search,
} from "lucide-react";
export const sidebarConfig: {
  sidebarData: {
    title: string;
    description?: string;
    layout?: "row" | "col";
    icon: React.ElementType;
    options: { id: string; label: string;}[];
  }[];
} = {
  sidebarData: [
    {
      title: "Workload Type",
      description: "",
      layout: "row",
      icon: Activity,
      options: [
        { id: "concurrent", label: "Concurrent" },
        { id: "single", label: "Single" },
      ],
    },
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
        { id: "arm", label: "ARM" },
        { id: "intel", label: "INTEL" },
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
    {
      title: "Queries",
      description: "",
      layout: "col",
      icon: Search,
      options: [
        { id: "aggregate_expansion_4", label: "Expand 4L" },
        { id: "aggregate_expansion_4_with_filter", label: "Expand 4L (Filtered)" },
        { id: "aggregate_expansion_3", label: "Expand 3L" },
        { id: "aggregate_expansion_3_with_filter", label: "Expand 3L (Filtered)" },
        { id: "aggregate_expansion_2", label: "Expand 2L" },
        { id: "aggregate_expansion_2_with_filter", label: "Expand 2L (Filtered)" },
        { id: "aggregate_expansion_1", label: "Expand 1L" },
        { id: "aggregate_expansion_1_with_filter", label: "Expand 1L (Filtered)" },
        { id: "single_edge_write", label: "Write Edge" },
        { id: "single_vertex_write", label: "Write Vertex" },
        { id: "write", label: "Write General" },
        { id: "single_vertex_read", label: "Read Vertex" },
      ],
    },
    {
      title: "Realistic Workload",
      description: "",
      layout: "col",
      icon: BarChart,
      options: [{ id: "1", label: "100% Read / 0% Write" }],
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
