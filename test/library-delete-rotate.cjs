// End-to-end with generated test copies only; never indexes user photos.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const {spawn,execFileSync} = require('node:child_process');
const {createHash} = require('node:crypto');
const root = fs.mkdtempSync(path.join(os.tmpdir(),'gx-delete-rotate-'));
const photos = path.join(root,'Photos'); fs.mkdirSync(photos);
const fixture = fs.readFileSync(path.join(__dirname,'fixtures/gps.jpg'));
for (const name of ['single.jpg','duplicate1.jpg','duplicate2.jpg','changed.jpg']) {
  fs.writeFileSync(path.join(photos,name), Buffer.concat([fixture,Buffer.from(name.startsWith('duplicate')?'dupe':name)]));
}
fs.writeFileSync(path.join(photos,'video.mp4'),'synthetic invalid video');
const exe = process.env.PVM_EXE || path.resolve(__dirname,'../target/release/photo-video-manager.exe');
const child = spawn(exe,['--no-window','--data-dir',path.join(root,'data')],{windowsHide:true,stdio:['ignore','pipe','pipe']});
let log=''; child.stdout.on('data',b=>log+=b); child.stderr.on('data',b=>log+=b);
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const hash=p=>createHash('sha256').update(fs.readFileSync(p)).digest('hex');
let checks=0; function check(name,fn){fn();checks++;console.log('PASS',name);}
(async()=>{try {
  let m; for(let n=0;n<200;n++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;if(child.exitCode!==null)throw Error(log);await delay(50);}
  assert(m,'server startup');
  const base='http://127.0.0.1:'+m[1], headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
  async function response(p,body){return (await fetch(base+p,{headers,method:body===undefined?'GET':'POST',body:body===undefined?undefined:JSON.stringify(body)})).json();}
  async function api(p,b){const j=await response(p,b);assert(j.ok,p+': '+JSON.stringify(j));return j.result;}
  async function wait(id){for(let n=0;n<1200;n++){const j=await api('/api/jobs/'+id);if(!['running','queued','cancelling'].includes(j.status))return j;await delay(50);}throw Error('job timeout '+id);}
  async function run(p,b){const j=await api(p,b);return {id:j.job_id,result:await wait(j.job_id)};}
  const src=await api('/api/sources',{path:photos}); await run('/api/sources/'+src.id+'/scan',{});
  await run('/api/hash',{all:true});
  let items=(await api('/api/media?limit=100')).items;
  const single=items.find(a=>a.file_name==='single.jpg');
  const orig=await api('/api/media/'+single.id), origFile=orig.files[0], origHash=hash(origFile.absolute_path);
  const output=path.join(root,'Unified'); await api('/api/settings',{output_root:output});
  const first=await run('/api/edit/run',{ids:[single.id],op:'rot90'});
  let current=await api('/api/media/'+single.id);
  check('default rotation changes existing file and keeps library count',()=>{
    assert.equal(first.result.succeeded,1);assert.equal(current.files[0].absolute_path,origFile.absolute_path);
    assert.equal(current.width,orig.height);assert.equal(current.height,orig.width);assert.notEqual(hash(origFile.absolute_path),origHash);
    assert.equal(fs.readdirSync(photos).length,5);assert.equal(fs.existsSync(path.join(output,'edited')),false);
    assert.equal(fs.readdirSync(path.join(output,'backups','edit-'+first.id)).length,1);
  });
  const second=await run('/api/edit/run',{ids:[single.id],op:'rot90'});
  current=await api('/api/media/'+single.id);
  const rotatedCount=(await api('/api/media?limit=100')).total;
  check('consecutive rotation stays on the same file without a new image',()=>{
    assert.equal(second.result.succeeded,1);assert.equal(current.files[0].id,origFile.id);assert.equal(current.width,orig.width);
    assert.equal((fs.readdirSync(photos)).length,5);assert.equal(rotatedCount,items.length);
  });
  await api('/api/edit/backups/restore',{job_id:first.id});
  check('automatic original backup restores exact bytes',()=>assert.equal(hash(origFile.absolute_path),origHash));
  const merged=items.find(a=>a.file_name.startsWith('duplicate'));
  const dupe=await api('/api/media/'+merged.id); assert.equal(dupe.files.length,2);
  const untouchedHash=hash(dupe.files[1].absolute_path);
  await run('/api/edit/run',{ids:[merged.id],op:'rot90'});
  const editedId=(await api('/api/media/by-file/'+dupe.files[0].id)).id;
  check('rotated duplicate can be reselected by file identity',()=>{
    assert.notEqual(editedId,merged.id);assert.equal(hash(dupe.files[1].absolute_path),untouchedHash);
  });
  await run('/api/edit/run',{ids:[editedId],op:'rot90'});
  check('second rotation edits the changed duplicate, leaving its sibling intact',()=>assert.equal(hash(dupe.files[1].absolute_path),untouchedHash));
  const noIds=await response('/api/media/trash/plan',{filter:{}});
  const video=items.find(a=>a.media_type==='video');
  const noVideo=await response('/api/media/trash/plan',{ids:[video.id]});
  const noPlan=await response('/api/media/trash/run',{ids:[single.id]});
  check('delete requires explicit photo selection and a confirmation plan',()=>{assert.equal(noIds.ok,false);assert.equal(noVideo.ok,false);assert.equal(noPlan.ok,false);});
  const plan=await api('/api/media/trash/plan',{ids:[single.id,single.id]});
  check('delete preview lists exact paths, deduplicates, and does not delete',()=>{
    assert.equal(plan.count,1);assert.equal(plan.files[0].path,origFile.absolute_path);assert(fs.existsSync(origFile.absolute_path));
  });
  const changed=items.find(a=>a.file_name==='changed.jpg'), changedPath=path.join(photos,'changed.jpg');
  const stale=await api('/api/media/trash/plan',{ids:[changed.id]}); fs.appendFileSync(changedPath,'changed after confirmation');
  const skipped=await run('/api/media/trash/run',{plan_id:stale.plan_id});
  check('file changed after confirmation remains untouched and indexed',()=>{
    assert.equal(skipped.result.failed,1);assert(fs.existsSync(changedPath));assert(skipped.result.message.includes('确认后'));
  }); await api('/api/media/'+changed.id);
  const deleted=await run('/api/media/trash/run',{plan_id:plan.plan_id});
  check('confirmed delete actually recycles synthetic file and removes library record',()=>{
    assert.equal(deleted.result.succeeded,1,JSON.stringify(deleted.result));assert.equal(fs.existsSync(origFile.absolute_path),false);
  });
  const missing=await response('/api/media/'+single.id), replay=await response('/api/media/trash/run',{plan_id:plan.plan_id});
  check('deleted entry is gone and a confirmation cannot be replayed',()=>{assert.equal(missing.ok,false);assert.equal(replay.ok,false);});
  check('photo deletion preserves editing backups and unrelated files',()=>{
    assert(fs.existsSync(path.join(output,'backups','edit-'+first.id,'single.jpg')));assert(fs.existsSync(changedPath));assert(fs.existsSync(dupe.files[1].absolute_path));
  });
  // Fresh identical copies: default plan deletes only the displayed file.
  const dupDir=path.join(root,'Copies');fs.mkdirSync(dupDir);
  for(const n of ['copy1.jpg','copy2.jpg'])fs.writeFileSync(path.join(dupDir,n),Buffer.concat([fixture,Buffer.from('fresh copies')]));
  const source2=await api('/api/sources',{path:dupDir});await run('/api/sources/'+source2.id+'/scan',{});await run('/api/hash',{all:true});
  const copy=(await api('/api/media?limit=100')).items.find(a=>a.file_name.startsWith('copy'));
  const copies=await api('/api/media/'+copy.id);
  const primary=await api('/api/media/trash/plan',{ids:[copy.id]});
  const all=await api('/api/media/trash/plan',{ids:[copy.id],all_copies:true});
  check('duplicate deletion is one file by default, all copies only on request',()=>{assert.equal(primary.count,1);assert.equal(all.count,2);});
  const del1=await run('/api/media/trash/run',{plan_id:primary.plan_id});
  const remaining=await api('/api/media/'+copy.id);
  check('deleting one duplicate retains its remaining file and updates primary',()=>{
    assert.equal(del1.result.succeeded,1);assert.equal(remaining.files.length,1);assert.equal(remaining.files[0].id,copies.files[1].id);
  });
  const last=await api('/api/media/trash/plan',{ids:[copy.id],all_copies:true});
  const del2=await run('/api/media/trash/run',{plan_id:last.plan_id});
  check('last duplicate deletion removes empty asset',()=>assert.equal(del2.result.succeeded,1));
  assert.equal((await response('/api/media/'+copy.id)).ok,false);
  // An output root inside the scanned library must not import automatic backups.
  const inside=path.join(photos,'Outputs');await api('/api/settings',{output_root:inside});
  const beforeScan=(await api('/api/state')).files;
  await run('/api/edit/run',{ids:[changed.id],op:'rot90'});
  await run('/api/sources/'+src.id+'/scan',{});
  check('scanning a nested output root does not import automatic backups',()=>assert.equal(fs.existsSync(path.join(inside,'backups')),true));
  assert.equal((await api('/api/state')).files,beforeScan);
  await api('/api/settings',{output_root:path.join(root,'MovedOutput')});
  await run('/api/sources/'+src.id+'/scan',{});
  check('old backup directories stay excluded after changing output root',()=>assert(fs.existsSync(changedPath)));
  assert.equal((await api('/api/state')).files,beforeScan);
  const recycled=execFileSync('powershell.exe',['-NoProfile','-Command',
    "$s = New-Object -ComObject Shell.Application; @($s.Namespace(10).Items() | Where-Object { $_.ExtendedProperty('System.Recycle.DeletedFrom') -eq $env:GX_TEST_PHOTOS } | ForEach-Object { $_.Name }) | ConvertTo-Json -Compress"],
    {windowsHide:true,encoding:'utf8',env:{...process.env,GX_TEST_PHOTOS:fs.realpathSync.native(photos)}});
  check('deleted test photo is present in Windows Recycle Bin',()=>assert(recycled.includes('single')));
  console.log(`PASS ${checks} deletion and rotation checks; fixtures: ${root}`);
} finally {child.kill();}})().catch(e=>{console.error(e);fs.writeFileSync(path.join(root,'failure.log'),String(e)+'\n'+log);process.exitCode=1;});
