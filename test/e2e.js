const fs = require('fs');
const path = require('path');
const os = require('os');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-e2e');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
const BASE = 'http://127.0.0.1:' + m[1];
const TOKEN = m[2];
const H = { 'X-PVM-Token': TOKEN, 'Content-Type': 'application/json' };
const sleep = ms => new Promise(r => setTimeout(r, ms));

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

async function waitJob(id, rounds = 120) {
  for (let i = 0; i < rounds; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('job timeout');
}

(async () => {
  const st = await call('/api/state');
  console.log('state:', st.version, 'assets', st.assets, 'files', st.files);

  const photosDir = path.join(root, 'Photos');
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
  const known = await call('/api/sources');
  const existing = known.find(s => norm(s.root_path) === norm(photosDir))
    || known.find(s => norm(s.root_path).endsWith('/photos'));
  const src = existing || (await call('/api/sources', { method: 'POST', body: { path: photosDir } }));
  console.log('add source:', src.root_path, existing ? '(已存在，复用)' : '(新建)');

  const job = await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
  const sj = await waitJob(job.job_id);
  console.log('scan:', sj.status, sj.done + '/' + sj.total, '|', sj.message);

  const media = await call('/api/media?limit=50');
  console.log('\n== 索引资产数:', media.total);
  for (const it of media.items) {
    console.log(' ', it.media_type === 'video' ? '[V]' : '[I]',
      String(it.taken_at || '待确认').padEnd(20),
      (it.confidence || '').padEnd(6),
      (it.date_source || '').padEnd(10),
      (it.needs_review ? '需确认 ' : '       ') + it.file_name);
  }
  fs.writeFileSync(path.join(root, 'media.json'), JSON.stringify(media, null, 1));

  const img = media.items.find(i => i.media_type === 'image' && i.ext === 'jpg');
  const t = await fetch(BASE + '/api/thumb/' + img.id + '?size=256&token=' + TOKEN);
  const buf = Buffer.from(await t.arrayBuffer());
  console.log('\n缩略图:', t.status, t.headers.get('content-type'), buf.length, 'bytes', buf[0].toString(16), buf[1].toString(16));

  const heic = media.items.find(i => (i.ext || '') === 'heic');
  if (heic) {
    const r = await fetch(BASE + '/api/thumb/' + heic.id + '?size=256&token=' + TOKEN);
    if (r.status === 200) {
      const b = Buffer.from(await r.arrayBuffer());
      console.log('HEIC 缩略图:', r.status, r.headers.get('content-type'), b.length, 'bytes');
    } else {
      const j = await r.json();
      console.log('HEIC 缩略图:', r.status, j.error);
    }
  }

  const vid = media.items.find(i => i.media_type === 'video');
  if (vid) console.log('视频:', vid.width + 'x' + vid.height, vid.duration_ms + 'ms', '|', vid.taken_at, '|', vid.camera || '-');

  // 重复检测
  const hj = await call('/api/hash', { method: 'POST', body: { all: false } });
  const hjob = await waitJob(hj.job_id);
  console.log('\n哈希:', hjob.status, hjob.done + '/' + hjob.total, '|', hjob.message);
  const dup = await call('/api/duplicates');
  console.log('重复组:', dup.group_count, '可回收', dup.reclaimable_bytes, '未计算', dup.unhashed);
  dup.groups.forEach(g => {
    console.log(' 组', g.sha256.slice(0, 12), g.count, '份');
    g.files.forEach(f => console.log('   ', f.path.replace(root, ''), f.availability));
  });

  // 归档预览 + 执行（先清空上次的输出目录）
  const out = path.join(root, 'Archive');
  fs.rmSync(out, { recursive: true, force: true });
  const mediaHashed = await call('/api/media?limit=200');
  const plan = await call('/api/archive/plan', {
    method: 'POST',
    body: { asset_ids: mediaHashed.items.map(i => i.id), out_dir: out, template: '{YYYY}/{YYYY-MM}/{original_name}', conflict: 'skip' },
  });
  console.log('\n计划:', plan.total, '个文件', plan.total_bytes, 'bytes, 冲突', plan.conflicts, '警告', JSON.stringify(plan.warnings));
  plan.items.slice(0, 6).forEach(i => console.log('  ', i.status, i.target_path.replace(root, '')));

  const rj = await call('/api/archive/plans/' + plan.plan_id + '/run', { method: 'POST' });
  const rjob = await waitJob(rj.job_id);
  console.log('复制:', rjob.status, rjob.message);

  const written = [];
  (function walk(d) {
    if (!fs.existsSync(d)) return;
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const f = path.join(d, e.name);
      e.isDirectory() ? walk(f) : written.push(f);
    }
  })(out);
  console.log('输出文件数:', written.length);
  written.sort().forEach(f => console.log('  ', f.replace(root, '')));

  // 内容校验：副本 SHA-256 必须与某个源文件一致
  const crypto = require('crypto');
  const sha = f => crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex');
  const sourceHashes = new Set();
  (function walkSrc(d){ for (const e of fs.readdirSync(d,{withFileTypes:true})) { const f=path.join(d,e.name); e.isDirectory()?walkSrc(f):sourceHashes.add(sha(f)); } })(path.join(root,'Photos'));
  let bad = 0;
  for (const f of written) {
    const h = sha(f);
    if (!sourceHashes.has(h)) { bad++; console.log('  ✗ 哈希不匹配:', f); }
  }
  console.log('副本内容校验:', bad === 0 ? '全部通过（' + written.length + ' 个）' : bad + ' 个不一致');

  // 幂等：再次执行同一计划不应重复写入
  const rj2 = await call('/api/archive/plans/' + plan.plan_id + '/run', { method: 'POST' });
  const rjob2 = await waitJob(rj2.job_id);
  const count2 = (function walk(d) { let n = 0; for (const e of fs.readdirSync(d, { withFileTypes: true })) { const f = path.join(d, e.name); e.isDirectory() ? n += walk(f) : n++; } return n; })(out);
  console.log('重跑后输出文件数:', count2, '|', rjob2.message);

  // 原始文件零改动
  const baseline = JSON.parse(fs.readFileSync(path.join(root, 'baseline.json'), 'utf8'));
  let changed = 0;
  for (const [f, size, mtime] of baseline) {
    const now = fs.statSync(f);
    if (now.size !== size || Math.abs(now.mtimeMs - mtime) > 1) { changed++; console.log('  ✗ 被修改:', f); }
  }
  console.log('\n原件校验: ' + (changed === 0 ? '全部 ' + baseline.length + ' 个文件大小与修改时间未变' : changed + ' 个文件发生变化'));

  // 日志
  const logs = await call('/api/logs?limit=6');
  console.log('\n最近日志:');
  logs.forEach(l => console.log('  ', l.timestamp, l.operation, l.status, (l.detail || '').slice(0, 70)));

  // 离线源保护：断开（改名）后扫描应失败，索引不得被删除
  const photos = path.join(root, 'Photos');
  const offlineDir = path.join(root, 'Photos_offline');
  fs.renameSync(photos, offlineDir);
  let offlineMsg = '';
  try {
    await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
    await sleep(600);
  } catch (e) { offlineMsg = e.message; }
  await sleep(1500);
  const after = await call('/api/media?limit=5');
  const srcAfter = (await call('/api/sources')).find(s => s.id === src.id);
  console.log('\n离线源: 提示=' + (offlineMsg || '（任务内失败）') + ' | 索引仍为 ' + after.total + ' 项 | online=' + srcAfter.online);
  fs.renameSync(offlineDir, photos);
  const resc = await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
  const rescJob = await waitJob(resc.job_id);
  console.log('恢复后重扫:', rescJob.status, '|', rescJob.message);
})().catch(e => { console.error('FAIL', e.stack); process.exit(1); });
