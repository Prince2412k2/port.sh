(() => {
  "use strict";
  const STRIDE = 32,
    WET_SECONDS = 0.65;
  const metrics = (window.portfolioV2RenderMetrics = {
    renderCalls: 0,
    paintedFrames: 0,
    uploadedBytes: 0,
    activePackage: "canonical",
    averagePaintMs: 0,
    wetInkDetails: 0,
  });
  let device,
    context,
    atlas,
    atlasView,
    instances,
    uniform,
    scenePipeline,
    presentPipeline;
  let sceneGroup,
    presentGroup,
    target,
    targetView,
    sampler,
    initPromise,
    latest,
    uploaded;
  let capacity = 0,
    width = 0,
    height = 0,
    buffer,
    floats,
    timer = 0,
    paintTotal = 0,
    wetUntil = 0,
    generation = 0;
  const slots = new Map(),
    identities = new Map();
  let previous = [],
    layoutKey = "",
    gpuRecoveries = 0,
    glRenderer;
  const resourceBase = new URL(".", document.currentScript.src);

  const sceneShader = `
    struct Params { viewport: vec2f, grid: vec2f, time: f32, mode: u32, mono: u32, reduced: u32 };
    struct Cell { uv: vec4f, fg: vec4f, bg: vec4f, historyUV: vec4f, historyFG: vec4f, life: vec4f, spare: vec4f, spare2: vec4f };
    @group(0) @binding(0) var<uniform> p: Params;
    @group(0) @binding(1) var<storage, read> cells: array<Cell>;
    @group(0) @binding(2) var font: texture_2d<f32>;
    @group(0) @binding(3) var fontSampler: sampler;
    struct Out { @builtin(position) position: vec4f, @location(0) local: vec2f, @location(1) @interpolate(flat) index: u32 };
    @vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) index: u32) -> Out {
      let corners = array<vec2f,6>(vec2f(0,0),vec2f(1,0),vec2f(0,1),vec2f(0,1),vec2f(1,0),vec2f(1,1));
      let local = corners[vertex];
      let cell = vec2f(f32(index % u32(p.grid.x)), f32(index / u32(p.grid.x)));
      var out: Out;
      out.position = vec4f((cell + local) / p.grid * vec2f(2,-2) + vec2f(-1,1),0,1);
      out.local = local; out.index = index; return out;
    }
    fn noise(v: vec2f) -> f32 { return fract(sin(dot(v,vec2f(127.1,311.7))) * 43758.5453); }
    fn cover(uv: vec4f, local: vec2f) -> f32 {
      if (uv.z == 0.0 || any(local < vec2f(0)) || any(local > vec2f(1))) { return 0.0; }
      return textureSampleLevel(font,fontSampler,uv.xy + local * uv.zw,0).a;
    }
    @fragment fn fs(in: Out) -> @location(0) vec4f {
      let cell = cells[in.index];
      var fg = cell.fg.rgb; var bg = cell.bg.rgb;
      if (p.mono == 1u) { fg = vec3f(dot(fg,vec3f(.299,.587,.114))); bg = vec3f(dot(bg,vec3f(.299,.587,.114))); }
      if(p.mode==1u){
        let px=vec2f(1.0/8.0,1.0/17.0);
        let core=cover(cell.uv,in.local);
        let beam=max(max(cover(cell.uv,in.local+vec2f(px.x*.35,0)),cover(cell.uv,in.local-vec2f(px.x*.35,0))),max(cover(cell.uv,in.local+vec2f(0,px.y*.25)),cover(cell.uv,in.local-vec2f(0,px.y*.25))));
        let age=max(p.time-cell.life.y,0.0);
        let residue=select(cover(cell.historyUV,in.local)*exp(-age/.12),0.0,p.reduced==1u);
        let emission=fg*(core*.94+max(beam-core,0.0)*.22)+cell.historyFG.rgb*residue*.28;
        return vec4f(bg*(1.0-core)+emission,1);
      }
      if(p.mode==2u){
        let lag=vec2f(.055,0);
        let coverage=vec3f(cover(cell.uv,in.local+lag),cover(cell.uv,in.local),cover(cell.uv,in.local-lag));
        let lineNoise=(noise(vec2f(floor(in.position.y),floor(p.time*24.0)))-.5)*.015;
        return vec4f(mix(bg,fg,coverage)+lineNoise*coverage,1);
      }
      let mask=cover(cell.uv,in.local);
      return vec4f(mix(bg,fg,mask),1);
    }
  `;
  const presentShader = `
    struct Params { viewport: vec2f, grid: vec2f, time: f32, mode: u32, mono: u32, reduced: u32 };
    @group(0) @binding(0) var<uniform> p: Params;
    @group(0) @binding(1) var image: texture_2d<f32>;
    @group(0) @binding(2) var imageSampler: sampler;
    struct Out { @builtin(position) position: vec4f, @location(0) uv: vec2f };
    @vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
      let vertices = array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));
      var out: Out; out.position = vec4f(vertices[i],0,1); out.uv = vertices[i]*vec2f(.5,-.5)+.5; return out;
    }
    fn sampleAt(uv: vec2f) -> vec3f { return textureSampleLevel(image,imageSampler,clamp(uv,vec2f(0),vec2f(1)),0).rgb; }
    fn noise(v: vec2f) -> f32 { return fract(sin(dot(v,vec2f(12.9898,78.233)))*43758.5453); }
    @fragment fn fs(in: Out) -> @location(0) vec4f {
      var uv = in.uv;
      if (p.mode == 1u) {
        let q = uv*2-1;
        let pixel = 1.0/p.viewport;
        let base = sampleAt(uv);
        let halo = (sampleAt(uv+pixel*vec2f(2,0))+sampleAt(uv-pixel*vec2f(2,0))+sampleAt(uv+pixel*vec2f(0,2))+sampleAt(uv-pixel*vec2f(0,2)))*.06;
        let scan = .88+.12*sin(in.position.y*3.14159265);
        let mask = select(vec3f(.91,1,.91),vec3f(1,.92,.92),u32(in.position.x)%3u==0u);
        var color = (base+halo)*scan*mask*(1.0-dot(q,q)*.06);
        if (p.mono==1u) { color = dot(color,vec3f(.299,.587,.114))*vec3f(1,.83,.43); }
        return vec4f(color,1);
      }
      if (p.mode == 2u) {
        let t = select(p.time,0.0,p.reduced==1u);
        let line = floor(in.position.y);
        let band = exp(-pow((uv.y-fract(t*.073))*40,2));
        uv.x += sin(line*.071+t*7)*.0004+band*sin(line*.31+t*31)*.003;
        let lag = 1.5/p.viewport.x;
        let base = sampleAt(uv);
        var color = vec3f(sampleAt(uv+vec2f(lag,0)).r,base.g,sampleAt(uv-vec2f(lag,0)).b);
        color += (noise(vec2f(line,floor(t*24)))-.5)*(.012+band*.07);
        return vec4f(clamp(color,vec3f(0),vec3f(1)),1);
      }
      return textureSampleLevel(image,imageSampler,uv,0);
    }
  `;

  async function initialize() {
    if (initPromise) return initPromise;
    initPromise = (async () => {
      const adapter = await navigator.gpu?.requestAdapter().catch(() => null);
      // Build-time coverage is identical across browser engines and devices.
      const [manifestResponse, imageResponse] = await Promise.all([
        fetch(new URL("glyph-atlas.json", resourceBase)),
        fetch(new URL("glyph-atlas.png", resourceBase)),
      ]);
      if (!manifestResponse.ok || !imageResponse.ok)
        throw new Error("Glyph atlas unavailable");
      const manifest = await manifestResponse.json();
      if (
        manifest.abi !== 1 ||
        manifest.width > 4096 ||
        manifest.height > 4096 ||
        manifest.slots.length > 16000
      )
        throw new Error("Invalid glyph atlas");
      const canvas = await createImageBitmap(await imageResponse.blob(), {
        premultiplyAlpha: "none",
        colorSpaceConversion: "none",
      });
      if (canvas.width !== manifest.width || canvas.height !== manifest.height)
        throw new Error("Glyph atlas dimensions mismatch");
      slots.clear();
      for (const [bold, code, ...uv] of manifest.slots)
        slots.set(`${bold}:${String.fromCodePoint(code)}`, uv);
      if (!adapter) {
        glRenderer = window.createPortfolioWebGL(
          document.getElementById("stage"),
          canvas,
          slots,
        );
        return;
      }
      device = await adapter.requestDevice();
      context = document.getElementById("stage").getContext("webgpu");
      const format = navigator.gpu.getPreferredCanvasFormat();
      context.configure({ device, format, alphaMode: "premultiplied" });
      atlas = device.createTexture({
        size: [canvas.width, canvas.height],
        format: "rgba8unorm",
        usage:
          GPUTextureUsage.TEXTURE_BINDING |
          GPUTextureUsage.COPY_DST |
          GPUTextureUsage.RENDER_ATTACHMENT,
      });
      device.queue.copyExternalImageToTexture(
        { source: canvas },
        { texture: atlas },
        [canvas.width, canvas.height],
      );
      atlasView = atlas.createView();
      sampler = device.createSampler({
        magFilter: "linear",
        minFilter: "linear",
      });
      uniform = device.createBuffer({
        size: 32,
        usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
      });
      const scene = device.createShaderModule({ code: sceneShader }),
        present = device.createShaderModule({ code: presentShader });
      for (const module of [scene, present]) {
        const info = await module.getCompilationInfo();
        const errors = info.messages.filter((m) => m.type === "error");
        if (errors.length)
          throw new Error(errors.map((e) => e.message).join("; "));
      }
      scenePipeline = await device.createRenderPipelineAsync({
        layout: "auto",
        vertex: { module: scene, entryPoint: "vs" },
        fragment: {
          module: scene,
          entryPoint: "fs",
          targets: [{ format: "rgba8unorm" }],
        },
      });
      presentPipeline = await device.createRenderPipelineAsync({
        layout: "auto",
        vertex: { module: present, entryPoint: "vs" },
        fragment: { module: present, entryPoint: "fs", targets: [{ format }] },
      });
      device.addEventListener("uncapturederror", (event) =>
        fallback(event.error.message),
      );
      device.lost.then(() => {
        device = undefined;
        initPromise = undefined;
        capacity = 0;
        target = undefined;
        uploaded = undefined;
        previous = [];
        if (++gpuRecoveries <= 2 && latest) requestPaint();
        else fallback("GPU device lost");
      });
    })();
    return initPromise;
  }

  function resources(frame) {
    const canvas = document.getElementById("stage");
    const dpr = Math.min(devicePixelRatio || 1, 2),
      w = Math.ceil(innerWidth * dpr),
      h = Math.ceil(innerHeight * dpr);
    canvas.style.width = `${innerWidth}px`;
    canvas.style.height = `${innerHeight}px`;
    if (!target || w !== width || h !== height) {
      width = w;
      height = h;
      canvas.width = w;
      canvas.height = h;
      target?.destroy();
      target = device.createTexture({
        size: [w, h],
        format: "rgba8unorm",
        usage:
          GPUTextureUsage.RENDER_ATTACHMENT |
          GPUTextureUsage.TEXTURE_BINDING |
          GPUTextureUsage.COPY_SRC,
        // Public diagnostics can verify actual output rather than submissions.
      });
      targetView = target.createView();
      presentGroup = device.createBindGroup({
        layout: presentPipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: { buffer: uniform } },
          { binding: 1, resource: targetView },
          { binding: 2, resource: sampler },
        ],
      });
    }
    const count = frame.fallback.cells.length;
    if (capacity < count) {
      instances?.destroy();
      capacity = Math.max(count, 10800);
      buffer = new ArrayBuffer(capacity * STRIDE * 4);
      floats = new Float32Array(buffer);
      instances = device.createBuffer({
        size: buffer.byteLength,
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
      });
      sceneGroup = device.createBindGroup({
        layout: scenePipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: { buffer: uniform } },
          { binding: 1, resource: { buffer: instances } },
          { binding: 2, resource: atlasView },
          { binding: 3, resource: sampler },
        ],
      });
      previous = [];
      uploaded = undefined;
    }
  }

  function upload(frame, now) {
    const key = `${frame.fallback.cols}/${frame.fallback.rows}/${frame.variant.package}/${frame.variant.color}/${frame.fallback.cells[0]?.background.join()}`;
    const reset = key !== layoutKey;
    if (reset) {
      floats.fill(0);
      previous = [];
      identities.clear();
      layoutKey = key;
    }
    if (uploaded === frame && !reset) return;
    const cells = frame.fallback.cells;
    let first = cells.length,
      last = 0;
    for (let i = 0; i < cells.length; i++) {
      const c = cells[i],
        old = previous[i],
        offset = i * STRIDE;
      if (old === c) continue;
      const changed =
        !old ||
        old.glyph !== c.glyph ||
        old.bold !== c.bold ||
        old.detail !== c.detail;
      const detail = c.detail ? frame.details[c.detail - 1] : null;
      const identity = detail
        ? `${detail.class}:${detail.id}:${c.glyph}`
        : `${i}:${c.glyph}`;
      if (changed) {
        floats.copyWithin(offset + 12, offset, offset + 4);
        floats.copyWithin(offset + 16, offset + 4, offset + 8);
        const uv =
          slots.get(`${c.bold ? 1 : 0}:${c.glyph}`) || slots.get("0:?");
        floats.set(c.glyph === " " ? [0, 0, 0, 0] : uv, offset);
        let birth = identities.get(identity);
        if (birth === undefined) {
          birth = now;
          identities.set(identity, birth);
        }
        floats[offset + 20] = birth;
        floats[offset + 21] = now;
        floats[offset + 22] = c.glyph.codePointAt(0) + i * 0.17;
        wetUntil = Math.max(wetUntil, now + WET_SECONDS);
      }
      if (
        changed ||
        !old ||
        old.foreground.some((v, j) => v !== c.foreground[j]) ||
        old.background.some((v, j) => v !== c.background[j])
      ) {
        floats.set(
          c.foreground.map((v) => v / 255),
          offset + 4,
        );
        floats.set(
          c.background.map((v) => v / 255),
          offset + 8,
        );
        first = Math.min(first, i);
        last = i + 1;
      }
    }
    if (first < last) {
      const start = first * STRIDE * 4,
        size = (last - first) * STRIDE * 4;
      device.queue.writeBuffer(instances, start, buffer, start, size);
      metrics.uploadedBytes += size;
    }
    previous = cells;
    uploaded = frame;
    if (identities.size > 32768) {
      for (const [key, birth] of identities) {
        if (now - birth > 30) identities.delete(key);
      }
    }
  }
  async function paint() {
    timer = 0;
    if (document.hidden || !latest) return;
    const token = generation;
    try {
      await initialize();
      if (token !== generation) return;
      const started = performance.now(),
        frame = latest,
        now = started / 1000;
      if (glRenderer) {
        const animating = glRenderer.draw(frame, now);
        metrics.activePackage = frame.variant.package;
        metrics.paintedFrames++;
        paintTotal += performance.now() - started;
        metrics.averagePaintMs = paintTotal / metrics.paintedFrames;
        document.documentElement.dataset.renderer = `webgl2-${frame.variant.package}`;
        if (animating) requestPaint();
        return;
      }
      resources(frame);
      upload(frame, now);
      const data = new ArrayBuffer(32),
        f = new Float32Array(data),
        u = new Uint32Array(data);
      f[0] = width;
      f[1] = height;
      f[2] = frame.fallback.cols;
      f[3] = frame.fallback.rows;
      f[4] = now;
      u[5] = ["canonical", "crt", "vhs"].indexOf(
        frame.variant.package,
      );
      u[6] = frame.variant.color === "monochrome" ? 1 : 0;
      u[7] = frame.variant.reduced_motion ? 1 : 0;
      device.queue.writeBuffer(uniform, 0, data);
      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view: targetView,
            loadOp: "clear",
            storeOp: "store",
            clearValue: { r: 0, g: 0, b: 0, a: 1 },
          },
        ],
      });
      pass.setPipeline(scenePipeline);
      pass.setBindGroup(0, sceneGroup);
      pass.draw(6, frame.fallback.cells.length);
      pass.end();
      const present = encoder.beginRenderPass({
        colorAttachments: [
          {
            view: context.getCurrentTexture().createView(),
            loadOp: "clear",
            storeOp: "store",
            clearValue: { r: 0, g: 0, b: 0, a: 1 },
          },
        ],
      });
      present.setPipeline(presentPipeline);
      present.setBindGroup(0, presentGroup);
      present.draw(3);
      present.end();
      device.queue.submit([encoder.finish()]);
      metrics.activePackage = frame.variant.package;
      metrics.paintedFrames++;
      paintTotal += performance.now() - started;
      metrics.averagePaintMs = paintTotal / metrics.paintedFrames;
      document.documentElement.dataset.renderer = `webgpu-${frame.variant.package}`;
      metrics.wetInkDetails = now < wetUntil ? identities.size : 0;
      if (
        !frame.variant.reduced_motion &&
        (frame.variant.package === "vhs" ||
          (frame.variant.package === "crt" && now < wetUntil))
      )
        requestPaint();
    } catch (error) {
      fallback(error.message);
    }
  }
  function requestPaint() {
    if (!timer && !document.hidden) timer = requestAnimationFrame(paint);
  }
  function fallback(reason) {
    metrics.activePackage = "semantic";
    metrics.fallbackReason = reason;
    document.documentElement.dataset.renderer = "semantic-fallback";
    document.getElementById("semantic").classList.add("visible-fallback");
    const status = document.getElementById("status");
    status.textContent = `GPU view unavailable: ${reason}`;
    status.dataset.empty = "false";
    console.warn("V2 renderer:", reason);
  }
  window.portfolioV2 = {
    async inspect_present() {
      if (!device || !target) return null;
      const t = device.createTexture({
        size: [width, height],
        format: navigator.gpu.getPreferredCanvasFormat(),
        usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
      });
      const b = device.createBuffer({
        size: 256 * 17,
        usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
      });
      const e = device.createCommandEncoder();
      const p = e.beginRenderPass({
        colorAttachments: [
          {
            view: t.createView(),
            loadOp: "clear",
            storeOp: "store",
            clearValue: { r: 0, g: 0, b: 0, a: 1 },
          },
        ],
      });
      p.setPipeline(presentPipeline);
      p.setBindGroup(0, presentGroup);
      p.draw(3);
      p.end();
      e.copyTextureToBuffer(
        { texture: t, origin: [72, 0] },
        { buffer: b, bytesPerRow: 256 },
        [8, 17],
      );
      device.queue.submit([e.finish()]);
      await b.mapAsync(GPUMapMode.READ);
      const bytes = new Uint8Array(b.getMappedRange());
      const colors = new Set();
      for (let row = 0; row < 17; row++)
        for (let x = 0; x < 8; x++)
          colors.add(
            Array.from(
              bytes.slice(row * 256 + x * 4, row * 256 + x * 4 + 4),
            ).join(","),
          );
      b.unmap();
      b.destroy();
      t.destroy();
      return [...colors];
    },
    async inspect_pixels() {
      if (!device || !target) return null;
      const b = device.createBuffer({
        size: 256 * 17,
        usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
      });
      const e = device.createCommandEncoder();
      e.copyTextureToBuffer(
        { texture: target, origin: [72, 0] },
        { buffer: b, bytesPerRow: 256 },
        [8, 17],
      );
      device.queue.submit([e.finish()]);
      await b.mapAsync(GPUMapMode.READ);
      const bytes = new Uint8Array(b.getMappedRange());
      const colors = new Set();
      for (let row = 0; row < 17; row++)
        for (let x = 0; x < 8; x++)
          colors.add(
            Array.from(
              bytes.slice(row * 256 + x * 4, row * 256 + x * 4 + 4),
            ).join(","),
          );
      b.unmap();
      b.destroy();
      return [...colors];
    },
    render(frame) {
      latest = typeof frame === "string" ? JSON.parse(frame) : frame;
      generation++;
      metrics.renderCalls++;
      requestPaint();
    },
  };
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      cancelAnimationFrame(timer);
      timer = 0;
    } else {
      uploaded = undefined;
      requestPaint();
    }
  });
  document
    .getElementById("stage")
    .addEventListener("webglcontextlost", (event) => {
      event.preventDefault();
      cancelAnimationFrame(timer);
      timer = 0;
    });
  document
    .getElementById("stage")
    .addEventListener("webglcontextrestored", () => {
      glRenderer = undefined;
      initPromise = undefined;
      requestPaint();
    });

  let controlKey = "";
  window.portfolioV2Controls = {
    paint(theme, index) {
      const key = `${theme}/${index}/${devicePixelRatio}`;
      if (controlKey === key) return;
      controlKey = key;
      for (const [id, size] of [
        ["power-face", 22],
        ["knob-face", 38],
      ]) {
        const canvas = document.getElementById(id),
          dpr = devicePixelRatio || 1;
        canvas.width = size * dpr;
        canvas.height = size * dpr;
        canvas.style.width = `${size}px`;
        canvas.style.height = `${size}px`;
        const c = canvas.getContext("2d");
        c.scale(dpr, dpr);
        c.translate(size / 2, size / 2);
        c.strokeStyle = theme === "light" ? "#625e55" : "#606670";
        c.lineWidth = 1.4;
        if (id === "power-face") {
          c.beginPath();
          c.arc(0, 0, 7, -Math.PI / 2 + 0.42, Math.PI * 1.5 - 0.42);
          c.stroke();
          c.beginPath();
          c.moveTo(0, -9.4);
          c.lineTo(0, -1.4);
          c.stroke();
        } else {
          c.beginPath();
          c.arc(0, 0, 11, 0, Math.PI * 2);
          c.stroke();
           for (let i = 0; i < 3; i++) {
             const a = Math.PI * 0.75 + (Math.PI * 1.5 * i) / 2;
            c.beginPath();
            c.moveTo(Math.cos(a) * 14, Math.sin(a) * 14);
            c.lineTo(Math.cos(a) * 17, Math.sin(a) * 17);
            c.stroke();
          }
           const a = Math.PI * 0.75 + (Math.PI * 1.5 * index) / 2;
          c.strokeStyle = "#ffb040";
          c.lineWidth = 1.8;
          c.beginPath();
          c.moveTo(Math.cos(a) * 2, Math.sin(a) * 2);
          c.lineTo(Math.cos(a) * 9, Math.sin(a) * 9);
          c.stroke();
        }
      }
    },
  };
})();
