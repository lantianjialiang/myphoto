// 幻灯片合成的端到端测试（需要 FFmpeg）。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-slidetest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-slidetest');
const PHOTOS = path.join(ROOT, 'Photos');
const OUT = path.join(ROOT, 'SlideOut');
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

async function waitJob(id, rounds = 300) {
  for (let i = 0; i < rounds; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('job timeout');
}

/** 读 MP4 里的 ftyp/moov 标记，确认是合法视频容器 */
function isMp4(buf) {
  return buf.length > 100 && buf.slice(4, 8).toString('latin1') === 'ftyp';
}
/** GIF 头 */
function isGif(buf) {
  return buf.length > 6 && buf.slice(0, 3).toString('latin1') === 'GIF';
}

(async () => {
  fs.mkdirSync(PHOTOS, { recursive: true });
  fs.mkdirSync(OUT, { recursive: true });
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
  const sj = await waitJob((await call('/api/sources/' + src.id + '/scan', { method: 'POST' })).job_id);
  console.log('scan:', sj.status);

  const caps = await call('/api/capabilities');
  const hasFfmpeg = !!(caps.ffmpeg && caps.ffmpeg.ok);
  console.log('FFmpeg：' + (hasFfmpeg ? caps.ffmpeg.version : '未检测到'));
  if (!hasFfmpeg) {
    console.log('跳过：没有 FFmpeg 无法合成');
    process.exit(0);
  }

  const media = await call('/api/media?limit=200');
  const imgs = media.items.filter(i => i.media_type === 'image');
  const vids = media.items.filter(i => i.media_type === 'video');
  check('有照片素材', imgs.length >= 2, imgs.length + ' 张');
  check('有视频素材', vids.length >= 1, vids.length + ' 个');

  // ---- 1. 合成 MP4（纯照片） ----
  section('合成 MP4（照片）');
  const ids = imgs.slice(0, 3).map(x => x.id);
  const r1 = await call('/api/slideshow/run', {
    method: 'POST',
    body: {
      ids, out_dir: OUT, file_name: 'photo_slide', format: 'mp4',
      photo_seconds: 1, video_seconds: 0, size: 480,
    },
  });
  const j1 = await waitJob(r1.job_id);
  check('任务完成', j1.status === 'completed', j1.status + ' | ' + (j1.message || ''));
  const mp4Path = path.join(OUT, 'photo_slide.mp4');
  check('MP4 已生成', fs.existsSync(mp4Path));
  if (fs.existsSync(mp4Path)) {
    const buf = fs.readFileSync(mp4Path);
    check('是合法 MP4', isMp4(buf), buf.length + ' 字节');
    check('体积合理', buf.length > 1000, buf.length + ' 字节');
  }

  // ---- 2. 混合照片与视频 ----
  section('合成 MP4（照片 + 视频）');
  const mixed = imgs.slice(0, 2).map(x => x.id).concat(vids.slice(0, 1).map(x => x.id));
  const r2 = await call('/api/slideshow/run', {
    method: 'POST',
    body: {
      ids: mixed, out_dir: OUT, file_name: 'mixed_slide', format: 'mp4',
      photo_seconds: 1, video_seconds: 2, size: 480,
    },
  });
  const j2 = await waitJob(r2.job_id);
  check('任务完成', j2.status === 'completed', j2.status + ' | ' + (j2.message || ''));
  const mixedPath = path.join(OUT, 'mixed_slide.mp4');
  check('混合 MP4 已生成', fs.existsSync(mixedPath));
  if (fs.existsSync(mixedPath)) {
    const buf = fs.readFileSync(mixedPath);
    check('是合法 MP4', isMp4(buf), buf.length + ' 字节');
  }

  // ---- 3. 合成 GIF ----
  section('合成 GIF');
  const r3 = await call('/api/slideshow/run', {
    method: 'POST',
    body: {
      ids: imgs.slice(0, 2).map(x => x.id), out_dir: OUT, file_name: 'gif_slide',
      format: 'gif', photo_seconds: 1, video_seconds: 0, size: 320,
    },
  });
  const j3 = await waitJob(r3.job_id);
  check('任务完成', j3.status === 'completed', j3.status + ' | ' + (j3.message || ''));
  const gifPath = path.join(OUT, 'gif_slide.gif');
  check('GIF 已生成', fs.existsSync(gifPath));
  if (fs.existsSync(gifPath)) {
    const buf = fs.readFileSync(gifPath);
    check('是合法 GIF', isGif(buf), buf.length + ' 字节');
  }

  // ---- 4. 参数校验 ----
  section('参数校验');
  let bad = false;
  try { await call('/api/slideshow/run', { method: 'POST', body: { ids, out_dir: OUT, format: 'avi' } }); }
  catch (e) { bad = true; console.log('    错误信息：' + e.message); }
  check('不支持的格式被拒绝', bad);

  let bad2 = false;
  try { await call('/api/slideshow/run', { method: 'POST', body: { ids, out_dir: '', format: 'mp4' } }); }
  catch (e) { bad2 = true; console.log('    错误信息：' + e.message); }
  check('缺输出目录被拒绝', bad2);

  let bad3 = false;
  try { await call('/api/slideshow/run', { method: 'POST', body: { ids: [], out_dir: OUT, format: 'mp4' } }); }
  catch (e) { bad3 = true; console.log('    错误信息：' + e.message); }
  check('空选择被拒绝', bad3);

  // ---- 5. 格式列表接口 ----
  section('格式列表');
  const fmts = await call('/api/slideshow/formats');
  check('列出 mp4 与 gif', (fmts.formats || []).length === 2, JSON.stringify(fmts.formats.map(f => f.id)));

  console.log(`\n通过 ${pass}，失败 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });
