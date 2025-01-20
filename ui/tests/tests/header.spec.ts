import { test, expect } from '@playwright/test';
import BrowserWrapper from '../infra/ui/browserWrapper';
import MainPage from '../logic/POM/mainPage';
import urls from '../config/urls.json';

test.describe(' Navbar tests', () => {
  let browser: BrowserWrapper;

  test.beforeAll(async () => {
    browser = new BrowserWrapper();
  });

  test.afterAll(async () => {
    await browser.closeBrowser();
  });

  test("Verify clicking on falkordb logo redirects to specified URL", async () => {    
    const header = await browser.createNewPage(MainPage, urls.baseUrl)
    const page = await header.clickOnFalkor();
    expect(page.url()).toBe(urls.falkorDBUrl)
  })
});