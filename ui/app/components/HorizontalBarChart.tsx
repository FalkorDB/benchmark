"use client";

import React, { useRef, useEffect } from "react";
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
  subtitle: string;
  yAxisTitle: string;
}

const HorizontalBarChart: React.FC<HorizontalBarChartProps> = ({
  data,
  dataKey,
  chartLabel,
  title,
  subtitle,
  yAxisTitle,
}) => {
  const containerRef = useRef<null | HTMLDivElement>(null);
  const chartRef = useRef<Chart | null>(null);

  const chartData = {
    labels: data.map((item) => item.vendor),
    datasets: [
      {
        label: chartLabel,
        data: data.map((item) => item[dataKey]),
        backgroundColor: ["#FF66B3", "#0B6190"],
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
        },
        padding: {
          bottom: 2,
        },
      },
      subtitle: {
        display: true,
        text: subtitle,
        font: {
          size: 12,
          weight: "normal" as const,
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
          // eslint-disable-next-line
          callback: function (value: any) {
            return Number(value).toLocaleString();
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

  useEffect(() => {
    const handleResize = () => {
      if (chartRef.current) {
        chartRef.current.resize();
      }
    };

    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

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
