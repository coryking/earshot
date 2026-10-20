import { lazy, Suspense, useEffect } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { useAlerts } from "@/app/alerts";
import { Palette, ShortcutsDialog, useShortcuts } from "@/app/Palette";
import { useRoute, useWide } from "@/app/ui";
import { Header, TabBar } from "@/components/Chrome";
import { PlayerBar } from "@/components/PlayerBar";
import { SheetHost } from "@/components/Sheets";
import { Toaster } from "@/components/ui/sonner";
import { CatchUpScreen } from "@/screens/CatchUp";
import { Exploration } from "@/screens/Exploration";
import { RadioTab } from "@/screens/RadioTab";
import { useBlocks } from "@/state/radio";

const OperatorScreen = lazy(() => import("@/screens/Operator").then((m) => ({ default: m.OperatorScreen })));

export function App() {
  const route = useRoute();
  const { desktop, landscape } = useWide();
  const player = usePlayerInstance();
  const recorded = usePlayerSelect((s) => s.kind === "call");
  const { current } = useBlocks();
  useAlerts();
  useShortcuts();
  useEffect(() => player.setBlockName(current?.name ?? ""), [player, current?.name]);

  const main = (() => {
    switch (route) {
      case "operator":
        return (
          <Suspense fallback={<p className="p-4 text-muted-foreground">Loading operator tools…</p>}>
            <OperatorScreen />
          </Suspense>
        );
      case "catch-up":
        return <CatchUpScreen timeline={desktop} />;
      case "radio":
        return <RadioTab />;
      default:
        return <Exploration />;
    }
  })();

  return (
    <div className="flex h-dvh flex-col">
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:absolute focus:top-2 focus:left-2 focus:z-50 focus:rounded focus:bg-card focus:p-2"
      >
        Skip to content
      </a>
      <Header wide={desktop || landscape} />
      <main
        id="main"
        className={route === "now" ? "flex min-h-0 flex-1" : "min-h-0 flex-1 overflow-y-auto"}
      >
        {route === "now" ? (
          main
        ) : (
          <div className={route === "operator" ? "" : "mx-auto w-full max-w-2xl"}>{main}</div>
        )}
      </main>
      {(route !== "now" || recorded) && <PlayerBar />}
      {!desktop && !landscape && <TabBar />}
      <SheetHost />
      <Palette />
      <ShortcutsDialog />
      <Toaster position={desktop ? "bottom-right" : "top-center"} />
    </div>
  );
}
