
import { Locator, Page } from "playwright";
import BasePage from "../../infra/ui/basePage";

export default class NavBarComponent extends BasePage {

    /* Header Locators */

    private get falkorDBLogo(): Locator {
        return this.page.locator("//header//img[@alt='FalkorDB']")
    }

    private get headerSocialLink(): (navItem: string) => Locator {
        return (navItem: string) => this.page.locator(`//a[@title="${navItem}"]`);
    }

    private get headerLink(): (navItem: string) => Locator {
        return (navItem: string) => this.page.locator(`//a[contains(text(), '${navItem}')]`);
    }

     /* Footer Locators */

     private get footerLink(): (item: string) => Locator {
        return (item: string) => this.page.locator(`//a[contains(text(), '${item}')]`);
    }

    /* Header Functionality  */

    async clickOnFalkorLogo(): Promise<void> {
        await this.falkorDBLogo.click();
    }

    async clickOnFalkordb(): Promise<Page> {
        await this.page.waitForLoadState('networkidle');
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.clickOnFalkorLogo(),
        ]);
        return newPage
    }

    async getHeaderSocialLink(navItem : string): Promise<Page> {
        await this.page.waitForLoadState('networkidle'); 
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.headerSocialLink(navItem).click(),
        ]);
        return newPage
    }

    async getHeaderLink(navItem : string): Promise<Page> {
        await this.page.waitForLoadState('networkidle'); 
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.headerLink(navItem).click(),
        ]);
        return newPage
    }

    /* Footer Functionality  */

    async getFooterLink(item : string): Promise<Page> {
        await this.page.waitForLoadState('networkidle'); 
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.footerLink(item).click(),
        ]);
        return newPage
    }
}