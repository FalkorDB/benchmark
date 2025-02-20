"use client";

import React, { useRef } from "react";
import { Bar } from "react-chartjs-2";
import {
  Chart as ChartJS,
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title,
  SubTitle,
} from "chart.js";

import type { Chart } from "chart.js";

ChartJS.register(
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
  Title,
  SubTitle
);

interface HorizontalBarChartProps {
  // eslint-disable-next-line
  data: { [key: string]: any }[];
  dataKey: string;
  chartLabel: string;
  unit?: string;
  ratio: number;
  maxValue: number;
  minValue: number;
}

const HorizontalBarChart: React.FC<HorizontalBarChartProps> = ({
  data,
  dataKey,
  chartLabel,
  unit,
  ratio,
  maxValue,
  minValue
}) => {
  const containerRef = useRef<null | HTMLDivElement>(null);
  const chartRef = useRef<Chart | null>(null);
  const backgroundColors = typeof window !== "undefined"
  ? [
      getComputedStyle(document.documentElement).getPropertyValue("--FalkorDB-color").trim(),
      getComputedStyle(document.documentElement).getPropertyValue("--Neo4j-color").trim(),
    ]
  : ["#FF66B3", "#0B6190"];

  const chartData = {
    labels: data.map((item) => item.vendor),
    datasets: [
      {
        label: chartLabel,
        data: data.map((item) => item[dataKey]),
        backgroundColor: backgroundColors,
        hoverBackgroundColor: backgroundColors,
        borderRadius: 8,
        barThickness: "flex" as const,
        categoryPercentage: 1.1,
      },
    ],
  };

  const options = {
    indexAxis: "y" as const,
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      legend: {
        display: false,
      },
      datalabels: {
        anchor: "end" as const,
        align: "right" as const,
        color: "grey",
        font: {
          weight: "bold" as const,
          family: "font-fira",
          size: 18,
        },
        // eslint-disable-next-line
        formatter: (_: any, context: { dataIndex: any; dataset: { data: { [x: string]: any; }; }; }) => {
          const index = context.dataIndex;
          const value = context.dataset.data[index];
          
          return value === maxValue ? ` x${ratio} ` : "";
        },
      },
    },
    scales: {
      x: {
        beginAtZero: true,
        max: maxValue * 1.1,
        ticks: {
          padding: 10,
          font: {
            size: 15,
            family: "font-fira",
          },
          color: "#333",
          // eslint-disable-next-line
          callback: (value: any) => `${Math.round(value)}${unit}`,
          stepSize: dataKey === "memory" ? maxValue / 5 : minValue / 0.5,
        },
        grid: {
          display: false,
          drawBorder: false,
        },
      },
      y: {
        ticks: {
          font: {
            size: 16,
            family: "font-fira",
          },
          color: "#333",
        },
        grid: {
          display: false,
          drawBorder: false,
        },
      },
    },
  };
  

  return (
    <div ref={containerRef} className="w-full h-full relative">
      <Bar
        ref={(el) => {
          if (el) {
            chartRef.current = el;
          }
        }}
        data={chartData}
        options={options}
      />
    </div>
  );
};

export default HorizontalBarChart;
