// 相册多主题导出的端到端测试。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-edittest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-edittest');
const PHOTOS = path.join(ROOT, 'Photos');
const OUT = path.join(ROOT, 'AlbumOut');
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

async function waitJob(id, rounds = 200) {
  for (let i = 0; i < rounds; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('job timeout');
}

(async () => {
  // ---- 1. 主题列表 ----
  section('主题列表');
  const tl = await call('/api/album/themes');
  check('返回主题列表', Array.isArray(tl.themes) && tl.themes.length >= 3, (tl.themes || []).length + ' 个');
  const keys = (tl.themes || []).map(t => t.key);
  check('包含深色与浅色', keys.includes('dark') && keys.includes('light'), keys.join(', '));
  check('每个主题都有名称与说明',
    (tl.themes || []).every(t => t.name && t.desc));

  // ---- 2. 逐个主题导出 ----
  section('按主题导出');
  const media = await call('/api/media?limit=50');
  const imgs = media.items.filter(i => i.media_type === 'image' && i.ext !== 'heic');
  check('有照片可导出', imgs.length > 0, imgs.length + ' 张');
  if (!imgs.length) { console.log('\n没有照片，测试结束'); process.exit(1); }

  const ids = imgs.slice(0, 3).map(i => i.id);
  for (const t of (tl.themes || [])) {
    const dir = path.join(OUT, t.key);
    const r = await call('/api/album/run', {
      method: 'POST',
      body: { ids, out_dir: dir, title: '主题测试 ' + t.name, theme: t.key, zip: false },
    });
    const j = await waitJob(r.job_id);
    const htmlPath = path.join(dir, 'index.html');
    const ok = j.status === 'completed' && fs.existsSync(htmlPath);
    check('主题「' + t.name + '」导出成功', ok, j.status + (fs.existsSync(htmlPath) ? '' : '（无 index.html）'));
    if (!fs.existsSync(htmlPath)) continue;
    const html = fs.readFileSync(htmlPath, 'utf8');
    check('  页面标题正确', html.includes('主题测试 ' + t.name));
    check('  没有残留占位符', !html.includes('__VARS__') && !html.includes('__EXTRA__') && !html.includes('__DATA__'));
    check('  CSS 变量已注入', html.includes('--bg:#'), '');
    check('  数据已注入', html.includes('const PHOTOS = ['));
  }

  // ---- 3. 不同主题的样式确实不同 ----
  section('主题差异');
  const dark = fs.readFileSync(path.join(OUT, 'dark', 'index.html'), 'utf8');
  const light = fs.readFileSync(path.join(OUT, 'light', 'index.html'), 'utf8');
  check('深色与浅色主题的配色不同', dark !== light);
  check('浅色主题是白底', /--bg:#f7f7f5/.test(light), '');
  check('深色主题是深底', /--bg:#0e1015/.test(dark), '');

  // ---- 4. 未知主题回落到默认 ----
  section('未知主题');
  const dir = path.join(OUT, 'unknown-theme');
  const r = await call('/api/album/run', {
    method: 'POST',
    body: { ids, out_dir: dir, title: '未知主题', theme: 'no-such-theme' },
  });
  const j = await waitJob(r.job_id);
  check('未知主题不报错', j.status === 'completed', j.status + ' | ' + (j.message || ''));
  const html = fs.existsSync(path.join(dir, 'index.html'))
    ? fs.readFileSync(path.join(dir, 'index.html'), 'utf8') : '';
  check('回落到默认主题（深色）', /--bg:#0e1015/.test(html), '');

  // ---- 5. zip 打包 ----
  section('zip 打包');
  const zdir = path.join(OUT, 'ziptheme');
  const r2 = await call('/api/album/run', {
    method: 'POST',
    body: { ids, out_dir: zdir, title: '打包测试', theme: 'film', zip: true },
  });
  const j2 = await waitJob(r2.job_id);
  check('导出任务完成', j2.status === 'completed', j2.status);
  const zipPath = zdir + '.zip';
  check('zip 已生成', fs.existsSync(zipPath), zipPath);
  if (fs.existsSync(zipPath)) {
    check('zip 不为空', fs.statSync(zipPath).size > 1000, fs.statSync(zipPath).size + ' 字节');
  }

  console.log(`\n结果：${pass} 通过 / ${fail} 失败`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(2); });
