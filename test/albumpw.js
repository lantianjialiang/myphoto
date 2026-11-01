// 相册密码保护的端到端测试：
// 1) 导出带密码的相册，校验 index.html 结构是“内嵌密文 + 密码门”；
// 2) 用与浏览器 Web Crypto 相同的参数（PBKDF2-HMAC-SHA256 + AES-256-GCM）在 Node 里解密，
//    验证能还原出合法 JPEG —— 即浏览器双击打开输入密码后能看到图。
// 用法：先启动 --no-window 实例并把输出重定向到 %TEMP%\pvm-edittest\server.log。
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');

const ROOT = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-edittest');
const OUT = path.join(ROOT, 'AlbumPw');
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
  const media = await call('/api/media?limit=50');
  const imgs = media.items.filter(i => i.media_type === 'image' && i.ext !== 'heic');
  check('有照片可导出', imgs.length > 0, imgs.length + ' 张');
  if (!imgs.length) { console.log('\n没有照片，测试结束'); process.exit(1); }

  const ids = imgs.slice(0, 3).map(i => i.id);
  const password = 'test-pass-123';

  section('导出带密码的相册');
  const dir = path.join(OUT, 'enc');
  const r = await call('/api/album/run', {
    method: 'POST',
    body: { ids, out_dir: dir, title: '加密相册', theme: 'dark', zip: false, password },
  });
  const j = await waitJob(r.job_id);
  const htmlPath = path.join(dir, 'index.html');
  check('导出任务完成', j.status === 'completed', j.status + (fs.existsSync(htmlPath) ? '' : '（无 index.html）'));
  if (!fs.existsSync(htmlPath)) { console.log('\n结果：' + pass + ' 通过 / ' + fail + ' 失败'); process.exit(fail ? 1 : 0); }

  const html = fs.readFileSync(htmlPath, 'utf8');
  check('标记了加密 ALBUM_ENC="1"', /ALBUM_ENC\s*=\s*"1"/.test(html));
  check('包含密码门 pw-gate', html.includes('id="pw-gate"'));
  check('包含密文数据 thumbData/largeData', html.includes('thumbData') && html.includes('largeData'));
  check('没有残留占位符', !html.includes('__ENC__') && !html.includes('__SALT__') && !html.includes('__DATA__'));
  check('没有外链 photos/ 或 thumbs/（已内嵌）', !/"(photos|thumbs)\//.test(html));

  const saltM = html.match(/const ALBUM_SALT = "([^"]*)"/);
  const itersM = html.match(/const PBKDF2_ITERS = (\d+)/);
  check('提取到盐 ALBUM_SALT', !!saltM, saltM ? saltM[1].slice(0, 12) + '…' : '缺失');
  check('提取到迭代次数 PBKDF2_ITERS', !!itersM, itersM ? itersM[1] : '缺失');

  // 取第一张照片的密文 + nonce
  const photosM = html.match(/const PHOTOS = (\[[\s\S]*?\]);/);
  let photos = [];
  try { photos = JSON.parse(photosM[1]); } catch (e) { /* ignore */ }
  const first = photos.find(p => p.thumbData && p.nonce && p.nonceT);
  check('找到内嵌密文条目', !!first, first ? first.n : '无');

  if (saltM && itersM && first) {
    section('用浏览器相同算法解密（Node 侧验证）');
    const salt = Buffer.from(saltM[1], 'base64');
    const iters = parseInt(itersM[1], 10);
    const key = crypto.pbkdf2Sync(password, salt, iters, 32, 'sha256');
    check('派生密钥长度 32 字节', key.length === 32);

    const ct = Buffer.from(first.thumbData, 'base64');
    const nonce = Buffer.from(first.nonceT, 'base64');
    const tag = ct.subarray(ct.length - 16);
    const body = ct.subarray(0, ct.length - 16);
    let plain;
    try {
      const decipher = crypto.createDecipheriv('aes-256-gcm', key, nonce);
      decipher.setAuthTag(tag);
      plain = Buffer.concat([decipher.update(body), decipher.final()]);
    } catch (e) {
      check('AES-GCM 解密成功', false, e.message);
      plain = null;
    }
    if (plain) {
      const isJpeg = plain[0] === 0xFF && plain[1] === 0xD8 && plain[2] === 0xFF;
      check('解密得到合法 JPEG（FF D8 FF 头）', isJpeg, plain.length + ' 字节');
      check('错误密码无法解密', (() => {
        const wrong = crypto.pbkdf2Sync('wrong-password', salt, iters, 32, 'sha256');
        try {
          const d = crypto.createDecipheriv('aes-256-gcm', wrong, nonce);
          d.setAuthTag(tag);
          d.update(body); d.final();
          return false; // 没抛错 = 异常
        } catch (e) { return true; } // 认证失败抛错 = 符合预期
      })());
    }
  }

  console.log('\n结果：' + pass + ' 通过 / ' + fail + ' 失败');
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });
