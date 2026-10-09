// Execute the same trace in actual native and WASM binaries and compare every
// byte of the canonical surface through SHA-256, across sections and themes.
import { readFile, writeFile, mkdtemp, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
const endpoint=process.argv[2]||"http://127.0.0.1:8322";
const dist=resolve("v2/crates/browser/dist");
const {default:init,Engine}=await import(pathToFileURL(`${dist}/portfolio_v2_browser.js`));
await init({module_or_path:await readFile(`${dist}/portfolio_v2_browser_bg.wasm`)});
const bootstrap=await (await fetch(`${endpoint}/api/v2/bootstrap`)).json();
const actions=[];
for(const [width,height]of [[640,408],[1280,765],[1440,884]]){
  actions.push({type:"resize",width,height});
  for(const reduced of [true,false]){
    actions.push({type:"motion",reduced});
    for(const key of ["1","2","3","4","5","6"]){
      actions.push({type:"key",key},{type:"tick",seconds:0.1},{type:"frame"});
      // Ask treats ordinary letters/digits as input; Escape leaves it.
      if(key==="6")actions.push({type:"key",key:"Escape"});
    }
  }
  actions.push({type:"key",key:"i"},{type:"frame"});
}
const engine=new Engine();engine.bootstrap(new TextEncoder().encode(JSON.stringify(bootstrap)));
const wasm=[];
const atlas=JSON.parse(await readFile(`${dist}/glyph-atlas.json`,'utf8'));
const supported=new Set(atlas.slots.map(s=>s[1]));const missing=new Set();
for(const action of actions){
  switch(action.type){
    case "resize":engine.resize(action.width,action.height,1);break;
    case "key":engine.key(action.key);break;
    case "tick":engine.tick(action.seconds);break;
    case "motion":engine.reduced_motion(action.reduced);break;
    case "frame":{const frame=engine.frame();const view=new DataView(frame.buffer,frame.byteOffset,frame.byteLength);for(let i=0;i<frame.byteLength;i+=24){const code=view.getUint32(i,true);if(!supported.has(code))missing.add(code);}wasm.push(createHash("sha256").update(frame).digest("hex"));break;}
  }
}
const directory=await mkdtemp("/tmp/opencode/v2-parity-");
try{
  const fixture=`${directory}/trace.json`;await writeFile(fixture,JSON.stringify({bootstrap,actions}));
  const native=JSON.parse(execFileSync(resolve("v2/target/release/portfolio-v2-native"),["--replay",fixture],{encoding:"utf8"}));
  for(let i=0;i<wasm.length;i++)if(wasm[i]!==native[i])throw new Error(`Canonical parity failed at frame ${i}: WASM ${wasm[i]} native ${native[i]}`);
  console.log(`PASS: ${wasm.length} native/WASM canonical frames, all sections, layout sizes, reduced motion and themes`);
  if(missing.size)throw new Error(`Production glyph coverage missing: ${[...missing].map(code=>'U+'+code.toString(16)).join(', ')}`);
  console.log('PASS: prebaked glyph atlas covers every glyph in the parity trace');
}finally{engine.free();await rm(directory,{recursive:true});}
