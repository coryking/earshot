import { describe, it, expect } from "vitest";
import { AudioWire } from "./audio-wire";
const metadata = {
  type: "audio",
  encoding: "s16le",
  channels: 1,
  rate_hz: 8000,
  samples: 2,
  channel_id: "452341815:NXDN48:1",
  sequence: 1,
  generation: 2,
  source_generation: 3,
  ingress_age_ms: 18,
};
describe("live audio wire boundary", () => {
  it("accepts a Rust metadata frame followed by exactly its PCM samples", () => {
    const wire = new AudioWire();
    expect(wire.read(JSON.stringify(metadata))).toBeNull();
    const frame = wire.read(new Uint8Array([1, 0, 255, 127]).buffer);
    expect(frame?.channel_id).toBe(metadata.channel_id);
    expect(frame?.source_generation).toBe(3);
    expect([...frame!.bytes]).toEqual([1, 0, 255, 127]);
  });
  it("drops unmatched and truncated binary frames rather than routing stale audio", () => {
    const wire = new AudioWire();
    expect(wire.read(new Uint8Array([1, 0]).buffer)).toBeNull();
    wire.read(JSON.stringify(metadata));
    expect(wire.read(new Uint8Array([1, 0]).buffer)).toBeNull();
    wire.read(JSON.stringify(metadata));
    wire.reset();
    expect(wire.read(new Uint8Array([1, 0, 2, 0]).buffer)).toBeNull();
  });
  it("retains compatibility with the current JSON PCM stream", () => {
    const frame = new AudioWire().read(
      JSON.stringify({
        ...metadata,
        pcm: btoa(String.fromCharCode(1, 0, 2, 0)),
      }),
    );
    expect([...frame!.bytes]).toEqual([1, 0, 2, 0]);
  });
  it("rejects unsupported sample formats and odd PCM byte counts", () => {
    const wire = new AudioWire();
    wire.read(JSON.stringify({ ...metadata, encoding: "f32le" }));
    expect(wire.read(new Uint8Array([1, 0, 2, 0]).buffer)).toBeNull();
    expect(
      wire.read(JSON.stringify({ ...metadata, pcm: btoa("x") })),
    ).toBeNull();
  });
});
