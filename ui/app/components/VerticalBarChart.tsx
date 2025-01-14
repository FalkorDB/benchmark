"use client";

import React from "react";
import { Bar } from "react-chartjs-2";
import {
  Chart as ChartJS,
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title,
} from "chart.js";

ChartJS.register(
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title
);

interface VerticalBarChartProps {
  data: { vendor: string; p50: number; p95: number; p99: number }[];
  title: string;
  subtitle: string;
  xAxisTitle: string;
}

const getBarColor = (vendor: string, defaultColor: string) => {
  switch (vendor.toLowerCase()) {
    case "falkordb":
      return "#FF66B3";
    case "neo4j":
      return "#0B6190";
    default:
      return defaultColor;
  }
};

const VerticalBarChart: React.FC<VerticalBarChartProps> = ({
  data,
  title,
  subtitle,
  xAxisTitle,
}) => {
  const chartData = {
    labels: ["P50", "P95", "P99"],
    datasets: data.flatMap((item, index) => [
      {
        label: `${item.vendor} P50`,
        data: [item.p50, 0, 0],
        backgroundColor: getBarColor(item.vendor, "#FF804D"),
        stack: `${index}`,
      },
      {
        label: `${item.vendor} P95`,
        data: [0, item.p95, 0],
        backgroundColor: getBarColor(item.vendor, "#7466FF"),
        stack: `${index}`,
      },
      {
        label: `${item.vendor} P99`,
        data: [0, 0, item.p99],
        backgroundColor: getBarColor(item.vendor, "#191919"),
        stack: `${index}`,
      },
    ]),
  };

  const options = {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      title: {
        display: true,
        text: title,
        font: {
          size: 20,
          weight: "bold" as const,
        },
      },
      subtitle: {
        display: true,
        text: subtitle,
        font: { size: 12 },
      },
      legend: {
        display: true,
        position: "top" as const,
      },
    },
    scales: {
      x: {
        grid: { display: false },
        title: {
          display: true,
          text: xAxisTitle,
          font: { size: 16 },
        },
        stacked: true,
      },
      y: {
        beginAtZero: true,
        grid: { display: true },
      },
    },
  };

  return (
    <div className="w-full h-full">
      <Bar data={chartData} options={options} />
    </div>
  );
};

export default VerticalBarChart;
