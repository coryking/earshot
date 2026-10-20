import { expect, test } from "@playwright/test";
import { expectAccessible } from "./helpers";

test.beforeEach(async ({ page }) => {
  await page.goto("/?demo");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
});

async function choose(page: import("@playwright/test").Page, name = "Downtown ambassadors") {
  await page.getByRole("button", { name: "Channels", exact: true }).click();
  await page.getByRole("textbox", { name: "Search channels" }).fill(name);
  await page.getByRole("list", { name: "Channel frequencies" }).getByRole("button", { name: /^Tune to / }).first().click();
}

test("exploration gives the spectrum space and tunes from a folded channel list", async ({ page }, info) => {
  await expect(page.getByRole("button", { name: "Channels", exact: true })).toHaveAttribute("aria-expanded", "false");
  await choose(page);
  const tuning = page.getByRole("region", { name: "Live tuning" });
  await expect(tuning).toContainText("Playing");
  await expect(tuning.getByRole("textbox", { name: "Frequency (MHz)" })).toHaveValue("452.68550");
  await expect(page.getByRole("region", { name: "Player" })).toHaveCount(0);
  const waterfall = (await page.locator('[data-waterfall="full"]').boundingBox())!;
  expect(waterfall.height).toBeGreaterThan(page.viewportSize()!.height * 0.5);
  await page.screenshot({ path: info.outputPath(`explore-${info.project.name}.png`) });
  await tuning.getByRole("button", { name: "Stop listening" }).click();
  await expect(tuning).toContainText("Audio off");
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
    await choose(page, "Private business radio");
    await page.getByRole("button", { name: "Signal", exact: true }).click();
    await page.getByRole("button", { name: "About channel", exact: true }).click();
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

test("keyboard: tune, mute, stop and search", async ({ page, isMobile }) => {
  test.skip(isMobile, "keyboard shortcuts are for desktop");
  await choose(page);
  await page.keyboard.press("m");
  const tuning = page.getByRole("region", { name: "Live tuning" });
  await expect(tuning.getByRole("button", { name: "Unmute speaker", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(tuning).toContainText("Audio off");
  await page.keyboard.press("Control+k");
  await expect(page.getByPlaceholder("Channel or action…")).toBeVisible();
});

test("catch up plays recorded calls down the list", async ({ page }) => {
  await page.goto("/?demo#/catch-up");
  await page.getByRole("button", { name: /Play call from/ }).first().click();
  await expect(page.getByRole("region", { name: "Player" })).toContainText(/[Rr]ecorded/);
});

test("speaker mute and zero volume retain the held frequency until Stop listening", async ({ page }) => {
  await choose(page);
  const player = page.getByRole("region", { name: "Live tuning" });
  const tuning = page.getByRole("region", { name: "Live tuning" });
  const frequency = tuning.getByRole("textbox", { name: "Frequency (MHz)" });
  const held = await frequency.inputValue();
  await expect(player).toContainText("Playing");
  await player.getByRole("button", { name: "Mute speaker", exact: true }).click();
  await expect(player).toContainText("Muted");
  await expect(frequency).toHaveValue(held);
  await player.getByRole("button", { name: "Unmute speaker", exact: true }).click();
  const volume = player.getByRole("slider", { name: "Speaker volume" });
  await volume.focus();
  await volume.press("Home");
  await expect(volume).toHaveValue("0");
  await expect(player).toContainText("Muted");
  await player.getByRole("button", { name: "Unmute speaker", exact: true }).click();
  await expect(volume).not.toHaveValue("0");
  await expect(frequency).toHaveValue(held);
  await player.getByRole("button", { name: "Stop listening", exact: true }).click();
  await expect(player).toContainText("Audio off");
  await expect(page.getByRole("button", { name: "Stop receiving", exact: true })).toBeEnabled();
});

test("receiver start and stop are distinct from listening", async ({ page }) => {
  await choose(page);
  await expect(page.getByRole("region", { name: "Live tuning" })).toContainText("Playing");
  await page.getByRole("button", { name: "Stop receiving", exact: true }).click();
  await expect(page.getByRole("button", { name: "Start receiving", exact: true })).toBeEnabled();
  await expect(page.getByRole("region", { name: "Player" })).toHaveCount(0);
  await page.getByRole("button", { name: "Start receiving", exact: true }).click();
  await expect(page.getByRole("button", { name: "Stop receiving", exact: true })).toBeEnabled();
  await expect(page.getByRole("region", { name: "Player" })).toHaveCount(0);
});

test("phone: channel rows are tap targets and signal details are separate", async ({ page, isMobile }) => {
  test.skip(!isMobile, "tap targets are checked at phone width");
  await page.getByRole("button", { name: "Channels", exact: true }).click();
  const row = page.getByRole("list", { name: "Channel frequencies" }).getByRole("button", { name: /^Tune to / }).first();
  const bounds = (await row.boundingBox())!;
  expect(bounds.height).toBeGreaterThanOrEqual(44);
  expect(bounds.width).toBeGreaterThan(250);
  await row.tap();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Channels", exact: true })).toHaveAttribute("aria-expanded", "false");
  await page.getByRole("button", { name: "Signal", exact: true }).tap();
  await expect(page.getByRole("complementary", { name: "Signal details" })).toBeVisible();
});
