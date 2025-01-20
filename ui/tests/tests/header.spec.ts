import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { headerItems } from "../config/testData";

test.describe("Header tests", () => {
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

  test("Verify clicking on falkordb logo redirects to specified URL", async () => {
    const header = await browser.createNewPage(MainPage, urls.baseUrl);
    const page = await header.clickOnFalkordb();
    expect(page.url()).toBe(urls.falkorDBUrl);
  });

  headerItems.slice(0, 3).forEach(({ navItem, expectedRes }) => {
    test(`Verify clicking on ${navItem} redirects to specified ${navItem}`, async () => {
      const header = await browser.createNewPage(MainPage, urls.baseUrl);
      const page = await header.getHeaderSocialLink(navItem);
      expect(page.url()).toBe(expectedRes);
    });
  });

  headerItems.slice(3, 5).forEach(({ navItem, expectedRes }) => {
    test(`Verify clicking on ${navItem} redirects to specified ${navItem}`, async () => {
      const header = await browser.createNewPage(MainPage, urls.baseUrl);
      const page = await header.getHeaderLink(navItem);
      expect(page.url()).toBe(expectedRes);
    });
  });
});
