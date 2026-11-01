const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = path.join(os.tmpdir(), 'pvm-perf');
const m = fs.readFileSync(path.join(root, 'server.log'), 'utf8').match(/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
const target = 'http://127.0.0.1:' + m[1] + '/?token=' + m[2];

(async () => {
  const b = await chromium.launch({ channel: 'msedge', headless: true });
  const p = await b.newPage({ viewport: { width: 1600, height: 1000 } });
  const errs = [];
  p.on('pageerror', e => errs.push(e.message));
  const t0 = Date.now();
  await p.goto(target, { waitUntil: 'load' });
  await p.waitForSelector('.tile', { timeout: 20000 });
  console.log('首屏渲染:', (Date.now() - t0) + 'ms, DOM 卡片数:', await p.locator('.tile').count());
  const t1 = Date.now();
  await p.evaluate(() => { document.getElementById('grid-scroll').scrollTop = 1000 * 40; });
  await p.waitForTimeout(900);
  console.log('跳转到第 40 屏耗时:', (Date.now() - t1) + 'ms, DOM 卡片数:', await p.locator('.tile').count());
  console.log('可见卡片日期:', await p.locator('.tile .cap .d').first().innerText());
  await p.evaluate(() => { document.getElementById('grid-scroll').scrollTop = 0; });
  await p.waitForTimeout(600);
  await p.screenshot({ path: path.join(root, 'ui-3000.png') });
  console.log('JS 错误:', errs.length ? errs.slice(0, 2).join(' | ') : '无');
  await b.close();
})().catch(e => { console.error('FAIL', e.message); process.exit(1); });
