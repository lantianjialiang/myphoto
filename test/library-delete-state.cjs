const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const source=fs.readFileSync('web/app.js','utf8');
const elements=new Map(),calls=[];
const $=id=>{if(!elements.has(id))elements.set(id,{isConnected:true,disabled:false,checked:false,innerHTML:'',textContent:''});return elements.get(id);};
let refresh=0,closedEditor=0,closedDetail=0,closedModal=0;
const ctx=vm.createContext({$,Set,Promise,EDIT:{busy:false,open:false,ids:[]},S:{sel:new Set(['original']),detailId:null},toast(){},
  esc:s=>String(s).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;'),
  modal(html){ctx.html=html;},closeModal(){closedModal++;},closeDetail(){closedDetail++;ctx.S.detailId=null;},closeLibraryEditor(){closedEditor++;ctx.EDIT.open=false;},
  async loadState(){},async loadFacets(){},async loadMedia(){refresh++;},loadJobs(){},
  api:async(p,opts)=>{calls.push({p,body:opts.body});if(p.endsWith('/plan'))return {plan_id:opts.body.all_copies?'all':'primary',count:opts.body.all_copies?2:1,files:[{path:'C:/Photos/<test>&.jpg'}]};return {job_id:'delete-job'};},
  apiGet:async()=>({status:'completed',message:'done',failed:0}),
});
vm.runInContext(source.slice(source.indexOf('let deletingPhotos = false;'),source.indexOf("$('#wm-font-browse').onclick")),ctx);
(async()=>{
  await vm.runInContext('openDeletePhotos([])',ctx);assert.equal(calls.length,0);console.log('PASS empty selection never falls back to library filter');
  await vm.runInContext('openDeletePhotos(["original"])',ctx);
  assert.equal($('#delete-paths').innerHTML.includes('&lt;test&gt;&amp;'),true);
  $('#delete-cancel').onclick();assert.equal(closedModal,1);assert.equal(calls.some(c=>c.p.endsWith('/run')),false);
  console.log('PASS cancel previews exact escaped paths without submitting deletion');
  await vm.runInContext('openDeletePhotos(["original"])',ctx);
  ctx.S.sel=new Set(['later-selection']);$('#delete-copies').checked=true;
  await $('#delete-copies').onchange();
  assert.equal(calls.at(-1).body.ids[0],'original');assert.equal(calls.at(-1).body.all_copies,true);assert.equal($('#delete-count').textContent,2);
  console.log('PASS all-copies option replans the original selection snapshot');
  ctx.S.detailId='original';ctx.EDIT.open=true;ctx.EDIT.ids=['original'];
  await Promise.all([$('#delete-confirm').onclick(),$('#delete-confirm').onclick()]);
  assert.equal(calls.filter(c=>c.p.endsWith('/run')).length,1);assert.equal(calls.at(-1).body.plan_id,'all');
  assert.equal(refresh,1);assert.equal(closedEditor,1);assert.equal(closedDetail,1);assert(ctx.S.sel.has('later-selection'));
  console.log('PASS confirmation runs once, closes stale previews and refreshes library');
  // Refresh the selected asset by physical file identity if a merged duplicate splits during editing.
  ctx.loadEditView=()=>{};ctx.EDIT.open=true;
  ctx.apiGet=async p=>p.includes('/by-file/')?{id:'edited-asset'}:{status:'completed',message:'done',failed:0};
  vm.runInContext(source.slice(source.indexOf('async function followLibraryEdit('),source.indexOf("$('#view-library').appendChild")),ctx);
  await vm.runInContext('followLibraryEdit("edit-job",["physical-file"])',ctx);
  assert.equal(ctx.EDIT.ids[0],'edited-asset');assert(ctx.S.sel.has('edited-asset'));assert.equal(ctx.EDIT.busy,false);
  console.log('PASS rotation reselects the edited physical file for the next operation');
})().catch(e=>{console.error(e);process.exitCode=1;});
