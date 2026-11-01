// V1.1 能力端到端测试：标签、星标、时间偏移、相似照片、实况配对、移动与撤销、目录监控。
// 运行前先启动一个 --no-window 实例，并保证 PVM_TEST_ROOT 指向其数据目录的父目录。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-v11');
const PHOTOS = path.join(ROOT, 'Photos');
const FIXTURES = path.join(__dirname, 'fixtures', 'v11');
const logPath = path.join(ROOT, 'server.log');
if (!fs.existsSync(logPath)) {
  console.error('找不到 ' + logPath + '，请先启动实例并把输出重定向到该文件');
  process.exit(2);
}
const log = fs.readFileSync(logPath, 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) {
  console.error('无法从日志中解析地址与令牌');
  process.exit(2);
}
const BASE = 'http://127.0.0.1:' + m[1];
const TOKEN = m[2];
const H = { 'X-PVM-Token': TOKEN, 'Content-Type': 'application/json' };
const sleep = ms => new Promise(r => setTimeout(r, ms));

let pass = 0, fail = 0;
function check(name, cond, extra = '') {
  if (cond) { pass++; console.log('  ✓ ' + name + (extra ? ' — ' + extra : '')); }
  else { fail++; console.log('  ✗ ' + name + (extra ? ' — ' + extra : '')); }
}
function section(t) { console.log('\n== ' + t); }

async function call(p, opt = {}) {
  const r = await fetch(BASE + p, {
    method: opt.method || 'GET',
    headers: H,
    body: opt.body ? JSON.stringify(opt.body) : undefined,
  });
  const j = await r.json();
  if (j.ok === false) throw new Error(p + ' -> ' + j.error);
  return j.result;
}

async function waitJob(id, rounds = 150) {
  for (let i = 0; i < rounds; i++) {
    await sleep(300);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running' && j.status !== 'cancelling') return j;
  }
  throw new Error('任务超时：' + id);
}

function prepareFixtures() {
  fs.mkdirSync(PHOTOS, { recursive: true });
  for (const f of fs.readdirSync(FIXTURES)) {
    const dst = path.join(PHOTOS, f);
    if (!fs.existsSync(dst)) fs.copyFileSync(path.join(FIXTURES, f), dst);
  }
}

