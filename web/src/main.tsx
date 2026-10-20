import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "@/app/App";
import { ErrorBoundary } from "@/app/ErrorBoundary";
import { Providers } from "@/app/Providers";
import { HttpBackend } from "@/backend/http";
import type { Backend } from "@/backend/types";

// `?demo` runs the whole UI on the typed mock (sample data, clearly badged).
// The mock is loaded only then, so the real app doesn't ship it.
const demo = new URLSearchParams(location.search).has("demo");
const backend: Backend = demo ? new (await import("@/backend/mock")).MockBackend() : new HttpBackend();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <Providers backend={backend}>
        <App />
      </Providers>
    </ErrorBoundary>
  </StrictMode>,
);
