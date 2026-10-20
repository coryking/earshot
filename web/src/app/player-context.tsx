import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
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
  const player = useContext(PlayerContext);
  if (!player) throw new Error("PlayerProvider missing");
  return [player, useStore(player.state)];
}
