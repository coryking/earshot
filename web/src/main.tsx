import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "@/app/App";
import { ErrorBoundary } from "@/app/ErrorBoundary";
import { Providers } from "@/app/Providers";
import { HttpBackend } from "@/backend/http";
import { MockBackend } from "@/backend/mock";

// `?demo` runs the whole UI on the typed mock (sample data, clearly badged).
const demo = new URLSearchParams(location.search).has("demo");
const backend = demo ? new MockBackend() : new HttpBackend();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Providers backend={backend}>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </Providers>
  </StrictMode>,
);
