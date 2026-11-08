// A clean portable copy must work with only the tracked 7z component, without installed 7-Zip.
const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path'),{spawn}=require('node:child_process'),{createHash}=require('node:crypto');
const root=fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(),'gx-compressed-')));
const sourceExe=process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe');
const sourceLib=fs.existsSync(path.join(path.dirname(sourceExe),'lib/ffmpeg.7z'))?path.join(path.dirname(sourceExe),'lib'):path.resolve('lib');
const delay=ms=>new Promise(r=>setTimeout(r,ms));let child;
function copyApp(name,bad=false){const app=path.join(root,name),lib=path.join(app,'lib');fs.mkdirSync(lib,{recursive:true});fs.copyFileSync(sourceExe,path.join(app,'Guangxia.exe'));for(const n of ['ffmpeg.7z','ffmpeg.sha256'])fs.copyFileSync(path.join(sourceLib,n),path.join(lib,n));if(bad)fs.writeFileSync(path.join(lib,'ffmpeg.sha256'),'0'.repeat(64)+'  ffmpeg.exe\n');return app;}
async function start(app,name){let log='';const env={...process.env,PATH:path.join(process.env.WINDIR,'System32')+';'+process.env.WINDIR};delete env.PVM_FFMPEG;
child=spawn(path.join(app,'Guangxia.exe'),['--no-window','--data-dir',path.join(root,name)],{windowsHide:true,env,stdio:['ignore','pipe','pipe']});child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);
let m;for(let i=0;i<600;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;if(child.exitCode!==null)throw Error(log);await delay(50);}assert(m,log);
return async p=>{const r=await(await fetch('http://127.0.0.1:'+m[1]+p,{headers:{'X-PVM-Token':m[2]}})).json();assert(r.ok,JSON.stringify(r));return r.result;};}
async function stop(){await new Promise(r=>{child.once('exit',r);child.kill();});child=null;}
(async()=>{try{
const app=copyApp('程序 Chinese space');assert(!fs.existsSync(path.join(app,'lib/ffmpeg.exe')));let api=await start(app,'good-data');
const exe=path.join(app,'lib/ffmpeg.exe'),expected=fs.readFileSync(path.join(sourceLib,'ffmpeg.sha256'),'utf8').trim().split(/\s+/)[0];
assert.equal(createHash('sha256').update(fs.readFileSync(exe)).digest('hex'),expected);assert((await api('/api/capabilities')).ffmpeg.ok);assert(!fs.readdirSync(path.join(app,'lib')).some(n=>n.startsWith('.ffmpeg-unpack-')));
console.log('PASS clean portable package extracts FFmpeg offline and verifies exact SHA-256');await stop();
const stamp=fs.statSync(exe).mtimeMs;api=await start(app,'good-data');assert((await api('/api/capabilities')).ffmpeg.ok);assert.equal(fs.statSync(exe).mtimeMs,stamp);console.log('PASS subsequent startup reuses component without extracting again');await stop();
const bad=copyApp('invalid-checksum',true);api=await start(bad,'bad-data');assert.equal((await api('/api/capabilities')).ffmpeg.ok,false);assert.equal(fs.existsSync(path.join(bad,'lib/ffmpeg.exe')),false);assert(fs.readFileSync(path.join(root,'bad-data/logs/ffmpeg-setup.log'),'utf8').includes('完整性校验失败'));console.log('PASS invalid checksum rejects component while photo application remains available');await stop();
console.log('Artifacts:',root);
}finally{child?.kill();}})().catch(e=>{console.error(e);process.exitCode=1;});
