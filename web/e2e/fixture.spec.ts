import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

// Real engine + the DMR slot 2 fixture through engine/examples/fixture_ingress.
// Match the fixture's measured frequency and slot; catalog names can change.
test.skip(!process.env.FIXTURE_ENGINE, "needs the engine binary and the private DMR fixture");

test("real engine: the fixture's DMR call shows up and plays", async ({ page }, info) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Channels", exact: true }).click();
  const row = page
    .getByRole("list", { name: "Channel frequencies" })
    .getByRole("button", { name: /^Tune to .*454\.029\d* MHz.*DMR.*Slot 2/ });
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.click();
  const player = page.getByRole("region", { name: "Live tuning" });
  await expect(player).toContainText("Playing", { timeout: 15_000 });
  const iq = page.getByRole("region", { name: "IQ capture", exact: true });
  await expect(iq).toContainText(/\d+\.\d \/ 30 s/);
  await expect(page.locator("[data-iq-range]")).toBeVisible();
  const save = iq.getByRole("button", { name: "Save IQ" });
  await expect(save).toBeEnabled();
  const downloaded = page.waitForEvent("download");
  await save.click();
  const snapshot = await downloaded;
  expect(snapshot.suggestedFilename()).toMatch(/^iq_\d+Hz\.sigmf\.tar$/);
  expect(await snapshot.failure()).toBeNull();
  await page.screenshot({ path: info.outputPath(`fixture-${info.project.name}.png`) });
  await expectAccessible(page, "live fixture");
});

test("real engine: the call shows up in catch up", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Channels", exact: true }).click();
  await expect(
    page.getByRole("list", { name: "Channel frequencies" }).getByRole("button", { name: /^Tune to .*454\.029\d* MHz.*DMR.*Slot 2/ }),
  ).toBeVisible({ timeout: 30_000 });
  await page.goto("/#/catch-up");
  await expect(page.getByText(/454\.029\d* MHz/).first()).toBeVisible({ timeout: 30_000 });
});
