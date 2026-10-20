import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

test.beforeEach(async ({ page }) => {
  await page.goto("/?demo");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
});

test("home answers what's happening and plays with one tap", async ({ page }, info) => {
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/talking|Quiet/);
  const row = page.getByRole("button", { name: /^Listen to .* Talking/ }).first();
  await row.click();
  const player = page.getByRole("region", { name: "Player" });
  await expect(player).toContainText("Live. Talking now.", { timeout: 10_000 });
  await page.screenshot({ path: info.outputPath(`home-${info.project.name}.png`) });
  await player.getByRole("button", { name: "Stop" }).click();
  await expect(player).toHaveCount(0);
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
    await page.getByRole("button", { name: /More about Private business radio/ }).click();
    await expect(page.getByRole("dialog")).toContainText("What is this?");
    await expectAccessible(page, `source sheet (${theme})`);
  });
}

test("changing the listening area is explained and deliberate", async ({ page }) => {
  await page.getByRole("button", { name: /Listening area:/ }).click();
  const sheet = page.getByRole("dialog");
  await expect(sheet).toContainText("one stretch of the airwaves at a time");
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
  await expect(player.getByRole("button", { name: "Unmute" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(player).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await expect(page.getByPlaceholder("Channel or action…")).toBeVisible();
});

test("catch up plays recorded calls down the list", async ({ page }) => {
  await page.goto("/?demo#/catch-up");
  await page.getByRole("button", { name: /Play call from/ }).first().click();
  await expect(page.getByRole("region", { name: "Player" })).toContainText("Recorded call");
});
