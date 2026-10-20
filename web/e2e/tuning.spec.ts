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
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const entered = await selection.getByRole("textbox", { name: "Frequency (MHz)" }).inputValue();
  // Browser pointer coordinates are rounded to fractional CSS pixels.
  expect(Number(entered)).toBeCloseTo(452.6955, 3);
});

test("desktop unnamed signals tune inline and retain a raw capture after stopping", async ({ page, isMobile }) => {
  test.skip(isMobile, "the portrait strip is not interactive");
  await page.goto("/?demo");
  const waterfall = page.locator('[data-waterfall="full"]');
  await expect(waterfall).toHaveAttribute("aria-label", /451\.2 to 457\.2/);
  const bounds = (await waterfall.boundingBox())!;
  await waterfall.click({ position: { x: bounds.width * 0.96, y: bounds.height * 0.5 } });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const selection = page.getByRole("region", { name: "Live tuning" });
  await expect(selection).toContainText("FM signal audio · unsquelched");
  const entered = Number(await selection.getByRole("textbox", { name: "Frequency (MHz)" }).inputValue()) * 1e6;
  expect(Math.round(entered) % 6250).toBe(0);
  await expect(selection.getByRole("button", { name: "Save FM audio WAV" })).toBeEnabled();
  await selection.getByRole("button", { name: "Stop listening" }).click();
  await expect(selection.getByRole("button", { name: "Save FM audio WAV" })).toBeEnabled();
});

test("desktop dragging follows the pointer and stopped listening can resume", async ({ page, isMobile }) => {
  test.skip(isMobile, "the portrait strip is not interactive");
  await page.goto("/?demo");
  const selection = page.getByRole("region", { name: "Live tuning" });
  await expect(selection).toContainText("Listening off");
  await expect(selection).toContainText("Audio will not start automatically");
  const waterfall = page.locator('[data-waterfall="full"]');
  await expect(waterfall).toHaveAttribute("aria-label", /451\.2 to 457\.2/);
  const box = (await waterfall.boundingBox())!;
  await page.mouse.move(box.x + 35, box.y + 100);
  await page.mouse.down();
  const frequency = selection.getByRole("textbox", { name: "Frequency (MHz)" });
  const first = await frequency.inputValue();
  await page.mouse.move(box.x + box.width * 0.8, box.y + 100, { steps: 12 });
  await page.mouse.up();
  await expect(frequency).not.toHaveValue(first);
  await expect(selection).toContainText("Playing FM signal audio");
  await selection.getByRole("button", { name: "Stop listening" }).click();
  await expect(selection).toContainText("Listening off");
  await expect(selection).toContainText("Audio will not start automatically");
  const stopped = await frequency.inputValue();
  await selection.getByRole("button", { name: "Resume listening" }).click();
  await expect(frequency).toHaveValue(stopped);
  await expect(selection).toContainText("Playing FM signal audio");
});

test("landscape tuning and waterfall remain reachable after raw listening expands the panel", async ({ page, isMobile }) => {
  test.skip(isMobile, "uses the desktop context at a short landscape size");
  await page.setViewportSize({ width: 844, height: 390 });
  await page.goto("/?demo");
  const selection = page.getByRole("region", { name: "Live tuning" });
  await selection.getByRole("textbox", { name: "Frequency (MHz)" }).fill("456.70000");
  await selection.getByRole("button", { name: "Listen", exact: true }).click();
  await expect(selection).toContainText("Playing FM signal audio");
  const waterfall = page.locator('[data-waterfall="full"]');
  await waterfall.scrollIntoViewIfNeeded();
  await expect(waterfall).toBeInViewport({ ratio: 0.5 });
  const stop = selection.getByRole("button", { name: "Stop listening", exact: true });
  await stop.scrollIntoViewIfNeeded();
  await expect(stop).toBeInViewport();
  await stop.click();
  await expect(selection).toContainText("Listening off");
});
