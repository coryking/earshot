import { expect, test } from "@playwright/test";

test("desktop waterfall previews a nearby channel and Shift bypasses snapping", async ({ page, isMobile }) => {
  test.skip(isMobile, "the portrait strip is not interactive");
  await page.goto("/?demo");
  const waterfall = page.locator('[data-waterfall="full"]');
  await expect(waterfall).toHaveAttribute("aria-label", /451\.2 to 457\.2/);
  const rect = (await waterfall.boundingBox())!;
  // Aim 10 kHz beside the demo ambassadors carrier, in its older history.
  const x = (452_685_504 + 10_000 - (454_216_998 - 3_000_000)) / 6_000_000 * rect.width;
  const y = rect.height * 0.75;
  await waterfall.hover({ position: { x, y } });
  await expect(waterfall.getByText("Snap to", { exact: false })).toContainText("Downtown ambassadors");
  await waterfall.click({ position: { x, y } });
  const selection = page.getByRole("region", { name: "Live tuning" });
  await expect(selection).toContainText("Downtown ambassadors");
  await expect(selection).toContainText("452.6855 MHz");
  const selectedRect = (await waterfall.boundingBox())!;
  await waterfall.click({ position: { x, y: selectedRect.height * 0.5 }, modifiers: ["Shift"] });
  const sheet = page.getByRole("dialog");
  await expect(sheet).toContainText("Tune signal");
  const entered = await sheet.getByRole("textbox", { name: "Frequency (MHz)" }).inputValue();
  // Browser pointer coordinates are rounded to fractional CSS pixels.
  expect(Number(entered)).toBeCloseTo(452.6955, 3);
});
