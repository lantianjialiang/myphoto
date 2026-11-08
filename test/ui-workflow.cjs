const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {spawn,execFileSync}=require('node:child_process');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'gx-ui-')),photos=path.join(root,'Photos'),profile=path.join(root,'Browser');fs.mkdirSync(photos);
for(const name of ['copy1.jpg','copy2.jpg'])fs.copyFileSync(path.join(__dirname,'fixtures/gps.jpg'),path.join(photos,name));
execFileSync('powershell.exe',['-NoProfile','-Command',
  "Add-Type -AssemblyName System.Drawing; $b=New-Object System.Drawing.Bitmap(3200,1800);$g=[System.Drawing.Graphics]::FromImage($b);$g.Clear([System.Drawing.Color]::FromArgb(128,166,205));$g.FillEllipse([System.Drawing.Brushes]::LightGoldenrodYellow,2200,200,400,400);$g.FillRectangle([System.Drawing.Brushes]::DarkSlateBlue,0,1300,3200,500);$b.Save($env:GX_FIXTURE,[System.Drawing.Imaging.ImageFormat]::Png);$g.Dispose();$b.Dispose()"],
  {windowsHide:true,env:{...process.env,GX_FIXTURE:path.join(photos,'large.png')}});
const exe=process.env.PVM_EXE||path.resolve('target/release/photo-video-manager.exe');
const child=spawn(exe,['--no-window','--data-dir',path.join(root,'data')],{windowsHide:true,stdio:['ignore','pipe','pipe']});
let log='',browser,ws;child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);
const delay=ms=>new Promise(r=>setTimeout(r,ms));let checks=0;function check(name,fn){fn();checks++;console.log('PASS',name);}
(async()=>{try{
  let m;for(let i=0;i<300;i++){m=log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);if(m)break;await delay(50);}assert(m,log);
  const base='http://127.0.0.1:'+m[1],headers={'X-PVM-Token':m[2],'Content-Type':'application/json'};
  async function api(p,b){const r=await fetch(base+p,{headers,method:b===undefined?'GET':'POST',body:b===undefined?undefined:JSON.stringify(b)});const j=await r.json();assert(j.ok,JSON.stringify(j));return j.result;}
  async function job(p,b){const r=await api(p,b);for(let i=0;i<300;i++){const j=await api('/api/jobs/'+r.job_id);if(!['running','queued','cancelling'].includes(j.status)){assert.equal(j.status,'completed',JSON.stringify(j));return;}await delay(50);}throw Error('timeout');}
  const source=await api('/api/sources',{path:photos});await job('/api/sources/'+source.id+'/scan',{});await job('/api/hash',{all:true});
  let items=(await api('/api/media?limit=100')).items;
  const large=items.find(p=>p.file_name==='large.png'),dupe=items.find(p=>p.file_name.startsWith('copy'));
  const edge=['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe','C:/Program Files/Microsoft/Edge/Application/msedge.exe'].find(fs.existsSync);assert(edge,'Edge available');
  browser=spawn(edge,['--headless=new','--disable-gpu','--no-first-run','--no-default-browser-check','--remote-debugging-port=0','--user-data-dir='+profile,'--window-size=1440,1000','about:blank'],{windowsHide:true,stdio:'ignore'});
  const portFile=path.join(profile,'DevToolsActivePort');for(let i=0;i<300&&!fs.existsSync(portFile);i++)await delay(50);assert(fs.existsSync(portFile),'browser startup');
  const port=fs.readFileSync(portFile,'utf8').split('\n')[0];
  let targets=await(await fetch('http://127.0.0.1:'+port+'/json')).json();const page=targets.find(t=>t.type==='page');assert(page);
  ws=new WebSocket(page.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j;});
  let id=0;const pending=new Map(),errors=[];
  ws.onmessage=e=>{const j=JSON.parse(e.data);if(j.id){const p=pending.get(j.id);pending.delete(j.id);if(j.error)p.reject(Error(JSON.stringify(j.error)));else p.resolve(j.result);}else if(j.method==='Runtime.exceptionThrown')errors.push(j.params.exceptionDetails.text);};
  const cdp=(method,params={})=>new Promise((resolve,reject)=>{const n=++id;pending.set(n,{resolve,reject});ws.send(JSON.stringify({id:n,method,params}));});
  async function evaluate(expression){const r=await cdp('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw Error(JSON.stringify(r.exceptionDetails));return r.result.value;}
  async function until(expression){for(let i=0;i<400;i++){if(await evaluate(expression))return;await delay(50);}throw Error('UI timeout '+expression);}
  await cdp('Runtime.enable');await cdp('Page.enable');await cdp('Page.navigate',{url:base+'/?token='+m[2]});
  await until("typeof S!=='undefined' && S.state && G.items.size>0");
  await evaluate(`openDetail(${JSON.stringify(large.id)},true)`);
  await until("!$('#photo-viewer').hidden && $('#viewer-canvas img')?.naturalWidth===3200");
  const geometry=await evaluate("({w:$('#photo-viewer').getBoundingClientRect().width,h:$('#photo-viewer').getBoundingClientRect().height,iw:innerWidth,ih:innerHeight})");
  check('library photo opens in full-window viewer at original 3200px resolution',()=>{assert.equal(geometry.w,geometry.iw);assert.equal(geometry.h,geometry.ih);});
  await evaluate("$('#viewer-actual').click()");assert.equal(await evaluate("$('#viewer-canvas img').style.width"),'3200px');
  await evaluate("$('#viewer-fit').click()");
  check('100% and fit controls change actual image size',()=>{});
  await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape'});
  check('Escape closes viewer while preserving photo details',()=>{});assert.equal(await evaluate("$('#photo-viewer').hidden && S.detailId!==null"),true);
  async function act(name){await evaluate(`$('#d-body').onclick({target:document.querySelector('[data-act="${name}"]')})`);await delay(80);}
  await act('fav');assert.equal(Boolean((await api('/api/media/'+large.id)).favorite),true);
  await evaluate("$('#d-tag-input').value='UI tag'");await act('add-tag');assert((await api('/api/media/'+large.id)).tags.some(t=>t.name==='UI tag'));
  await evaluate("$('#d-date').value='2020-02-03';$('#d-time').value='04:05:06'");await act('set-date');assert((await api('/api/media/'+large.id)).taken_at.startsWith('2020-02-03'));
  await act('clear-date');assert.equal((await api('/api/media/'+large.id)).override_taken_at,null);
  await act('refresh');check('detail collection/tag/date/refresh buttons call working APIs',()=>{});
  assert.equal(await evaluate("document.querySelector('[data-act=star]')===null && document.querySelector('[data-act=mark]')===null"),true);
  assert.equal(await evaluate("Boolean(document.querySelector('#d-date').parentNode.querySelector('button'))"),true);
  await evaluate(`closeDetail();closeViewer();S.sel=new Set([${JSON.stringify(large.id)}]);updateSelBar();$('#sel-right').click()`);
  await until("!EDIT.busy && $('#library-editor').hidden");
  assert.equal((await api('/api/media/'+large.id)).width,1800);
  await evaluate("$('#sel-left').click()");await until("!EDIT.busy");assert.equal((await api('/api/media/'+large.id)).width,3200);
  check('left/right rotation stays inside library without opening editor',()=>{});
  const reveal=await(await fetch(base+'/api/reveal',{headers,method:'POST',body:JSON.stringify({file_id:'missing-test-file'})})).json();
  check('reveal endpoint is registered and reports file errors correctly',()=>assert.equal(reveal.error,'文件不存在'));
  await evaluate("closeDetail();switchView('dupes')");await until("document.querySelector('[data-dup-mark]')");
  const dupeDetails=await api('/api/media/'+dupe.id),second=dupeDetails.files[1];
  await evaluate(`document.querySelector('[data-dup-mark="${second.id}"]').click()`);
  for(let i=0;i<50;i++){const d=await api('/api/media/'+dupe.id);if(d.files.find(f=>f.id===second.id).marked)break;await delay(50);}
  const marked=await api('/api/media/'+dupe.id);
  check('duplicate keep mark affects exactly the chosen physical copy',()=>{assert.equal(Boolean(marked.files[1].marked),true);assert.equal(Boolean(marked.files[0].marked),false);});
  await until(`document.querySelector('[data-dup-delete="${second.id}"]')`);
  await evaluate(`document.querySelector('[data-dup-delete="${second.id}"]').click()`);await until("!$('#modal-root').hidden && $('#delete-confirm')");
  check('duplicate delete confirmation lists only clicked copy',()=>{});assert.equal(await evaluate("$('#delete-count').textContent"),'1');assert.equal((await evaluate("$('#delete-paths').textContent")).trim(),second.absolute_path);
  await evaluate("$('#delete-cancel').click()");assert(fs.existsSync(second.absolute_path));
  await evaluate(`document.querySelector('[data-dup-delete="${second.id}"]').click()`);await until("$('#delete-confirm')");await evaluate("$('#delete-confirm').click()");
  await until("!deletingPhotos && $('#dup-list').textContent.includes('没有发现')");
  check('duplicate delete recycles chosen copy and refreshes duplicate groups',()=>{assert.equal(fs.existsSync(second.absolute_path),false);assert(fs.existsSync(dupeDetails.files[0].absolute_path));});
  await evaluate(`S.sel=new Set([${JSON.stringify(large.id)}]);$('#wm-scope-mode').value='selected';switchView('watermark')`);
  await until("WM.samples.length && !WM.loading && document.querySelector('[data-wm-full]')");
  await evaluate("wmCfg().text='FULLSCREEN WATERMARK';wmCfg().require_date=false;wmWriteUI();wmRefreshPreviews()");
  await until("!WM.loading && document.querySelector('[data-wm-full]')");
  await evaluate("document.querySelector('button[data-wm-full]').click()");
  await until("!$('#photo-viewer').hidden && $('#viewer-title').textContent.includes('3200×1800') && $('#viewer-canvas img').naturalWidth===3200");
  check('watermark fullscreen rerenders original resolution and exposes 100% inspection',()=>{});
  const screenshot=await cdp('Page.captureScreenshot',{format:'png'});fs.writeFileSync(path.join(root,'watermark-fullscreen.png'),Buffer.from(screenshot.data,'base64'));
  await evaluate(`closeViewer();closeDetail();switchView('library');S.sel=new Set([${JSON.stringify(large.id)},${JSON.stringify(dupe.id)}]);updateSelBar()`);
  await delay(250);
  const library=await cdp('Page.captureScreenshot',{format:'png'});fs.writeFileSync(path.join(root,'library-toolbar.png'),Buffer.from(library.data,'base64'));
  assert.equal(await evaluate("document.querySelector('.topbar').getBoundingClientRect().height<110 && $('#selbar').getBoundingClientRect().height<100"),true);
  await cdp('Emulation.setDeviceMetricsOverride',{width:1100,height:900,deviceScaleFactor:1,mobile:false});await delay(200);
  assert.equal(await evaluate("document.querySelector('.topbar').getBoundingClientRect().height<110 && $('#selbar').getBoundingClientRect().height<110"),true);
  check('compact toolbar fits normal and smaller desktop widths',()=>{});
  check('no unhandled browser errors during tested interactions',()=>assert.deepEqual(errors,[]));
  console.log(`PASS ${checks} browser checks; screenshot: ${root}/watermark-fullscreen.png`);
}finally{if(ws)ws.close();browser?.kill();child.kill();}})().catch(e=>{console.error(e);console.error('Artifacts:',root);process.exitCode=1;});
