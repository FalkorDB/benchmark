"use client";

import React, { useMemo } from "react";
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
  minValue,
  getBarColor,
}) => {
  const chartDataForMemory = useMemo(() => {
    return {
      labels: singleMemory.map(({ vendor }) => vendor),
      datasets: [
        {
          label: "Memory Usage",
          data: singleMemory.map(({ memory }) => memory),
          backgroundColor: singleMemory.map(({ vendor }) => getBarColor(vendor)),
          hoverBackgroundColor: singleMemory.map(({ vendor }) => getBarColor(vendor)),
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
        anchor: "start" as const,
        align: "start" as const,
        font: {
          weight: "bold" as const,
          family: "Fira Code",
          size: 18,
        },
        color: "grey",
        formatter: (value: number) => {
          return value === minValue ? `x${ratio}` : "";
        },
      },
    },
    scales: {
      x: {
        position: "top" as const,
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
        reverse: true,
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
