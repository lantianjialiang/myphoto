// Exercise picker state transitions without opening desktop dialogs.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const src = fs.readFileSync(path.join(__dirname,'../web/app.js'),'utf8');
const block = src.slice(src.indexOf('let folderPickerPending'),src.indexOf('/* ============================ 设置'));
class Element {
  constructor(){this.isConnected=true;this.disabled=false;this.value='';this.style={};this.children=[];this.listeners={};}
  set innerHTML(html){this.html=html;for(const id of ['pp-path','pp-ok','pp-cancel'])this.children[id]=new Element();this.children['pp-path'].value='';}
  querySelector(s){return this.children[s.slice(1)];}
  focus(){document.activeElement=this;}
  remove(){this.isConnected=false;document.body.layers=document.body.layers.filter(x=>x!==this);}
  addEventListener(k,f){this.listeners[k]=f;}
  dispatchEvent(){}
}
const originalForm = new Element(); originalForm.value='保留标题与主题';
const output = new Element(); output.value='D:/原路径';
const browse = new Element();
const document = {activeElement:browse,createElement:()=>new Element(),body:{layers:[],appendChild(el){this.layers.push(el);}}};
let apiCall,requests=0;
const context=vm.createContext({document,Event:class{},toast(){},esc:x=>String(x),
  $:(s,r)=>r?r.querySelector(s):(s==='#al-out'?output:originalForm),$$:()=>[browse],
  api:async(...args)=>{requests++;return apiCall(...args);}});
vm.runInContext(block,context);
const run=s=>vm.runInContext(s,context);
const tick=()=>new Promise(r=>setImmediate(r));
(async()=>{
  let release;
  apiCall=()=>new Promise(r=>release=r);
  const first=run('pickInto("#al-out")'); await tick();
  assert.equal(browse.disabled,true);
  await run('pickFolder()');assert.equal(requests,1,'repeat click must not open another dialog');
  release({path:'D:/中文照片'});await first;
  assert.equal(output.value,'D:/中文照片');assert.equal(browse.disabled,false);
  console.log('PASS repeat click guarded and Unicode path applied');
  apiCall=async()=>({path:null});await run('pickInto("#al-out")');
  assert.equal(output.value,'D:/中文照片');console.log('PASS native cancel preserves existing path');
  apiCall=async()=>{throw Error('native dialog unavailable');};
  const fallback=run('pickInto("#al-out")');await tick();
  assert.equal(document.body.layers.length,1);const overlay=document.body.layers[0];
  assert.equal(originalForm.isConnected,true);assert.equal(originalForm.value,'保留标题与主题');
  overlay.querySelector('#pp-path').value='D:/手动目录';overlay.querySelector('#pp-ok').onclick();await fallback;
  assert.equal(output.value,'D:/手动目录');assert.equal(document.body.layers.length,0);
  assert.equal(originalForm.value,'保留标题与主题');console.log('PASS manual fallback preserves original form and applies path');
  const cancelled=run('pickInto("#al-out")');await tick();document.body.layers[0].querySelector('#pp-cancel').onclick();await cancelled;
  assert.equal(output.value,'D:/手动目录');assert.equal(browse.disabled,false);console.log('PASS manual cancel resolves and restores button');
  apiCall=()=>new Promise(r=>release=r);const late=run('pickInto("#al-out")');await tick();output.isConnected=false;release({path:'D:/迟到结果'});await late;
  assert.equal(output.value,'D:/手动目录');console.log('PASS late result ignored after form closes');
})().catch(e=>{console.error(e);process.exitCode=1;});
