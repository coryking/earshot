import { createContext, useContext, useEffect, useState, useSyncExternalStore, type ReactNode } from "react";
import { Player, type PlayerState } from "@/audio/player";
import { useStore } from "@/lib/store";
import { useBackend } from "@/state/radio";

const PlayerContext = createContext<Player | null>(null);

export function PlayerProvider({ children }: { children: ReactNode }) {
  const backend = useBackend();
  const [player] = useState(() => new Player(backend));
  useEffect(() => () => player.dispose(), [player]);
  return <PlayerContext.Provider value={player}>{children}</PlayerContext.Provider>;
}

export function usePlayer(): [Player, PlayerState] {
  const player = usePlayerInstance();
  return [player, useStore(player.state)];
}

/** The player without subscribing to its state (for handlers only). */
export function usePlayerInstance(): Player {
  const player = useContext(PlayerContext);
  if (!player) throw new Error("PlayerProvider missing");
  return player;
}

/** Subscribe to one derived value, so a row doesn't re-render on every progress tick. */
export function usePlayerSelect<T extends string | number | boolean | null>(select: (state: PlayerState) => T): T {
  const player = usePlayerInstance();
  return useSyncExternalStore(player.state.subscribe, () => select(player.state.get()));
}
