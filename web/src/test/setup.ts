import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

import { paletteStore, sheetStore } from "@/app/ui";
import { prefsStore } from "@/state/prefs";
import { localCatalog, sessionActivity, sessionCalls } from "@/state/radio";

const initialPrefs = prefsStore.get();
const initialActivity = sessionActivity.get();
afterEach(() => {
  cleanup();
  location.hash = "";
  sessionActivity.set(initialActivity);
  localCatalog.set([]);
  sheetStore.set(null);
  paletteStore.set(false);
  prefsStore.set(initialPrefs);
  sessionCalls.set([]);
  localStorage.clear();
});

// jsdom has no Web Audio, ResizeObserver or matchMedia. Minimal fakes so the
// player and layout run; tests assert on what reaches the backend and the DOM.
class FakeParam {
  value = 1;
  setTargetAtTime(v: number) {
    this.value = v;
  }
}
class FakeNode {
  port = { postMessage: vi.fn(), onmessage: null as unknown };
  gain = new FakeParam();
  connect() {}
  disconnect() {}
}
class FakeAudioContext {
  state = "running";
  currentTime = 0;
  destination = {};
  audioWorklet = { addModule: () => Promise.resolve() };
  resume() {
    return Promise.resolve();
  }
  close() {
    return Promise.resolve();
  }
  createGain() {
    return new FakeNode();
  }
  createMediaStreamDestination() {
    return { stream: {} };
  }
}
vi.stubGlobal("AudioContext", FakeAudioContext);
vi.stubGlobal("AudioWorkletNode", FakeNode);
vi.stubGlobal(
  "ResizeObserver",
  class {
    observe() {}
    disconnect() {}
  },
);
if (!window.matchMedia)
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      addEventListener() {},
      removeEventListener() {},
    }) as unknown as MediaQueryList;
HTMLCanvasElement.prototype.getContext = (() => null) as unknown as HTMLCanvasElement["getContext"];
Element.prototype.setPointerCapture ??= () => {};
Element.prototype.releasePointerCapture ??= () => {};
Element.prototype.hasPointerCapture ??= () => false;
// jsdom doesn't implement media playback.
HTMLMediaElement.prototype.play = () => Promise.resolve();
HTMLMediaElement.prototype.pause = () => {};
HTMLMediaElement.prototype.load = () => {};
