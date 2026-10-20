import { useEffect, useSyncExternalStore } from "react";
import { usePrefs } from "@/state/prefs";

const query = () => window.matchMedia?.("(prefers-color-scheme: light)");

function subscribe(listener: () => void) {
  const mq = query();
  mq?.addEventListener("change", listener);
  return () => mq?.removeEventListener("change", listener);
}

/** Dark first: "system" only goes light when the OS asks for it. */
export function useResolvedTheme(): "dark" | "light" {
  const { theme } = usePrefs();
  const systemLight = useSyncExternalStore(subscribe, () => query()?.matches ?? false, () => false);
  return theme === "system" ? (systemLight ? "light" : "dark") : theme;
}

export function useApplyTheme() {
  const theme = useResolvedTheme();
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle("dark", theme === "dark");
    root.style.colorScheme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", theme === "dark" ? "#0f1a2b" : "#f2f5fa");
  }, [theme]);
}
