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
  title: string;
  subTitle: string;
  yAxisTitle: string;
  unit?: string;
}

const HorizontalBarChart: React.FC<HorizontalBarChartProps> = ({
  data,
  dataKey,
  chartLabel,
  title,
  subTitle,
  yAxisTitle,
  unit
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
        borderRadius: 8,
        barThickness: "flex" as const,
        categoryPercentage: 1,
      },
    ],
  };

  const maxDataValue = Math.max(...data.map((item) => item[dataKey]));

  const options = {
    indexAxis: "y" as const,
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
        padding: {
          bottom: 2,
        },
      },
      subtitle: {
        display: true,
        text: subTitle,
        font: {
          size: 13,
          weight: "normal" as const,
          family: "fira"
        },
        padding: {
          bottom: 2,
        },
      },
      legend: {
        display: false,
      },
    },
    scales: {
      x: {
        beginAtZero: true,
        max: maxDataValue * 1.1,
        grid: {
          display: false,
        },
        ticks: {
          font: {
            size: 14,
          },
          stepSize: dataKey === "memory" ? 5000 : 300,
          // eslint-disable-next-line
          callback: function (value: any) {
            return `${Math.round(Number(value)).toLocaleString()}${unit}`;
          },
        },
      },
      y: {
        grid: {
          display: false,
        },
        title: {
          display: true,
          text: yAxisTitle,
          font: {
            size: 16,
          },
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