(async () => {
  prepareFixtures();
  section('索引');
  const known = await call('/api/sources');
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
  const existing = known.find(s => norm(s.root_path) === norm(PHOTOS))
    || known.find(s => norm(s.root_path).endsWith('/photos'));
  const src = existing || (await call('/api/sources', { method: 'POST', body: { path: PHOTOS } }));
  const scanJob = await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
  const sj = await waitJob(scanJob.job_id);
  const media = await call('/api/media?limit=100');
  check('扫描完成', sj.status === 'completed', sj.message);
  check('索引了 7 个文件', media.total >= 7, media.total + ' 项');

  const byName = n => media.items.find(i => i.file_name === n);
  const va = byName('variant-a.jpg'), vb = byName('variant-b.jpg'), vc = byName('variant-c.jpg');
  const live = byName('IMG_LIVE.JPG'), dated = byName('IMG_20210503_141500.jpg');
  check('测试样例齐全', !!(va && vb && vc && live && dated));

  section('标签');
  await call('/api/tags', { method: 'POST', body: { name: '旅行' } });
  let r = await call('/api/media/batch-tags', { method: 'POST', body: { ids: [va.id, vb.id], tags: ['旅行'], mode: 'add' } });
  check('批量打标签', r.updated === 2, r.updated + ' 项');
  const tags = await call('/api/tags');
  const travel = tags.find(t => t.name === '旅行');
  check('标签用量正确', travel && travel.count === 2, travel ? travel.count + ' 项' : '(缺失)');
  const tagged = await call('/api/media?tag=' + encodeURIComponent('旅行'));
  check('按标签筛选', tagged.total === 2, tagged.total + ' 项');
  const detail = await call('/api/media/' + va.id);
  check('详情包含标签', (detail.tags || []).some(t => t.name === '旅行'));
  await call('/api/media/batch-tags', { method: 'POST', body: { ids: [va.id], tags: ['旅行'], mode: 'remove' } });
  const after = await call('/api/media?tag=' + encodeURIComponent('旅行'));
  check('移除标签生效', after.total === 1, after.total + ' 项');

  section('星标与收藏');
  await call('/api/media/batch-flags', { method: 'POST', body: { ids: [va.id, vb.id], star: true } });
  await call('/api/media/batch-flags', { method: 'POST', body: { ids: [vc.id], favorite: true } });
  const starred = await call('/api/media?star=1');
  check('按星标筛选', starred.total === 2, starred.total + ' 项');
  const faved = await call('/api/media?favorite=1');
  check('按收藏筛选', faved.total === 1, faved.total + ' 项');
  check('列表带星标字段', starred.items.every(i => i.star === true));

  section('时间偏移');
  const before = dated.taken_at;
  const shifted = await call('/api/media/shift', { method: 'POST', body: { ids: [dated.id], minutes: 90 } });
  const afterShift = await call('/api/media/' + dated.id);
  const delta = (new Date(afterShift.taken_at.replace(' ', 'T')) - new Date(before.replace(' ', 'T'))) / 60000;
  check('偏移写入人工覆盖', afterShift.date_source === 'override', afterShift.date_source + ' / ' + afterShift.taken_at);
  check('偏移量正确', delta === 90, before + ' → ' + afterShift.taken_at);
  check('计入了修改数', shifted.updated === 1);

  section('相似照片');
  const hashJob = await call('/api/similar/hash', { method: 'POST', body: {} });
  const hj = await waitJob(hashJob.job_id, 300);
  check('相似指纹任务完成', hj.status === 'completed', hj.message);
  const sim = await call('/api/similar?threshold=8');
  const names = ['variant-a.jpg', 'variant-b.jpg', 'variant-c.jpg'];
  const g = (sim.groups || []).find(gr => gr.items.filter(i => names.includes(i.file_name)).length >= 3);
  check('三个变体聚为一组', !!g, g ? g.count + ' 张，最大差异 ' + g.max_distance : '未找到');
  check('旋转图未被误判', g ? !g.items.some(i => i.file_name === 'not-similar.jpg') : true);
  check('完全重复不会重复出现在相似组', (sim.groups || []).every(gr => new Set(gr.items.map(i => i.id)).size === gr.items.length));

  section('实况照片');
  const lp = await call('/api/livepairs/run', { method: 'POST' });
  check('配对成功', lp.pairs >= 1, lp.pairs + ' 组，多对多跳过 ' + lp.ambiguous);
  const liveDetail = await call('/api/media/' + live.id);
  check('照片侧显示配对视频', liveDetail.live_companion && liveDetail.live_companion.media_type === 'video',
    liveDetail.live_companion ? liveDetail.live_companion.file_name : '(无)');
  const videoAsset = await call('/api/media/' + liveDetail.live_companion.id);
  check('视频侧也指回照片', videoAsset.live_companion && videoAsset.live_companion.id === live.id);

  section('移动与撤销');
  const movedDir = path.join(ROOT, 'Moved');
  const plan = await call('/api/archive/plan', {
    method: 'POST',
    body: { asset_ids: [va.id, vb.id], out_dir: movedDir, mode: 'move', template: '{original_name}', conflict: 'skip' },
  });
  check('计划标记为移动', plan.mode === 'move' && plan.total === 2, plan.total + ' 项');
  const run = await call('/api/archive/plans/' + plan.plan_id + '/run', { method: 'POST' });
  const rj = await waitJob(run.job_id);
  check('移动任务完成', rj.status === 'completed', rj.message);
  const planAfter = await call('/api/archive/plans/' + plan.plan_id);
  const done = (planAfter.items || []).filter(i => i.status === 'verified');
  const srcPath = done[0] && done[0].source_path;
  const tgtPath = done[0] && done[0].target_path;
  check('源文件已从原位置移除', srcPath && !fs.existsSync(srcPath), srcPath || '');
  check('目标文件已写入', tgtPath && fs.existsSync(tgtPath), tgtPath || '');
  const movedDetail = await call('/api/media/' + va.id);
  const movedPaths = (movedDetail.files || []).map(f => f.absolute_path.toLowerCase().replace(/\//g, '\\'));
  check('索引路径已同步', movedPaths.includes(String(tgtPath).toLowerCase().replace(/\//g, '\\')),
    movedPaths[0] || '');

  const undo = await call('/api/archive/plans/' + plan.plan_id + '/undo', { method: 'POST' });
  const uj = await waitJob(undo.job_id);
  check('撤销任务完成', uj.status === 'completed', uj.message);
  check('文件已移回原位置', srcPath && fs.existsSync(srcPath), srcPath || '');
  check('目标副本已清理', tgtPath && !fs.existsSync(tgtPath), tgtPath || '');
  const backDetail = await call('/api/media/' + va.id);
  const backPaths = (backDetail.files || []).map(f => f.absolute_path.toLowerCase().replace(/\//g, '\\'));
  check('索引恢复原路径', backPaths.includes(String(srcPath).toLowerCase().replace(/\//g, '\\')));

  section('复制与撤销');
  const copiedDir = path.join(ROOT, 'Copied');
  const cplan = await call('/api/archive/plan', {
    method: 'POST',
    body: { asset_ids: [vc.id], out_dir: copiedDir, mode: 'copy', template: '{original_name}', conflict: 'skip' },
  });
  check('计划标记为复制', cplan.mode === 'copy');
  const crun = await call('/api/archive/plans/' + cplan.plan_id + '/run', { method: 'POST' });
  await waitJob(crun.job_id);
  const cplanAfter = await call('/api/archive/plans/' + cplan.plan_id);
  const citem = (cplanAfter.items || []).find(i => i.status === 'verified');
  check('副本已生成', citem && fs.existsSync(citem.target_path), citem ? citem.target_path : '');
  check('源文件仍在', citem && fs.existsSync(citem.source_path));
  const cundo = await call('/api/archive/plans/' + cplan.plan_id + '/undo', { method: 'POST' });
  const cuj = await waitJob(cundo.job_id);
  check('复制撤销完成', cuj.status === 'completed', cuj.message);
  check('副本已删除', citem && !fs.existsSync(citem.target_path));
  check('源文件未被影响', citem && fs.existsSync(citem.source_path));

  section('目录监控');
  await call('/api/settings', { method: 'POST', body: { watch_enabled: '1', watch_interval: '30' } });
  const beforeCount = (await call('/api/state')).assets;
  const watchFile = path.join(PHOTOS, 'watch_added_20240101.jpg');
  fs.copyFileSync(path.join(FIXTURES, 'variant-a.jpg'), watchFile);
  const t0 = Date.now();
  let detected = false;
  while (Date.now() - t0 < 55000) {
    await sleep(3000);
    const st = await call('/api/state');
    if (st.assets > beforeCount) { detected = true; break; }
  }
  check('监控触发增量扫描', detected, detected ? `新增后共 ${(await call('/api/state')).assets} 项` : '55 秒内未检测到');

  console.log(`\n===== V1.1 测试结果：${pass} 通过，${fail} 失败 =====`);
  process.exit(fail ? 1 : 0);
})().catch(e => {
  console.error('\n测试异常终止：', e.message);
  process.exit(1);
});
