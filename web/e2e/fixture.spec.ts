import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

// Real engine + the DMR slot 2 fixture through engine/examples/fixture_ingress.
test.skip(!process.env.FIXTURE_ENGINE, "needs the engine binary and the private DMR fixture");

test("real engine: the fixture's DMR call shows up and plays", async ({ page }, info) => {
  await page.goto("/");
  const row = page.getByRole("button", { name: /^Listen to 454\.029 MHz, line 2 Talking/ });
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("454.029 MHz, line 2 is talking.");
  await row.click();
  const player = page.getByRole("region", { name: "Player" });
  await expect(player).toContainText("Live. Talking now.", { timeout: 15_000 });
  await page.screenshot({ path: info.outputPath(`fixture-${info.project.name}.png`) });
  await expectAccessible(page, "live fixture");
});

test("real engine: calls heard this session appear in catch up without audio", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("button", { name: /^Listen to 454\.029 MHz, line 2 Talking/ })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("button", { name: /^(Listen to|Stop listening to) 454\.029 MHz, line 2 (just now|\d+ sec ago)/ })).toBeVisible({ timeout: 30_000 });
  await page.goto("/#/catch-up");
  await expect(page.getByText("doesn't record calls yet")).toBeVisible();
  await expect(page.getByText(/no recording/).first()).toBeVisible();
});
