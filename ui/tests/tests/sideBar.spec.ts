import { test, expect } from "@playwright/test";
import BrowserWrapper from "../infra/ui/browserWrapper";
import MainPage from "../logic/POM/mainPage";
import urls from "../config/urls.json";
import { sideBarItems } from "../config/testData";

function extractSecondValues(
  graphDetails: { key: any; value: any[] }[],
  updatedGraphDetails: { key: any; value: any[] }[],
  index: number
): { originalValue: any; updatedValue: any } {
  const matchedUpdatedData = updatedGraphDetails.find(
    (data: { key: any }) => data.key === graphDetails[index].key
  );
  const matchedOriginalData = graphDetails.find(
    (data: { key: any }) => data.key === updatedGraphDetails[index].key
  );

  if (!matchedUpdatedData || !matchedOriginalData) {
    throw new Error(`Matching data not found for index ${index}`);
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

  sideBarItems.forEach(({ item, expectedRes}) => {
    test(`verify selecting different hardware changes the charts ${item}`, async () => {
      const sidebar = await browser.createNewPage(MainPage, urls.baseUrl);
      const graphDetails = await sidebar.getGraphDetails();
      await sidebar.clickOnSidebarSelection(item);
      const updatedGraphDetails = await sidebar.getGraphDetails();
  
      for (let i = 0; i < graphDetails.length; i++) {
        const { originalValue, updatedValue } = extractSecondValues(
          graphDetails,
          updatedGraphDetails,
          i
        );
        const areValuesDifferent = originalValue !== updatedValue;
        expect(areValuesDifferent).toBe(expectedRes);
      }
    });
  });
  
});
