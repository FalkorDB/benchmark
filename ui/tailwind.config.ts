import type { Config } from "tailwindcss";

export default {
  content: [
    "./pages/**/*.{js,ts,jsx,tsx,mdx}",
    "./components/**/*.{js,ts,jsx,tsx,mdx}",
    "./app/**/*.{js,ts,jsx,tsx,mdx}",
  ],
  theme: {
    extend: {
      colors: {
        background: "var(--background)",
        foreground: "var(--foreground)",
      },
      fontFamily: {
        space: ['"Space Grotesk"', 'sans-serif'], // Custom font for Space Grotesk
        fira: ['"Fira Code"', 'monospace'],
      },
    },
  },
  plugins: [],
} satisfies Config;
