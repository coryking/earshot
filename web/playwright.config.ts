import { defineConfig, devices } from "@playwright/test";

// Tests run against the production build (vite preview) with /api proxied to a
// private dev engine on 8830 started by e2e/global-setup.ts. Never the
// production service (8810).
const port = 5190;
const launch = {
  args: ["--autoplay-policy=no-user-gesture-required", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
};

export default defineConfig({
  testDir: "e2e",
  timeout: 120_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  globalSetup: "./e2e/global-setup.ts",
  use: { baseURL: `http://127.0.0.1:${port}`, trace: "retain-on-failure", launchOptions: launch },
  projects: [
    {
      name: "phone",
      use: { ...devices["Pixel 7"], browserName: "chromium", viewport: { width: 390, height: 844 } },
    },
    { name: "desktop", use: { browserName: "chromium", viewport: { width: 1440, height: 900 } } },
  ],
  webServer: {
    command: `npm run build && npx vite preview --port ${port} --strictPort`,
    url: `http://127.0.0.1:${port}`,
    env: { ENGINE_URL: "http://127.0.0.1:8830" },
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
