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

interface MemoryBarChartProps {
  singleMemory: { vendor: string; memory: number }[];
  unit: string;
  ratio: number;
  maxValue: number;
  minValue: number;
  getBarColor: (vendor: string) => string;
}

const MemoryBarChart: React.FC<MemoryBarChartProps> = ({
  singleMemory,
  unit,
  ratio,
  maxValue,
  getBarColor,
}) => {
  const chartDataForMemory = useMemo(() => {
    return {
      labels: singleMemory.map(({ vendor }) => vendor),
      datasets: [
        {
          label: "Memory Usage",
          data: singleMemory.map(({ memory }) => memory),
          // eslint-disable-next-line
          backgroundColor: (context: any) => {
            const i = context?.dataIndex ?? 0;
            const vendor = (singleMemory[i]?.vendor ?? "").toString();
            if (normalizeVendor(vendor) === "unknown") return getBarColor(vendor);
            const h = context?.chart?.chartArea?.height;
            return vendorGradient(context.chart.ctx, vendor, "vertical", h);
          },
          // eslint-disable-next-line
          hoverBackgroundColor: (context: any) => {
            const i = context?.dataIndex ?? 0;
            const vendor = (singleMemory[i]?.vendor ?? "").toString();
            if (normalizeVendor(vendor) === "unknown") return getBarColor(vendor);
            const h = context?.chart?.chartArea?.height;
            return vendorGradient(context.chart.ctx, vendor, "vertical", h);
          },
          borderRadius: 8,
          barPercentage: 0.9,
          categoryPercentage: 1,
        },
      ],
    };
  }, [singleMemory, getBarColor]);

  const options = {
    responsive: true,
    maintainAspectRatio: false,
    layout: {
      padding: {
        top: 40,
      },
    },
    plugins: {
      legend: { display: false },
      tooltip: {
        callbacks: {
          // eslint-disable-next-line
          label: function (context: any) {
            const value = context.raw;
            return `${context.dataset.label}: ${value}${unit}`;
          },
        },
      },
      datalabels: {
        display: true,
        anchor: "end" as const,
        align: "top" as const,
        font: {
          weight: "bold" as const,
          family: "Fira Code",
          size: 18,
        },
        color: "grey",
        formatter: (value: number) => {
          return value === maxValue ? `x${ratio}` : "";
        },
      },
    },
    scales: {
      x: {
        grid: { display: false },
        ticks: {
          font: {
            size: 14,
            family: "Fira Code",
          },
          color: "#000",
          // eslint-disable-next-line
          callback: (value: any, index: number) => chartDataForMemory.labels[index],
        },
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
          // eslint-disable-next-line
          callback: (value: any) => `${value}${unit}`,
        },
      },
    },
  };

  return (
    <div className="w-full h-full relative">
      <Bar data={chartDataForMemory} options={options} />
    </div>
  );
};

export default MemoryBarChart;
