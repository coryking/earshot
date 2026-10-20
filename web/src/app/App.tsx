import { lazy, Suspense, useEffect } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { useAlerts } from "@/app/alerts";
import { Palette, ShortcutsDialog, useShortcuts } from "@/app/Palette";
import { useRoute, useWide } from "@/app/ui";
import { Header, TabBar } from "@/components/Chrome";
import { PlayerBar } from "@/components/PlayerBar";
import { SheetHost } from "@/components/Sheets";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { Toaster } from "@/components/ui/sonner";
import { LiveSelection } from "@/components/Tuning";
import { Waterfall } from "@/components/Waterfall";
import { CatchUpScreen } from "@/screens/CatchUp";
import { NowScreen, useHome } from "@/screens/Now";
import { RadioTab } from "@/screens/RadioTab";
import { useBlocks } from "@/state/radio";

const OperatorScreen = lazy(() => import("@/screens/Operator").then((m) => ({ default: m.OperatorScreen })));

/** Live channel selection and spectrum; recordings belong to Catch up. */
function WideNow() {
  const { view } = useHome();
  const player = usePlayerInstance();
  const tunedFrequencyHz = usePlayerSelect((s) => s.kind === "live" && s.phase !== "taken" && s.phase !== "error" ? s.frequencyHz : null);
  const activeSourceId = usePlayerSelect((s) => (s.kind === "live" && s.phase !== "taken" && s.phase !== "error" ? s.sourceId : null));
  return (
    <ResizablePanelGroup orientation="horizontal" className="min-h-0 flex-1">
      <ResizablePanel defaultSize="38%" minSize="26%" className="min-h-0">
        <div className="h-full overflow-y-auto">
          <NowScreen showStrip={false} />
        </div>
      </ResizablePanel>
      <ResizableHandle withHandle />
      <ResizablePanel defaultSize="62%" minSize="30%" className="min-h-0">
        <div className="flex h-full min-h-0 flex-col gap-3 p-3">
          <LiveSelection />
          <div className="space-y-1 text-xs text-muted-foreground">
            <p>Click or drag to hold a frequency. UHF tuning snaps to 6.25 kHz steps; hold Shift for an exact frequency.</p>
            <p><span className="text-lamp">Amber: recent activity</span> · <span className="text-signal">Blue: held channel</span>. Only the held channel feeds the speaker.</p>
          </div>
          <Waterfall
            variant="full"
            sources={view.all}
            talking={view.talking}
            activeSourceId={activeSourceId}
            tunedFrequencyHz={tunedFrequencyHz}
            onListen={(s) => void player.listen(s, { raw: !s.heardVoice || s.control || s.encryption === "encrypted" })}
            onUnnamed={(frequencyHz) => void player.listenFrequency(frequencyHz)}
            className="flex-1"
          />
        </div>
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

export function App() {
  const route = useRoute();
  const { wide, desktop } = useWide();
  const player = usePlayerInstance();
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
        return wide ? <WideNow /> : <NowScreen showStrip />;
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
      <Header wide={desktop} />
      <main
        id="main"
        className={wide && route === "now" ? "flex min-h-0 flex-1" : "min-h-0 flex-1 overflow-y-auto"}
      >
        {wide && route === "now" ? (
          main
        ) : (
          <div className={route === "operator" ? "" : "mx-auto w-full max-w-2xl"}>{main}</div>
        )}
      </main>
      <PlayerBar />
      {!desktop && <TabBar />}
      <SheetHost />
      <Palette />
      <ShortcutsDialog />
      <Toaster position={desktop ? "bottom-right" : "top-center"} />
    </div>
  );
}
