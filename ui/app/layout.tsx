import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Benchmark by FalkorDB",
  description: "Benchmark application by FalkorDB",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>
        {children}
      </body>
    </html>
  );
}
