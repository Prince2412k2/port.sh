#!/usr/bin/env python3
"""Exercise provider streaming, durable idempotency, cancel and crash recovery.

Starts an isolated real backend and an instrumented local provider fixture.
Never connects to a paid provider. Pass the compiled backend executable.
"""
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,subprocess,sys,tempfile,threading,time,urllib.request,urllib.error,uuid
from pathlib import Path

calls=[]
class Provider(BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_POST(self):
        request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        question=request['messages'][-1]['content'];calls.append(question)
        self.send_response(200);self.send_header('Content-Type','application/x-ndjson');self.end_headers()
        try:
            if question=='map tool':
                self.wfile.write((json.dumps({'message':{'content':'','tool_calls':[{'function':{'name':'show_map','arguments':{'title':'Ahmedabad','lon':72.54,'lat':23.03,'zoom':12}}}]},'done':True})+'\n').encode());self.wfile.flush();return
            if question=='diagram tool':
                self.wfile.write((json.dumps({'message':{'content':'','tool_calls':[{'function':{'name':'show_diagram','arguments':{'title':'Client server','nodes':[{'id':'client','label':'Local rendering'},{'id':'backend','label':'Structured data'}],'edges':[{'from':'backend','to':'client','label':'CBOR'}]}}}]},'done':True})+'\n').encode());self.wfile.flush();return
            for text in ['A streamed ','answer from the fixture.']:
                self.wfile.write((json.dumps({'message':{'content':text},'done':False})+'\n').encode());self.wfile.flush();time.sleep(.1)
            if question=='hold': time.sleep(10)
            self.wfile.write(b'{"message":{"content":""},"done":true}\n');self.wfile.flush()
        except (BrokenPipeError,ConnectionResetError): pass

fixture=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=fixture.serve_forever,daemon=True).start()
binary=str(Path(sys.argv[1]).resolve());base='http://127.0.0.1:8341';backend=None

def http(path,body=None):
    request=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={} if body is None else {'Content-Type':'application/json'})
    try:
        with urllib.request.urlopen(request,timeout=5) as response:return response.status,json.load(response)
    except urllib.error.HTTPError as error:return error.code,error.read().decode()

def start(directory):
    global backend
    env=dict(os.environ,PORTFOLIO_V2_ADDR='127.0.0.1:8341',PORTFOLIO_V2_MAP_DIR=directory,PORTFOLIO_V2_SESSION_DIR=directory,
        PORTFOLIO_V2_AI_URL=f'http://127.0.0.1:{fixture.server_port}/chat',OLLAMA_API_KEY='local-fixture-key',PORTFOLIO_V2_AI_MODEL='fixture')
    backend=subprocess.Popen([binary],env=env,stdout=subprocess.DEVNULL)
    end=time.monotonic()+10
    while time.monotonic()<end:
        try:http('/api/v2/bootstrap');return
        except (OSError,urllib.error.URLError):time.sleep(.05)
    raise AssertionError('backend startup failed')

def wait(session,predicate):
    end=time.monotonic()+10
    while time.monotonic()<end:
        _,snapshot=http('/api/v2/sessions/'+session)
        if predicate(snapshot):return snapshot
        time.sleep(.05)
    raise AssertionError(snapshot)

try:
    with tempfile.TemporaryDirectory(prefix='v2-sessions-',dir='/tmp/opencode') as directory:
        start(directory)
        subprocess.run(['node',str(Path(__file__).with_name('test-v2-session-wire.mjs')),base],check=True)
        _,tool=http('/api/v2/sessions',{});tool_id=tool['session_id']
        http(f'/api/v2/sessions/{tool_id}/requests',{'request_id':uuid.uuid4().hex,'question':'map tool'})
        tool=wait(tool_id,lambda s:s['exchanges'][0]['status']=='completed')
        assert tool['exchanges'][0]['presentations'][0]['kind']=='map'
        _,diagram=http('/api/v2/sessions',{});diagram_id=diagram['session_id']
        http(f'/api/v2/sessions/{diagram_id}/requests',{'request_id':uuid.uuid4().hex,'question':'diagram tool'})
        diagram=wait(diagram_id,lambda s:s['exchanges'][0]['status']=='completed')
        assert diagram['exchanges'][0]['presentations'][0]['kind']=='diagram'
        code,snapshot=http('/api/v2/sessions',{});assert code==200
        session=snapshot['session_id'];request=uuid.uuid4().hex
        command={'request_id':request,'question':'normal'}
        code,first=http(f'/api/v2/sessions/{session}/requests',command);assert code==200
        code,duplicate=http(f'/api/v2/sessions/{session}/requests',command);assert code==200
        answer=wait(session,lambda s:s['exchanges'][0]['status']=='completed')
        assert answer['exchanges'][0]['answer']=='A streamed answer from the fixture.'
        assert calls.count('normal')==1
        note_id=uuid.uuid4().hex;note={'request_id':note_id,'question':'/reach A durable test message.'}
        before=len(calls);http(f'/api/v2/sessions/{session}/requests',note)
        note_snapshot=wait(session,lambda s:s['exchanges'][-1]['status']=='completed')
        assert note_snapshot['exchanges'][-1]['answer']=='Message saved for Prince.'
        http(f'/api/v2/sessions/{session}/requests',note);assert len(calls)==before
        assert len(list(Path(directory).glob('*.message.json')))==1
        code,_=http(f'/api/v2/sessions/{session}/requests',dict(command,question='different'));assert code==409
        hold=uuid.uuid4().hex
        http(f'/api/v2/sessions/{session}/requests',{'request_id':hold,'question':'hold'})
        wait(session,lambda s:bool(s['exchanges'][-1]['answer']))
        code,cancelled=http(f'/api/v2/sessions/{session}/requests/{hold}/cancel',{});assert code==200
        assert cancelled['exchanges'][-1]['status']=='cancelled'
        http(f'/api/v2/sessions/{session}/requests',{'request_id':hold,'question':'hold'})
        assert calls.count('hold')==1
        crash=uuid.uuid4().hex
        http(f'/api/v2/sessions/{session}/requests',{'request_id':crash,'question':'hold'})
        wait(session,lambda s:bool(s['exchanges'][-1]['answer']))
        backend.kill();backend.wait();start(directory)
        _,restored=http(f'/api/v2/sessions/{session}')
        assert restored['exchanges'][-1]['status']=='failed'
        assert restored['exchanges'][0]['answer']==answer['exchanges'][0]['answer']
        assert restored['sequence']>answer['sequence']
        http(f'/api/v2/sessions/{session}/requests',{'request_id':crash,'question':'hold'})
        time.sleep(.2);assert calls.count('hold')==2
        assert http('/api/v2/sessions/../../etc/passwd')[0]==404
        print('PASS: actual provider stream, duplicate/conflicting request IDs, cancellation, durable restart/replay, no duplicated work after crash')
finally:
    if backend is not None:backend.terminate();backend.wait(timeout=5)
    fixture.shutdown()
