(() => {
  "use strict";

  const CELL_WIDTH = 8;
  const CELL_HEIGHT = 17;
  const glyphStates = new Map();
  let device;
  let context;
  let format;
  let sampler;
  let atlasSampler;
  let scenePipeline;
  let presentPipeline;
  let inkPipeline;
  let frameUniform;
  let sceneTexture;
  let sceneSize = [0, 0];
  let sceneBuffer;
  let sceneCapacity = 0;
  let atlas;
  let atlasTexture;
  let atlasDpr = 0;
  let inkTexture;
  let freshTexture;
  let inkSize = [0, 0];
  let starting;
  let latest;
  let frameId = 0;
  let animationTimer = 0;
  let cachedFrame;
  let cachedPackage = "";
  let cachedLayout = "";
  let cachedGeometry;

  const metrics = window.portfolioV2RenderMetrics = {
    renderCalls: 0,
    paintedFrames: 0,
    averagePaintMs: 0,
    requestedPackage: "canonical",
    activePackage: "canonical",
    fallbackReason: "",
    sceneVertices: 0,
    sceneVertexBytes: 0,
    staleFramesDiscarded: 0,
    activeInkDetails: 0,
    wetInkDetails: 0,
  };
  let paintTotal = 0;

  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  const hash = (value) => {
    const x = Math.sin(value * 12.9898) * 43758.5453;
    return x - Math.floor(x);
  };
  const renderDpr = (packageName) => packageName === "canonical"
    ? devicePixelRatio || 1
    : Math.min(devicePixelRatio || 1, 1.25);

  function showSemanticFallback(reason) {
    metrics.activePackage = "semantic";
    metrics.fallbackReason = reason;
    document.documentElement.dataset.renderer = "semantic-fallback";
    document.getElementById("semantic")?.classList.add("visible-fallback");
    const status = document.getElementById("status");
    if (status) {
      status.textContent = !isSecureContext
        ? "GPU view needs HTTPS; showing the readable version."
        : "GPU view unavailable; showing the readable version.";
      status.dataset.empty = "false";
    }
  }

  function sizeCanvas(canvas, width, height, dpr) {
    const pixelWidth = Math.ceil(width * dpr);
    const pixelHeight = Math.ceil(height * dpr);
    if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
    if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
  }

  function makeAtlas(dpr) {
    if (atlas && atlasDpr === dpr) return atlas;
    const chars = [...new Set([
      ...Array.from({ length: 95 }, (_, i) => String.fromCodePoint(0x20 + i)),
      ...Array.from({ length: 112 }, (_, i) => String.fromCodePoint(0x2190 + i)),
      ...Array.from({ length: 160 }, (_, i) => String.fromCodePoint(0x2500 + i)),
      ...Array.from({ length: 96 }, (_, i) => String.fromCodePoint(0x25a0 + i)),
      ...Array.from({ length: 256 }, (_, i) => String.fromCodePoint(0x2800 + i)),
      ...Array.from({ length: 60 }, (_, i) => String.fromCodePoint(0x1fb00 + i)),
      ..."◆·°—←→↑↓█▓▒░▄▀▌▐■▪▫•",
    ])];
    const guard = 2;
    const cellWidth = Math.ceil(CELL_WIDTH * dpr);
    const cellHeight = Math.ceil(CELL_HEIGHT * dpr);
    const width = cellWidth + guard * 2;
    const height = cellHeight + guard * 2;
    const columns = 32;
    const canvas = document.createElement("canvas");
    canvas.width = columns * width;
    canvas.height = Math.ceil(chars.length * 2 / columns) * height;
    const ctx = canvas.getContext("2d");
    const slots = new Map();
    ctx.textBaseline = "alphabetic";
    ctx.fillStyle = "white";
    for (let bold = 0; bold < 2; bold++) {
      ctx.font = `${bold ? 700 : 400} ${13 * dpr}px "Iosevka Portfolio"`;
      chars.forEach((glyph, i) => {
        const slot = bold * chars.length + i;
        const x = slot % columns * width;
        const y = Math.floor(slot / columns) * height;
        ctx.fillText(glyph, x + guard, y + guard + 13 * dpr);
        slots.set(`${bold}:${glyph}`, [x + guard, y + guard, cellWidth, cellHeight]);
      });
    }
    atlasDpr = dpr;
    atlas = { canvas, slots };
    return atlas;
  }

  function ensureAtlas(dpr) {
    if (atlasTexture && atlasDpr === dpr) return;
    atlasTexture?.destroy();
    const source = makeAtlas(dpr);
    atlasTexture = device.createTexture({
      size: [source.canvas.width, source.canvas.height],
      format: "rgba8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST | GPUTextureUsage.RENDER_ATTACHMENT,
    });
    device.queue.copyExternalImageToTexture({ source: source.canvas }, { texture: atlasTexture }, [source.canvas.width, source.canvas.height]);
  }

  async function startGpu() {
    if (starting) return starting;
    starting = (async () => {
      if (!navigator.gpu) return false;
      const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
      if (!adapter) return false;
      device = await adapter.requestDevice();
      context = document.getElementById("stage").getContext("webgpu");
      format = navigator.gpu.getPreferredCanvasFormat();
      context.configure({ device, format, alphaMode: "opaque" });
      sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
      atlasSampler = device.createSampler({ magFilter: "nearest", minFilter: "nearest" });
      frameUniform = device.createBuffer({ size: 32, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });

      const sceneModule = device.createShaderModule({ code: `
        @group(0) @binding(0) var atlasSampler: sampler;
        @group(0) @binding(1) var atlasTexture: texture_2d<f32>;
        struct Out { @builtin(position) position: vec4f, @location(0) color: vec4f, @location(1) uv: vec2f, @location(2) glyph: f32 };
        @vertex fn vs(@location(0) position: vec2f, @location(1) color: vec4f, @location(2) uv: vec2f, @location(3) glyph: f32) -> Out {
          var out: Out; out.position = vec4f(position, 0.0, 1.0); out.color = color; out.uv = uv; out.glyph = glyph; return out;
        }
        @fragment fn fs(in: Out) -> @location(0) vec4f {
          let coverage = select(1.0, textureSample(atlasTexture, atlasSampler, in.uv).a, in.glyph > 0.5);
          return vec4f(in.color.rgb, in.color.a * coverage);
        }
      ` });
      scenePipeline = device.createRenderPipeline({
        layout: "auto",
        vertex: { module: sceneModule, entryPoint: "vs", buffers: [{
          arrayStride: 36,
          attributes: [
            { shaderLocation: 0, offset: 0, format: "float32x2" },
            { shaderLocation: 1, offset: 8, format: "float32x4" },
            { shaderLocation: 2, offset: 24, format: "float32x2" },
            { shaderLocation: 3, offset: 32, format: "float32" },
          ],
        }] },
        fragment: { module: sceneModule, entryPoint: "fs", targets: [{
          format: "rgba8unorm",
          blend: {
            color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
            alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
          },
        }] },
        primitive: { topology: "triangle-list" },
      });

      const fullscreen = `
        struct Out { @builtin(position) position: vec4f, @location(0) uv: vec2f };
        @vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
          var positions = array<vec2f, 3>(vec2f(-1.0, -1.0), vec2f(3.0, -1.0), vec2f(-1.0, 3.0));
          var out: Out; out.position = vec4f(positions[i], 0.0, 1.0); out.uv = positions[i] * vec2f(0.5, -0.5) + vec2f(0.5); return out;
        }
      `;
      const presentModule = device.createShaderModule({ code: `
        struct Params { mode: u32, mono: u32, light: u32, reduced: u32, time: f32, width: f32, height: f32, dpr: f32 };
        @group(0) @binding(0) var frameSampler: sampler;
        @group(0) @binding(1) var frameTexture: texture_2d<f32>;
        @group(0) @binding(2) var<uniform> p: Params;
        ${fullscreen}
        fn sampleFrame(uv: vec2f) -> vec3f { return textureSampleLevel(frameTexture, frameSampler, clamp(uv, vec2f(0.0), vec2f(1.0)), 0.0).rgb; }
        fn rand(v: vec2f) -> f32 { return fract(sin(dot(v, vec2f(12.9898, 78.233))) * 43758.5453); }
        @fragment fn fs(in: Out) -> @location(0) vec4f {
          var uv = in.uv;
          if (p.mode == 0u) { return vec4f(sampleFrame(uv), 1.0); }
          if (p.mode == 1u) {
            var q = uv * 2.0 - 1.0;
            q += q * abs(q.yx) * abs(q.yx) / vec2f(30.0, 24.0);
            uv = q * 0.5 + 0.5;
            if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) { return vec4f(0.003, 0.004, 0.003, 1.0); }
            var color = sampleFrame(uv);
            let px = vec2f(1.0 / p.width, 1.0 / p.height);
            color += (sampleFrame(uv + px * 2.0) + sampleFrame(uv - px * 2.0)) * 0.08;
            color *= 0.70 + 0.30 * sin(in.position.y * 3.14159265 / max(p.dpr, 1.0));
            let lum = dot(color, vec3f(0.299, 0.587, 0.114));
            if (p.mono == 1u) { color = vec3f(lum * 0.95, lum * 0.78, lum * 0.42) + vec3f(0.8, 0.12, 0.02) * max(lum - 0.72, 0.0); }
            color *= 1.0 - dot(q, q) * 0.13;
            return vec4f(clamp(color, vec3f(0.0), vec3f(1.0)), 1.0);
          }
          if (p.mode == 2u) {
            let line = floor(uv.y * p.height / max(p.dpr, 1.0));
            let t = select(p.time, 0.0, p.reduced == 1u);
            uv.x += sin(line * 0.071 + t * 7.0) * 0.0015 + (rand(vec2f(line, floor(t * 18.0))) - 0.5) * 0.002;
            uv.x += smoothstep(0.82, 0.98, uv.y) * sin(line * 0.31 + t * 31.0) * 0.012;
            let lag = 2.6 / p.width;
            let base = sampleFrame(uv);
            var color = vec3f(sampleFrame(uv + vec2f(lag, 0.0)).r, base.g, sampleFrame(uv - vec2f(lag * 1.6, 0.0)).b);
            let band = exp(-pow((uv.y - fract(t * 0.073)) * 24.0, 2.0));
            color += (rand(in.position.xy + vec2f(t * 73.0)) - 0.5) * (0.035 + band * 0.16);
            if (p.mono == 1u) { let lum = dot(color, vec3f(0.299, 0.587, 0.114)); color = vec3f(lum) + vec3f(0.08, 0.35, 0.46) * band * (rand(vec2f(line, t)) - 0.5); }
            return vec4f(clamp(color, vec3f(0.0), vec3f(1.0)), 1.0);
          }
          let dried = sampleFrame(uv);
          let density = 1.0 - dot(dried, vec3f(0.299, 0.587, 0.114));
          let hue = dried - vec3f(dot(dried, vec3f(0.299, 0.587, 0.114)));
          let grain = (rand(in.position.xy * 0.37) - 0.5) * 0.035;
          let paper = select(vec3f(0.91, 0.87, 0.76), vec3f(0.13, 0.115, 0.09), p.light == 0u) + grain;
          let darkInk = clamp(vec3f(0.11, 0.085, 0.06) + hue * 1.5, vec3f(0.01), vec3f(0.62));
          let lightInk = clamp(vec3f(0.84, 0.80, 0.70) + hue * 4.5, vec3f(0.16), vec3f(1.0));
          let ink = select(darkInk, lightInk, p.light == 0u);
          return vec4f(mix(paper, ink, clamp(density * 1.45, 0.0, 1.0)), 1.0);
        }
      ` });
      presentPipeline = device.createRenderPipeline({
        layout: "auto", vertex: { module: presentModule, entryPoint: "vs" },
        fragment: { module: presentModule, entryPoint: "fs", targets: [{ format }] }, primitive: { topology: "triangle-list" },
      });

      const inkModule = device.createShaderModule({ code: `
        @group(0) @binding(0) var inkSampler: sampler;
        @group(0) @binding(1) var currentTexture: texture_2d<f32>;
        @group(0) @binding(2) var freshTexture: texture_2d<f32>;
        ${fullscreen}
        fn density(texture: texture_2d<f32>, uv: vec2f) -> f32 {
          let c = textureSampleLevel(texture, inkSampler, clamp(uv, vec2f(0.0), vec2f(1.0)), 0.0).rgb;
          return 1.0 - dot(c, vec3f(0.299, 0.587, 0.114));
        }
        fn sampleInk(texture: texture_2d<f32>, uv: vec2f) -> vec3f {
          return textureSampleLevel(texture, inkSampler, clamp(uv, vec2f(0.0), vec2f(1.0)), 0.0).rgb;
        }
        @fragment fn fs(in: Out) -> @location(0) vec4f {
          let dimensions = vec2f(textureDimensions(freshTexture));
          let px = 1.0 / dimensions;
          let fresh = density(freshTexture, in.uv);
          let neighbourInk = min(min(sampleInk(freshTexture, in.uv + vec2f(px.x, 0.0)), sampleInk(freshTexture, in.uv - vec2f(px.x, 0.0))), min(sampleInk(freshTexture, in.uv + vec2f(0.0, px.y)), sampleInk(freshTexture, in.uv - vec2f(0.0, px.y))));
          let neighbours = 1.0 - dot(neighbourInk, vec3f(0.299, 0.587, 0.114));
          let edge = max(neighbours - fresh, 0.0) * 0.34;
          let current = min(sampleInk(currentTexture, in.uv), sampleInk(freshTexture, in.uv));
          let spread = mix(vec3f(1.0), neighbourInk, edge);
          return vec4f(min(current, spread), 1.0);
        }
      ` });
      inkPipeline = device.createRenderPipeline({
        layout: "auto", vertex: { module: inkModule, entryPoint: "vs" },
        fragment: { module: inkModule, entryPoint: "fs", targets: [{ format: "rgba8unorm" }] }, primitive: { topology: "triangle-list" },
      });
      device.lost.then(() => {
        device = context = sampler = atlasSampler = scenePipeline = presentPipeline = inkPipeline = starting = undefined;
        sceneTexture = atlasTexture = inkTexture = freshTexture = undefined;
        cachedFrame = undefined;
        if (latest) paint(latest, ++frameId);
      });
      return true;
    })();
    return starting;
  }

  function rgba(value, alpha = value[3] / 255) {
    return [value[0] / 255, value[1] / 255, value[2] / 255, alpha];
  }

  function pushVertex(vertices, layout, x, y, color, u = 0, v = 0, glyph = 0) {
    vertices.push(x / layout.width * 2 - 1, 1 - y / layout.height * 2, ...color, u, v, glyph);
  }

  function pushQuad(vertices, layout, x, y, width, height, color, uv, glyph = 0) {
    const [u0, v0, u1, v1] = uv || [0, 0, 0, 0];
    const p = (px, py, u, v) => pushVertex(vertices, layout, px, py, color, u, v, glyph);
    p(x, y, u0, v0); p(x + width, y, u1, v0); p(x, y + height, u0, v1);
    p(x, y + height, u0, v1); p(x + width, y, u1, v0); p(x + width, y + height, u1, v1);
  }

  function pushGlyph(vertices, layout, glyph, x, y, width, height, color, bold) {
    const source = makeAtlas(layout.dpr);
    const slot = source.slots.get(`${bold ? 1 : 0}:${glyph}`);
    if (!slot) return;
    const uv = [slot[0] / source.canvas.width, slot[1] / source.canvas.height, (slot[0] + slot[2]) / source.canvas.width, (slot[1] + slot[3]) / source.canvas.height];
    pushQuad(vertices, layout, x, y, width, height, color, uv, 1);
  }

  function buildGeometry(frame, layout, packageName, now) {
    const surface = frame.fallback;
    const ink = packageName === "ink";
    const page = ink ? [255, 255, 255, 255] : surface.cells[0]?.background || [8, 9, 11, 255];
    const vertices = [];
    const freshVertices = [];
    const active = new Map();
    if (ink) {
      for (const state of glyphStates.values()) state.seen = false;
      for (const cell of surface.cells) {
        if (!cell.detail) continue;
        const detail = frame.details[cell.detail - 1];
        if (detail) active.set(`${detail.class}:${detail.id}`, detail);
      }
      for (const key of active.keys()) {
        let state = glyphStates.get(key);
        if (!state) {
          state = { first: now, present: false, seen: true, last: now };
          glyphStates.set(key, state);
        }
        if (!state.present) state.first = now;
        state.present = true;
        state.seen = true;
        state.last = now;
      }
      for (const [key, state] of glyphStates) {
        if (!state.seen) state.present = false;
        if (!state.present && now - state.last > 30000) glyphStates.delete(key);
      }
      metrics.activeInkDetails = active.size;
      metrics.wetInkDetails = [...active.keys()].filter((key) => now - glyphStates.get(key).first < 60).length;
    }
    surface.cells.forEach((cell, index) => {
      const x = index % surface.cols;
      const y = Math.floor(index / surface.cols);
      const left = x * layout.cellWidth;
      const top = y * layout.cellHeight;
      if (!ink && cell.background.some((value, i) => i < 3 && value !== page[i])) pushQuad(vertices, layout, left, top, layout.cellWidth, layout.cellHeight, rgba(cell.background));
      if (cell.glyph === " ") return;
      if (!ink) {
        pushGlyph(vertices, layout, cell.glyph, left, top, layout.cellWidth, layout.cellHeight, rgba(cell.foreground), cell.bold);
        return;
      }
      const detail = cell.detail ? frame.details[cell.detail - 1] : undefined;
      const key = detail ? `${detail.class}:${detail.id}` : "";
      const state = key ? glyphStates.get(key) : undefined;
      const seed = index * 31 + cell.glyph.codePointAt(0);
      const impact = detail ? 0.64 + hash(seed) * 0.32 : 1;
      const ribbon = detail ? 0.78 + hash(x * 0.73 + y * 0.19) * 0.22 : 1;
      const progress = !detail || frame.variant.reduced_motion ? 1 : clamp((now - state.first) / 60, 0, 1);
      const dx = detail ? (hash(seed + 1) - 0.5) * 1.25 : 0;
      const dy = detail ? (hash(seed + 2) - 0.5) * 1.0 : 0;
      const chroma = Math.max(...cell.foreground.slice(0, 3)) - Math.min(...cell.foreground.slice(0, 3));
      const source = frame.variant.color === "color" && chroma > 24
        ? [6 + cell.foreground[0] * 0.35, 6 + cell.foreground[1] * 0.35, 6 + cell.foreground[2] * 0.35, 255]
        : [35, 28, 21, 255];
      const color = rgba(source, impact * ribbon * progress);
      pushGlyph(vertices, layout, cell.glyph, left + dx, top + dy, layout.cellWidth, layout.cellHeight, color, cell.bold);
      if (cell.bold || impact > 0.88) pushGlyph(vertices, layout, cell.glyph, left + dx + 0.35, top + dy + 0.18, layout.cellWidth, layout.cellHeight, [...color.slice(0, 3), color[3] * 0.32], cell.bold);
      if (detail && !frame.variant.reduced_motion && progress < 1) {
        const wet = rgba([0, 0, 0, 255], 1 - progress);
        pushGlyph(freshVertices, layout, cell.glyph, left + dx, top + dy, layout.cellWidth, layout.cellHeight, wet, cell.bold);
      }
    });
    if (!ink) {
      glyphStates.clear();
      metrics.activeInkDetails = 0;
      metrics.wetInkDetails = 0;
    }
    const sceneVertexCount = vertices.length / 9;
    vertices.push(...freshVertices);
    return {
      vertices: new Float32Array(vertices),
      sceneVertexCount,
      freshVertexCount: freshVertices.length / 9,
      clear: rgba(page),
    };
  }

  function ensureResources(width, height, bytes) {
    if (!sceneTexture || sceneSize[0] !== width || sceneSize[1] !== height) {
      sceneTexture?.destroy();
      sceneTexture = device.createTexture({ size: [width, height], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT });
      sceneSize = [width, height];
    }
    if (!sceneBuffer || sceneCapacity < bytes) {
      sceneBuffer?.destroy();
      sceneCapacity = 36;
      while (sceneCapacity < bytes) sceneCapacity *= 2;
      sceneBuffer = device.createBuffer({ size: sceneCapacity, usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST });
    }
  }

  function ensureInk(width, height) {
    if (inkTexture && freshTexture && inkSize[0] === width && inkSize[1] === height) return;
    inkTexture?.destroy();
    freshTexture?.destroy();
    inkTexture = device.createTexture({ size: [width, height], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT });
    freshTexture = device.createTexture({ size: [width, height], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT });
    inkSize = [width, height];
  }

  function renderPackage(frame, layout, packageName, now) {
    const canvas = document.getElementById("stage");
    const dpr = renderDpr(packageName);
    layout.dpr = dpr;
    sizeCanvas(canvas, layout.width, layout.height, dpr);
    ensureAtlas(dpr);
    const layoutKey = `${canvas.width}x${canvas.height}`;
    const ink = packageName === "ink";
    const rebuild = cachedFrame !== frame || cachedPackage !== packageName || cachedLayout !== layoutKey || ink;
    const geometry = rebuild ? buildGeometry(frame, layout, packageName, now) : cachedGeometry;
    cachedFrame = frame;
    cachedPackage = packageName;
    cachedLayout = layoutKey;
    cachedGeometry = geometry;
    ensureResources(canvas.width, canvas.height, geometry.vertices.byteLength);
    if (rebuild && geometry.vertices.byteLength) device.queue.writeBuffer(sceneBuffer, 0, geometry.vertices);
    metrics.sceneVertices = geometry.sceneVertexCount;
    metrics.sceneVertexBytes = geometry.vertices.byteLength;
    const encoder = device.createCommandEncoder();
    if (rebuild) {
      const pass = encoder.beginRenderPass({ colorAttachments: [{ view: sceneTexture.createView(), clearValue: { r: geometry.clear[0], g: geometry.clear[1], b: geometry.clear[2], a: 1 }, loadOp: "clear", storeOp: "store" }] });
      const group = device.createBindGroup({ layout: scenePipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: atlasSampler }, { binding: 1, resource: atlasTexture.createView() }] });
      pass.setPipeline(scenePipeline); pass.setBindGroup(0, group);
      if (geometry.sceneVertexCount) { pass.setVertexBuffer(0, sceneBuffer); pass.draw(geometry.sceneVertexCount); }
      pass.end();
    }
    let presented = sceneTexture;
    if (ink) {
      ensureInk(canvas.width, canvas.height);
      const freshPass = encoder.beginRenderPass({ colorAttachments: [{ view: freshTexture.createView(), clearValue: { r: 1, g: 1, b: 1, a: 1 }, loadOp: "clear", storeOp: "store" }] });
      const freshGroup = device.createBindGroup({ layout: scenePipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: atlasSampler }, { binding: 1, resource: atlasTexture.createView() }] });
      freshPass.setPipeline(scenePipeline); freshPass.setBindGroup(0, freshGroup);
      if (geometry.freshVertexCount) { freshPass.setVertexBuffer(0, sceneBuffer); freshPass.draw(geometry.freshVertexCount, 1, geometry.sceneVertexCount); }
      freshPass.end();
      const pass = encoder.beginRenderPass({ colorAttachments: [{ view: inkTexture.createView(), clearValue: { r: 1, g: 1, b: 1, a: 1 }, loadOp: "clear", storeOp: "store" }] });
      const group = device.createBindGroup({ layout: inkPipeline.getBindGroupLayout(0), entries: [
        { binding: 0, resource: sampler }, { binding: 1, resource: sceneTexture.createView() }, { binding: 2, resource: freshTexture.createView() },
      ] });
      pass.setPipeline(inkPipeline); pass.setBindGroup(0, group); pass.draw(3); pass.end();
      presented = inkTexture;
    }
    const modes = { canonical: 0, crt: 1, vhs: 2, ink: 3 };
    const data = new ArrayBuffer(32);
    const ints = new Uint32Array(data);
    const floats = new Float32Array(data);
    ints[0] = modes[packageName]; ints[1] = frame.variant.color === "monochrome" ? 1 : 0;
    ints[2] = frame.fallback.cells[0]?.background[0] > 80 ? 1 : 0; ints[3] = frame.variant.reduced_motion ? 1 : 0;
    floats[4] = now / 1000; floats[5] = canvas.width; floats[6] = canvas.height; floats[7] = dpr;
    device.queue.writeBuffer(frameUniform, 0, data);
    const pass = encoder.beginRenderPass({ colorAttachments: [{ view: context.getCurrentTexture().createView(), clearValue: { r: 0, g: 0, b: 0, a: 1 }, loadOp: "clear", storeOp: "store" }] });
    const group = device.createBindGroup({ layout: presentPipeline.getBindGroupLayout(0), entries: [
      { binding: 0, resource: sampler }, { binding: 1, resource: presented.createView() }, { binding: 2, resource: { buffer: frameUniform } },
    ] });
    pass.setPipeline(presentPipeline); pass.setBindGroup(0, group); pass.draw(3); pass.end();
    device.queue.submit([encoder.finish()]);
  }

  function animationNeeded(frame, now) {
    if (frame.variant.reduced_motion) return false;
    if (frame.variant.package === "vhs") return true;
    if (frame.variant.package === "ink") {
      for (const state of glyphStates.values()) if (state.present && now - state.first < 60) return true;
    }
    return false;
  }

  async function paint(frame, token) {
    const start = performance.now();
    const gpu = await Promise.race([
      startGpu().catch(() => false),
      new Promise((resolve) => setTimeout(() => resolve(false), 2500)),
    ]);
    await document.fonts.ready;
    if (token !== frameId) { metrics.staleFramesDiscarded++; return; }
    const requested = frame.variant.package;
    metrics.requestedPackage = requested;
    if (!gpu) {
      showSemanticFallback(!isSecureContext ? "webgpu-requires-https" : "webgpu-unavailable");
      return;
    }
    const surface = frame.fallback;
    const layout = {
      width: surface.cols * CELL_WIDTH,
      height: surface.rows * CELL_HEIGHT,
      cellWidth: CELL_WIDTH,
      cellHeight: CELL_HEIGHT,
    };
    renderPackage(frame, layout, requested, performance.now());
    metrics.activePackage = requested;
    metrics.fallbackReason = "";
    document.documentElement.dataset.renderer = `webgpu-${requested}`;
    document.documentElement.dataset.renderPackage = requested;
    paintTotal += performance.now() - start;
    metrics.paintedFrames++;
    metrics.averagePaintMs = paintTotal / metrics.paintedFrames;
    const now = performance.now();
    if (animationNeeded(frame, now) && !animationTimer) {
      animationTimer = setTimeout(() => requestAnimationFrame(() => {
        animationTimer = 0;
        if (latest === frame) paint(frame, token);
      }), requested === "vhs" ? 120 : 32);
    }
  }

  const SWEEP_FROM = Math.PI * 0.75;
  const SWEEP_TO = Math.PI * 2.25;
  let controlState = { theme: "dark", packageIndex: 0 };
  let controlsInstalled = false;

  function controlContext(canvas, width, height) {
    const dpr = devicePixelRatio || 1;
    const pixelWidth = Math.round(width * dpr);
    const pixelHeight = Math.round(height * dpr);
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth;
      canvas.height = pixelHeight;
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
    }
    const context = canvas.getContext("2d");
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, width, height);
    return context;
  }

  function drawPower(context, ink) {
    context.save();
    context.translate(11, 11);
    context.strokeStyle = ink;
    context.lineWidth = 1.6;
    context.lineCap = "round";
    const radius = 7;
    const gap = 0.42;
    context.beginPath();
    context.arc(0, 0, radius, -Math.PI / 2 + gap, -Math.PI / 2 - gap + Math.PI * 2);
    context.stroke();
    context.beginPath();
    context.moveTo(0, -(radius + 2.4));
    context.lineTo(0, -radius * 0.2);
    context.stroke();
    context.restore();
  }

  function drawKnob(context, at, ink) {
    const radius = 11;
    const angle = (index) => SWEEP_FROM + (SWEEP_TO - SWEEP_FROM) * index / 3;
    context.save();
    context.translate(19, 19);
    context.lineCap = "butt";
    for (let index = 0; index < 4; index++) {
      const selected = index === at;
      const a = angle(index);
      const inner = radius + 3;
      const outer = inner + (selected ? 4.5 : 2.5);
      context.strokeStyle = selected ? "#ffb040" : ink;
      context.lineWidth = selected ? 1.6 : 1;
      context.beginPath();
      context.moveTo(Math.cos(a) * inner, Math.sin(a) * inner);
      context.lineTo(Math.cos(a) * outer, Math.sin(a) * outer);
      context.stroke();
    }
    context.strokeStyle = ink;
    context.lineWidth = 1.2;
    context.beginPath();
    context.arc(0, 0, radius, 0, Math.PI * 2);
    context.stroke();
    const a = angle(at);
    context.strokeStyle = "#ffb040";
    context.lineWidth = 1.8;
    context.lineCap = "round";
    context.beginPath();
    context.moveTo(Math.cos(a) * radius * 0.18, Math.sin(a) * radius * 0.18);
    context.lineTo(Math.cos(a) * radius * 0.82, Math.sin(a) * radius * 0.82);
    context.stroke();
    context.restore();
  }

  function paintControls() {
    const power = document.getElementById("theme-toggle");
    const dial = document.getElementById("package-toggle");
    const powerFace = document.getElementById("power-face");
    const knobFace = document.getElementById("knob-face");
    if (!power || !dial || !powerFace || !knobFace) return;
    const hot = power.matches(":hover, :focus-visible") || dial.matches(":hover, :focus-visible");
    const ink = hot ? "#c4c8ce" : controlState.theme === "light" ? "#625e55" : "#606670";
    drawPower(controlContext(powerFace, 22, 22), controlState.theme === "light" ? "#ffb040" : ink);
    drawKnob(controlContext(knobFace, 38, 38), controlState.packageIndex, ink);
    if (!controlsInstalled) {
      controlsInstalled = true;
      for (const control of [power, dial]) {
        for (const event of ["pointerenter", "pointerleave", "focus", "blur"]) {
          control.addEventListener(event, paintControls);
        }
      }
      addEventListener("resize", paintControls);
    }
  }

  window.portfolioV2Controls = {
    paint(theme, packageIndex) {
      controlState = { theme, packageIndex };
      paintControls();
    },
  };

  window.portfolioV2 = {
    render(serialized) {
      metrics.renderCalls++;
      if (animationTimer) {
        clearTimeout(animationTimer);
        animationTimer = 0;
      }
      latest = JSON.parse(serialized);
      paint(latest, ++frameId).catch((error) => {
        console.warn("renderer failed", error);
        showSemanticFallback("renderer-failure");
      });
    },
  };
})();
