import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { sideBarItems } from "../config/testData";

function extractSecondValues(
  graphDetails: { key: string; value: Array<Record<string, number>> }[],
  updatedGraphDetails: { key: string; value: Array<Record<string, number>> }[],
  index: number
): { originalValue: number; updatedValue: number } {
  if (
    index < 0 ||
    index >= graphDetails.length ||
    index >= updatedGraphDetails.length
  ) {
    throw new Error(
      `Invalid index ${index}. Index must be within array bounds.`
    );
  }
  const matchedUpdatedData = updatedGraphDetails.find(
    (data: { key: any }) => data.key === graphDetails[index].key
  );
  const matchedOriginalData = graphDetails.find(
    (data: { key: any }) => data.key === updatedGraphDetails[index].key
  );

  if (!matchedUpdatedData || !matchedOriginalData) {
    throw new Error(
      `No matching data found for key "${graphDetails[index].key}" at index ${index}`
    );
  }

  const originalSecondValue = matchedOriginalData.value[1];
  const updatedSecondValue = matchedUpdatedData.value[1];

  return {
    originalValue: Object.values(originalSecondValue)[1],
    updatedValue: Object.values(updatedSecondValue)[1],
  };
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
      const graphDetails = await sidebar.getGraphDetails();
      await sidebar.clickOnSidebarSelection(item);
      const updatedGraphDetails = await sidebar.getGraphDetails();

      for (let i = 0; i < graphDetails.length; i++) {
        await expect(async () => {
          const { originalValue, updatedValue } = extractSecondValues(
            graphDetails,
            updatedGraphDetails,
            i
          );
          const areValuesDifferent = originalValue !== updatedValue;
          expect(areValuesDifferent).toBe(expectedRes);
        }).toPass({ timeout: 5000 });
      }
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
});
