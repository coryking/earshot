import { expect, it } from "vitest";
import { RawCapture } from "./capture";
import type { AudioDescriptor } from "@/api/engine";
const meta = { rate_hz: 48000, generation: 1, source_generation: 2 } as AudioDescriptor;

it("writes signed16 mono48k WAV with frequency and gap provenance", () => {
  const capture = new RawCapture(464912500);
  capture.push(meta, new Int16Array([123, -456]));
  capture.gap("socket closed");
  capture.push({ ...meta, generation: 2 }, new Int16Array([789]));
  const wav = capture.wav();
  const view = new DataView(wav);
  expect(new TextDecoder().decode(wav.slice(0, 4))).toBe("RIFF");
  expect(view.getUint32(4, true)).toBe(wav.byteLength - 8);
  expect(view.getUint32(24, true)).toBe(48000);
  expect(view.getUint32(40, true)).toBe(6);
  expect([44, 46, 48].map((at) => view.getInt16(at, true))).toEqual([123, -456, 789]);
  const text = new TextDecoder().decode(wav);
  expect(text).toContain('"frequency_hz":464912500');
  expect(text).toContain('"sample":2,"reason":"socket closed"');
});

it("bounds capture memory and adjusts gap sample positions after rolling eviction", () => {
  const capture = new RawCapture(452675000, 48000, 0.001); // 48 samples
  capture.push(meta, new Int16Array(30).fill(1));
  capture.gap("source gap");
  capture.push(meta, new Int16Array(30).fill(2));
  expect(capture.seconds).toBe(0.001);
  const wav = capture.wav();
  const view = new DataView(wav);
  expect(view.getUint32(40, true)).toBe(96);
  expect(view.getInt16(44, true)).toBe(1);
  expect(view.getInt16(44 + 18 * 2, true)).toBe(2);
  const text = new TextDecoder().decode(wav);
  expect(text).toContain('"retained_from_sample":12');
  expect(text).toContain('"sample":18,"reason":"source gap"');
});
