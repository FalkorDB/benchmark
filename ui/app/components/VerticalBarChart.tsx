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
  subTitle: string;
  xAxisTitle: string;
}

const getBarColor = (vendor: string) => {
  switch (vendor.toLowerCase()) {
    case "falkordb":
      return getComputedStyle(document.documentElement).getPropertyValue("--FalkorDB-color").trim();
    case "neo4j":
      return getComputedStyle(document.documentElement).getPropertyValue("--Neo4j-color").trim();;
    default:
      return "#191919";
  }
};

const VerticalBarChart: React.FC<VerticalBarChartProps> = ({
  data,
  title,
  subTitle,
  xAxisTitle,
}) => {
  const chartData = {
    labels: ["P50", "P95", "P99"],
    datasets: data.flatMap((item, index) => [
      {
        label: `${item.vendor} P50`,
        data: [item.p50, 0, 0],
        backgroundColor: getBarColor(item.vendor),
        stack: `${index}`,
        borderRadius: 8,
      },
      {
        label: `${item.vendor} P95`,
        data: [0, item.p95, 0],
        backgroundColor: getBarColor(item.vendor),
        stack: `${index}`,
        borderRadius: 8,
      },
      {
        label: `${item.vendor} P99`,
        data: [0, 0, item.p99],
        backgroundColor: getBarColor(item.vendor),
        stack: `${index}`,
        borderRadius: 8,
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
          family: "space"
        },
      },
      subtitle: {
        display: true,
        text: subTitle,
        font: { size: 13 , family: "fira"},
      },
      legend: {
        display: true,
        position: "top" as const,
      },
      tooltip: {
        callbacks: {
          // eslint-disable-next-line
          label: function (context: any) {
            const value = context.raw;
            return `${context.dataset.label}: ${value} ms`;
          },
        },
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
        ticks: {
          // eslint-disable-next-line
          callback: (value: any) => `${value} ms`,
        },
      },
    },
  };

  return (
    <div className="w-full h-full relative">
      <Bar data={chartData} options={options} />
    </div>
  );
};

export default VerticalBarChart;
