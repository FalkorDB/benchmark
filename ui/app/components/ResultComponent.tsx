"use client";

import React, { useState } from "react";

const ToggleResultsView: React.FC = () => {
  const [view, setView] = useState<string>("individual");

  return (
    <div className="flex flex-col h-full w-full">
      <div className="relative flex items-center w-full mb-4">
        <label className="absolute left-0 text-gray-700 text-lg font-semibold">
          Show me:
        </label>
        <div className="mx-auto flex items-center bg-gray-100 p-1 rounded-md space-x-2">
          <button
            onClick={() => setView("individual")}
            className={`px-6 py-2 rounded-md font-semibold transition-all ${
              view === "individual"
                ? "bg-[#7466FF] text-white"
                : "bg-transparent text-gray-600 hover:text-[#7466FF]"
            }`}
          >
            INDIVIDUAL VENDOR
          </button>
          <button
            onClick={() => setView("side-by-side")}
            className={`px-6 py-2 rounded-md font-semibold transition-all ${
              view === "side-by-side"
                ? "bg-[#7466FF] text-white"
                : "bg-transparent text-gray-600 hover:text-[#7466FF]"
            }`}
          >
            SIDE-BY-SIDE
          </button>
        </div>
      </div>
      <div className="flex-grow w-full bg-gray-200 flex items-center justify-center rounded-md">
        <p className="text-gray-600 font-bold text-lg">
          {view === "individual"
            ? "SHOW RESULTS HERE"
            : "SHOW SIDE-BY-SIDE RESULTS HERE"}
        </p>
      </div>
    </div>
  );
};

export default ToggleResultsView;
