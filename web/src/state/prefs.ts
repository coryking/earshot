/** Per-device listener preferences. Follow/mute are personal, so they stay on the device. */
import { z } from "zod";
import { createStore, local, useStore } from "@/lib/store";

export type ThemeChoice = "dark" | "light" | "system";

export interface Prefs {
  followed: string[];
  muted: string[];
  alertFollowed: boolean;
  alertBusy: boolean;
  theme: ThemeChoice;
  volume: number;
}

const KEY = "radio.prefs.v1";
const defaults: Prefs = {
  followed: [],
  muted: [],
  alertFollowed: false,
  alertBusy: false,
  theme: "dark",
  volume: 0.8,
};

const ids = z.array(z.string());
/** Each field falls back on its own, so one bad value doesn't reset the rest. */
const prefsSchema = z.object({
  followed: ids.catch(defaults.followed),
  muted: ids.catch(defaults.muted),
  alertFollowed: z.boolean().catch(defaults.alertFollowed),
  alertBusy: z.boolean().catch(defaults.alertBusy),
  theme: z.enum(["dark", "light", "system"]).catch(defaults.theme),
  volume: z.number().min(0).max(1).catch(defaults.volume),
});

export const prefsStore = createStore<Prefs>(local.read(KEY, prefsSchema, defaults));
prefsStore.subscribe(() => local.write(KEY, prefsStore.get()));

export function usePrefs() {
  return useStore(prefsStore);
}

function toggle(list: string[], id: string) {
  return list.includes(id) ? list.filter((x) => x !== id) : [...list, id];
}

export const prefs = {
  toggleFollow(id: string) {
    prefsStore.set((p) => ({ ...p, followed: toggle(p.followed, id), muted: p.muted.filter((x) => x !== id) }));
  },
  toggleMute(id: string) {
    prefsStore.set((p) => ({ ...p, muted: toggle(p.muted, id), followed: p.followed.filter((x) => x !== id) }));
  },
  set<K extends keyof Prefs>(key: K, value: Prefs[K]) {
    prefsStore.set((p) => ({ ...p, [key]: value }));
  },
};
