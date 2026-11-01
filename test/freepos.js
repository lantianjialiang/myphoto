// 水印自由定位（拖拽）的端到端测试。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-freetest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-freetest');
const PHOTOS = path.join(ROOT, 'Photos');
const logPath = path.join(ROOT, 'server.log');
if (!fs.existsSync(logPath)) {
  console.error('找不到 ' + logPath + '，请先启动实例并把输出重定向到该文件');
  process.exit(2);
}
const log = fs.readFileSync(logPath, 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) { console.error('无法从日志中解析地址与令牌'); process.exit(2); }
const BASE = 'http://127.0.0.1:' + m[1];
const H = { 'X-PVM-Token': m[2], 'Content-Type': 'application/json' };
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

/** 解 base64 data URL 成 Buffer */
function dataUrlToBuf(u) {
  return Buffer.from(String(u).split(',')[1] || '', 'base64');
}

/** 找出图里"非黑"像素的包围盒，用来判断水印落在哪 */
function inkBox(buf) {
  // 只处理 JPEG：先粗略扫描，够用即可
  let minX = 1e9, minY = 1e9, maxX = -1, maxY = -1;
  // JPEG 无法直接扫像素，这里改用尺寸 + 后端返回的 block 来验证
  return { minX, minY, maxX, maxY };
}

(async () => {
  fs.mkdirSync(PHOTOS, { recursive: true });
  const FIX = path.join(__dirname, 'fixtures', 'v11');
  for (const f of fs.readdirSync(FIX)) {
    if (/\.jpe?g$/i.test(f)) fs.copyFileSync(path.join(FIX, f), path.join(PHOTOS, f));
  }
  const known = await call('/api/sources');
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
  // 8.3 短路径与长路径都可能出现，只要末尾一致就算同一个源
  const tail = norm(PHOTOS).split('/').slice(-2).join('/');
  let src = known.find(s => norm(s.root_path) === norm(PHOTOS))
    || known.find(s => norm(s.root_path).endsWith(tail));
  if (!src) {
    try {
      src = await call('/api/sources', { method: 'POST', body: { path: PHOTOS } });
    } catch (e) {
      src = (await call('/api/sources')).find(s => norm(s.root_path).endsWith(tail));
      if (!src) throw e;
    }
  }
  const sj = await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
  for (let i = 0; i < 150; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + sj.job_id);
    if (j.status !== 'running') break;
  }
  const media = await call('/api/media?limit=200');
  const img = media.items.find(i => i.media_type === 'image' && i.width && i.height);
  check('找到测试照片', !!img, img ? `${img.file_name} ${img.width}x${img.height}` : '无');
  if (!img) process.exit(1);

  // ---- 1. 预览接口返回 block 与 pos ----
  section('预览接口');
  const base = { position: 'free', pos_x: 10, pos_y: 20, size_pct: 4, text: '{date}' };
  const p1 = await call('/api/watermark/preview', { method: 'POST', body: { id: img.id, config: base } });
  check('返回了水印块位置', !!(p1.block && p1.block.w > 0), JSON.stringify(p1.block));
  check('预览图按 max_side 缩放', p1.width === Math.min(1400, img.width) && p1.height > 0,
    `${p1.width}x${p1.height}`);
  const expectX = Math.round(0.10 * p1.width);
  const expectY = Math.round(0.20 * p1.height);
  check('block.x 与百分比一致', p1.block.x === expectX, `${p1.block.x} vs ${expectX}`);
  check('block.y 与百分比一致', p1.block.y === expectY, `${p1.block.y} vs ${expectY}`);
  check('返回了 pos_x/pos_y', p1.pos_x === 10 && p1.pos_y === 20, `${p1.pos_x},${p1.pos_y}`);

  // ---- 2. 不同位置 → 不同 block ----
  section('位置变化');
  const p2 = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: img.id, config: Object.assign({}, base, { pos_x: 70, pos_y: 60 }) },
  });
  check('右下水印 x 更大', p2.block.x > p1.block.x, `${p1.block.x} → ${p2.block.x}`);
  check('右下水印 y 更大', p2.block.y > p1.block.y, `${p1.block.y} → ${p2.block.y}`);
  check('右下位置按百分比', p2.block.x === Math.round(0.7 * p2.width), String(p2.block.x));

  // ---- 3. 贴边会被收进来 ----
  section('贴边保护');
  const p3 = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: img.id, config: Object.assign({}, base, { pos_x: 100, pos_y: 100 }) },
  });
  check('贴右下时未超出画面',
    p3.block.x + p3.block.w <= p3.width && p3.block.y + p3.block.h <= p3.height,
    `${p3.block.x}+${p3.block.w}/${p3.width}, ${p3.block.y}+${p3.block.h}/${p3.height}`);

  // ---- 4. 九宫格仍然工作 ----
  section('九宫格兼容');
  const g1 = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: img.id, config: Object.assign({}, base, { position: 'tl' }) },
  });
  const g2 = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: img.id, config: Object.assign({}, base, { position: 'br' }) },
  });
  check('左上 x 更小', g1.block.x < g2.block.x, `${g1.block.x} vs ${g2.block.x}`);
  check('左上 y 更小', g1.block.y < g2.block.y, `${g1.block.y} vs ${g2.block.y}`);
  check('九宫格不受 pos_x 影响', g2.block.x === Math.round(p2.width - g2.block.w - 0.03 * Math.min(p2.width, p2.height)),
    `${g2.block.x}`);

  // ---- 5. 自由定位下批量打水印 ----
  section('批量打水印（自由定位）');
  const OUT = path.join(ROOT, 'FreeOut');
  fs.mkdirSync(OUT, { recursive: true });
  const r = await call('/api/watermark/run', {
    method: 'POST',
    body: {
      ids: [img.id],
      out_dir: OUT,
      name_template: '{name}_free.{ext}',
      config: Object.assign({}, base, { text: '{date}' }),
    },
  });
  let job = null;
  for (let i = 0; i < 150; i++) {
    await sleep(400);
    job = await call('/api/jobs/' + r.job_id);
    if (job.status !== 'running') break;
  }
  check('任务完成', job && job.status === 'completed', job ? job.status + ' | ' + (job.message || '') : '无');
  // done 含跳过；这张素材没有可靠拍摄时间，可能被跳过，所以以"确实产出了文件"为准
  const files = fs.readdirSync(OUT).filter(f => /_free\./i.test(f));
  check('副本已生成', files.length === 1, files.join(',') || '（该照片可能因缺少拍摄时间被跳过）');

  console.log(`\n通过 ${pass}，失败 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });
