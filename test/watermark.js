// V1.2 能力端到端测试：时间水印（预览/自适应/批量/跳过/命名模板）与网页相册导出。
// 用法：先启动 --no-window 实例，把 server.log 放到 ROOT 下（默认 %TEMP%\pvm-wmtest）。
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-wmtest');
const PHOTOS = path.join(ROOT, 'Photos');
const OUT = path.join(ROOT, 'WmOut');
const ALBUM = path.join(ROOT, 'Album');
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
    await sleep(250);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running' && j.status !== 'cancelling') return j;
  }
  throw new Error('任务超时：' + id);
}

function countFiles(dir) {
  if (!fs.existsSync(dir)) return 0;
  let n = 0;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) n += countFiles(path.join(dir, e.name));
    else n += 1;
  }
  return n;
}

(async () => {
  fs.mkdirSync(PHOTOS, { recursive: true });
  for (const f of fs.readdirSync(FIXTURES)) {
    const dst = path.join(PHOTOS, f);
    if (!fs.existsSync(dst)) fs.copyFileSync(path.join(FIXTURES, f), dst);
  }
  const norm = p => p.replace(/\\/g, '/').toLowerCase();
  const sources = await call('/api/sources');
  // 8.3 短路径与长路径都可能出现，只要末尾一致就算同一个源
  const tail = norm(PHOTOS).split('/').slice(-2).join('/');
  let src = sources.find(s => norm(s.root_path) === norm(PHOTOS))
    || sources.find(s => norm(s.root_path).endsWith(tail));
  if (!src) {
    try {
      src = await call('/api/sources', { method: 'POST', body: { path: PHOTOS } });
    } catch (e) {
      src = (await call('/api/sources')).find(s => norm(s.root_path).endsWith(tail));
      if (!src) throw e;
    }
  }
  const scan = await waitJob((await call('/api/sources/' + src.id + '/scan', { method: 'POST' })).job_id);
  check('扫描完成', scan.status === 'completed', scan.message);

  const media = await call('/api/media?limit=100');
  const images = media.items.filter(i => i.media_type === 'image' && i.width && i.height);
  check('有可处理的照片', images.length >= 4, images.length + ' 张');

  section('字体与预设');
  const fonts = await call('/api/fonts');
  check('系统字体可用', (fonts.fonts || []).length > 0, (fonts.fonts || []).map(f => f.name).join(','));
  check('默认字体在列表中', (fonts.fonts || []).some(f => f.name === '微软雅黑'));
  const presets = await call('/api/watermark/presets');
  check('内置预设 3 套', (presets.presets || []).filter(p => p.builtin).length === 3,
    (presets.presets || []).map(p => p.name).join(','));
  const frame = (presets.presets || []).find(p => p.id === 'wm-frame');
  check('相框预设带底板', !!(frame && frame.config && frame.config.plate === true));

  section('预览与自适应');
  const biggest = images.slice().sort((a, b) => b.width * b.height - a.width * a.height)[0];
  const small = images.slice().sort((a, b) => a.width * a.height - b.width * b.height)[0];
  const prev = await call('/api/watermark/preview', {
    method: 'POST', body: { id: biggest.id, config: { position: 'br', size_pct: 3 }, max_side: 1200 },
  });
  check('预览返回图片', typeof prev.image === 'string' && prev.image.startsWith('data:image/jpeg;base64,'));
  check('预览有字号', prev.font_px > 10, prev.font_px + 'px');
  check('预览带文字', typeof prev.text === 'string' && prev.text.length >= 8, prev.text);

  const big8 = await call('/api/watermark/preview', {
    method: 'POST', body: { id: biggest.id, config: { size_pct: 8 }, max_side: 1200 },
  });
  const big16 = await call('/api/watermark/preview', {
    method: 'POST', body: { id: biggest.id, config: { size_pct: 1.6 }, max_side: 1200 },
  });
  check('字号按比例缩放', big8.font_px >= big16.font_px * 4, big8.font_px + ' vs ' + big16.font_px);

  const longText = await call('/api/watermark/preview', {
    method: 'POST',
    body: { id: biggest.id, config: { size_pct: 10, max_width_pct: 30 }, max_side: 1200 },
  });
  const longLabel = await call('/api/watermark/preview', {
    method: 'POST',
    body: {
      id: biggest.id,
      config: { size_pct: 10, max_width_pct: 30, text: '{date} 这是一段很长很长很长的说明文字用来触发宽度保护' },
    },
  });
  check('超宽文字自动缩小', longLabel.font_px < longText.font_px, longLabel.font_px + ' vs ' + longText.font_px);

  const tiny = await call('/api/watermark/preview', {
    method: 'POST', body: { id: small.id, config: { size_pct: 0.5, min_px: 14 }, max_side: 1200 },
  });
  check('最小字号生效', tiny.font_px >= 13, tiny.font_px + 'px');

  section('批量生成');
  const ids = images.slice(0, 3).map(i => i.id);
  const before = ids.map(id => {
    const it = images.find(i => i.id === id);
    const st = fs.statSync(it.path);
    return { size: st.size, mtime: st.mtimeMs };
  });
  fs.rmSync(OUT, { recursive: true, force: true });
  const cfg = { position: 'br', size_pct: 3, date_format: '%Y.%m.%d', color: '#FF8A1E' };
  const run = await call('/api/watermark/run', {
    method: 'POST',
    body: { ids, out_dir: OUT, config: cfg, quality: 90, keep_exif: true, skip_existing: true },
  });
  const job = await waitJob(run.job_id);
  check('水印任务完成', job.status === 'completed', job.message);
  check('输出 3 个文件', countFiles(OUT) === 3, countFiles(OUT) + ' 个');
  const outNames = fs.readdirSync(OUT);
  check('命名模板生效', outNames.some(n => /_wm\.jpg$/.test(n)), outNames.join(','));
  const after = ids.map(id => {
    const it = images.find(i => i.id === id);
    const st = fs.statSync(it.path);
    return { size: st.size, mtime: st.mtimeMs };
  });
  check('原图未被修改', JSON.stringify(before) === JSON.stringify(after));
  const outBytes = fs.readFileSync(path.join(OUT, outNames[0]));
  check('输出是 JPEG', outBytes[0] === 0xff && outBytes[1] === 0xd8, outBytes.length + ' 字节');

  const again = await waitJob((await call('/api/watermark/run', {
    method: 'POST',
    body: { ids, out_dir: OUT, config: cfg, skip_existing: true },
  })).job_id);
  check('已存在时跳过', again.skipped === 3, `跳过 ${again.skipped}`);

  const nested = path.join(ROOT, 'WmNested');
  fs.rmSync(nested, { recursive: true, force: true });
  const tpl = await waitJob((await call('/api/watermark/run', {
    method: 'POST',
    body: { ids: [ids[0]], out_dir: nested, config: cfg, name_template: '{YYYY}/{name}.{ext}' },
  })).job_id);
  check('命名模板支持子目录', tpl.succeeded === 1 && countFiles(nested) === 1,
    fs.existsSync(nested) ? fs.readdirSync(nested).join(',') : '无输出');
  check('视频被跳过', again.skipped >= 0 && job.failed === 0, `失败 ${job.failed}`);

  section('网页相册');
  fs.rmSync(ALBUM, { recursive: true, force: true });
  const albumTitle = '测试相册 <script>';
  const album = await waitJob((await call('/api/album/run', {
    method: 'POST',
    body: { ids, out_dir: ALBUM, title: albumTitle, max_side: 1200 },
  })).job_id);
  check('相册任务完成', album.status === 'completed', album.message);
  const index = path.join(ALBUM, 'index.html');
  check('生成 index.html', fs.existsSync(index));
  const html = fs.readFileSync(index, 'utf8');
  check('标题被转义', html.includes('&lt;script&gt;') && !html.includes('<h1>测试相册 <script>'));
  check('页面内嵌照片数据', (html.match(/thumbs\//g) || []).length >= 3);
  check('生成大图与缩略图', countFiles(path.join(ALBUM, 'photos')) === 3 && countFiles(path.join(ALBUM, 'thumbs')) === 3,
    countFiles(path.join(ALBUM, 'photos')) + '/' + countFiles(path.join(ALBUM, 'thumbs')));
  check('默认不附带原图', !fs.existsSync(path.join(ALBUM, 'originals')));
  check('自包含（无外链 http）', !/src="https?:/.test(html));

  const ALBUM2 = path.join(ROOT, 'Album2');
  fs.rmSync(ALBUM2, { recursive: true, force: true });
  await waitJob((await call('/api/album/run', {
    method: 'POST',
    body: { ids: [ids[0]], out_dir: ALBUM2, title: '带原图', include_originals: true },
  })).job_id);
  check('可选附带原图', countFiles(path.join(ALBUM2, 'originals')) === 1);
  check('附带原图时给出隐私提示', fs.readFileSync(path.join(ALBUM2, 'index.html'), 'utf8').includes('注意'));

  section('画面方向与原地覆盖');
  const pv0 = await call('/api/watermark/preview', {
    method: 'POST', body: { id: ids[0], config: { position: 'br', rotate_image: 0 } },
  });
  const pv90 = await call('/api/watermark/preview', {
    method: 'POST', body: { id: ids[0], config: { position: 'br', rotate_image: 90 } },
  });
  check('画面方向 90° 交换宽高', pv0.width === pv90.height && pv0.height === pv90.width,
    `${pv0.width}×${pv0.height} → ${pv90.width}×${pv90.height}`);

  const covered = images.find(i => i.id === ids[0]);
  const origBytes = fs.readFileSync(covered.path);
  const ov = await waitJob((await call('/api/watermark/run', {
    method: 'POST',
    body: { ids: [ids[0]], mode: 'overwrite', backup: true, config: { position: 'br', size_pct: 4 } },
  })).job_id);
  check('覆盖任务完成', ov.status === 'completed', ov.message);
  const newBytes = fs.readFileSync(covered.path);
  check('原文件已被改写', !newBytes.equals(origBytes), `${origBytes.length} → ${newBytes.length} 字节`);
  check('改写后仍是 JPEG', newBytes[0] === 0xff && newBytes[1] === 0xd8);
  const bk = /原件备份：([^；]+)/.exec(ov.message || '');
  check('给出备份目录', !!bk, bk ? bk[1] : '无');
  if (bk) {
    const bkFile = path.join(bk[1], path.basename(covered.path));
    check('备份存在且是原件', fs.existsSync(bkFile) && fs.readFileSync(bkFile).equals(origBytes));
  }
  const idxAfter = await call('/api/media?limit=100');
  const row = idxAfter.items.find(i => i.id === ids[0]);
  check('索引已更新文件大小', row && row.size_bytes === newBytes.length,
    row ? `${row.size_bytes} vs ${newBytes.length}` : '未找到');

  const badFmt = await waitJob((await call('/api/watermark/run', {
    method: 'POST',
    body: { ids: [ids[1]], mode: 'overwrite', config: { output_format: 'png' } },
  })).job_id);
  check('覆盖模式拒绝改格式', badFmt.failed === 1, badFmt.message);

  section('覆盖记录与一键还原');
  const batches = await call('/api/watermark/backups');
  const batch = (batches.batches || [])[0];
  check('覆盖记录里有批次', !!batch, batch ? `${batch.total} 张` : '无');
  check('批次标记为可还原', !!batch && batch.pending === 1, batch ? `pending=${batch.pending}` : '');
  const restored = await call('/api/watermark/backups/restore', { method: 'POST', body: { job_id: batch.job_id } });
  check('还原成功', restored.restored === 1 && restored.failed === 0, JSON.stringify(restored).slice(0, 120));
  const backBytes = fs.readFileSync(covered.path);
  check('还原后与原件一致', backBytes.equals(origBytes), `${backBytes.length} vs ${origBytes.length} 字节`);
  const afterRestore = await call('/api/media?limit=100');
  const row2 = afterRestore.items.find(i => i.id === ids[0]);
  check('还原后索引大小同步', row2 && row2.size_bytes === origBytes.length,
    row2 ? `${row2.size_bytes} vs ${origBytes.length}` : '未找到');
  const batches2 = await call('/api/watermark/backups');
  const b2 = (batches2.batches || [])[0];
  check('批次状态变为已还原', b2 && b2.pending === 0 && b2.restored >= 1,
    b2 ? `pending=${b2.pending} restored=${b2.restored}` : '无');
  const restoreAgain = await call('/api/watermark/backups/restore', { method: 'POST', body: { job_id: batch.job_id } })
    .then(() => null).catch(e => e.message);
  check('重复还原给出提示', typeof restoreAgain === 'string', String(restoreAgain).slice(0, 40));
  const dropped = await call('/api/watermark/backups/drop', { method: 'POST', body: { job_id: batch.job_id } });
  check('删除备份成功', dropped.removed >= 1, `删除 ${dropped.removed} 个`);
  const batches3 = await call('/api/watermark/backups');
  check('删除后批次消失', (batches3.batches || []).every(b => b.job_id !== batch.job_id));

  section('相册 zip 打包');
  const ALBUM3 = path.join(ROOT, 'AlbumZip');
  const zipPath = ALBUM3 + '.zip';
  fs.rmSync(ALBUM3, { recursive: true, force: true });
  fs.rmSync(zipPath, { force: true });
  const az = await waitJob((await call('/api/album/run', {
    method: 'POST',
    body: { ids: ids.slice(0, 2), out_dir: ALBUM3, title: '打包测试', zip: true },
  })).job_id);
  check('相册任务（带 zip）完成', az.status === 'completed', az.message);
  check('zip 已生成', fs.existsSync(zipPath), fs.existsSync(zipPath) ? `${fs.statSync(zipPath).size} 字节` : '缺失');
  if (fs.existsSync(zipPath)) {
    const buf = fs.readFileSync(zipPath);
    check('zip 是合法压缩包', buf[0] === 0x50 && buf[1] === 0x4b, buf.slice(0, 2).toString('hex'));
    check('zip 内含 index.html', buf.toString('latin1').includes('index.html'));
    check('zip 内含大图', buf.toString('latin1').includes('photos/0001.jpg'));
  }
  check('zip 提示写进任务消息', /zip/i.test(az.message || ''), (az.message || '').slice(0, 60));

  console.log(`\n结果：${pass} 通过，${fail} 失败`);
  if (fail) process.exitCode = 1;
})().catch(e => {
  console.error('测试异常：', e);
  process.exit(1);
});
