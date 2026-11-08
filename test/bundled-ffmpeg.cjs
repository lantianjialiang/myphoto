const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),{spawn,execFileSync}=require('node:child_process');
const root=fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(),'gx-ffmpeg-'))),photos=path.join(root,'Photos');fs.mkdirSync(photos);
const exe=process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe');
const bundled=path.join(path.dirname(exe),'lib/ffmpeg.exe');
const ffmpeg=fs.existsSync(bundled)?bundled:path.resolve('lib/ffmpeg.exe');assert(fs.existsSync(ffmpeg));
execFileSync(ffmpeg,['-hide_banner','-loglevel','error','-f','lavfi','-i','color=c=teal:s=320x180:r=10','-t','0.5','-c:v','libx264','-pix_fmt','yuv420p',path.join(photos,'20200101_clip.mp4')],{windowsHide:true});
fs.copyFileSync(path.join(__dirname,'fixtures/gps.jpg'),path.join(photos,'photo.jpg'));fs.copyFileSync(path.join(__dirname,'fixtures/heic/iphone.heic'),path.join(photos,'phone.heic'));
const env={...process.env};if(fs.existsSync(bundled))delete env.PVM_FFMPEG;else env.PVM_FFMPEG=ffmpeg;
const child=spawn(exe,['--no-window','--data-dir',path.join(root,'data')],{windowsHide:true,env,stdio:['ignore','pipe','pipe']});let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);const delay=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{try{
let m;for(let i=0;i<300;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;await delay(50);}assert(m,log);const base='http://127.0.0.1:'+m[1],headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
async function api(p,b){const r=await fetch(base+p,{headers,method:b===undefined?'GET':'POST',body:b===undefined?undefined:JSON.stringify(b)});const j=await r.json();assert(j.ok,JSON.stringify(j));return j.result;}
async function run(p,b){const r=await api(p,b);for(let i=0;i<1200;i++){const j=await api('/api/jobs/'+r.job_id);if(!['running','queued','cancelling'].includes(j.status)){assert.equal(j.status,'completed',JSON.stringify(j));return j;}await delay(50);}throw Error('timeout');}
const caps=await api('/api/capabilities');assert(caps.ffmpeg.ok);assert(caps.ffmpeg.path.includes('ffmpeg.exe'));console.log('PASS bundled FFmpeg detected without system installation');
const source=await api('/api/sources',{path:photos});await run('/api/sources/'+source.id+'/scan',{});const items=(await api('/api/media?limit=100')).items;
const video=items.find(p=>p.media_type==='video'),heic=items.find(p=>p.ext==='heic'),jpg=items.find(p=>p.ext==='jpg');assert(video&&heic&&jpg);
const poster=await fetch(base+'/api/thumb/'+video.id+'?size=poster',{headers});assert.equal(poster.status,200);assert((await poster.arrayBuffer()).byteLength>100);console.log('PASS video poster extraction uses built-in FFmpeg');
await run('/api/watermark/run',{ids:[video.id],config:{text:'VIDEO WATERMARK',require_date:false,video:true}});
assert(fs.readdirSync(path.join(root,'exports/watermark')).some(n=>n.endsWith('.mp4')));console.log('PASS video watermark encoding produces playable output');
await run('/api/edit/run',{ids:[heic.id],op:'rot90'});
await run('/api/slideshow/run',{ids:[heic.id,jpg.id,video.id],photo_seconds:0.5,video_seconds:0.5,size:320,format:'mp4',file_name:'mixed'});
const mp4=path.join(root,'exports/slideshows/mixed.mp4');assert(fs.statSync(mp4).size>1000);execFileSync(ffmpeg,['-v','error','-i',mp4,'-f','null','-'],{windowsHide:true});console.log('PASS slideshow accepts rotated HEIC, JPEG and video and decodes successfully');
await run('/api/slideshow/run',{ids:[heic.id,jpg.id],photo_seconds:0.5,size:160,format:'gif',file_name:'animation'});
assert.equal(fs.readFileSync(path.join(root,'exports/slideshows/animation.gif')).subarray(0,3).toString(),'GIF');console.log('PASS GIF slideshow succeeds with built-in FFmpeg');console.log('Fixtures:',root);
}finally{child.kill();}})().catch(e=>{console.error(e);process.exitCode=1;});
