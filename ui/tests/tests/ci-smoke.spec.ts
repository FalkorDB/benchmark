import { test, expect } from "@playwright/test";
import urls from "../config/urls.json";

test("CI smoke: benchmark home page loads", async ({ page }) => {
  await page.goto(urls.baseUrl);
  await expect(page).toHaveTitle(/FalkorDB/i);
  await expect(page.locator("body")).toContainText(/Benchmark/i);
});
