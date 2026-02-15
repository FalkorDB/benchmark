import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { hoverItems, sideBarItems } from "../config/testData";

function pickNumericValue(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;

  if (Array.isArray(value)) {
    if (!value.length) return null;
    // Prefer "second" element (historical assumption), but fall back to first.
    return pickNumericValue(value[Math.min(1, value.length - 1)]);
  }

  if (value && typeof value === "object") {
    const obj = value as Record<string, unknown>;

    // Prefer known keys used in our chart data.
    const preferredKeys = [
      "actualMessagesPerSecond",
      "p99",
      "p95",
      "p50",
      "memory",
    ];

    for (const k of preferredKeys) {
      const v = obj[k];
      const n = typeof v === "number" ? v : Number(v);
      if (Number.isFinite(n)) return n;
    }

    // Fallback: first numeric value.
    for (const v of Object.values(obj)) {
      if (typeof v === "number" && Number.isFinite(v)) return v;
    }
  }

  return null;
}

function valuesByKey(
  graphDetails: { key: string; value: unknown }[]
): Record<string, number> {
  const out: Record<string, number> = {};

  for (const item of graphDetails ?? []) {
    const k = (item?.key ?? "").toString();
    if (!k) continue;

    const n = pickNumericValue(item.value);
    if (n === null) continue;

    out[k] = n;
  }

  return out;
}

test.describe("SideBar tests", () => {
  let browser: BrowserWrapper;

  test.beforeAll(async () => {
    try {
      browser = new BrowserWrapper();
    } catch (error) {
      console.error("Failed to initialize browser:", error);
      throw error;
    }
  });

  test.afterAll(async () => {
    try {
      await browser.closeBrowser();
    } catch (error) {
      console.error("Failed to close browser:", error);
      throw error;
    }
  });

  sideBarItems.forEach(({ item, expectedRes }) => {
    test(`Verify ${item} selection updates the chart results`, async () => {
      const sidebar = await browser.createNewPage(MainPage, urls.baseUrl);
      await sidebar.selectWorkloadType("Concurrent");
      const graphDetails = await sidebar.getGraphDetails();
      await sidebar.clickOnSidebarSelection(item);
      const updatedGraphDetails = await sidebar.getGraphDetails();

      const original = valuesByKey(graphDetails);

      await expect(async () => {
        const updated = valuesByKey(updatedGraphDetails);

        // Compare only keys that exist on both sides.
        for (const key of Object.keys(original)) {
          if (!(key in updated)) continue;
          const areValuesDifferent = original[key] !== updated[key];
          expect(areValuesDifferent).toBe(expectedRes);
        }
      }).toPass({ timeout: 15000 });
    });
  });

  test("Verify Sidebar trigger button toggles the sidebar open and closed", async () => {
    const sidebar = await browser.createNewPage(MainPage, urls.baseUrl);
    await sidebar.clickOnSideBarToggle();
    expect(await sidebar.getSideBarState()).toBe("collapsed");
    await sidebar.clickOnSideBarToggle();
    expect(await sidebar.getSideBarState()).toBe("expanded");
  });

  test(`Verify manual scroll functionality`, async () => {
    const sidebar = await browser.createNewPage(MainPage, urls.baseUrl);
    await sidebar.scrollToBottomInSidebar();
    expect(await sidebar.isScrolledToBottomInSidebar()).toBe(true);
  });

  hoverItems.forEach(({ item, expectedRes }) => {
    test(`Verify hover behavior for hardware item: ${item}`, async () => {
      const sidebar = await browser.createNewPage(MainPage, urls.baseUrl);
      await sidebar.selectWorkloadType("Concurrent");
      await sidebar.hoverOnSideBarHardware(item);
      expect(await sidebar.isHoverElementVisible()).toBe(expectedRes);
    });
  });

});
