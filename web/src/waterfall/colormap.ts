/**
 * Dark-to-amber ramp, inferno-like but starting at the app's ink blue so an
 * empty waterfall sits in the page instead of punching a black hole in it.
 * Lightness rises monotonically, so it stays readable for colour-blind users.
 */
const stops: [number, [number, number, number]][] = [
  [0.0, [15, 26, 43]],
  [0.18, [27, 35, 80]],
  [0.38, [84, 40, 128]],
  [0.56, [168, 52, 110]],
  [0.72, [226, 96, 62]],
  [0.86, [245, 165, 36]],
  [1.0, [255, 243, 205]],
];

export function buildColormap(): Uint8Array {
  const out = new Uint8Array(256 * 4);
  for (let i = 0; i < 256; i++) {
    const t = i / 255;
    let k = 0;
    while (k < stops.length - 2 && t > stops[k + 1][0]) k++;
    const [t0, c0] = stops[k];
    const [t1, c1] = stops[k + 1];
    const f = Math.min(1, Math.max(0, (t - t0) / (t1 - t0)));
    for (let c = 0; c < 3; c++) out[i * 4 + c] = Math.round(c0[c] + (c1[c] - c0[c]) * f);
    out[i * 4 + 3] = 255;
  }
  return out;
}

export const COLORMAP = buildColormap();
