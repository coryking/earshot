import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

// Real engine + the DMR slot 2 fixture through engine/examples/fixture_ingress.
// 454.029 MHz carries the starter name from the RF research (Columbia Tower).
test.skip(!process.env.FIXTURE_ENGINE, "needs the engine binary and the private DMR fixture");

test("real engine: the fixture's DMR call shows up and plays", async ({ page }, info) => {
  await page.goto("/");
  const row = page
    .getByRole("list", { name: "Recent activity" })
    .getByRole("button", { name: /^Tune to Columbia Tower shared radio/ });
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.click();
  const player = page.getByRole("region", { name: "Player" });
  await expect(player).toContainText("Playing live voice", { timeout: 15_000 });
  const iq = page.getByRole("region", { name: "IQ capture", exact: true });
  await expect(iq).toContainText(/s buffered \/ 30 s maximum/);
  await expect(iq).toContainText("Sampled range");
  const save = iq.getByRole("button", { name: "Download IQ snapshot" });
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
  await expect(
    page.getByRole("list", { name: "Recent activity" }).getByRole("button", { name: /^Tune to Columbia Tower shared radio/ }),
  ).toBeVisible({ timeout: 30_000 });
  await page.goto("/#/catch-up");
  await expect(page.getByText("Columbia Tower shared radio").first()).toBeVisible({ timeout: 30_000 });
});
