import { COLORMAP } from "./colormap";

/**
 * Waterfall renderer. Rows go into a ring texture (one row per spectrum line,
 * newest at the top); the colormap is a lookup texture applied in the shader.
 * Per-bin exponential averaging smooths speckle before upload. Falls back to
 * a 2D canvas when WebGL2 is unavailable.
 */
export interface Renderer {
  push(row: Uint8Array): void;
  resize(width: number, height: number): void;
  draw(): void;
  dispose(): void;
}

const VERTEX = `#version 300 es
in vec2 p;
out vec2 uv;
void main() { uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5); gl_Position = vec4(p, 0.0, 1.0); }`;

const FRAGMENT = `#version 300 es
precision highp float;
in vec2 uv;
uniform sampler2D rows;
uniform sampler2D colormap;
uniform float newest;
uniform float visible;
uniform float capacity;
uniform float floorLevel;
uniform float ceilLevel;
out vec4 color;
void main() {
  float r = mod(newest - floor(uv.y * visible) + capacity, capacity);
  float v = texture(rows, vec2(uv.x, (r + 0.5) / capacity)).r;
  float t = clamp((v - floorLevel) / max(0.02, ceilLevel - floorLevel), 0.0, 1.0);
  color = texture(colormap, vec2(t, 0.5));
}`;

export interface RendererOptions {
  bins: number;
  /** Rows kept in the ring (history height). */
  capacity: number;
  /** Rows shown; ≤ capacity. */
  visible: number;
  /** EMA weight of the newest row, 0..1 (1 = no averaging). */
  smoothing?: number;
}

/** Noise floor and peak estimates so the colormap adapts to the data. */
class Levels {
  floor = 0.25;
  ceil = 0.85;
  private seeded = false;
  update(row: Uint8Array) {
    const sorted = Array.from(row).sort((a, b) => a - b);
    const floor = sorted[Math.floor(sorted.length * 0.2)] / 255 - 0.01;
    const ceil = Math.max(floor + 0.12, sorted[sorted.length - 1] / 255);
    if (!this.seeded) {
      this.floor = floor;
      this.ceil = ceil;
      this.seeded = true;
      return;
    }
    this.floor = this.floor * 0.9 + floor * 0.1;
    this.ceil = this.ceil * 0.97 + Math.max(this.floor + 0.12, ceil) * 0.03;
  }
}

class Smoother {
  private state: Float32Array | null = null;
  private alpha: number;

  constructor(alpha: number) {
    this.alpha = alpha;
  }
  apply(row: Uint8Array): Uint8Array {
    if (this.alpha >= 1) return row;
    if (!this.state || this.state.length !== row.length) this.state = Float32Array.from(row);
    const out = new Uint8Array(row.length);
    for (let i = 0; i < row.length; i++) {
      this.state[i] += (row[i] - this.state[i]) * this.alpha;
      out[i] = this.state[i];
    }
    return out;
  }
}

export function createRenderer(canvas: HTMLCanvasElement, options: RendererOptions): Renderer {
  const gl = canvas.getContext("webgl2", { antialias: false, preserveDrawingBuffer: false });
  return gl ? webgl(canvas, gl, options) : canvas2d(canvas, options);
}

function webgl(canvas: HTMLCanvasElement, gl: WebGL2RenderingContext, o: RendererOptions): Renderer {
  const compile = (type: number, source: string) => {
    const shader = gl.createShader(type)!;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    return shader;
  };
  const program = gl.createProgram()!;
  gl.attachShader(program, compile(gl.VERTEX_SHADER, VERTEX));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, FRAGMENT));
  gl.linkProgram(program);
  gl.useProgram(program);

  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(program, "p");
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  const rows = gl.createTexture();
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, rows);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, o.bins, o.capacity, 0, gl.RED, gl.UNSIGNED_BYTE, new Uint8Array(o.bins * o.capacity));
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.REPEAT);

  const lut = gl.createTexture();
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, lut);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, COLORMAP);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);

  const u = (name: string) => gl.getUniformLocation(program, name);
  gl.uniform1i(u("rows"), 0);
  gl.uniform1i(u("colormap"), 1);
  gl.uniform1f(u("capacity"), o.capacity);
  gl.uniform1f(u("visible"), o.visible);

  let newest = 0;
  const levels = new Levels();
  const smoother = new Smoother(o.smoothing ?? 0.6);
  return {
    push(row) {
      if (row.length !== o.bins) return;
      const smoothed = smoother.apply(row);
      levels.update(smoothed);
      newest = (newest + 1) % o.capacity;
      gl.activeTexture(gl.TEXTURE0);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, newest, o.bins, 1, gl.RED, gl.UNSIGNED_BYTE, smoothed);
    },
    resize(width, height) {
      canvas.width = width;
      canvas.height = height;
      gl.viewport(0, 0, width, height);
    },
    draw() {
      gl.uniform1f(u("newest"), newest);
      gl.uniform1f(u("floorLevel"), levels.floor);
      gl.uniform1f(u("ceilLevel"), levels.ceil);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    },
    dispose() {
      gl.deleteTexture(rows);
      gl.deleteTexture(lut);
      gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
    },
  };
}

function canvas2d(canvas: HTMLCanvasElement, o: RendererOptions): Renderer {
  const ctx = canvas.getContext("2d");
  const history: Uint8Array[] = [];
  const levels = new Levels();
  const smoother = new Smoother(o.smoothing ?? 0.6);
  return {
    push(row) {
      const smoothed = smoother.apply(row);
      levels.update(smoothed);
      history.unshift(smoothed);
      if (history.length > o.visible) history.pop();
    },
    resize(width, height) {
      canvas.width = width;
      canvas.height = height;
    },
    draw() {
      if (!ctx || !history.length) return;
      const width = o.bins;
      const image = ctx.createImageData(width, o.visible);
      const span = Math.max(0.02, levels.ceil - levels.floor);
      history.forEach((row, y) => {
        for (let x = 0; x < width; x++) {
          const t = Math.min(1, Math.max(0, (row[x] / 255 - levels.floor) / span));
          const c = Math.round(t * 255) * 4;
          const i = (y * width + x) * 4;
          image.data[i] = COLORMAP[c];
          image.data[i + 1] = COLORMAP[c + 1];
          image.data[i + 2] = COLORMAP[c + 2];
          image.data[i + 3] = 255;
        }
      });
      const scratch = new OffscreenCanvas(width, o.visible);
      scratch.getContext("2d")?.putImageData(image, 0, 0);
      ctx.imageSmoothingEnabled = true;
      ctx.drawImage(scratch, 0, 0, canvas.width, canvas.height);
    },
    dispose() {},
  };
}
