import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { engineChannelSchema } from "@/api/engine";
import { usePlayerInstance } from "@/app/player-context";
import { Providers } from "@/app/Providers";
import { isAudible, Player, type PlayerState } from "@/audio/player";
import { MockBackend } from "@/backend/mock";
import { toSources } from "@/domain/adapt";
import { PlayerBar, playerLine } from "./PlayerBar";

const backend = () => new MockBackend({ manual: true });
const initial = new Player(backend());
const defaults = initial.state.get();
initial.dispose();
const state = (patch: Partial<PlayerState>): PlayerState => ({ ...defaults, kind: "live", ...patch });

describe("speaker status", () => {
  it.each(["starting", "waiting", "reconnecting", "taken", "error", "idle"] as const)("never describes %s as playing", (phase) => {
    const s = state({ phase, error: phase === "error" ? "Channel gone" : null });
    expect(isAudible(s)).toBe(false);
    expect(playerLine(s, null)).not.toMatch(/^Playing/);
  });

  it("keeps takeover and error visible while muted", () => {
    expect(playerLine(state({ phase: "taken", muted: true }), "Other")).toBe("Switched to Other elsewhere");
    expect(playerLine(state({ phase: "error", muted: true, error: "Channel gone" }), null)).toBe("Channel gone");
  });

  it("distinguishes selected-but-waiting, incoming muted audio, and output", () => {
    expect(playerLine(state({ phase: "waiting" }), null)).toBe("Held · waiting for decoded voice");
    expect(playerLine(state({ phase: "playing", volume: 0 }), null)).toBe("Speaker muted · live audio arriving");
    expect(playerLine(state({ phase: "playing", muted: true }), null)).toBe("Speaker muted · live audio arriving");
    expect(playerLine(state({ phase: "playing", muted: false, volume: 0.8 }), null)).toBe("Playing live voice");
    expect(playerLine(state({ kind: "call", phase: "playing", muted: true }), null)).toMatch(/^Speaker muted · Recorded/);
    expect(playerLine(state({ kind: "call", phase: "waiting" }), null)).toBe("Buffering recording…");
  });
});

describe("player speaker controls", () => {
  it("exposes mute beside volume; muting holds selection until Stop listening", async () => {
    const radio = backend();
    const monitor = vi.spyOn(radio, "monitor").mockResolvedValue();
    let player!: Player;
    function Probe() { player = usePlayerInstance(); return null; }
    render(<Providers backend={radio}><Probe /><PlayerBar /></Providers>);
    const source = toSources([engineChannelSchema.parse({ id: "452685504-DMR-1", frequency_hz: 452685504, protocol: "DMR", decoded: true })], null)[0];
    await act(() => player.listen(source));
    const region = screen.getByRole("region", { name: "Player" });
    expect(within(region).getByRole("slider", { name: "Speaker volume" })).toBeVisible();
    const user = userEvent.setup();
    await user.click(within(region).getByRole("button", { name: "Mute speaker" }));
    expect(within(region).getByRole("button", { name: "Unmute speaker" })).toHaveAttribute("aria-pressed", "true");
    expect(player.state.get()).toMatchObject({ kind: "live", phase: "waiting", muted: true });
    expect(monitor.mock.calls).toEqual([[source.channelIds[0]]]);
    await user.click(within(region).getByRole("button", { name: "Stop listening" }));
    expect(monitor).toHaveBeenLastCalledWith(null);
    expect(screen.queryByRole("region", { name: "Player" })).not.toBeInTheDocument();
  });
});
