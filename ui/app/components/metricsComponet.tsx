"use client";

import React from "react";

const MetricsComponent: React.FC = () => {
  const metrics = [
    { label: "MEMORY USAGE", value: "{#}" },
    { label: "CPU USAGE", value: "{#}" },
    { label: "THROUGHPUT", value: "{#}" },
    { label: "LAG", value: "{#}" },
    { label: "TOTAL RUNTIME", value: "{#}" },
    { label: "WORKLOAD (READ/WRITE)", value: "{#}" },
  ];

  return (
    <div className="flex flex-col gap-4 p-4 bg-gray-50 rounded-md h-full overflow-y-auto">
      {metrics.map((metric, index) => (
        <div
          key={index}
          className="flex items-center justify-between bg-white p-4 rounded-md shadow border border-gray-200"
        >
          <span className="font-bold text-gray-800 text-sm truncate max-w-[50%]">
            {metric.label}:
          </span>
          <div className="bg-white border border-[#7466FF] text-[#7466FF] rounded-md px-6 py-2 font-semibold text-sm shadow truncate max-w-[40%]">
            {metric.value}
          </div>
        </div>
      ))}
      <div className="bg-gray-100 border border-gray-300 rounded-md p-6 shadow-sm text-center">
        <div>
          <div className="font-medium text-gray-700 text-sm">
            Machine Specifications
          </div>
          <div className="text-gray-500 text-xs">(Technical)</div>
        </div>
      </div>
    </div>
  );
};

export default MetricsComponent;
