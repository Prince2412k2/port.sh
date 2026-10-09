// Experimental geometry renderer: real terrain triangles and draped vector
// paths, a depth buffer, directional lighting and a deliberate pixel target.
export class PixelRenderer {
  constructor() {
    this.canvas = document.createElement("canvas");
    this.canvas.id = "pixel-stage";
    Object.assign(this.canvas.style, {
      position: "absolute",
      inset: "0 auto auto 0",
      pointerEvents: "none",
      imageRendering: "pixelated",
    });
    document.body.insertBefore(this.canvas, document.getElementById("stage"));
    const gl = (this.gl = this.canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
    }));
    if (!gl) throw new Error("Pixel terrain requires WebGL2");
    const shader = (type, source) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, source);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS))
        throw new Error(gl.getShaderInfoLog(s));
      return s;
    };
    const program = (this.program = gl.createProgram());
    gl.attachShader(
      program,
      shader(
        gl.VERTEX_SHADER,
        `#version 300 es
      precision highp float;layout(location=0)in vec3 position;layout(location=1)in vec3 normal;layout(location=2)in vec3 material;layout(location=3)in vec3 surface;
      out vec3 n;out vec3 color;out vec3 ground;void main(){gl_Position=vec4(position.xy,position.z*2.-1.,1);n=normal;color=material;ground=surface;}`,
      ),
    );
    gl.attachShader(
      program,
      shader(
        gl.FRAGMENT_SHADER,
        `#version 300 es
      precision highp float;in vec3 n;in vec3 color;in vec3 ground;uniform float time;uniform bool reduced;uniform bool mono;uniform sampler2D heights;uniform float aspect;out vec4 fragment;
      float shadow(vec3 sun){
        vec3 ray=ground;float stepSize=1./64.;
        for(int i=0;i<32;i++){ray.xy+=sun.xy*stepSize*vec2(1.,aspect);ray.z+=sun.z*stepSize;
          if(any(lessThan(ray.xy,vec2(0)))||any(greaterThan(ray.xy,vec2(1))))break;
          if(texture(heights,ray.xy).r>ray.z+.0002)return .35;
        }return 1.;
      }
      void main(){vec3 sun=normalize(vec3(-.45,-.65,1.));float light=.30+.70*max(dot(normalize(n),sun),0.);
        light*=shadow(sun);
        light=floor(light*6.+.5)/6.;vec3 lit=color*light;
        if(color.b>color.r*1.7){float t=reduced?0.:time;float ripple=sin(gl_FragCoord.x*.7+gl_FragCoord.y*.2+t*1.8);lit+=vec3(.05,.09,.10)*step(.85,ripple);}
        if(mono)lit=vec3(dot(lit,vec3(.299,.587,.114)));
        float dither=mod(gl_FragCoord.x+gl_FragCoord.y,2.)*.008;fragment=vec4(floor((lit+dither)*31.+.5)/31.,1);}`,
      ),
    );
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS))
      throw new Error(gl.getProgramInfoLog(program));
    this.time = gl.getUniformLocation(program, "time");
    this.reduced = gl.getUniformLocation(program, "reduced");
    this.mono = gl.getUniformLocation(program, "mono");
    this.aspect = gl.getUniformLocation(program, "aspect");
    this.heightTexture = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, this.heightTexture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.R32F,
      65,
      65,
      0,
      gl.RED,
      gl.FLOAT,
      new Float32Array(65 * 65),
    );
    this.buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    for (let i = 0; i < 4; i++) {
      gl.enableVertexAttribArray(i);
      gl.vertexAttribPointer(i, 3, gl.FLOAT, false, 48, i * 12);
    }
    gl.enable(gl.DEPTH_TEST);
    this.capacity = 0;
    this.timer = 0;
    this.delay = 0;
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) {
        clearTimeout(this.delay);
        cancelAnimationFrame(this.timer);
      } else if (!this.canvas.hidden) this.draw();
    });
  }
  render(mesh, heights, meta) {
    this.meta = meta;
    this.count = mesh.length / 12;
    this.canvas.hidden = !this.count;
    cancelAnimationFrame(this.timer);
    clearTimeout(this.delay);
    this.timer = 0;
    if (!this.count) return;
    const gl = this.gl,
      w = meta.cols * 2,
      h = Math.ceil((meta.rows * 17) / 4);
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    this.canvas.style.width = `${meta.cols * 8}px`;
    this.canvas.style.height = `${meta.rows * 17}px`;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    if (this.capacity < mesh.byteLength) {
      this.capacity = mesh.byteLength;
      gl.bufferData(gl.ARRAY_BUFFER, this.capacity, gl.DYNAMIC_DRAW);
    }
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, mesh);
    if (heights.length !== 65 * 65 + 1)
      throw new Error("Invalid terrain shadow field");
    this.fieldAspect = heights[0];
    gl.bindTexture(gl.TEXTURE_2D, this.heightTexture);
    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      65,
      65,
      gl.RED,
      gl.FLOAT,
      heights.subarray(1),
    );
    this.water = false;
    for (let i = 0; i < mesh.length; i += 12) {
      if (mesh[i + 8] > mesh[i + 6] * 1.7) {
        this.water = true;
        break;
      }
    }
    this.draw();
  }
  draw() {
    const gl = this.gl;
    if (this.canvas.hidden || document.hidden) return;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0.08, 0.11, 0.12, 1);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    gl.useProgram(this.program);
    gl.uniform1f(this.time, performance.now() / 1000);
    gl.uniform1i(this.reduced, this.meta.variant.reduced_motion ? 1 : 0);
    gl.uniform1i(this.mono, this.meta.variant.color === "monochrome" ? 1 : 0);
    gl.uniform1f(this.aspect, this.fieldAspect);
    gl.drawArrays(gl.TRIANGLES, 0, this.count);
    // Experimental water runs at an explicit 30 Hz quality tier.
    if (this.water && !this.meta.variant.reduced_motion) {
      clearTimeout(this.delay);
      this.delay = setTimeout(() => {
        this.timer = requestAnimationFrame(() => this.draw());
      }, 33);
    }
  }
  hide() {
    this.canvas.hidden = true;
    cancelAnimationFrame(this.timer);
    clearTimeout(this.delay);
  }
}
