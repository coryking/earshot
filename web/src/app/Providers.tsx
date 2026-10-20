import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import type { Backend } from "@/backend/types";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useApplyTheme } from "@/lib/theme";
import { BackendProvider, CatalogMigration, RadioSync } from "@/state/radio";
import { useSpectrumFeed } from "@/waterfall/feed";
import { PlayerProvider } from "./player-context";

function Live() {
  useApplyTheme();
  useSpectrumFeed();
  return (
    <>
      <RadioSync />
      <CatalogMigration />
    </>
  );
}

export function Providers({ backend, children }: { backend: Backend; children: ReactNode }) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: true, staleTime: 2_000 } },
      }),
  );
  return (
    <QueryClientProvider client={client}>
      <BackendProvider backend={backend}>
        <PlayerProvider>
          <TooltipProvider delayDuration={400}>
            <Live />
            {children}
          </TooltipProvider>
        </PlayerProvider>
      </BackendProvider>
    </QueryClientProvider>
  );
}
