// HEIC/HEIF 解码链路验证：真实 HEIC 文件 → 扫描 → 取宽高/EXIF → 生成缩略图。
// 用法：先把程序以 --no-window 启动并把输出写到 %TEMP%\pvm-heic\server.log，
//       再执行 node test\heic.js
const fs = require('fs');
const path = require('path');
const os = require('os');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-heic');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) throw new Error('server.log 里没有找到地址，请先启动 --no-window 实例');
const BASE = 'http://127.0.0.1:' + m[1];
const TOKEN = m[2];
const H = { 'X-PVM-Token': TOKEN, 'Content-Type': 'application/json' };
const sleep = ms => new Promise(r => setTimeout(r, ms));

let failed = 0;
function check(name, cond, detail) {
  console.log((cond ? '✓ ' : '✗ ') + name + (detail ? ' — ' + detail : ''));
  if (!cond) failed++;
}

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
    await sleep(300);
    const j = await call('/api/jobs/' + id);
    if (j.status !== 'running') return j;
  }
  throw new Error('任务超时');
}

(async () => {
  const samples = process.env.PVM_HEIC_SAMPLES || path.join(__dirname, 'fixtures', 'heic');
  const srcDir = path.join(root, '源目录 中文');
  fs.mkdirSync(srcDir, { recursive: true });
  const files = [
    ['sample1.heic', '生日照片 2023.heic'],
    ['example.heic', '海边日落.HEIC'],
    ['iphone.heic', 'IMG_2927 手机原图.HEIC'],
  ];
  let copied = 0;
  for (const [from, to] of files) {
    const p = path.join(samples, from);
    if (fs.existsSync(p)) {
      fs.copyFileSync(p, path.join(srcDir, to));
      copied++;
    }
  }
  console.log('样本文件:', copied, '个 →', srcDir);
  if (!copied) throw new Error('没有找到 HEIC 样本，请检查 test\\fixtures\\heic');

  const caps = await call('/api/capabilities');
  console.log('解码能力: HEIC =', caps.heic.ok, '|', caps.heic.detail);
  if (caps.heic.ok) check('解码器已加载', true, caps.heic.detail);
  else console.log('（未安装解码器：本次只验证降级路径，应返回 415 且带原因）');

  const existing = await call('/api/sources');
  const src = existing.find(s => s.root_path === srcDir) || (await call('/api/sources', { method: 'POST', body: { path: srcDir } }));
  const job = await call('/api/sources/' + src.id + '/scan', { method: 'POST' });
  const done = await waitJob(job.job_id);
  console.log('扫描:', done.status, done.done + '/' + done.total, '|', done.message || '');

  const media = await call('/api/media?limit=50');
  const heics = media.items.filter(i => ['heic', 'heif'].includes((i.ext || '').toLowerCase()));
  check('HEIC 全部进入索引', heics.length >= copied, heics.length + ' 项');

  for (const it of heics) {
    const detail = await call('/api/media/' + it.id);
    const asset = detail.asset || detail;
    console.log(
      '  文件', it.file_name,
      '| 尺寸', asset.width + 'x' + asset.height,
      '| 日期', asset.taken_at || '(无)',
      '| 来源', asset.date_source_label || asset.date_source || '-',
      '| 相机', asset.camera || '-'
    );
    if ((it.ext || '').toLowerCase() === 'heic' && /手机原图/.test(it.file_name) && caps.heic.ok) {
      check('手机原图从 HEIF 内的 EXIF 取到拍摄时间', asset.date_source === 'exif', asset.taken_at + ' / ' + asset.date_source_label);
      check('手机原图有相机型号', !!asset.camera, asset.camera || '(空)');
    }

    const r = await fetch(BASE + '/api/thumb/' + it.id + '?size=256&token=' + TOKEN);
    if (!caps.heic.ok) {
      const text = await r.text();
      check(it.file_name + ' 无解码器时降级为 415', r.status === 415 && text.includes('heif.dll'), r.status + ' ' + text.slice(0, 100));
      continue;
    }
    if (r.status !== 200) {
      check(it.file_name + ' 缩略图', false, r.status + ' ' + (await r.text()).slice(0, 120));
      continue;
    }
    const buf = Buffer.from(await r.arrayBuffer());
    const isJpeg = buf[0] === 0xff && buf[1] === 0xd8;
    check(it.file_name + ' 缩略图', isJpeg && buf.length > 2000, buf.length + ' bytes, jpeg=' + isJpeg);

    const big = await fetch(BASE + '/api/thumb/' + it.id + '?size=640&token=' + TOKEN);
    const bigBuf = Buffer.from(await big.arrayBuffer());
    check(it.file_name + ' 640 预览', big.status === 200 && bigBuf.length > buf.length, bigBuf.length + ' bytes');
  }

  // 缩略图应写入缓存表，重复请求走缓存
  if (caps.heic.ok) {
    const again = await fetch(BASE + '/api/thumb/' + heics[0].id + '?size=256&token=' + TOKEN);
    check('缩略图二次请求命中缓存', again.status === 200 && again.headers.get('cache-control') !== null);
  }

  console.log(failed === 0 ? '\nHEIC 链路全部通过' : `\n${failed} 项失败`);
  process.exit(failed === 0 ? 0 : 1);
})().catch(e => {
  console.error('测试失败:', e.message);
  process.exit(1);
});
