// 裁剪 / 调色 / 打码 的端到端测试。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-edittest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-edittest');
const PHOTOS = path.join(ROOT, 'Photos');
const OUT = path.join(ROOT, 'CropOut');
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

async function waitJob(id, rounds = 200) {
  for (let i = 0; i < rounds; i++) {
    await sleep(400);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('job timeout');
}

/** 读 JPEG 像素尺寸 */
function jpegSize(buf) {
  let i = 2;
  while (i + 9 < buf.length) {
    if (buf[i] !== 0xFF) { i++; continue; }
    const mm = buf[i + 1];
    if (mm >= 0xC0 && mm <= 0xCF && mm !== 0xC4 && mm !== 0xC8 && mm !== 0xCC) {
      return { h: buf.readUInt16BE(i + 5), w: buf.readUInt16BE(i + 7) };
    }
    if (mm === 0xDA || mm === 0xD9) break;
    const len = buf.readUInt16BE(i + 2);
    i += 2 + len;
  }
  return null;
}

/** 解码 data:image/jpeg;base64,... 拿到原始字节 */
function dataUrlToBuf(dataUrl) {
  const b64 = dataUrl.split(',')[1];
  return Buffer.from(b64, 'base64');
}

/** 取图片某个像素的 RGB（只处理 JPEG，够用来判断明显色差） */
function jpegPixel(buf, x, y) {
  // 走 Node 内置的 zlib 解析 SOF 拿尺寸；像素颜色用平均值近似：
  // 解不出精确像素时返回 null，测试里只用尺寸与"两张图是否相同"来断言
  return null;
}

(async () => {
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
  await waitJob((await call('/api/sources/' + src.id + '/scan', { method: 'POST' })).job_id);

  const media = await call('/api/media?limit=200');
  const imgs = media.items.filter(i => i.media_type === 'image' && i.ext !== 'heic' && i.width && i.height);
  check('索引里有照片', imgs.length > 0, imgs.length + ' 张');
  if (!imgs.length) { console.log('\n没有可用照片，测试结束'); process.exit(1); }
  const target = imgs[0];
  const srcFile = path.join(PHOTOS, target.file_name);

  // ---- 1. 预览接口 ----
  section('编辑预览');
  const pv = await call('/api/edit/preview', {
    method: 'POST',
    body: { id: target.id, action: 'crop', params: { aspect: 1.0, h: 'center', v: 'center' }, max_side: 600 },
  });
  check('预览返回图片', typeof pv.image === 'string' && pv.image.startsWith('data:image/jpeg;base64,'),
    (pv.image || '').slice(0, 30) + '…');
  check('预览宽度按 1:1 裁剪', pv.width === pv.height, `${pv.width}×${pv.height}`);
  check('预览给出文件名', !!pv.file_name, pv.file_name || '');

  const pvBad = await fetch(BASE + '/api/edit/preview', {
    method: 'POST', headers: H,
    body: JSON.stringify({ id: target.id, action: 'nope', params: {} }),
  }).then(r => r.json());
  check('未知动作被拒绝', pvBad.ok === false, pvBad.error || '');

  // ---- 2. 按比例裁剪（覆盖） ----
  section('按比例裁剪 · 覆盖原图');
  const origSize = jpegSize(fs.readFileSync(srcFile));
  const r1 = await call('/api/edit/run', {
    method: 'POST',
    body: {
      ids: [target.id], action: 'crop',
      params: { aspect: 1.0, h: 'center', v: 'center' },
      mode: 'overwrite', backup: true,
    },
  });
  const j1 = await waitJob(r1.job_id);
  check('任务完成', j1.status === 'completed', j1.status + ' | ' + (j1.message || ''));
  check('成功计数为 1', j1.done === 1, String(j1.done));

  const afterSize = jpegSize(fs.readFileSync(srcFile));
  check('文件裁成 1:1', afterSize && afterSize.w === afterSize.h,
    afterSize ? `${origSize.w}x${origSize.h} → ${afterSize.w}x${afterSize.h}` : '读不出');

  const det = await call('/api/media/' + target.id);
  check('索引里的宽高同步更新', det.width === afterSize.w && det.height === afterSize.h,
    `${det.width}x${det.height}`);

  // ---- 3. 还原 ----
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

  // ---- 4. 按区域裁剪（新建副本） ----
  section('按区域裁剪 · 新建副本');
  const r2 = await call('/api/edit/run', {
    method: 'POST',
    body: {
      ids: [target.id], action: 'crop',
      params: { rect: { x: 10, y: 20, w: 50, h: 40 } },
      mode: 'copy', out_dir: OUT, name_template: '{name}_crop.{ext}',
    },
  });
  const j2 = await waitJob(r2.job_id);
  check('任务完成', j2.status === 'completed', j2.status + ' | ' + (j2.message || ''));
  const cropFile = path.join(OUT, target.file_name.replace(/\.[^.]+$/, '') + '_crop.jpg');
  const cropExists = fs.existsSync(cropFile);
  check('副本已生成', cropExists, cropFile);
  if (cropExists) {
    const cs = jpegSize(fs.readFileSync(cropFile));
    const expW = Math.round(origSize.w * 0.5);
    const expH = Math.round(origSize.h * 0.4);
    check('副本尺寸符合区域百分比', cs && Math.abs(cs.w - expW) <= 2 && Math.abs(cs.h - expH) <= 2,
      cs ? `${cs.w}x${cs.h}（期望约 ${expW}x${expH}）` : '读不出');
  }

  // ---- 5. 调色 ----
  section('调色');
  const toneBefore = await call('/api/edit/preview', {
    method: 'POST',
    body: { id: target.id, action: 'tone', params: { preset: 'custom', brightness: 0, contrast: 0, saturation: 0, warmth: 0 }, max_side: 400 },
  });
  const toneMono = await call('/api/edit/preview', {
    method: 'POST',
    body: { id: target.id, action: 'tone', params: { preset: 'mono' }, max_side: 400 },
  });
  check('原图调色预览可渲染', !!toneBefore.image);
  check('黑白预设改变了图像内容', toneMono.image !== toneBefore.image);
  check('调色不改变尺寸', toneMono.width === toneBefore.width && toneMono.height === toneBefore.height,
    `${toneBefore.width}x${toneBefore.height} → ${toneMono.width}x${toneMono.height}`);

  const r3 = await call('/api/edit/run', {
    method: 'POST',
    body: {
      ids: [target.id], action: 'tone',
      params: { preset: 'sepia' },
      mode: 'copy', out_dir: OUT, name_template: '{name}_tone.{ext}',
    },
  });
  const j3 = await waitJob(r3.job_id);
  check('调色任务完成', j3.status === 'completed', j3.status + ' | ' + (j3.message || ''));
  const toneFile = path.join(OUT, target.file_name.replace(/\.[^.]+$/, '') + '_tone.jpg');
  check('调色副本已生成', fs.existsSync(toneFile), toneFile);
  if (fs.existsSync(toneFile)) {
    const ts = jpegSize(fs.readFileSync(toneFile));
    check('调色不改变尺寸', ts && ts.w === origSize.w && ts.h === origSize.h,
      ts ? `${ts.w}x${ts.h}` : '读不出');
  }

  // ---- 6. 打码 ----
  section('局部打码');
  const maskBefore = await call('/api/edit/preview', {
    method: 'POST',
    body: { id: target.id, action: 'mask', params: { rect: { x: 20, y: 20, w: 30, h: 30 }, mode: 'mosaic', strength: 16 }, max_side: 400 },
  });
  const maskBlur = await call('/api/edit/preview', {
    method: 'POST',
    body: { id: target.id, action: 'mask', params: { rect: { x: 20, y: 20, w: 30, h: 30 }, mode: 'blur', strength: 12 }, max_side: 400 },
  });
  check('马赛克预览可渲染', !!maskBefore.image);
  check('模糊与马赛克结果不同', maskBlur.image !== maskBefore.image);
  check('打码不改变尺寸', maskBlur.width === maskBefore.width && maskBlur.height === maskBefore.height);

  const r4 = await call('/api/edit/run', {
    method: 'POST',
    body: {
      ids: [target.id], action: 'mask',
      params: { rect: { x: 20, y: 20, w: 30, h: 30 }, mode: 'mosaic', strength: 16 },
      mode: 'copy', out_dir: OUT, name_template: '{name}_mask.{ext}',
    },
  });
  const j4 = await waitJob(r4.job_id);
  check('打码任务完成', j4.status === 'completed', j4.status + ' | ' + (j4.message || ''));
  const maskFile = path.join(OUT, target.file_name.replace(/\.[^.]+$/, '') + '_mask.jpg');
  check('打码副本已生成', fs.existsSync(maskFile), maskFile);

  // ---- 7. 参数校验 ----
  section('参数校验');
  const bad = await fetch(BASE + '/api/edit/run', {
    method: 'POST', headers: H,
    body: JSON.stringify({ ids: [target.id], action: 'crop', params: {}, mode: 'copy', out_dir: OUT }),
  }).then(r => r.json());
  check('裁剪缺参数被拒绝', bad.ok === false, bad.error || '');

  const bad2 = await fetch(BASE + '/api/edit/run', {
    method: 'POST', headers: H,
    body: JSON.stringify({ ids: [target.id], action: 'mask', params: {}, mode: 'copy', out_dir: OUT }),
  }).then(r => r.json());
  check('打码缺区域被拒绝', bad2.ok === false, bad2.error || '');

  const bad3 = await fetch(BASE + '/api/edit/run', {
    method: 'POST', headers: H,
    body: JSON.stringify({ ids: [target.id], action: 'tone', params: { preset: 'zzz' }, mode: 'copy', out_dir: OUT }),
  }).then(r => r.json());
  check('未知调色预设被拒绝', bad3.ok === false, bad3.error || '');

  // ---- 8. 旧接口仍可用（向后兼容） ----
  section('向后兼容');
  const r5 = await call('/api/edit/run', {
    method: 'POST',
    body: { ids: [target.id], op: 'rot180', mode: 'copy', out_dir: OUT, name_template: '{name}_old.{ext}' },
  });
  const j5 = await waitJob(r5.job_id);
  check('旧的 op 参数仍可用', j5.status === 'completed', j5.status + ' | ' + (j5.message || ''));

  // ---- 9. 调色预设列表 ----
  section('预设列表');
  const tp = await call('/api/edit/tone-presets');
  check('返回预设列表', Array.isArray(tp.presets) && tp.presets.length >= 6, (tp.presets || []).length + ' 个');

  console.log(`\n结果：${pass} 通过 / ${fail} 失败`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(2); });
