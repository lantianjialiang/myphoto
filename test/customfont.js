// 自定义字体导入的端到端测试。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-fonttest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-fonttest');
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

(async () => {
  fs.mkdirSync(PHOTOS, { recursive: true });
  const FIX = path.join(__dirname, 'fixtures', 'v11');
  for (const f of fs.readdirSync(FIX)) {
    if (/\.jpe?g$/i.test(f)) fs.copyFileSync(path.join(FIX, f), path.join(PHOTOS, f));
  }
  const known = await call('/api/sources');
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
  const tail = norm(PHOTOS).split('/').slice(-2).join('/');
  let src = known.find(s => norm(s.root_path) === norm(PHOTOS))
    || known.find(s => norm(s.root_path).endsWith(tail));
  if (!src) {
    try { src = await call('/api/sources', { method: 'POST', body: { path: PHOTOS } }); }
    catch (e) {
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
  check('找到测试照片', !!img, img ? img.file_name : '无');
  if (!img) process.exit(1);

  // ---- 1. 用户字体目录 ----
  section('用户字体目录');
  const uf = await call('/api/fonts/user');
  check('返回字体目录路径', !!uf.dir, uf.dir);
  check('扫描到自定义字体', (uf.fonts || []).length >= 2, (uf.fonts || []).length + ' 个');
  const named = (uf.fonts || []).map(x => x.name);
  check('包含中文名文件', named.some(n => n.includes('黑体')), named.join(','));

  // ---- 2. 字体校验 ----
  section('字体校验');
  const good = (uf.fonts || [])[0];
  const v1 = await call('/api/fonts/validate', { method: 'POST', body: { path: good.path } });
  check('合法字体校验通过', v1.ok === true, JSON.stringify(v1));
  check('返回字体名', !!v1.name, v1.name);

  let bad = false;
  try {
    await call('/api/fonts/validate', { method: 'POST', body: { path: 'C:/nope/missing.ttf' } });
  } catch (e) { bad = true; console.log('    错误信息：' + e.message); }
  check('不存在的文件被拒绝', bad);

  let bad2 = false;
  try {
    await call('/api/fonts/validate', { method: 'POST', body: { path: 'C:/Windows/System32/cmd.exe' } });
  } catch (e) { bad2 = true; console.log('    错误信息：' + e.message); }
  check('非字体文件被拒绝', bad2);

  // ---- 3. 用自定义字体渲染预览 ----
  section('自定义字体渲染');
  const sysCfg = { position: 'bc', size_pct: 5, text: '{date}', font: '微软雅黑', bold: true };
  const withSys = await call('/api/watermark/preview', { method: 'POST', body: { id: img.id, config: sysCfg } });
  const withCustom = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: img.id, config: Object.assign({}, sysCfg, { font_path: good.path }) },
  });
  check('系统字体预览成功', !!withSys.image && withSys.image.length > 100);
  check('自定义字体预览成功', !!withCustom.image && withCustom.image.length > 100);
  check('两种字体渲染结果不同', withSys.image !== withCustom.image,
    `系统 ${withSys.image.length} 字节 / 自定义 ${withCustom.image.length} 字节`);
  check('文字宽度不同（字体确实换了）', withSys.block.w !== withCustom.block.w,
    `${withSys.block.w} vs ${withCustom.block.w}`);

  // ---- 4. 用自定义字体批量打水印 ----
  section('批量打水印（自定义字体）');
  const OUT = path.join(ROOT, 'FontOut');
  fs.mkdirSync(OUT, { recursive: true });
  const r = await call('/api/watermark/run', {
    method: 'POST',
    body: {
      ids: [img.id],
      out_dir: OUT,
      name_template: '{name}_cf.{ext}',
      config: Object.assign({}, sysCfg, { font_path: good.path, text: '{date}', require_date: false }),
    },
  });
  let job = null;
  for (let i = 0; i < 150; i++) {
    await sleep(400);
    job = await call('/api/jobs/' + r.job_id);
    if (job.status !== 'running') break;
  }
  check('任务完成', job && job.status === 'completed', job ? job.status + ' | ' + (job.message || '') : '无');
  const files = fs.readdirSync(OUT).filter(f => /_cf\./i.test(f));
  check('副本已生成', files.length >= 1, files.join(',') || '（可能被跳过）');

  console.log(`\n通过 ${pass}，失败 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });
