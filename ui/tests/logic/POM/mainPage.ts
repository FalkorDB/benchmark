
import { Locator, Page } from "playwright";
import BasePage from "../../infra/ui/basePage";

export default class NavBarComponent extends BasePage {

    private get falkorDBLogo(): Locator {
        return this.page.locator("//header//img[@alt='FalkorDB']")
    }

    async clickOnFalkorLogo(): Promise<void> {
        await this.falkorDBLogo.click();
    }

    async clickOnFalkor(): Promise<Page> {
        await this.page.waitForLoadState('networkidle');
        const [newPage] = await Promise.all([
            this.page.waitForEvent('popup'),
            this.clickOnFalkorLogo(),
        ]);
        return newPage
    }
}