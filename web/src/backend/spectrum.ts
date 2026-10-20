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
    generation: json.generation,
    centerHz: json.center_hz || null,
    spanHz: json.span_hz || null,
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
  if (buffer.byteLength !== RSP1_HEADER + count * stride || (count > 0 && bins === 0)) return null;
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
    generation: view.getUint32(40, true),
    centerHz: view.getFloat64(8, true) || null,
    spanHz: view.getFloat64(16, true) || null,
    lowDb: view.getFloat32(24, true),
    highDb: view.getFloat32(28, true),
    unit: "dB",
    sequence: Number(view.getBigUint64(32, true)),
    rows,
  };
}
