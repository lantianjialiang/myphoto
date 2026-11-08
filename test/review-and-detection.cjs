// Real HTTP/database/image workflows using isolated synthetic files only.
const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {spawn,execFileSync}=require('node:child_process'),{createHash}=require('node:crypto');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'gx-review-')),photos=path.join(root,'Photos');fs.mkdirSync(photos);
execFileSync('powershell.exe',['-NoProfile','-Command',
  'Add-Type -AssemblyName System.Drawing; $b=New-Object System.Drawing.Bitmap(1200,800);$g=[System.Drawing.Graphics]::FromImage($b);$g.Clear([System.Drawing.Color]::FromArgb(40,70,110));$b.Save($env:GX_FIXTURE,[System.Drawing.Imaging.ImageFormat]::Jpeg);$g.Dispose();$b.Dispose()'],
  {windowsHide:true,env:{...process.env,GX_FIXTURE:path.join(photos,'plain.jpg')}});
const jpeg=fs.readFileSync(path.join(photos,'plain.jpg'));
for(let i=0;i<86;i++){
  const p=path.join(photos,'case_'+i+'.jpg');fs.writeFileSync(p,Buffer.concat([jpeg,Buffer.from('case '+i)]));
  const t=new Date(Date.UTC(2020,5,1,0,i));fs.utimesSync(p,t,t);
}
const digest=p=>createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const before=new Map(fs.readdirSync(photos).map(n=>[n,digest(path.join(photos,n))]));
const child=spawn(process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe'),['--no-window','--data-dir',path.join(root,'data')],{windowsHide:true,stdio:['ignore','pipe','pipe']});
let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);
const delay=ms=>new Promise(r=>setTimeout(r,ms));let checks=0;function check(name,fn){fn();checks++;console.log('PASS',name);}
(async()=>{try{
  let m;for(let i=0;i<300;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;await delay(50);}assert(m,log);
  const base='http://127.0.0.1:'+m[1],headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
  async function response(p,b){return (await fetch(base+p,{headers,method:b===undefined?'GET':'POST',body:b===undefined?undefined:JSON.stringify(b)})).json();}
  async function api(p,b){const j=await response(p,b);assert(j.ok,p+': '+JSON.stringify(j));return j.result;}
  async function job(p,b){const r=await api(p,b);for(let i=0;i<600;i++){const j=await api('/api/jobs/'+r.job_id);if(!['running','queued','cancelling'].includes(j.status)){assert.equal(j.status,'completed',JSON.stringify(j));return {...j,id:r.job_id};}await delay(50);}throw Error('timeout');}
  const source=await api('/api/sources',{path:photos});await job('/api/sources/'+source.id+'/scan',{});
  const review=await api('/api/review?limit=80'),ids=await api('/api/review/ids?mode=all');
  check('review pagination and select all include pending items beyond the first page',()=>{assert.equal(review.total,87);assert.equal(review.items.length,80);assert.equal(ids.length,87);});
  assert.deepEqual(await api('/api/watermark/scope',{ids:[],video:false}),[]);
  assert.equal((await api('/api/watermark/scope',{ids:[...ids,ids[0],'missing-id'],video:false})).length,87);
  check('watermark selection scope uses explicit IDs and never falls back to the entire library',()=>{});
  const expected=new Map();for(const id of ids){const a=await api('/api/media/'+id);expected.set(id,JSON.parse(a.files[0].candidates_json).find(c=>c.source==='filesystem').value);}
  const invalid=await response('/api/media/batch-date-source',{source:'filesystem'});assert.equal(invalid.ok,false);
  const confirmed=await api('/api/media/batch-date-source',{ids:[...ids,ids[0],'missing-test-id'],source:'filesystem'});
  check('filesystem confirmation deduplicates IDs, skips missing files and updates all selected items',()=>{assert.equal(confirmed.updated,87);assert.equal(confirmed.skipped,1);});
  for(const id of ids){const a=await api('/api/media/'+id);assert.equal(a.taken_at,expected.get(id));assert.equal(a.needs_review,false);}
  for(const mode of ['all','low','unknown','conflict']){assert.equal((await api('/api/review?mode='+mode)).total,0);assert.equal((await api('/api/review/ids?mode='+mode)).length,0);}
  assert.equal((await api('/api/state')).needs_review,0);
  check('each photo keeps its own date, and pending lists plus sidebar count refresh',()=>{});
  await job('/api/sources/'+source.id+'/scan',{});assert.equal((await api('/api/review')).total,0);
  for(const [n,h] of before)assert.equal(digest(path.join(photos,n)),h);
  check('confirmed dates survive rescan without changing source photo bytes',()=>{});
  const plain=(await api('/api/media?limit=200')).items.find(a=>a.file_name==='plain.jpg');
  await api('/api/media/'+plain.id+'/date',{date:null});assert.equal((await api('/api/review')).total,1);
  await api('/api/media/batch-date',{ids:[plain.id],date:null,unknown:true});
  await job('/api/sources/'+source.id+'/scan',{});
  const unknown=await api('/api/media/'+plain.id);
  check('explicit unknown date is confirmed and stays out of review after rescan',()=>{assert.equal(unknown.taken_at,null);assert.equal(unknown.needs_review,false);assert.equal((unknown.override_taken_at),'__confirmed_unknown__');});
  assert.equal((await api('/api/review')).total,0);
  await api('/api/media/'+plain.id+'/date',{date:'2024-09-16T13:33'});
  assert.equal((await api('/api/media/'+plain.id)).taken_at,'2024-09-16T13:33:00');
  check('native time inputs without seconds save successfully',()=>{});
  const raw=await api('/api/watermark/detect',{id:plain.id});assert.equal(raw.suspected,false);assert.equal(raw.known,false);
  const cfg={text:'2024-09-16 13:33:00',require_date:false,video:false,color:'#FFFFFF',stroke:0,shadow:0,size_pct:4,position:'br'};
  const preview=await api('/api/watermark/preview',{id:plain.id,config:cfg,max_side:0});
  const externalPath=path.join(photos,'external_stamp.jpg');fs.writeFileSync(externalPath,Buffer.from(preview.image.split(',')[1],'base64'));
  await job('/api/sources/'+source.id+'/scan',{});
  const external=(await api('/api/media?limit=200')).items.find(a=>a.file_name==='external_stamp.jpg');assert(external);
  const detected=await api('/api/watermark/detect',{id:external.id});
  check('offline detector distinguishes a clean photo from an external bottom timestamp',()=>{assert.equal(detected.suspected,true,JSON.stringify(detected));assert.equal(detected.known,false);assert(detected.regions.length);});
  const normal=await api('/api/watermark/preview',{id:external.id,config:cfg,max_side:800});assert.equal(Boolean(normal.watermarked),false);assert(normal.font_px>0);assert(normal.warning.includes('疑似'));
  const cautious=await api('/api/watermark/preview',{id:external.id,config:{...cfg,skip_suspected:true},max_side:800});assert.equal(cautious.font_px,0);
  const skipped=await job('/api/watermark/run',{ids:[external.id],config:{...cfg,skip_suspected:true},out_dir:path.join(root,'Skipped')});assert.equal(skipped.skipped,1);assert.equal(skipped.succeeded,0);
  assert.equal((await api('/api/media?watermarked=1')).total,0);
  check('suspected watermark is a warning by default; explicit opt-in skips without silently marking it',()=>{});
  await api('/api/watermark/mark',{ids:[external.id],marked:true});assert.equal((await api('/api/watermark/detect',{id:external.id})).known,true);
  await api('/api/watermark/mark',{ids:[external.id],marked:false});
  assert.equal((await api('/api/watermark/detect',{id:external.id})).suspected,true);
  const stat=fs.statSync(externalPath);assert(jpeg.length<stat.size);
  fs.writeFileSync(externalPath,Buffer.concat([jpeg,Buffer.alloc(stat.size-jpeg.length)]));fs.utimesSync(externalPath,stat.atime,stat.mtime);
  const replaced=await api('/api/watermark/detect',{id:external.id});
  check('detection cache invalidates by actual content even when file size and time stay the same',()=>{assert.equal(replaced.suspected,false);assert.equal(replaced.known,false);});
  console.log(`PASS ${checks} integration checks; artifacts: ${root}`);
}finally{child.kill();}})().catch(e=>{console.error(e);console.error('Artifacts:',root);process.exitCode=1;});
