/// <reference types="vitest/config" />
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The engine serves web/dist itself. In development, proxy the API and both
// WebSockets to a dev engine instance (never the production service's port).
const engine = process.env.ENGINE_URL ?? "http://127.0.0.1:8820";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // ~210 KB gzipped: React, Radix, cmdk, vaul, zod. Fine on the tailnet.
  build: { chunkSizeWarningLimit: 800 },
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
  server: {
    host: "127.0.0.1",
    proxy: { "/api": { target: engine, ws: true } },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    testTimeout: 15_000,
  },
});
