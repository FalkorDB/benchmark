
import { Locator, Page } from "playwright";
import BasePage from "../../infra/ui/basePage";

export default class NavBarComponent extends BasePage {

    /* Header Locators */

    private get falkorDBLogo(): Locator {
        return this.page.locator("//header//img[@alt='FalkorDB']")
    }

    private get navBarSocialLink(): (navItem: string) => Locator {
        return (navItem: string) => this.page.locator(`//a[@title="${navItem}"]`);
    }

    private get navBarLink(): (navItem: string) => Locator {
        return (navItem: string) => this.page.locator(`//a[contains(text(), '${navItem}')]`);
    }

    async clickOnFalkorLogo(): Promise<void> {
        await this.falkorDBLogo.click();
    }

    /* Header Functionality  */

    async clickOnFalkor(): Promise<Page> {
        await this.page.waitForLoadState('networkidle');
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.clickOnFalkorLogo(),
        ]);
        return newPage
    }

    async getNavBarSocialLink(navItem : string): Promise<Page> {
        await this.page.waitForLoadState('networkidle'); 
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.navBarSocialLink(navItem).click(),
        ]);
        return newPage
    }

    async getNavBarLink(navItem : string): Promise<Page> {
        await this.page.waitForLoadState('networkidle'); 
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.navBarLink(navItem).click(),
        ]);
        return newPage
    }
}