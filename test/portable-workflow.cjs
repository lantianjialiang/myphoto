// Uses a fake LOCALAPPDATA and generated folders; never reads the real user library.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os');
const {spawn}=require('node:child_process');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'gx-portable-'));
const local=path.join(root,'FakeLocal'),legacy=path.join(local,'PhotoVideoManager'),external=path.join(root,'ExternalPhotos');
fs.mkdirSync(legacy,{recursive:true});fs.mkdirSync(external);
fs.copyFileSync(path.join(__dirname,'fixtures/gps.jpg'),path.join(external,'original.jpg'));
const original=fs.readFileSync(path.join(external,'original.jpg'));
const exe=process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe');
let active; const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function start(binary,dataDir){
  const child=spawn(binary,['--no-window',...(dataDir?['--data-dir',dataDir]:[])],{windowsHide:true,env:{...process.env,LOCALAPPDATA:local},stdio:['ignore','pipe','pipe']});active=child;
  let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);
  let m;for(let i=0;i<600;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;if(child.exitCode!==null)throw Error(log);await delay(50);}assert(m,log);
  const headers={'X-PVM-Token':m[2],'Content-Type':'application/json'},base='http://127.0.0.1:'+m[1];
  const raw=async(p,b)=>{const r=await fetch(base+p,{headers,method:b===undefined?'GET':'POST',body:b===undefined?undefined:JSON.stringify(b)});return r.json();};
  const api=async(p,b)=>{const r=await raw(p,b);assert(r.ok,p+': '+JSON.stringify(r));return r.result;};
  const wait=async id=>{for(let i=0;i<1200;i++){const j=await api('/api/jobs/'+id);if(!['running','queued','cancelling'].includes(j.status))return j;await delay(50);}throw Error('job timeout');};
  return {api,raw,async run(p,b){const j=await api(p,b);const r=await wait(j.job_id);assert.equal(r.status,'completed',JSON.stringify(r));return j;},async stop(){await new Promise(r=>{child.once('exit',r);child.kill();});active=null;}};
}
let checks=0;const check=(name,fn)=>{fn();checks++;console.log('PASS',name);};
(async()=>{try{
  // Build a legacy layout using an isolated explicit data directory.
  let app=await start(exe,legacy);
  await app.api('/api/settings',{output_root:path.join(legacy,'exports')});
  const source=await app.api('/api/sources',{path:external});await app.run('/api/sources/'+source.id+'/scan',{});
  let photo=(await app.api('/api/media?limit=100')).items[0];
  const edit=await app.run('/api/edit/run',{ids:[photo.id],op:'rot90'});
  const wm=await app.run('/api/watermark/run',{ids:[photo.id],config:{text:'PORTABLE',require_date:false}});
  const outputId=(await app.api('/api/media/ids?job_id='+wm.job_id))[0];
  await app.run('/api/album/run',{ids:[photo.id]});
  await app.stop();
  const a=path.join(root,'AppA'),b=path.join(root,'AppB');fs.mkdirSync(a);fs.copyFileSync(exe,path.join(a,'Guangxia.exe'));
  app=await start(path.join(a,'Guangxia.exe'));
  let state=await app.api('/api/state');
  check('default portable data and exports stay beside executable',()=>{assert.equal(fs.realpathSync.native(state.data_dir),fs.realpathSync.native(path.join(a,'data')));assert.equal(fs.realpathSync.native(state.output_root),fs.realpathSync.native(path.join(a,'exports')));});
  const imported=await app.api('/api/media/'+outputId);
  check('legacy snapshot imports generated photos and albums without deleting originals',()=>{
    assert(imported.files[0].absolute_path.toLowerCase().includes('appa\\exports\\watermark'));
    assert(fs.existsSync(imported.files[0].absolute_path));assert(fs.existsSync(path.join(a,'exports/albums/index.html')));assert(fs.existsSync(path.join(legacy,'library.db')));
  });
  await app.api('/api/edit/backups/restore',{job_id:edit.job_id});
  check('imported editing backup can restore external original',()=>assert.deepEqual(fs.readFileSync(path.join(external,'original.jpg')),original));
  check('output outside program folder is rejected',()=>{});
  assert.equal((await app.raw('/api/settings',{output_root:path.join(root,'Outside')})).ok,false);
  assert.equal((await app.raw('/api/album/run',{ids:[photo.id],out_dir:path.join(root,'Outside')})).ok,false);
  await app.api('/api/settings',{output_root:'exports'});
  const internal=path.join(a,'Media');fs.mkdirSync(internal);fs.writeFileSync(path.join(internal,'inside.jpg'),Buffer.concat([original,Buffer.from('inside')]));
  const is=await app.api('/api/sources',{path:internal});await app.run('/api/sources/'+is.id+'/scan',{});
  const inside=(await app.api('/api/media?limit=100')).items.find(p=>p.file_name==='inside.jpg');
  const edited=await app.run('/api/edit/run',{ids:[outputId],op:'rot90'});
  const before=fs.readFileSync(imported.files[0].absolute_path);
  const plan=await app.api('/api/archive/plan',{asset_ids:[inside.id],mode:'copy'});
  const fonts=path.join(a,'data/fonts');fs.mkdirSync(fonts,{recursive:true});fs.writeFileSync(path.join(fonts,'fixture.ttf'),'test path only');
  await app.api('/api/settings',{test_paths:{font_path:path.join(fonts,'fixture.ttf')}});
  await app.stop();
  // Verify both absolute targets are inside this synthetic test workspace before moving.
  assert(path.resolve(a).startsWith(path.resolve(root)+path.sep));assert(path.resolve(b).startsWith(path.resolve(root)+path.sep));
  fs.renameSync(a,b);
  app=await start(path.join(b,'Guangxia.exe'));
  state=await app.api('/api/state');
  const moved=await app.api('/api/media/'+outputId),movedInside=await app.api('/api/media/'+inside.id);
  check('moving complete program folder rebases internal sources and generated outputs',()=>{
    assert(moved.files[0].absolute_path.includes('AppB'));assert(fs.existsSync(moved.files[0].absolute_path));assert(fs.existsSync(movedInside.files[0].absolute_path));assert.equal(JSON.parse(state.settings.test_paths).font_path.includes('AppB'),true);
  });
  assert.equal((await app.api('/api/media/'+photo.id)).files[0].absolute_path.toLowerCase().includes('externalphotos'),true);
  await app.api('/api/edit/backups/restore',{job_id:edited.job_id});
  check('editing backup still restores after migration',()=>assert.notDeepEqual(fs.readFileSync(moved.files[0].absolute_path),before));
  await app.run('/api/archive/plans/'+plan.plan_id+'/run',{});
  check('saved archive plan writes under moved program root',()=>assert(fs.existsSync(path.join(b,'exports/archive'))));
  await app.run('/api/album/run',{ids:[inside.id]});
  check('new output after migration remains within moved exports',()=>assert(fs.existsSync(path.join(b,'exports/albums (2)/index.html'))));
  await app.stop();console.log(`PASS ${checks} portable checks; fixtures: ${root}`);
}finally{active?.kill();}})().catch(e=>{console.error(e);process.exitCode=1;});
