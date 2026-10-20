import type { EngineSpectrum } from "@/api/engine";
import { RSP1_HEADER } from "@/api/needs";
import type { SpectrumBatch, SpectrumRow } from "@/domain/model";

export function scaleToByte(db: number, low: number, high: number): number {
  if (!Number.isFinite(db)) return 0;
  const v = Math.round(((db - low) / (high - low)) * 255);
  return v < 0 ? 0 : v > 255 ? 255 : v;
}

/**
 * Today's JSON float rows → the compact byte rows the waterfall uses.
 *
 * The engine reports fixed low/high bounds (-125/-65) whatever the producer's
 * unit is, and real dBFS rows sit well above them. So quantize over a wide
 * window (0.63 dB per step) and let the renderer's auto-level set contrast.
 */
export function fromJson(json: EngineSpectrum): SpectrumBatch {
  const first = json.sequence - json.rows.length + 1;
  const low = Math.min(json.low_db, -150);
  const high = Math.max(json.high_db, 10);
  const rows: SpectrumRow[] = json.rows.map((row, i) => {
    const bins = new Uint8Array(row.length);
    for (let b = 0; b < row.length; b++) bins[b] = scaleToByte(row[b], low, high);
    return { sequence: first + i, time: json.times[i] ?? 0, bins };
  });
  return {
    centerHz: json.center_hz ?? null,
    spanHz: json.span_hz ?? null,
    lowDb: low,
    highDb: high,
    unit: json.unit,
    sequence: json.sequence,
    rows,
  };
}

/** Decode an RSP1 message (see api/needs.ts). Returns null when malformed. */
export function decodeRsp1(buffer: ArrayBuffer): SpectrumBatch | null {
  if (buffer.byteLength < RSP1_HEADER) return null;
  const view = new DataView(buffer);
  const magic = String.fromCharCode(view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
  if (magic !== "RSP1") return null;
  const bins = view.getUint16(4, true);
  const count = view.getUint16(6, true);
  const stride = 16 + bins;
  if (bins === 0 || buffer.byteLength !== RSP1_HEADER + count * stride) return null;
  const rows: SpectrumRow[] = [];
  for (let r = 0; r < count; r++) {
    const offset = RSP1_HEADER + r * stride;
    rows.push({
      sequence: Number(view.getBigUint64(offset, true)),
      time: view.getFloat64(offset + 8, true),
      bins: new Uint8Array(buffer, offset + 16, bins),
    });
  }
  return {
    centerHz: view.getFloat64(8, true),
    spanHz: view.getFloat64(16, true),
    lowDb: view.getFloat32(24, true),
    highDb: view.getFloat32(28, true),
    unit: "dB",
    sequence: rows.at(-1)?.sequence ?? 0,
    rows,
  };
}

/** Encoder used by the mock and tests; the engine would do the same in Rust. */
export function encodeRsp1(batch: SpectrumBatch): ArrayBuffer {
  const bins = batch.rows[0]?.bins.length ?? 0;
  const stride = 16 + bins;
  const buffer = new ArrayBuffer(RSP1_HEADER + batch.rows.length * stride);
  const view = new DataView(buffer);
  "RSP1".split("").forEach((c, i) => view.setUint8(i, c.charCodeAt(0)));
  view.setUint16(4, bins, true);
  view.setUint16(6, batch.rows.length, true);
  view.setFloat64(8, batch.centerHz ?? 0, true);
  view.setFloat64(16, batch.spanHz ?? 0, true);
  view.setFloat32(24, batch.lowDb, true);
  view.setFloat32(28, batch.highDb, true);
  batch.rows.forEach((row, r) => {
    const offset = RSP1_HEADER + r * stride;
    view.setBigUint64(offset, BigInt(row.sequence), true);
    view.setFloat64(offset + 8, row.time, true);
    new Uint8Array(buffer, offset + 16, bins).set(row.bins);
  });
  return buffer;
}
