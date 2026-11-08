const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path'),{spawn}=require('node:child_process'),{createHash}=require('node:crypto');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'gx-heic-')),photos=path.join(root,'Photos');fs.mkdirSync(photos);
for(const f of ['iphone.heic','example.heic','sample1.heic'])fs.copyFileSync(path.join(__dirname,'fixtures/heic',f),path.join(photos,f));
const hash=b=>createHash('sha256').update(b).digest('hex');const rawHashes=Object.fromEntries(fs.readdirSync(photos).map(n=>[n,hash(fs.readFileSync(path.join(photos,n)))]));
const child=spawn(process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe'),['--no-window','--data-dir',path.join(root,'data')],{windowsHide:true,stdio:['ignore','pipe','pipe']});let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);const delay=ms=>new Promise(r=>setTimeout(r,ms));
function dimensions(b){let i=2;while(i<b.length-8){if(b[i++]!==255)continue;const tag=b[i++];if(tag>=0xc0&&tag<=0xc3)return [b.readUInt16BE(i+5),b.readUInt16BE(i+3)];if(tag===0xd8||tag===0xd9)continue;const n=b.readUInt16BE(i);if(n<2)break;i+=n;}throw Error('JPEG size not found');}
(async()=>{try{
let m;for(let i=0;i<300;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;await delay(50);}assert(m,log);const base='http://127.0.0.1:'+m[1],headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
async function api(p,b){const r=await fetch(base+p,{headers,method:b===undefined?'GET':'POST',body:b===undefined?undefined:JSON.stringify(b)});const j=await r.json();assert(j.ok,JSON.stringify(j));return j.result;}
async function run(p,b){const r=await api(p,b);for(let i=0;i<1200;i++){const j=await api('/api/jobs/'+r.job_id);if(!['running','queued','cancelling'].includes(j.status)){assert.equal(j.status,'completed',JSON.stringify(j));return j;}await delay(50);}throw Error('timeout');}
const caps=await api('/api/capabilities');assert(caps.heic.ok,JSON.stringify(caps));
const source=await api('/api/sources',{path:photos});await run('/api/sources/'+source.id+'/scan',{});
const items=(await api('/api/media?limit=100')).items;assert.equal(items.length,3);
const before=new Map();for(const a of items){const d=await api('/api/media/'+a.id);const b=Buffer.from(await(await fetch(base+'/api/image/'+d.files[0].id+'?full=1',{headers})).arrayBuffer());before.set(a.id,{d,dim:dimensions(b),hash:hash(b)});}
await run('/api/edit/run',{ids:items.map(a=>a.id),op:'rot90',mode:'overwrite',backup:true});
for(const a of items){const old=before.get(a.id),d=await api('/api/media/'+a.id);assert.equal(hash(fs.readFileSync(d.files[0].absolute_path)),rawHashes[d.files[0].file_name]);assert.equal(d.width,old.d.height);assert.equal(d.height,old.d.width);
const image=Buffer.from(await(await fetch(base+'/api/image/'+d.files[0].id+'?full=1',{headers})).arrayBuffer());assert.deepEqual(dimensions(image),[old.dim[1],old.dim[0]]);
const preview=await api('/api/watermark/preview',{id:a.id,config:{text:'HEIC ORIENTATION',require_date:false},max_side:0});assert.equal(preview.width,old.dim[1]);assert.equal(preview.height,old.dim[0]);}
console.log('PASS HEIC rotation preserves all original bytes and updates library/fullscreen/watermark direction');
await run('/api/sources/'+source.id+'/scan',{});for(const a of items)assert.equal((await api('/api/media/'+a.id)).width,before.get(a.id).d.height);
console.log('PASS HEIC direction survives rescan');
await run('/api/album/run',{ids:[items[0].id],max_side:1600});const out=dimensions(fs.readFileSync(path.join(root,'exports/albums/photos/0001.jpg'))),orig=before.get(items[0].id).dim;assert.equal(out[0]>out[1],orig[1]>orig[0]);
console.log('PASS album export uses the same HEIC direction');
await run('/api/edit/run',{ids:items.map(a=>a.id),op:'rot270',mode:'overwrite'});
for(const a of items){const old=before.get(a.id);const image=Buffer.from(await(await fetch(base+'/api/image/'+old.d.files[0].id+'?full=1',{headers})).arrayBuffer());assert.equal(hash(image),old.hash);}
console.log('PASS inverse rotation restores exact rendered view without adding files');console.log('Fixtures:',root);
}finally{child.kill();}})().catch(e=>{console.error(e);process.exitCode=1;});
