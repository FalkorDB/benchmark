"use client";

import React, { useMemo } from "react";
import { Bar } from "react-chartjs-2";
import { normalizeVendor, vendorGradient } from "../lib/vendorColors";
import {
  Chart as ChartJS,
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title,
} from "chart.js";
import type {
  ChartData,
  Chart as ChartType,
  LegendItem,
  ScriptableContext,
} from "chart.js";
import ChartDataLabels from "chartjs-plugin-datalabels";

ChartJS.register(
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title,
  ChartDataLabels
);

interface LatencyStats {
  minValue: number;
  maxValue: number;
  ratio: number;
}

interface VerticalBarChartProps {
  chartId: string;
  chartData: ChartData<"bar", number[], string>;
  unit: string;
  latencyStats: {
    p50: LatencyStats;
    p95: LatencyStats;
    p99: LatencyStats;
  };
}

const VerticalBarChart: React.FC<VerticalBarChartProps> = ({
  chartId,
  chartData,
  unit,
  latencyStats
}) => {
  const xLabels = (chartData.labels ?? []) as string[];

  const legendFillForVendor = (chart: ChartType, vendorLike: string) => {
    // Canvas gradients are defined in absolute canvas coordinates. The legend box is drawn
    // at some x,y offset, so a small gradient (0..boxWidth) often collapses to a solid color.
    // To make the legend swatch reliably show the gradient, draw it into a tiny offscreen
    // canvas and use a repeating pattern.
    const w = 80;
    const h = 10;

    const off = document.createElement("canvas");
    off.width = w;
    off.height = h;

    const offCtx = off.getContext("2d");
    if (!offCtx) return vendorGradient(chart.ctx, vendorLike, "horizontal", w);

    const g = vendorGradient(offCtx, vendorLike, "horizontal", w);
    offCtx.fillStyle = g;
    offCtx.fillRect(0, 0, w, h);

    return chart.ctx.createPattern(off, "repeat") ?? g;
  };
  const chartDataWithGradients = useMemo(() => {
    const datasets = (chartData?.datasets ?? []).map((ds) => {
      const labelVendor = typeof ds.label === "string" ? ds.label.split(" ")[0] : "";
      const vendorLike = (labelVendor || "").toString();

      if (normalizeVendor(vendorLike) === "unknown") {
        return ds;
      }

      const backgroundColor = (context: ScriptableContext<"bar">) => {
        const h = context?.chart?.chartArea?.height;
        return vendorGradient(context.chart.ctx, vendorLike, "vertical", h);
      };

      return {
        ...ds,
        backgroundColor,
        hoverBackgroundColor: backgroundColor,
      };
    });

    return {
      ...chartData,
      datasets,
    };
  }, [chartData]);

  const options = {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      legend: {
        display: true,
        position: "top" as const,
        labels: {
          // Make the swatch wider so vendor gradients are visible (otherwise they can look solid).
          boxWidth: 80,
          boxHeight: 10,
          generateLabels: (chart: ChartType) => {
            const items: LegendItem[] =
              ChartJS.defaults.plugins.legend.labels.generateLabels(chart);

            return items.map((item) => {
              const text = (item?.text ?? "").toString();
              const vendorLike = text.trim().split(/\s+/)[0] ?? "";

              if (normalizeVendor(vendorLike) === "unknown") return item;

              // Force a legend-sized gradient. Using the chart-area gradient can look like a solid fill.
              const fill = legendFillForVendor(chart, vendorLike);
              return {
                ...item,
                fillStyle: fill,
                strokeStyle: fill,
                lineWidth: 0,
              };
            });
          },
        },
      },
      tooltip: {
        callbacks: {
          label: function (context: unknown) {
            const c = context as { raw?: unknown; dataset?: { label?: string } };
            const value = c.raw;
            return `${c.dataset?.label ?? ""}: ${String(value ?? "")}${unit}`;
          },
        },
      },
      datalabels: {
        display: chartId === "single" ? "auto" : true,
        anchor: "end" as const,
        align: "top" as const,
        font: {
          weight: "bold" as const,
          family: chartId !== "single" ? "Fira Code" : undefined,
          size: chartId !== "single" ? 14 : undefined,
        },
        color: "grey",
        formatter: (value: number, context: unknown) => {
          const ctx = context as { dataset?: { label?: string } };
          if (value <= 0) return "";

          // Single mode: show the raw histogram number.
          if (chartId === "single") {
            return `${Math.round(value)}`;
          }

          // Concurrent mode: show the bar value (ms/s), and also show the ratio on the max.
          const roundedValue = Math.round(value);
          const valueLabel = `${roundedValue}${unit}`;

          const label = ctx.dataset?.label;
          if (!label) return valueLabel;

          let percentileKey: keyof typeof latencyStats;
          if (label.includes("P50")) percentileKey = "p50";
          else if (label.includes("P95")) percentileKey = "p95";
          else if (label.includes("P99")) percentileKey = "p99";
          else return valueLabel;

          const maxValue = latencyStats[percentileKey].maxValue;
          const ratio = latencyStats[percentileKey].ratio;
          const isMaxValue = Math.abs(value - maxValue) < 0.5;

          return isMaxValue ? `${valueLabel}\n${Math.round(ratio)}x` : valueLabel;
        },
      },
      
      
    },
    scales: {
      x: {
        grid: { display: false },
        ticks: {
          font: {
            size: 16,
            family: 'Fira Code',
            weight: "bold" as const
          },
          color: "#000",
          padding: 10,
          callback: function (index: string | number) {
            const i = typeof index === "number" ? index : Number(index);
            return xLabels[i] ?? "";
          },
        },
        // title: { display: true, text: xAxisTitle, font: { size: 16 } }
      },
      y: {
        beginAtZero: true,
        grid: { display: true },
        ticks: {
          font: {
            size: 15,
            family: "Fira Code",
          },
          color: "#333",
          callback: (value: string | number) => `${value}${unit}`
        },
      },
    },
  };

  return (
    <div className="w-full h-full relative">
      <Bar data={chartDataWithGradients} options={options} />
    </div>
  );
};

export default VerticalBarChart;
