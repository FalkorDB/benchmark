import { test, expect } from '@playwright/test';
import BrowserWrapper from '../infra/ui/browserWrapper';
import MainPage from '../logic/POM/mainPage';
import urls from '../config/urls.json';
import { navitems } from '../config/testData';

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

  navitems.slice(0,3).forEach(({navItem, expectedRes}) => {
    test(`Verify clicking on ${navItem} redirects to specified ${navItem}`, async () => {
        const header = await browser.createNewPage(MainPage, urls.baseUrl)
        const page = await header.getNavBarSocialLink(navItem);
        expect(page.url()).toBe(expectedRes)
    })
  })

  navitems.slice(3,5).forEach(({navItem, expectedRes}) => {
    test(`Verify clicking on ${navItem} redirects to specified ${navItem}`, async () => {
        const header = await browser.createNewPage(MainPage, urls.baseUrl)
        const page = await header.getNavBarLink(navItem);
        expect(page.url()).toBe(expectedRes)
    })
  })
});