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

interface LatencyStats {
  minValue: number;
  maxValue: number;
  ratio: number;
}

interface VerticalBarChartProps {
  chartId: string;
  // eslint-disable-next-line
  chartData: any;
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
  const options = {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      legend: { display: true, position: "top" as const },
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
        display: chartId === "single" ? "auto" : true,
        anchor: "end" as const,
        align: "top" as const,
        font: {
          weight: "bold" as const,
          family: "font-fira",
          size: 18,
        },
        color: "grey",
        // eslint-disable-next-line
        formatter: (value: number, context: any) => {
          if (chartId === "single") {
            return value > 0 ? `${value}` : "";
          }
          const label = context.dataset.label;
          if (!label) return "";
          let percentileKey: keyof typeof latencyStats;
          if (label.includes("P50")) percentileKey = "p50";
          else if (label.includes("P95")) percentileKey = "p95";
          else if (label.includes("P99")) percentileKey = "p99";
          else return ""; 
      
          const maxValue = latencyStats[percentileKey].maxValue;
          const ratio = latencyStats[percentileKey].ratio; 
          const isMaxValue = Math.abs(value - maxValue) < 0.5;
  
          return isMaxValue ? `${Math.round(ratio)}x` : "";
        },
      },
      
      
    },
    scales: {
      x: {
        grid: { display: false },
        ticks: {
          font: {
            size: 16,
            family: 'font-fira',
            weight: "bold" as const
          },
          color: "#000",
          padding: 10,
          callback: function (index: string | number) {
            return chartData.labels[index];
          },
        },
        // title: { display: true, text: xAxisTitle, font: { size: 16 } }
      },
      y: {
        beginAtZero: true,
        grid: { display: true },
        // eslint-disable-next-line
        ticks: {
          font: {
            size: 15,
            family: "font-fira",
          },
          color: "#333",
          // eslint-disable-next-line
          callback: (value: any) => `${value}${unit}` 
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
