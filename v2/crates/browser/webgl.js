// WebGL2 cell-native fallback. Instanced glyphs use the same atlas and logical
// surface as WebGPU; there is no ANSI parser or CPU triangle expansion.
window.createPortfolioWebGL = (canvas, atlas, slots) => {
  const gl = canvas.getContext("webgl2", {
    alpha: true,
    antialias: false,
    preserveDrawingBuffer: false,
  });
  if (!gl) throw new Error("Neither WebGPU nor WebGL2 is available");
  const vertex = `#version 300 es
    precision highp float;
    layout(location=0) in vec4 cellUV; layout(location=1) in vec4 cellFG;
    layout(location=2) in vec4 cellBG; layout(location=3) in vec4 historyUV;
    layout(location=4) in vec4 life;
    uniform vec2 grid;
    out vec2 local; flat out vec4 uv; flat out vec4 fg; flat out vec4 bg;
    flat out vec4 oldUV; flat out vec4 state;
    void main(){
      vec2 corners[6]=vec2[6](vec2(0,0),vec2(1,0),vec2(0,1),vec2(0,1),vec2(1,0),vec2(1,1));
      local=corners[gl_VertexID];vec2 cell=vec2(float(gl_InstanceID%int(grid.x)),float(gl_InstanceID/int(grid.x)));
      gl_Position=vec4((cell+local)/grid*vec2(2,-2)+vec2(-1,1),0,1);
      uv=cellUV;fg=cellFG;bg=cellBG;oldUV=historyUV;state=life;
    }`;
  const fragment = `#version 300 es
    precision highp float;
    in vec2 local;flat in vec4 uv;flat in vec4 fg;flat in vec4 bg;flat in vec4 oldUV;flat in vec4 state;
    uniform sampler2D atlas;uniform float time;uniform int mode;uniform bool mono;uniform bool reduced;
    out vec4 color;
    float rand(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
    float coverage(vec4 slot,vec2 p){if(slot.z==0.||any(lessThan(p,vec2(0)))||any(greaterThan(p,vec2(1))))return 0.;return texture(atlas,slot.xy+p*slot.zw).a;}
    void main(){
      vec3 ink=fg.rgb,paper=bg.rgb;vec2 p=local;
      if(mono){ink=vec3(dot(ink,vec3(.299,.587,.114)));paper=vec3(dot(paper,vec3(.299,.587,.114)));}
      float t=reduced?0.:time;
      if(mode==2)p.x+=sin(floor(gl_FragCoord.y)*.071+t*7.)*.022;
      float mask=coverage(uv,p);
      if(mode==1){
        vec2 px=vec2(1./8.,1./17.);float beam=max(max(coverage(uv,p+vec2(px.x*.35,0)),coverage(uv,p-vec2(px.x*.35,0))),max(coverage(uv,p+vec2(0,px.y*.25)),coverage(uv,p-vec2(0,px.y*.25))));
        float residue=reduced?0.:coverage(oldUV,p)*exp(-max(time-state.y,0.)/.12)*.28;
        mask=clamp(mask*.94+max(beam-mask,0.)*.22+residue,0.,1.);
      }
      vec3 rgb=mix(paper,ink,mask);
      if(mode==1){
        float halo=max(coverage(uv,p+vec2(.08,0)),coverage(uv,p-vec2(.08,0)))*.1;
        rgb+=ink*halo;rgb*=.88+.12*sin(gl_FragCoord.y*3.14159265);
        if(mono)rgb=dot(rgb,vec3(.299,.587,.114))*vec3(1,.83,.43);
      }
      if(mode==2)rgb+=(rand(vec2(floor(gl_FragCoord.y),floor(t*24.)))-.5)*.02;
      color=vec4(rgb,1);
    }`;
  const shader = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS))
      throw new Error(gl.getShaderInfoLog(shader));
    return shader;
  };
  const program = gl.createProgram();
  gl.attachShader(program, shader(gl.VERTEX_SHADER, vertex));
  gl.attachShader(program, shader(gl.FRAGMENT_SHADER, fragment));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS))
    throw new Error(gl.getProgramInfoLog(program));
  gl.useProgram(program);
  const locations = Object.fromEntries(
    ["grid", "time", "mode", "mono", "reduced", "atlas"].map((name) => [
      name,
      gl.getUniformLocation(program, name),
    ]),
  );
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, atlas);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  const capacity = 180 * 60,
    stride = 20;
  const packed = new Float32Array(capacity * stride);
  gl.bufferData(gl.ARRAY_BUFFER, packed.byteLength, gl.DYNAMIC_DRAW);
  for (let i = 0; i < 5; i++) {
    gl.enableVertexAttribArray(i);
    gl.vertexAttribPointer(i, 4, gl.FLOAT, false, stride * 4, i * 16);
    gl.vertexAttribDivisor(i, 1);
  }
  let uploaded,
    previous = [],
    key = "",
    wetUntil = 0;
  const identities = new Map();
  return {
    draw(frame, now) {
      if (gl.isContextLost()) return false;
      const { cols, rows, cells } = frame.fallback,
        dpr = Math.min(devicePixelRatio || 1, 2);
      const width = Math.ceil(innerWidth * dpr),
        height = Math.ceil(innerHeight * dpr);
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      canvas.style.width = `${innerWidth}px`;
      canvas.style.height = `${innerHeight}px`;
      const nextKey = `${cols}/${rows}/${frame.variant.package}/${cells[0]?.background.join()}`;
      if (key !== nextKey) {
        packed.fill(0);
        previous = [];
        identities.clear();
        key = nextKey;
        uploaded = undefined;
      }
      if (uploaded !== frame) {
        let first = cells.length,
          last = 0;
        cells.forEach((cell, i) => {
          const old = previous[i],
            at = i * stride;
          if (old === cell) return;
          const changed =
            !old ||
            old.glyph !== cell.glyph ||
            old.bold !== cell.bold ||
            old.detail !== cell.detail;
          if (changed) {
            packed.copyWithin(at + 12, at, at + 4);
            packed.set(
              cell.glyph === " "
                ? [0, 0, 0, 0]
                : slots.get(`${cell.bold ? 1 : 0}:${cell.glyph}`) ||
                    slots.get("0:?"),
              at,
            );
            const detail = cell.detail ? frame.details[cell.detail - 1] : null;
            const identity = detail
              ? `${detail.class}:${detail.id}:${cell.glyph}`
              : `${i}:${cell.glyph}`;
            if (!identities.has(identity)) identities.set(identity, now);
            packed[at + 16] = identities.get(identity);
            packed[at + 17] = now;
            packed[at + 18] = cell.glyph.codePointAt(0) + i * 0.17;
            wetUntil = now + 0.65;
          }
          if (
            changed ||
            old.foreground.some((v, j) => v !== cell.foreground[j]) ||
            old.background.some((v, j) => v !== cell.background[j])
          ) {
            packed.set(
              cell.foreground.map((v) => v / 255),
              at + 4,
            );
            packed.set(
              cell.background.map((v) => v / 255),
              at + 8,
            );
            first = Math.min(first, i);
            last = i + 1;
          }
        });
        if (first < last) {
          gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
          gl.bufferSubData(
            gl.ARRAY_BUFFER,
            first * stride * 4,
            packed.subarray(first * stride, last * stride),
          );
        }
        previous = cells;
        uploaded = frame;
        if (identities.size > 32768)
          for (const [id, birth] of identities)
            if (now - birth > 30) identities.delete(id);
      }
      gl.viewport(0, 0, width, height);
      gl.useProgram(program);
      gl.uniform2f(locations.grid, cols, rows);
      gl.uniform1f(locations.time, now);
      gl.uniform1i(
        locations.mode,
        ["canonical", "crt", "vhs"].indexOf(
          frame.variant.package,
        ),
      );
      gl.uniform1i(
        locations.mono,
        frame.variant.color === "monochrome" ? 1 : 0,
      );
      gl.uniform1i(locations.reduced, frame.variant.reduced_motion ? 1 : 0);
      gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, cells.length);
      return (
        !frame.variant.reduced_motion &&
        (frame.variant.package === "vhs" ||
          (frame.variant.package === "crt" && now < wetUntil))
      );
    },
  };
};
