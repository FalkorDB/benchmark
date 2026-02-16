export type VendorKey = "falkordb" | "neo4j" | "memgraph" | "unknown";

type GradientStops = { offset: number; color: string }[];

type Orientation = "vertical" | "horizontal";

export function normalizeVendor(vendor: string): VendorKey {
  const k = (vendor ?? "").toString().trim().toLowerCase();
  if (k === "falkordb" || k === "falkor") return "falkordb";
  if (k === "neo4j") return "neo4j";
  if (k === "memgraph") return "memgraph";
  return "unknown";
}

function cssVar(name: string, fallback: string) {
  if (typeof window === "undefined") return fallback;
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v || fallback;
}

function getStops(vendor: VendorKey): GradientStops {
  switch (vendor) {
    case "falkordb":
      // Pink -> purple
      return [
        { offset: 0.0, color: cssVar("--FalkorDB-gradient-start", "#ff66b3") },
        { offset: 1.0, color: cssVar("--FalkorDB-gradient-end", "#7568F2") },
      ];
    case "neo4j":
      // Neo4j "Baltic" palette: Dark -> Mid -> Baltic -> Light
      return [
        {
          offset: 0.0,
          color: cssVar("--Neo4j-gradient-dark-baltic", "#014063"),
        },
        {
          offset: 0.33,
          color: cssVar("--Neo4j-gradient-mid-baltic", "#0A6190"),
        },
        { offset: 0.66, color: cssVar("--Neo4j-gradient-baltic", "#4C99A4") },
        {
          offset: 1.0,
          color: cssVar("--Neo4j-gradient-light-baltic", "#8FE3E8"),
        },
      ];
    case "memgraph":
      // Red/pink -> orange -> yellow (Memgraph brand-like gradient)
      return [
        { offset: 0.0, color: cssVar("--Memgraph-gradient-start", "#ff2b4a") },
        { offset: 0.55, color: cssVar("--Memgraph-gradient-mid", "#ff7a00") },
        { offset: 1.0, color: cssVar("--Memgraph-gradient-end", "#ffd000") },
      ];
    default:
      return [{ offset: 0.0, color: "#191919" }];
  }
}

export function vendorGradient(
  ctx: CanvasRenderingContext2D,
  vendorLike: string,
  orientation: Orientation,
  length?: number
): string | CanvasGradient {
  const vendor = normalizeVendor(vendorLike);
  const stops = getStops(vendor);

  if (!ctx) return "#191919";

  // If the gradient spans a huge distance but the rendered area is tiny (e.g. legend box),
  // it will look like a solid fill. Let callers provide a more appropriate length.
  const size = Math.max(12, Math.round(Number(length ?? 300)));

  const g =
    orientation === "horizontal"
      ? ctx.createLinearGradient(0, 0, size, 0)
      : ctx.createLinearGradient(0, size, 0, 0);

  for (const s of stops) {
    g.addColorStop(s.offset, s.color);
  }

  return g;
}
