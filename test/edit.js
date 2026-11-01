// 旋转 / 翻转 + 视频封面抽取的端到端测试。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-edittest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-edittest');
const PHOTOS = path.join(ROOT, 'Photos');
const OUT = path.join(ROOT, 'EditOut');
const FIXTURES = path.join(__dirname, 'fixtures', 'v11');
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

async function waitJob(id, rounds = 150) {
  for (let i = 0; i < rounds; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('job timeout');
}

/** 读 PNG/JPEG 像素尺寸（不引第三方库） */
function jpegSize(buf) {
  let i = 2;
  while (i + 9 < buf.length) {
    if (buf[i] !== 0xFF) { i++; continue; }
    const m = buf[i + 1];
    if (m >= 0xC0 && m <= 0xCF && m !== 0xC4 && m !== 0xC8 && m !== 0xCC) {
      return { h: buf.readUInt16BE(i + 5), w: buf.readUInt16BE(i + 7) };
    }
    if (m === 0xDA || m === 0xD9) break;
    const len = buf.readUInt16BE(i + 2);
    i += 2 + len;
  }
  return null;
}

(async () => {
  // ---- 准备素材 ----
  fs.mkdirSync(PHOTOS, { recursive: true });
  fs.mkdirSync(OUT, { recursive: true });
  for (const f of fs.readdirSync(FIXTURES)) {
    if (f.toLowerCase().endsWith('.jpg') || f.toLowerCase().endsWith('.jpeg')) {
      fs.copyFileSync(path.join(FIXTURES, f), path.join(PHOTOS, f));
    }
  }

  const known = await call('/api/sources');
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
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
  const sj = await waitJob((await call('/api/sources/' + src.id + '/scan', { method: 'POST' })).job_id);
  console.log('scan:', sj.status, sj.done + '/' + sj.total);

  const media = await call('/api/media?limit=200');
  const imgs = media.items.filter(i => i.media_type === 'image' && i.ext !== 'heic');
  check('索引里有照片', imgs.length > 0, imgs.length + ' 张');

  // ---- 1. 覆盖模式：顺时针 90° ----
  section('覆盖原图 · 右转 90°');
  const target = imgs.find(i => i.width && i.height) || imgs[0];
  const before = { w: target.width, h: target.height };
  const srcFile = path.join(PHOTOS, target.file_name);
  const origSize = jpegSize(fs.readFileSync(srcFile));
  check('原图尺寸已读到', !!origSize, origSize ? origSize.w + 'x' + origSize.h : '无');

  const r1 = await call('/api/edit/run', {
    method: 'POST',
    body: { ids: [target.id], op: 'rot90', mode: 'overwrite', backup: true },
  });
  const j1 = await waitJob(r1.job_id);
  check('任务完成', j1.status === 'completed', j1.status + ' | ' + (j1.message || ''));
  check('成功计数为 1', j1.done === 1, String(j1.done));

  const afterSize = jpegSize(fs.readFileSync(srcFile));
  check('文件宽高已互换', afterSize && afterSize.w === origSize.h && afterSize.h === origSize.w,
    afterSize ? `${origSize.w}x${origSize.h} → ${afterSize.w}x${afterSize.h}` : '读不出');

  const det = await call('/api/media/' + target.id);
  check('索引里的宽高同步更新', det.width === afterSize.w && det.height === afterSize.h,
    `${det.width}x${det.height}`);

  // ---- 2. 备份与还原 ----
  section('覆盖记录 · 还原');
  const bks = await call('/api/edit/backups');
  const list = Array.isArray(bks) ? bks : (bks.batches || []);
  check('出现备份批次', list.length > 0, list.length + ' 个批次');
  const batch = list[0];
  check('批次计数为 1', batch.total === 1, String(batch.total));

  const res = await call('/api/edit/backups/restore', { method: 'POST', body: { job_id: batch.job_id } });
  check('还原成功 1 张', res.restored === 1, JSON.stringify(res));
  const restoredSize = jpegSize(fs.readFileSync(srcFile));
  check('宽高回到原值', restoredSize && restoredSize.w === origSize.w && restoredSize.h === origSize.h,
    restoredSize ? `${restoredSize.w}x${restoredSize.h}` : '读不出');

  // ---- 3. 新建副本模式 ----
  section('新建副本 · 左转 90°');
  const r2 = await call('/api/edit/run', {
    method: 'POST',
    body: { ids: [target.id], op: 'rot270', mode: 'copy', out_dir: OUT, name_template: '{name}_rot.{ext}' },
  });
  const j2 = await waitJob(r2.job_id);
  check('任务完成', j2.status === 'completed', j2.status + ' | ' + (j2.message || ''));
  const copyPath = path.join(OUT, target.file_name.replace(/(\.[^.]+)$/, '_rot$1'));
  check('副本已生成', fs.existsSync(copyPath), copyPath);
  if (fs.existsSync(copyPath)) {
    const cs = jpegSize(fs.readFileSync(copyPath));
    check('副本宽高已互换', cs && cs.w === origSize.h && cs.h === origSize.w,
      cs ? `${cs.w}x${cs.h}` : '读不出');
  }
  const still = jpegSize(fs.readFileSync(srcFile));
  check('原图未被改动', still && still.w === origSize.w && still.h === origSize.h,
    still ? `${still.w}x${still.h}` : '读不出');

  // ---- 4. 翻转 ----
  section('翻转');
  const r3 = await call('/api/edit/run', {
    method: 'POST',
    body: { ids: [target.id], op: 'fliph', mode: 'copy', out_dir: OUT, name_template: '{name}_fh.{ext}' },
  });
  await waitJob(r3.job_id);
  const fhPath = path.join(OUT, target.file_name.replace(/(\.[^.]+)$/, '_fh$1'));
  check('左右翻转副本已生成', fs.existsSync(fhPath));

  // ---- 5. 删除备份 ----
  section('删除备份');
  const drop = await call('/api/edit/backups/drop', { method: 'POST', body: { job_id: batch.job_id } });
  check('备份记录已删除', drop.removed >= 1, JSON.stringify(drop));

  // ---- 6. 非法参数 ----
  section('参数校验');
  let bad = false;
  try { await call('/api/edit/run', { method: 'POST', body: { ids: [target.id], op: 'nope', mode: 'overwrite' } }); }
  catch (e) { bad = true; console.log('    错误信息：' + e.message); }
  check('未知操作被拒绝', bad);

  let bad2 = false;
  try { await call('/api/edit/run', { method: 'POST', body: { ids: [target.id], op: 'rot90', mode: 'copy' } }); }
  catch (e) { bad2 = true; console.log('    错误信息：' + e.message); }
  check('新建副本缺输出目录被拒绝', bad2);

  // ---- 7. 视频封面 ----
  section('视频封面');
  const caps = await call('/api/capabilities');
  const hasFfmpeg = !!(caps.ffmpeg && caps.ffmpeg.ok);
  console.log('    FFmpeg：' + (hasFfmpeg ? caps.ffmpeg.version : '未检测到'));
  const vids = media.items.filter(i => i.media_type === 'video');
  if (vids.length) {
    const v = vids[0];
    try {
      const r = await fetch(BASE + '/api/thumb/' + v.id + '?size=poster', { headers: H });
      const buf = Buffer.from(await r.arrayBuffer());
      check('视频封面可取', r.status === 200 && buf.length > 512, r.status + ' / ' + buf.length + ' 字节');
      check('是 JPEG', buf[0] === 0xFF && buf[1] === 0xD8);
    } catch (e) {
      check('视频封面可取', false, e.message);
    }
  } else {
    console.log('    （索引里没有视频，跳过）');
  }

  console.log(`\n通过 ${pass}，失败 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });
