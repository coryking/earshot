import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

test.beforeEach(async ({ page }) => {
  await page.goto("/?demo");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
});

test("home is titled by the area, pins who's talking, and plays with one tap", async ({ page }, info) => {
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/Downtown business radio/);
  const talking = page.getByRole("list", { name: "Recent activity" });
  await expect(talking.getByRole("listitem").first()).toBeVisible();
  await talking.getByRole("button", { name: /^Tune to / }).first().click();
  const player = page.getByRole("region", { name: "Player" });
  await expect(player).toContainText("Playing live voice", { timeout: 10_000 });
  await expect(player).toContainText(/\d+\.\d+ MHz/);
  // Re-selecting a channel must never silently turn its audio off.
  await talking.getByRole("button", { name: /^Tune to / }).first().click();
  await expect(player).toBeVisible();
  await expect(page.getByRole("region", { name: "Live tuning" })).toContainText(/\d+\.\d+ MHz/);
  await page.screenshot({ path: info.outputPath(`home-${info.project.name}.png`) });
  await player.getByRole("button", { name: "Stop listening" }).click();
  await expect(player).toHaveCount(0);
});

test("Now keeps live tuning separate from the Catch up tab", async ({ page }) => {
  await expect(page.getByRole("region", { name: "Live tuning" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Catch up", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Play call from/ })).toHaveCount(0);
  await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: /^Catch up/ }).click();
  await expect(page.getByRole("heading", { name: "Catch up", exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Live tuning" })).toHaveCount(0);
});

for (const theme of ["dark", "light"]) {
  for (const [name, path, ready] of [
    ["now", "/?demo", { level: 1 }],
    ["catch up", "/?demo#/catch-up", { name: "Catch up" }],
    ["radio", "/?demo#/radio", { name: "Radio" }],
    ["operator", "/?demo#/operator", { name: "Operator tools" }],
  ] as const) {
    test(`axe: ${name} (${theme})`, async ({ page }) => {
      await page.evaluate((t) => localStorage.setItem("radio.prefs.v1", JSON.stringify({ theme: t })), theme);
      await page.goto(path);
      await expect(page.getByRole("heading", ready).first()).toBeVisible();
      await expectAccessible(page, `${name} (${theme})`);
    });
  }
  test(`axe: source sheet (${theme})`, async ({ page }) => {
    await page.evaluate((t) => localStorage.setItem("radio.prefs.v1", JSON.stringify({ theme: t })), theme);
    await page.goto("/?demo");
    await page.getByRole("button", { name: "About Private business radio" }).first().click();
    await expect(page.getByRole("dialog")).toContainText("Encrypted");
    await expectAccessible(page, `source sheet (${theme})`);
  });
}

test("changing the listening area is deliberate", async ({ page }) => {
  await page.getByRole("button", { name: /Listening area:/ }).click();
  const sheet = page.getByRole("dialog");
  await expect(sheet).toContainText("Listening area");
  await sheet.getByRole("radio", { name: /Upper business band/ }).click();
  await sheet.getByRole("button", { name: "Switch to Upper business band" }).click();
  await expect(sheet).toBeHidden();
  await expect(page.getByRole("button", { name: /Listening area: Upper business band/ })).toBeVisible();
});

test("keyboard: move, listen, mute and stop", async ({ page, isMobile }) => {
  test.skip(isMobile, "keyboard shortcuts are for desktop");
  await page.keyboard.press("j");
  await page.keyboard.press("Enter");
  const player = page.getByRole("region", { name: "Player" });
  await expect(player).toBeVisible();
  await page.keyboard.press("m");
  await expect(player.getByRole("button", { name: "Unmute speaker", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(player).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await expect(page.getByPlaceholder("Channel or action…")).toBeVisible();
});

test("catch up plays recorded calls down the list", async ({ page }) => {
  await page.goto("/?demo#/catch-up");
  await page.getByRole("button", { name: /Play call from/ }).first().click();
  await expect(page.getByRole("region", { name: "Player" })).toContainText("Recorded");
});

test("phone: every row's main area listens and only the About button opens info", async ({ page, isMobile }) => {
  test.skip(!isMobile, "tap targets are checked at phone width");
  const list = page.getByRole("list", { name: "Channels" });
  await expect(list.locator("[data-source-row]").nth(3)).toBeVisible();
  const rows = list.locator(":scope > li");
  const count = await rows.count();
  for (let i = 0; i < count; i++) {
    const main = rows.nth(i).locator("[data-source-row]");
    const about = rows.nth(i).locator("[data-about]");
    const a = (await main.boundingBox())!;
    const b = (await about.boundingBox())!;
    expect(a.height, `row ${i} height`).toBeGreaterThanOrEqual(44);
    expect(b.width, `row ${i} about width`).toBeGreaterThanOrEqual(44);
    expect(b.height, `row ${i} about height`).toBeGreaterThanOrEqual(44);
    expect(a.x + a.width, `row ${i}: listen and about don't overlap`).toBeLessThanOrEqual(b.x + 0.5);
    // The listen area covers the row from the left edge to the About button.
    expect(a.x).toBeLessThanOrEqual(1);
    expect(a.width / 390).toBeGreaterThan(0.8);
  }
  // Taps anywhere on the main area listen, never open a sheet: left, middle, and just left of About.
  const player = page.getByRole("region", { name: "Player" });
  for (const [i, x] of [
    [1, 8],
    [2, 180],
    [3, 330],
  ] as const) {
    const main = rows.nth(i).locator("[data-source-row]");
    await main.tap({ position: { x, y: 20 } });
    await expect(player).toBeVisible();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(rows.nth(i)).toHaveAttribute("data-source-id", /.+/);
  }
  await rows.nth(1).locator("[data-about]").tap();
  await expect(page.getByRole("dialog")).toBeVisible();
});
