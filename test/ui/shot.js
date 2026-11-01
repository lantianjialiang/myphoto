// 临时截图脚本：水印页与时间线页的实际渲染效果
const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-wm');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) { console.error('no url'); process.exit(2); }
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
const shots = path.join(root, 'ui');
fs.mkdirSync(shots, { recursive: true });

(async () => {
  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
  const errors = [];
  page.on('console', msg => { if (msg.type() === 'error') errors.push('console: ' + msg.text()); });
  page.on('pageerror', e => errors.push('pageerror: ' + e.message));
  await page.goto(URL, { waitUntil: 'load' });
  await page.waitForSelector('.tile', { timeout: 15000 });

  await page.locator('#nav button[data-view="timeline"]').click();
  await page.waitForSelector('.tl-item img', { timeout: 15000 });
  await page.waitForTimeout(1200);
  await page.screenshot({ path: path.join(shots, 'wm-01-timeline.png') });

  await page.locator('#nav button[data-view="watermark"]').click();
  await page.waitForSelector('.wm-card img', { timeout: 30000 });
  await page.waitForTimeout(800);
  await page.screenshot({ path: path.join(shots, 'wm-02-watermark.png') });

  // 换成相框预设并加底板 + 旋转，检查效果与中文
  await page.selectOption('#wm-preset', 'wm-frame').catch(() => {});
  await page.waitForTimeout(400);
  await page.fill('#wm-custom', '地点：北京');
  await page.waitForTimeout(2500);
  await page.screenshot({ path: path.join(shots, 'wm-03-frame.png') });

  console.log(errors.length ? errors.join('\n') : '无 JS 错误');
  await browser.close();
})();
