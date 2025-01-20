import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { footerItems } from "../config/testData";

test.describe("Footer tests", () => {
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

  footerItems.forEach(({ item, expectedRes }) => {
    test(`Verify clicking on ${item} redirects to specified ${item}`, async () => {
      const header = await browser.createNewPage(MainPage, urls.baseUrl);
      const page = await header.getFooterLink(item);
      expect(page.url()).toBe(expectedRes);
    });
  });
});
