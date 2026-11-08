const fs=require('fs'),vm=require('vm'),assert=require('assert/strict');
const source=fs.readFileSync('web/app.js','utf8');
function fn(start,end){return source.slice(source.indexOf(start),source.indexOf(end,source.indexOf(start)+1));}
function el(){return {hidden:true,value:'stale per-task path',dataset:{},children:[],classList:{add(){},remove(){}},parentNode:{buttons:[],querySelector(){return this.buttons[0]},appendChild(b){this.buttons.push(b)}}};}
const elements=new Map();const $=id=>{if(!elements.has(id))elements.set(id,el());return elements.get(id);};
const ctx=vm.createContext({S:{settings:{output_root:'D:/Unified'},state:{data_dir:'D:/Data'},sel:new Set(),filter:{total:1000}},EDIT:{ids:['photo1','photo2']},document:{createElement:()=>({dataset:{}}),addEventListener(){}},$,toast(){},layout(){},closeDetail(){},loadEditView(){},switchView(v){ctx.S.view=v;},apiGet:async p=>({media_type:p.endsWith('video')?'video':'image'})});
vm.runInContext(fn('function outputRoot()','async function openLibraryEditor'),ctx);
vm.runInContext(fn('function editScope()','function editSyncMode'),ctx);
vm.runInContext(fn('async function openLibraryEditor','async function followLibraryEdit'),ctx);
vm.runInContext('syncOutputFields()',ctx);
assert.equal($('#wm-out').value,'D:/Unified/watermark');assert.equal($('#al-out').value,'D:/Unified/albums');assert.equal($('#edit-out').readOnly,true);
console.log('PASS every task shows configured output without re-entry');
vm.runInContext('syncOutputFields()',ctx);assert.equal($('#wm-out').parentNode.buttons.length,1);ctx.S.settings.output_root='E:/NewRoot';vm.runInContext('syncOutputFields()',ctx);assert.equal($('#edit-out').value,'E:/NewRoot/edited');
console.log('PASS changing output root updates existing fields without duplicate controls');
ctx.EDIT.ids=[];assert.equal(vm.runInContext('editScope().count',ctx),0);ctx.EDIT.ids=['selected'];const scope=vm.runInContext('editScope()',ctx);scope.ids.push('other');assert.equal(ctx.EDIT.ids.length,1);console.log('PASS editor never falls back to whole library or shares mutable selection');
(async()=>{await vm.runInContext('openLibraryEditor(["photo1","video"])',ctx);assert.equal(ctx.S.view,'library');assert.equal(ctx.EDIT.ids.length,1);assert.equal(ctx.EDIT.ids[0],'photo1');assert.equal($('#library-editor').hidden,false);console.log('PASS editor opens inside library with only selected photos');vm.runInContext('closeLibraryEditor()',ctx);assert.equal($('#library-editor').hidden,true);assert.equal(ctx.S.view,'library');console.log('PASS closing editor stays in media library');})().catch(e=>{console.error(e);process.exitCode=1;});
