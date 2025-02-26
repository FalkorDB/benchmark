import type { Metadata } from "next";
import "./globals.css";
import { Toaster } from "@/components/ui/toaster";
import GTM from "./GTM";

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
        <GTM />
        {children}
        <Toaster />
      </body>
    </html>
  );
}
