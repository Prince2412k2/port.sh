// Validate the actual WebGPU path in a Chromium instance with a GPU adapter.
// Launch Chromium with a dedicated --remote-debugging-port; never changes the
// user's existing browser profile or falls back silently to the WebGL test.
import {writeFile} from 'node:fs/promises';
const endpoint=process.argv[2]||'http://127.0.0.1:8322/v2/';
const cdp=process.argv[3]||'http://127.0.0.1:9333';
const target=await(await fetch(`${cdp}/json/new?${encodeURIComponent(endpoint)}`,{method:'PUT'})).json();
const socket=new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
let id=0,workerSession;const pending=new Map(),errors=[];
socket.onmessage=({data})=>{const value=JSON.parse(data);if(value.id){const task=pending.get(value.id);if(task){pending.delete(value.id);value.error?task.reject(new Error(JSON.stringify(value.error))):task.resolve(value.result);}}
  else if(value.method==='Target.attachedToTarget'&&value.params.targetInfo.type==='worker')workerSession=value.params.sessionId;
  else if(value.method==='Runtime.exceptionThrown')errors.push(value.params.exceptionDetails.exception?.description||value.params.exceptionDetails.text);
};
function command(method,params={},sessionId){return new Promise((resolve,reject)=>{const number=++id;pending.set(number,{resolve,reject});socket.send(JSON.stringify({id:number,method,params,sessionId}));});}
async function evaluate(expression){const value=await command('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(value.exceptionDetails)throw new Error(JSON.stringify(value.exceptionDetails));return value.result.value;}
await command('Runtime.enable');await command('Page.enable');
await command('Target.setAutoAttach',{autoAttach:true,waitForDebuggerOnStart:false,flatten:true});
await command('Emulation.setDeviceMetricsOverride',{width:1440,height:900,deviceScaleFactor:1,mobile:false});
await command('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:'reduce'}]});
await command('Page.addScriptToEvaluateOnNewDocument',{source:'localStorage.setItem("portfolio-v2-appearance",JSON.stringify({theme:"dark",package:"canonical",color:"color"}))'});
await command('Page.reload',{ignoreCache:true});
const end=Date.now()+20000;
let renderer;
while(Date.now()<end){renderer=await evaluate('document.documentElement?.dataset.renderer');if(renderer)break;await new Promise(r=>setTimeout(r,100));}
if(!renderer?.startsWith('webgpu-'))throw new Error(`WebGPU acceptance requires an adapter: ${renderer}; ${await evaluate('document.getElementById("status").textContent')}`);
async function keypress(key){await command('Input.dispatchKeyEvent',{type:'keyDown',key,text:key,unmodifiedText:key});await command('Input.dispatchKeyEvent',{type:'keyUp',key});}
await keypress('2');
const loadedDeadline=Date.now()+30000;
while(Date.now()<loadedDeadline){if(await evaluate('window.portfolioV2Worker?.decodedBytes>0&&document.getElementById("status").textContent===""'))break;await new Promise(r=>setTimeout(r,100));}
await new Promise(r=>setTimeout(r,500));
const pixels=await evaluate('window.portfolioV2.inspect_pixels?.()'),presented=await evaluate('window.portfolioV2.inspect_present?.()');
if(!pixels||pixels.length<2||!presented||presented.length<2)throw new Error('WebGPU rendered a blank logical/presentation target');
const image=await command('Page.captureScreenshot',{format:'png'});
await writeFile('/tmp/opencode/v2-webgpu-map.png',Buffer.from(image.data,'base64'));
for(const [key,expected]of [['i','ink'],['p','pixel']]){
  await keypress(key);const deadline=Date.now()+90000;
  while(Date.now()<deadline){if(await evaluate(`window.portfolioV2RenderMetrics.activePackage===${JSON.stringify(expected)}`))break;await new Promise(r=>setTimeout(r,100));}
  if(!(await evaluate(`window.portfolioV2RenderMetrics.activePackage===${JSON.stringify(expected)}`)))throw new Error(`Package ${expected} failed to present`);
}
const result=await evaluate('({renderer:document.documentElement.dataset.renderer,status:document.getElementById("status").textContent,metrics:window.portfolioV2RenderMetrics,worker:window.portfolioV2Worker})');
if(errors.length||result.status.includes('GPU view unavailable'))throw new Error(JSON.stringify({errors,result}));
console.log('PASS: actual WebGPU pipeline creation, map/ink/pixel switching without uncaught errors',JSON.stringify(result));
if(!workerSession)throw new Error('No engine worker target attached');
const originalWorker=workerSession;
await command('Runtime.evaluate',{expression:'setTimeout(()=>{throw new Error("intentional worker recovery fixture")},0)'},workerSession);
const recoveredDeadline=Date.now()+30000;
while(Date.now()<recoveredDeadline){if(workerSession!==originalWorker&&await evaluate('document.getElementById("status").textContent===""&&document.getElementById("semantic").textContent.includes("Knowledge High School")'))break;await new Promise(r=>setTimeout(r,100));}
if(workerSession===originalWorker)throw new Error('Engine worker did not restart');
console.log('PASS: actual worker exception restarts the engine and recovers the published section');
await command('Page.close');socket.close();
