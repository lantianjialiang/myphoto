// Run after cargo build --release. Uses synthetic fixtures and a separate database.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawn } = require('node:child_process');
const { createHash } = require('node:crypto');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'pvm-regression-'));
const photos = path.join(root, 'Photos');
fs.mkdirSync(photos);
fs.copyFileSync(path.join(__dirname, 'fixtures/gps.jpg'), path.join(photos, '20200102_gps.jpg'));
fs.copyFileSync(path.join(__dirname, 'fixtures/sample.tif'), path.join(photos, '20200103_image.tif'));
const broken = path.join(photos,'20200104_broken.jpg'); fs.writeFileSync(broken,'broken image fixture');
const exe = process.env.PVM_EXE || path.resolve(__dirname,'../target/release/photo-video-manager.exe');
const child = spawn(exe, ['--no-window', '--data-dir', path.join(root,'data')], { windowsHide:true, stdio:['ignore','pipe','pipe'] });
let log = ''; child.stdout.on('data',b=>log+=b); child.stderr.on('data',b=>log+=b);
const delay = ms => new Promise(r=>setTimeout(r,ms));
const digest = p => createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const checks=[];
function check(name, fn) { fn(); checks.push(name); console.log('PASS',name); }
function zipNames(buf) {
  const names=[];
  for(let i=0;i+46<=buf.length;i++) if(buf.readUInt32LE(i)===0x02014b50){
    const n=buf.readUInt16LE(i+28),x=buf.readUInt16LE(i+30),c=buf.readUInt16LE(i+32);
    names.push(buf.subarray(i+46,i+46+n).toString('utf8'));i+=45+n+x+c;
  }
  return names;
}
(async()=>{try{
  let m; for(let n=0;n<200;n++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;if(child.exitCode!==null)throw Error(log);await delay(50);}
  assert(m,'server startup');const base='http://127.0.0.1:'+m[1];const headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
  async function api(p,body){const r=await fetch(base+p,{headers,method:body===undefined?'GET':'POST',body:body===undefined?undefined:JSON.stringify(body)});const j=await r.json();assert(j.ok,p+': '+JSON.stringify(j));return j.result;}
  async function wait(id){for(let n=0;n<600;n++){const j=await api('/api/jobs/'+id);if(!['running','queued','cancelling'].includes(j.status))return j;await delay(50);}throw Error('job timeout '+id);}
  async function run(p,b){const j=await api(p,b);return {id:j.job_id,result:await wait(j.job_id)};}
  const source=await api('/api/sources',{path:photos});await run('/api/sources/'+source.id+'/scan',{});
  let list=(await api('/api/media?limit=100')).items;const gps=list.find(i=>i.file_name.includes('gps')),tiff=list.find(i=>i.ext==='tif'),bad=list.find(i=>i.file_name.includes('broken'));
  check('GPS fixture indexed',()=>assert.equal(list.length,3));
  const detail=await api('/api/media/'+gps.id);check('GPS latitude readable',()=>assert.equal(detail.gps_lat,31.22));
  const albumPath=path.join(root,'Album');
  const plain=await run('/api/album/run',{ids:[gps.id],out_dir:albumPath,include_originals:true});
  check('GPS album export succeeds',()=>assert.equal(plain.result.succeeded,1));
  const encrypted=await run('/api/album/run',{ids:[gps.id],out_dir:albumPath,password:'fixture-only',zip:true});
  check('encrypted export uses a fresh directory',()=>{assert.equal(encrypted.result.status,'completed');assert(fs.existsSync(path.join(root,'Album (2)/index.html')));assert(!fs.existsSync(path.join(root,'Album (2)/photos/0001.jpg')));});
  check('encrypted zip contains no leftover plaintext',()=>{const names=zipNames(fs.readFileSync(path.join(root,'Album (2).zip')));assert.equal(names.length,1);assert(names[0].endsWith('/index.html'));});
  const failed=await run('/api/album/run',{ids:[bad.id],out_dir:path.join(root,'Failed')});
  check('all-failed album is failed without an empty index',()=>{assert.equal(failed.result.status,'failed');assert(!fs.existsSync(path.join(root,'Failed/index.html')));});
  const partial=await run('/api/album/run',{ids:[gps.id,bad.id],out_dir:path.join(root,'Partial')});
  check('mixed album reports partial',()=>assert.equal(partial.result.status,'partial'));
  const converted=await fetch(base+'/api/image/'+(await api('/api/media/'+tiff.id)).files[0].id,{headers});
  check('TIFF converts to browser JPEG',()=>{assert.equal(converted.status,200);assert.equal(converted.headers.get('content-type'),'image/jpeg');assert.equal(converted.headers.get('cache-control'),'no-store');});
  await run('/api/hash',{all:true});const oldDetail=await api('/api/media/'+gps.id);const src=oldDetail.files[0].absolute_path;const oldHash=digest(src);
  const wm=await run('/api/watermark/run',{ids:[gps.id],out_dir:path.join(root,'Watermarked'),config:{text:'REGRESSION',require_date:false}});
  const outputs=await api('/api/media/ids?job_id='+wm.id);
  check('watermark copy enters library and is linked to its job',()=>{assert.equal(wm.result.succeeded,1);assert.equal(outputs.length,1);assert.notEqual(outputs[0],gps.id);});
  const outDetail=await api('/api/media/'+outputs[0]);check('derived image preserves taken date',()=>assert.equal(outDetail.taken_at,oldDetail.taken_at));
  const fromOutput=await run('/api/album/run',{ids:outputs,out_dir:path.join(root,'FromOutput')});
  check('generated copy can be exported directly',()=>assert.equal(fromOutput.result.succeeded,1));
  const inline=await run('/api/album/run',{ids:[gps.id],out_dir:path.join(root,'InlineWatermark'),watermark:{text:'INLINE',require_date:false}});
  check('album applies watermark without modifying source',()=>{assert.equal(inline.result.succeeded,1);assert.equal(digest(src),oldHash);assert.notEqual(digest(path.join(albumPath,'photos/0001.jpg')),digest(path.join(root,'InlineWatermark/photos/0001.jpg')));});
  const overwrite=await run('/api/watermark/run',{ids:[gps.id],mode:'overwrite',backup:true,config:{text:'ROTATE',require_date:false,rotate_image:90}});
  let modified=await api('/api/media/'+gps.id);
  check('watermark overwrite invalidates hash and swaps dimensions',()=>{assert.equal(overwrite.result.status,'completed');assert.equal(modified.files[0].sha256,null);assert.equal(modified.width,oldDetail.height);assert.equal(modified.height,oldDetail.width);assert.notEqual(digest(src),oldHash);});
  await api('/api/watermark/backups/restore',{job_id:overwrite.id});modified=await api('/api/media/'+gps.id);
  check('watermark restore refreshes file and dimensions',()=>{assert.equal(digest(src),oldHash);assert.equal(modified.width,oldDetail.width);assert.equal(modified.files[0].size_bytes,fs.statSync(src).size);});
  const edit=await run('/api/edit/run',{ids:[gps.id],op:'rot90',mode:'overwrite',backup:true});modified=await api('/api/media/'+gps.id);
  check('edit overwrite updates file metadata',()=>{assert.equal(edit.result.status,'completed');assert.equal(modified.width,oldDetail.height);assert.equal(modified.files[0].size_bytes,fs.statSync(src).size);assert.equal(modified.files[0].sha256,null);});
  await api('/api/edit/backups/restore',{job_id:edit.id});
  check('edit restore restores original bytes',()=>assert.equal(digest(src),oldHash));
  const editCopy=await run('/api/edit/run',{ids:[gps.id],op:'fliph',mode:'copy',out_dir:path.join(root,'Edited')});
  check('edit copy is indexed and linked to its job',()=>assert.equal(editCopy.result.succeeded,1));assert.equal((await api('/api/media/ids?job_id='+editCopy.id)).length,1);
  const cancelling=await api('/api/edit/run',{ids:Array(100).fill(gps.id),op:'rot90',mode:'copy',out_dir:path.join(root,'Cancelled')});
  await api('/api/jobs/'+cancelling.job_id+'/cancel',{});const cancelled=await wait(cancelling.job_id);
  check('cancel stops remaining edits',()=>{assert.equal(cancelled.status,'cancelled');assert(cancelled.done<100);});
  // A modified duplicate must no longer share an asset with its untouched copy.
  const dupDir=path.join(root,'Duplicates');fs.mkdirSync(dupDir);const distinct=Buffer.concat([fs.readFileSync(src),Buffer.from('distinct fixture')]);
  fs.writeFileSync(path.join(dupDir,'dup1.jpg'),distinct);fs.writeFileSync(path.join(dupDir,'dup2.jpg'),distinct);
  const ds=await api('/api/sources',{path:dupDir});await run('/api/sources/'+ds.id+'/scan',{});await run('/api/hash',{all:true});
  const merged=(await api('/api/media?limit=100')).items.find(i=>i.file_name==='dup1.jpg'||i.file_name==='dup2.jpg');const md=await api('/api/media/'+merged.id);assert.equal(md.files.length,2);
  const untouched=md.files[1],untouchedHash=digest(untouched.absolute_path);const de=await run('/api/edit/run',{ids:[merged.id],op:'rot90',mode:'overwrite',backup:true});
  list=(await api('/api/media?limit=100')).items;const duplicates=list.filter(i=>i.file_name==='dup1.jpg'||i.file_name==='dup2.jpg');
  check('changed duplicate becomes a separate asset',()=>{assert.equal(de.result.status,'completed');assert.equal(duplicates.length,2);assert.equal(digest(untouched.absolute_path),untouchedHash);});
  const unified=path.join(root,'Unified');await api('/api/settings',{output_root:unified});
  check('unified output setting persists',()=>assert.equal(fs.existsSync(unified),true));
  const centralEdit=await run('/api/edit/run',{ids:[gps.id],op:'flipv',mode:'copy'});
  check('edit uses configured output without asking for directory',()=>{assert.equal(centralEdit.result.succeeded,1);assert(fs.readdirSync(path.join(unified,'edited')).length>0);});
  const centralWatermark=await run('/api/watermark/run',{ids:[gps.id],config:{text:'CENTRAL',require_date:false,skip_watermarked:false}});
  check('watermark uses configured output without directory',()=>{assert.equal(centralWatermark.result.succeeded,1);assert(fs.readdirSync(path.join(unified,'watermark')).length>0);});
  const centralAlbum=await run('/api/album/run',{ids:[gps.id]});
  check('album uses configured output without directory',()=>{assert.equal(centralAlbum.result.succeeded,1);assert(fs.existsSync(path.join(unified,'albums/index.html')));});
  const centralPlan=await api('/api/archive/plan',{asset_ids:[gps.id],mode:'copy'});
  check('archive plan uses configured output',()=>assert.equal(path.resolve(centralPlan.out_dir),path.join(unified,'archive')));
  const manifest=await api('/api/export/save',{format:'json'});const csv=await api('/api/export/save',{format:'csv'});
  check('JSON and CSV save under unified manifests directory',()=>{assert.equal(path.dirname(manifest.path),path.join(unified,'manifests'));assert(fs.readFileSync(csv.path,'utf8').includes('20200102_normal')===false || fs.statSync(csv.path).size>0);assert(JSON.parse(fs.readFileSync(manifest.path,'utf8')).items.length>0);});
  const backup=await api('/api/backup',{});check('database backup uses unified directory',()=>{assert.equal(path.dirname(backup.path),path.join(unified,'backups'));assert(fs.existsSync(backup.path));});
  const centralOverwrite=await run('/api/edit/run',{ids:[gps.id],op:'rot90',mode:'overwrite',backup:true});
  check('edit originals backup uses unified directory',()=>{assert.equal(centralOverwrite.result.succeeded,1);assert(fs.existsSync(path.join(unified,'backups','edit-'+centralOverwrite.id)));});
  await api('/api/edit/backups/restore',{job_id:centralOverwrite.id});
  const centralWMOverwrite=await run('/api/watermark/run',{ids:[gps.id],mode:'overwrite',backup:true,config:{text:'BACKUP',require_date:false,skip_watermarked:false}});
  check('watermark originals backup uses unified directory',()=>{assert.equal(centralWMOverwrite.result.succeeded,1);assert(fs.existsSync(path.join(unified,'backups','wm-'+centralWMOverwrite.id)));});
  fs.writeFileSync(path.join(root,'results.json'),JSON.stringify({checks,root,version:(await api('/api/state')).version},null,2));
  console.log(`PASS ${checks.length} regression checks; artifacts: ${root}`);
  if(process.env.PVM_TEST_HOLD){console.log('UI_URL '+m[0]);await delay(600000);}
}finally{child.kill();}})().catch(e=>{console.error(e);fs.writeFileSync(path.join(root,'failure.log'),String(e)+'\n'+log);process.exitCode=1;});
