import { Locator, Page } from "playwright";
import BasePage from "../../infra/ui/basePage";

export default class NavBarComponent extends BasePage {
  private async navigateAndWaitForPopup(
    clickAction: () => Promise<void>
  ): Promise<Page> {
    try {
      await this.page.waitForLoadState("networkidle", { timeout: 5000 });
      const popupPromise = this.page.waitForEvent("popup", { timeout: 5000 });
      await clickAction();
      const newPage = await popupPromise;
      await newPage.waitForLoadState("load");
      return newPage;
    } catch (error) {
      throw new Error(`Navigation failed`);
    }
  }

  /* General Locators */

  private get hoverElementPopUp(): Locator {
    return this.page.locator("//div[@data-side='bottom']");
  }

  private get hoverElement(): (item: string) => Locator {
    return (item: string) =>
      this.page.locator(`//button[text()="${item}"]/following-sibling::a/span`);
  }

  private get deadlineInfoLink(): Locator {
    return this.page.locator("//div[@id='deadline-chart']/a/span");
  }

  /* Header Locators */

  private get falkorDBLogo(): Locator {
    return this.page.locator("//header//img[@alt='FalkorDB']");
  }

  private get headerSocialLink(): (navItem: string) => Locator {
    return (navItem: string) => this.page.locator(`//a[@title="${navItem}"]`);
  }

  private get headerLink(): (navItem: string) => Locator {
    return (navItem: string) =>
      this.page.locator(`//a[contains(text(), '${navItem}')]`);
  }

  /* Footer Locators */

  private get footerLink(): (item: string) => Locator {
    return (item: string) =>
      this.page.locator(`//a[contains(text(), '${item}')]`);
  }

  /* SideBar Locators */

  private get sideBarToggle(): Locator {
    return this.page.locator("//button[@data-sidebar='trigger']");
  }

  private get sideBarSelection(): (item: string) => Locator {
    return (item: string) => this.page.locator(`//button[text()='${item}']`);
  }

  private get sideBarContainer(): Locator {
    return this.page.locator("//div[@id='sidebar-container']");
  }

  private get sideBarContent(): Locator {
    return this.page.locator("div[data-sidebar='content']");
  }

  /* General Functionality  */

  async hoverOnSideBarHardware(item: string): Promise<void> {
    await this.hoverElement(item).hover();
  }

  async isHoverElementVisible(): Promise<boolean> {
    await this.page.waitForTimeout(2000);
    return this.hoverElementPopUp.isVisible();
  }

  async hoverOnDeadlineInfoLink(): Promise<void> {
    await this.deadlineInfoLink.hover();
  }

  async getDeadlineInfoLinkText(): Promise<string> {
    return await this.hoverElementPopUp.innerText();
  }

  /* Header Functionality  */

  async clickOnFalkorLogo(): Promise<Page> {
    return this.navigateAndWaitForPopup(() => this.falkorDBLogo.click());
  }

  async getHeaderSocialLink(navItem: string): Promise<Page> {
    return this.navigateAndWaitForPopup(() =>
      this.headerSocialLink(navItem).click()
    );
  }

  async getHeaderLink(navItem: string): Promise<Page> {
    return this.navigateAndWaitForPopup(() => this.headerLink(navItem).click());
  }

  /* Footer Functionality  */

  async getFooterLink(item: string): Promise<Page> {
    return this.navigateAndWaitForPopup(() => this.footerLink(item).click());
  }

  /* SideBar Functionality  */

  async clickOnSideBarToggle(): Promise<void> {
    await this.sideBarToggle.click();
  }

  async clickOnSidebarSelection(item: string): Promise<void> {
    await this.sideBarSelection(item).click();
  }

  async getSideBarState(): Promise<string | null> {
    return await this.sideBarContainer.getAttribute("data-state");
  }

  async scrollToBottomInSidebar(): Promise<void> {
    await this.sideBarContent.evaluate((el) => el.scrollTo(0, el.scrollHeight));
  }

  async isScrolledToBottomInSidebar(): Promise<boolean> {
    return await this.sideBarContent.evaluate((el) => {
      return el.scrollTop + el.clientHeight >= el.scrollHeight;
    });
  }

  async getGraphDetails(): Promise<any> {
    try {
      await this.page.waitForFunction(
        () =>
          typeof (window as any).allChartData !== "undefined" &&
          (window as any).allChartData !== null,
        { timeout: 5000 }
      );
      const graphData = await this.page.evaluate(() => {
        return (window as any).allChartData;
      });

      if (!graphData) {
        throw new Error("Graph data is not available in window.allChartData.");
      }

      return graphData;
    } catch (error) {
      console.error("Error fetching graph details:", error);
      throw error;
    }
  }
}
