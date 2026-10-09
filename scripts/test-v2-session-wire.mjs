import {readFile} from "node:fs/promises";
import {pathToFileURL} from "node:url";
import {resolve} from "node:path";
const base=process.argv[2];
const {default:init,Engine}=await import(pathToFileURL(resolve('v2/crates/browser/dist/portfolio_v2_browser.js')));
await init({module_or_path:await readFile('v2/crates/browser/dist/portfolio_v2_browser_bg.wasm')});
const engine=new Engine();
const create=await fetch(`${base}/api/v2/sessions`,{method:'POST'});if(!create.ok)throw new Error('create failed');
const snapshot=await create.json(),id=snapshot.session_id;
const socket=new WebSocket(`${base.replace('http','ws')}/api/v2/session?session=${id}`);socket.binaryType='arraybuffer';
const result=await new Promise((resolve,reject)=>{
  const timeout=setTimeout(()=>reject(new Error('wire timeout')),15000);
  socket.onerror=reject;
  let submitted=false,last=-1,streamed=false;
  socket.onmessage=async({data})=>{
    try{
      if(!(data instanceof ArrayBuffer))throw new Error('Expected binary CBOR frame');
      engine.session_cbor(new Uint8Array(data));
      const value=JSON.parse(engine.session_snapshot());
      if(value.session_id!==id||value.protocol!==2||value.sequence<last)throw new Error('Invalid session ordering');last=value.sequence;
      if(!submitted){submitted=true;
        const response=await fetch(`${base}/api/v2/sessions/${id}/requests`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:crypto.randomUUID().replaceAll('-',''),question:'wire fixture'})});
        if(!response.ok)throw new Error('submit failed');
      }
      const exchange=value.exchanges[0];
      if(exchange?.status==='running'&&exchange.answer)streamed=true;
      if(exchange?.status==='completed'){
        if(!streamed)throw new Error('No incremental answer received');
        clearTimeout(timeout);resolve(value);
      }
    }catch(error){clearTimeout(timeout);reject(error);}
  };
});
socket.close();engine.free();console.log(`PASS: binary CBOR WebSocket decoded by actual WASM client, monotonic snapshots and streamed answer (${result.sequence} events)`);
