"use client";

import html2canvas from "html2canvas";
import { jsPDF } from "jspdf";

export type DashboardPdfExportOptions = {
  root: HTMLElement;
  title?: string;
  subtitle?: string;
  queryNames?: string[];
  selectQuery?: (queryName: string) => void | Promise<void>;
  selectWorkload?: (workload: "concurrent" | "single") => void | Promise<void>;
  fileBaseName?: string;
  /** Optional cap; omit or set high to export every query histogram. */
  maxQueries?: number;
  onProgress?: (message: string) => void;
};

const PAGE_WIDTH_MM = 297;
const PAGE_HEIGHT_MM = 210;
const MARGIN_MM = 10;

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForChartsIdle(root: HTMLElement, timeoutMs = 5000) {
  const start = Date.now();
  await sleep(200);
  while (Date.now() - start < timeoutMs) {
    const canvases = Array.from(root.querySelectorAll("canvas")) as HTMLCanvasElement[];
    const ready = canvases.some((c) => c.width > 8 && c.height > 8);
    if (ready) {
      await sleep(300);
      return;
    }
    await sleep(120);
  }
}

/** html2canvas chokes on modern CSS (oklch/lab/color-mix). Flatten to safe colors in the clone. */
function sanitizeCloneForHtml2Canvas(doc: Document) {
  const root = doc.body;
  if (!root) return;

  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
  const modernColor = /(oklch|oklab|lab\(|lch\(|color-mix|color\()/i;

  while (walker.nextNode()) {
    const el = walker.currentNode as HTMLElement;
    if (!el?.style) continue;

    try {
      const cs = doc.defaultView?.getComputedStyle(el);
      if (!cs) continue;

      const bg = cs.backgroundColor;
      const fg = cs.color;
      const border = cs.borderColor;

      // Force opaque-ish backgrounds; replace unsupported functions with hex fallbacks.
      if (!bg || bg === "transparent" || modernColor.test(bg)) {
        el.style.backgroundColor = "#ffffff";
      } else {
        el.style.backgroundColor = bg;
      }

      if (!fg || modernColor.test(fg)) {
        el.style.color = "#111111";
      } else {
        el.style.color = fg;
      }

      if (border && modernColor.test(border)) {
        el.style.borderColor = "#e5e7eb";
      }

      // Drop gradients / filters that often break rasterization.
      el.style.backgroundImage = "none";
      el.style.boxShadow = "none";
      el.style.filter = "none";
      el.style.backdropFilter = "none";
      el.style.textShadow = "none";
    } catch {
      el.style.backgroundColor = "#ffffff";
      el.style.color = "#111111";
    }
  }
}

/**
 * Prefer cloning Chart.js canvases (reliable). Fall back to html2canvas of the section card.
 */
async function captureSection(el: HTMLElement): Promise<HTMLCanvasElement> {
  const chartCanvases = Array.from(
    el.querySelectorAll("canvas")
  ) as HTMLCanvasElement[];

  // If the section is mostly one chart canvas, compose a simple page-sized canvas
  // with title text + chart bitmap (avoids html2canvas CSS issues).
  if (chartCanvases.length === 1) {
    const src = chartCanvases[0];
    if (src.width > 8 && src.height > 8) {
      try {
        const title =
          el.querySelector("h2")?.textContent?.trim() ||
          el.dataset.pdfSection ||
          "";
        const subtitle = Array.from(el.querySelectorAll("p"))
          .slice(0, 2)
          .map((p) => p.textContent?.trim() || "")
          .filter(Boolean)
          .join(" — ");

        const out = document.createElement("canvas");
        const pad = 24;
        const titleH = title ? 48 : 0;
        const subH = subtitle ? 28 : 0;
        const header = titleH + subH + (title || subtitle ? 16 : 0);
        out.width = Math.max(src.width + pad * 2, 800);
        out.height = src.height + header + pad * 2;
        const ctx = out.getContext("2d");
        if (ctx) {
          ctx.fillStyle = "#ffffff";
          ctx.fillRect(0, 0, out.width, out.height);
          ctx.fillStyle = "#111111";
          ctx.font = "bold 28px Helvetica, Arial, sans-serif";
          if (title) ctx.fillText(title, pad, pad + 28);
          ctx.font = "16px Helvetica, Arial, sans-serif";
          ctx.fillStyle = "#4b5563";
          if (subtitle) ctx.fillText(subtitle.slice(0, 120), pad, pad + titleH + 20);
          const dx = Math.max(pad, Math.floor((out.width - src.width) / 2));
          ctx.drawImage(src, dx, pad + header);
          return out;
        }
      } catch {
        // fall through to html2canvas
      }
    }
  }

  // Multi-canvas / tables: html2canvas with color sanitization.
  const prevOverflow = el.style.overflow;
  const prevHeight = el.style.height;
  const prevMaxHeight = el.style.maxHeight;
  el.style.overflow = "visible";
  el.style.height = "auto";
  el.style.maxHeight = "none";

  try {
    return await html2canvas(el, {
      backgroundColor: "#ffffff",
      scale: Math.min(2, window.devicePixelRatio || 1.25),
      useCORS: true,
      allowTaint: true,
      logging: false,
      foreignObjectRendering: false,
      onclone: (doc) => sanitizeCloneForHtml2Canvas(doc),
      ignoreElements: (node) => {
        if (!(node instanceof HTMLElement)) return false;
        return (
          node.dataset.pdfIgnore === "true" ||
          node.getAttribute("data-sidebar") === "sidebar" ||
          node.getAttribute("data-slot") === "sidebar" ||
          node.classList.contains("print:hidden")
        );
      },
    });
  } finally {
    el.style.overflow = prevOverflow;
    el.style.height = prevHeight;
    el.style.maxHeight = prevMaxHeight;
  }
}

function addCanvasPage(
  pdf: jsPDF,
  canvas: HTMLCanvasElement,
  opts: { title?: string; pageLabel?: string }
) {
  if (!canvas || canvas.width < 2 || canvas.height < 2) {
    throw new Error("Captured chart canvas was empty");
  }

  const contentWidth = PAGE_WIDTH_MM - MARGIN_MM * 2;
  const headerH = opts.title || opts.pageLabel ? 14 : 0;
  const contentHeight = PAGE_HEIGHT_MM - MARGIN_MM * 2 - headerH;

  const imgW = canvas.width;
  const imgH = canvas.height;
  let drawW = Math.min(contentWidth, (imgW / imgH) * contentHeight);
  let drawH = (imgH / imgW) * drawW;
  if (drawH > contentHeight) {
    drawH = contentHeight;
    drawW = (imgW / imgH) * drawH;
  }

  if ((pdf as unknown as { __hasContent?: boolean }).__hasContent) {
    pdf.addPage();
  }
  (pdf as unknown as { __hasContent?: boolean }).__hasContent = true;

  if (opts.title || opts.pageLabel) {
    pdf.setFont("helvetica", "bold");
    pdf.setFontSize(12);
    if (opts.title) {
      pdf.text(String(opts.title).slice(0, 90), MARGIN_MM, MARGIN_MM + 4);
    }
    if (opts.pageLabel) {
      pdf.setFont("helvetica", "normal");
      pdf.setFontSize(10);
      pdf.setTextColor(80);
      pdf.text(String(opts.pageLabel).slice(0, 110), MARGIN_MM, MARGIN_MM + 10);
      pdf.setTextColor(0);
    }
  }

  const x = MARGIN_MM + (contentWidth - drawW) / 2;
  const y = MARGIN_MM + headerH;

  // JPEG shrinks multi-page exports dramatically vs PNG.
  let imgData: string;
  try {
    imgData = canvas.toDataURL("image/jpeg", 0.86);
  } catch {
    imgData = canvas.toDataURL("image/png");
  }
  const format = imgData.startsWith("data:image/png") ? "PNG" : "JPEG";
  pdf.addImage(imgData, format, x, y, drawW, drawH, undefined, "FAST");
}

function collectExportSections(root: HTMLElement): HTMLElement[] {
  const marked = Array.from(
    root.querySelectorAll<HTMLElement>("[data-pdf-section]")
  ).filter((el) => el.offsetWidth > 0 && el.offsetHeight > 0);
  if (marked.length) return marked;
  return Array.from(root.querySelectorAll<HTMLElement>("[class*='bg-muted']")).filter(
    (el) => el.offsetHeight > 120 && el.offsetWidth > 100
  );
}

async function captureSectionsToPdf(
  pdf: jsPDF,
  root: HTMLElement,
  reportTitle: string,
  pagePrefix: string,
  onProgress?: (message: string) => void
) {
  const sections = collectExportSections(root);
  if (!sections.length) {
    onProgress?.(`${pagePrefix}: no chart sections found (skipped)`);
    return;
  }

  for (let i = 0; i < sections.length; i++) {
    const section = sections[i];
    const label =
      section.dataset.pdfSection ||
      section.querySelector("h2")?.textContent?.trim() ||
      `Section ${i + 1}`;
    onProgress?.(`${pagePrefix}: ${label} (${i + 1}/${sections.length})…`);
    try {
      const canvas = await captureSection(section);
      addCanvasPage(pdf, canvas, {
        title: reportTitle,
        pageLabel: `${pagePrefix} — ${label}`,
      });
    } catch (err) {
      // Keep going; one bad panel shouldn't kill the whole export.
      const msg = err instanceof Error ? err.message : String(err);
      onProgress?.(`Skipped ${label}: ${msg}`);
      console.warn("PDF section capture failed", label, err);
    }
  }
}

/**
 * Export the current dashboard view to a multi-page PDF.
 * Walks concurrent overview panels, then (optionally) each query histogram.
 */
export async function exportDashboardToPdf(
  options: DashboardPdfExportOptions
): Promise<void> {
  const {
    root,
    title = "FalkorDB Benchmark Dashboard",
    subtitle,
    queryNames = [],
    selectQuery,
    selectWorkload,
    fileBaseName = "benchmark-dashboard",
    maxQueries,
    onProgress,
  } = options;

  if (typeof window === "undefined") {
    throw new Error("PDF export only runs in the browser");
  }

  const pdf = new jsPDF({ orientation: "landscape", unit: "mm", format: "a4" });
  const reportTitle = title;
  const stamp = new Date().toLocaleString();

  // Cover
  pdf.setFont("helvetica", "bold");
  pdf.setFontSize(18);
  pdf.text(reportTitle, MARGIN_MM, 40);
  pdf.setFont("helvetica", "normal");
  pdf.setFontSize(12);
  if (subtitle) pdf.text(String(subtitle).slice(0, 100), MARGIN_MM, 50);
  pdf.text(`Generated: ${stamp}`, MARGIN_MM, 60);
  pdf.text(
    "Concurrent overview + per-query latency histograms (capped list).",
    MARGIN_MM,
    72
  );
  (pdf as unknown as { __hasContent?: boolean }).__hasContent = true;

  // Concurrent overview
  onProgress?.("Capturing concurrent overview…");
  if (selectWorkload) {
    await selectWorkload("concurrent");
    await waitForChartsIdle(root);
  }
  await captureSectionsToPdf(pdf, root, reportTitle, "Concurrent", onProgress);

  // Per-query single histograms (full list unless maxQueries is set)
  const allQueries = queryNames.filter(Boolean);
  const queries =
    typeof maxQueries === "number" && maxQueries > 0
      ? allQueries.slice(0, maxQueries)
      : allQueries;
  if (queries.length && selectWorkload && selectQuery) {
    onProgress?.(
      `Single histograms: ${queries.length}${
        allQueries.length > queries.length ? ` of ${allQueries.length}` : ""
      }…`
    );
    await selectWorkload("single");
    await waitForChartsIdle(root);

    for (let qi = 0; qi < queries.length; qi++) {
      const q = queries[qi];
      onProgress?.(`Query ${qi + 1}/${queries.length}: ${q}`);
      await selectQuery(q);
      await waitForChartsIdle(root);
      await captureSectionsToPdf(
        pdf,
        root,
        reportTitle,
        `Single / ${q}`,
        onProgress
      );
    }

    onProgress?.("Restoring concurrent view…");
    await selectWorkload("concurrent");
    await waitForChartsIdle(root);
  }

  // Ensure we produced more than a cover page when possible
  if (pdf.getNumberOfPages() < 2) {
    throw new Error(
      "PDF export produced no chart pages. Try again after charts finish loading."
    );
  }

  onProgress?.("Saving PDF…");
  const safeName = fileBaseName.replace(/[^\w.-]+/g, "_") || "benchmark-dashboard";
  pdf.save(`${safeName}.pdf`);
  onProgress?.("Done");
}
