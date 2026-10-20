import { describe, expect, it } from "vitest";
import { engineEventSchema, engineStatusSchema, listenMessageSchema } from "./engine";

// Verbatim /api/status from engine/src/api.rs before any producer connects.
const coldStatus = {
  bandwidth_hz: null,
  center_hz: null,
  channels: [],
  continuity: "transport sequence checked; hardware continuity unknown",
  decode_latency_s: null,
  decoder_capacity: 8,
  decoder_error: null,
  dropped_batches: 0,
  error: null,
  generation: 0,
  last_samples_at: null,
  mode: "disconnected",
  processed_batches: 0,
  queue_depth: null,
  rate_hz: null,
  received_seconds: null,
  source: {},
  source_name: "Pluto channelized RIQ1",
  source_position_s: null,
  started_at: null,
  state: "stopped",
};

// A channel exactly as runtime.rs make_channel() builds it: no decoder_admitted yet,
// and rf_active/level_db copied from activity JSON that may lack them.
const newChannel = {
  id: "454029223-Unknown-1",
  stream_id: 1,
  frequency_hz: 454029223.0,
  protocol: "Unknown",
  slot: 1,
  kind: "unidentified",
  rf_active: null,
  decoded: false,
  speech_active: false,
  last_heard: null,
  last_rf: null,
  level_db: null,
  audio_seconds: 0.0,
  sequence: 0,
  in_range: true,
  available: false,
};

describe("engine status", () => {
  it("parses the cold engine's nulls", () => {
    const status = engineStatusSchema.parse(coldStatus);
    expect(status.center_hz).toBeNull();
    expect(status.queue_depth).toBeNull();
    expect(status.channels).toEqual([]);
  });

  it("accepts a channel with null activity fields and no decoder_admitted", () => {
    const status = engineStatusSchema.parse({ ...coldStatus, channels: [newChannel] });
    expect(status.channels[0].rf_active).toBeNull();
    expect(status.channels[0].decoder_admitted).toBeUndefined();
  });

  it("drops one malformed channel without losing the rest", () => {
    const status = engineStatusSchema.parse({ ...coldStatus, channels: [{ id: 3 }, newChannel] });
    expect(status.channels.map((c) => c.id)).toEqual(["454029223-Unknown-1"]);
  });

  it("survives wrong types in optional fields", () => {
    const status = engineStatusSchema.parse({ ...coldStatus, queue_depth: "lots", source: null });
    expect(status.queue_depth).toBeNull();
    expect(status.source).toBeNull();
  });
});

describe("events and listen messages", () => {
  it("parses a channel event", () => {
    const event = engineEventSchema.parse({ type: "channel", generation: 1, channel: newChannel });
    expect(event.type).toBe("channel");
  });

  it("parses the lag gap notice", () => {
    const event = engineEventSchema.parse({ type: "gap", dropped_events: 3, recover: "GET /api/status" });
    expect(event.type).toBe("gap");
  });

  it("parses an audio descriptor and a heartbeat with a null selection", () => {
    expect(
      listenMessageSchema.parse({
        type: "audio",
        encoding: "s16le",
        channels: 1,
        rate_hz: 8000,
        samples: 160,
        channel_id: "454029223-DMR-2",
        generation: 4,
        source_generation: 1,
        sequence: 9,
        ingress_age_ms: 12.5,
        ingress_age_basis: "host ingress receipt; device speech alignment unknown",
      }).type,
    ).toBe("audio");
    expect(listenMessageSchema.parse({ type: "heartbeat", generation: 1, source_generation: 1, channel_id: null }).type).toBe(
      "heartbeat",
    );
  });
});
