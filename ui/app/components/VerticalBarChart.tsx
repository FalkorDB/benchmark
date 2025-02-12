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

interface VerticalBarChartProps {
  chartId: string;
  chartData: any;
  title: string;
  subTitle: string;
  xAxisTitle: string;
}

const VerticalBarChart: React.FC<VerticalBarChartProps> = ({
  chartId,
  chartData,
  title,
  subTitle,
  xAxisTitle,
}) => {
  const options = {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      title: {
        display: true,
        text: title,
        font: { size: 20, weight: "bold" as const },
      },
      subtitle: {
        display: true,
        text: subTitle,
        font: { size: 13, weight: "bold" as const },
      },
      legend: { display: true, position: "top" as const },
      tooltip: {
        callbacks: {
          label: function (context: any) {
            const value = context.raw;
            return `${context.dataset.label}: ${value} ms`;
          },
        },
      },
      datalabels: {
        display: chartId === "2" ? "auto" : false,
        anchor: "end" as const,
        align: "top" as const,
        formatter: (value: number) => (value > 0 ? `${Math.round(value)}` : ""),
        font: { weight: "bold" as const},
        color: "#000",
      },
    },
    scales: {
      x: {
        grid: { display: false },
        title: { display: true, text: xAxisTitle, font: { size: 16 } },
      },
      y: {
        beginAtZero: true,
        grid: { display: true },
        ticks: { callback: (value: any) => `${value} ms` },
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
