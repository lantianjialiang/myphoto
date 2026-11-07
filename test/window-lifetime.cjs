const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),assert=require('node:assert/strict'),{spawn}=require('node:child_process');
const dir=fs.mkdtempSync(path.join(os.tmpdir(),'pvm-lifetime-'));
const exe=process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe');
const child=spawn(exe,['--data-dir',dir],{windowsHide:true,env:{...process.env,PVM_BROWSER:path.resolve('target/release/examples/browser_stub.exe')},stdio:['ignore','pipe','pipe']});
let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);const delay=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{try{
let m;for(let i=0;i<200;i++){m=log.match(/http:\/\/127.0.0.1:(\d+)\/\?token=([\w-]+)/);if(m)break;await delay(50);}assert(m,log);
const headers={'X-PVM-Token':m[2],'Content-Type':'application/json'},base='http://127.0.0.1:'+m[1];
async function api(p,body){const r=await fetch(base+p,{headers,method:body?'POST':'GET',body:body?JSON.stringify(body):undefined});const j=await r.json();assert(j.ok,JSON.stringify(j));return j.result;}
await delay(1500);assert.equal(child.exitCode,null,'server must survive browser launcher exit');await api('/api/state');console.log('PASS server survives launcher exit');
await api('/api/window/heartbeat',{window_id:'old-page'});await api('/api/window/close',{window_id:'old-page'});await api('/api/window/heartbeat',{window_id:'new-page'});
await delay(11000);assert.equal(child.exitCode,null);await api('/api/state');console.log('PASS page reload keeps service alive beyond close grace');
await api('/api/window/close',{window_id:'new-page'});
for(let i=0;i<260&&child.exitCode===null;i++)await delay(50);
assert.equal(child.exitCode,0,'server should stop after all pages close');console.log('PASS last window close stops idle server');
}finally{child.kill();fs.writeFileSync(path.join(dir,'test.log'),log);}})().catch(e=>{console.error(e);process.exitCode=1;});
