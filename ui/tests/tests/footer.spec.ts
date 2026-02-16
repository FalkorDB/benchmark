import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { footerItems } from "../config/testData";

test.describe("Footer tests", () => {
  let browser: BrowserWrapper;

  const normalizeUrl = (url: string) =>
    url
      .trim()
      .replace(/^https?:\/\//, "")
      .replace(/^www\./, "")
      .replace(/\/$/, "");

  const expectUrlEquals = (actual: string, expected: string) => {
    expect(normalizeUrl(actual)).toBe(normalizeUrl(expected));
  };

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

  footerItems.forEach(({ item, expectedRes }) => {
    test(`Verify clicking on ${item} redirects to specified ${item}`, async () => {
      const header = await browser.createNewPage(MainPage, urls.baseUrl);
      const page = await header.getFooterLink(item);
      expectUrlEquals(page.url(), expectedRes);
    });
  });
});
