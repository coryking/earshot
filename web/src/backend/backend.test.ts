import { afterEach, describe, expect, it, vi } from "vitest";
import { HttpBackend } from "./http";
import { decodeRsp1, encodeRsp1, fromJson } from "./spectrum";
import { BackendError } from "./types";

describe("spectrum", () => {
  it("round-trips RSP1", () => {
    const bins = Uint8Array.from({ length: 1024 }, (_, i) => i % 256);
    const buffer = encodeRsp1({
      centerHz: 454_216_998,
      spanHz: 6e6,
      lowDb: -150,
      highDb: 10,
      unit: "dB",
      sequence: 7,
      rows: [
        { sequence: 6, time: 1.5, bins },
        { sequence: 7, time: 1.6, bins },
      ],
    });
    expect(buffer.byteLength).toBe(32 + 2 * (16 + 1024));
    const batch = decodeRsp1(buffer)!;
    expect(batch.centerHz).toBe(454_216_998);
    expect(batch.rows.map((r) => r.sequence)).toEqual([6, 7]);
    expect(Array.from(batch.rows[1].bins.slice(0, 4))).toEqual([0, 1, 2, 3]);
  });

  it("rejects truncated RSP1", () => {
    expect(decodeRsp1(new ArrayBuffer(10))).toBeNull();
  });

  it("keeps real dBFS rows above the engine's fixed -125..-65 window", () => {
    const batch = fromJson({
      sequence: 3,
      generation: 1,
      rows: [[-61, -40, -16]],
      times: [0.1],
      center_hz: 454_216_998,
      span_hz: 6e6,
      low_db: -125,
      high_db: -65,
      unit: "dBFS",
      source: "live",
    });
    const [a, b, c] = batch.rows[0].bins;
    expect(a).toBeLessThan(b);
    expect(b).toBeLessThan(c);
    expect(c).toBeLessThan(255);
    expect(batch.rows[0].sequence).toBe(3);
  });
});

class FakeSocket {
  static last: FakeSocket;
  binaryType = "";
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onopen: (() => void) | null = null;
  url: string;
  constructor(url: string) {
    this.url = url;
    FakeSocket.last = this;
  }
  close() {
    this.onclose?.();
  }
}

describe("HttpBackend", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("pairs each audio descriptor with the binary PCM that follows", () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const onAudio = vi.fn();
    const onHeartbeat = vi.fn();
    new HttpBackend().listen({ onAudio, onHeartbeat, onGap: vi.fn(), onClose: vi.fn() });
    const socket = FakeSocket.last;
    expect(socket.url).toMatch(/\/api\/listen$/);
    const descriptor = {
      type: "audio",
      encoding: "s16le",
      channels: 1,
      rate_hz: 8000,
      samples: 2,
      channel_id: "454029223-DMR-2",
      generation: 1,
      source_generation: 1,
      sequence: 1,
      ingress_age_ms: 3,
    };
    socket.onmessage!({ data: JSON.stringify(descriptor) });
    socket.onmessage!({ data: new Int16Array([100, -100]).buffer });
    expect(onAudio).toHaveBeenCalledOnce();
    expect(Array.from(onAudio.mock.calls[0][1] as Int16Array)).toEqual([100, -100]);
    // A binary message without a descriptor, or with the wrong size, is dropped.
    socket.onmessage!({ data: new Int16Array([1]).buffer });
    socket.onmessage!({ data: JSON.stringify(descriptor) });
    socket.onmessage!({ data: new Int16Array([1, 2, 3]).buffer });
    expect(onAudio).toHaveBeenCalledOnce();
    socket.onmessage!({ data: JSON.stringify({ type: "heartbeat", generation: 1, source_generation: 1, channel_id: null }) });
    expect(onHeartbeat).toHaveBeenCalledWith(null);
  });

  it("surfaces the engine's plain-text monitor errors and JSON detail errors", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(new Response("Channel protocol is not confirmed yet", { status: 422 }))
        .mockResolvedValueOnce(new Response(JSON.stringify({ detail: "Only live sources are supported" }), { status: 400 })),
    );
    const backend = new HttpBackend();
    await expect(backend.monitor("x")).rejects.toMatchObject({ status: 422, message: "Channel protocol is not confirmed yet" });
    const error = await backend.configureSource({ centerHz: 1, rateHz: 1, bandwidthHz: 1 }).catch((e) => e);
    expect(error).toBeInstanceOf(BackendError);
    expect(error.message).toBe("Only live sources are supported");
  });

  it("treats a missing frontend-needs endpoint as null, not an error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 404 })));
    const backend = new HttpBackend();
    expect(await backend.catalog()).toBeNull();
    expect(await backend.calls({})).toBeNull();
    expect(await backend.saveCatalogEntry({} as never)).toBe(false);
  });
});
