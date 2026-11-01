const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');
const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-wm');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
const shots = path.join(root, 'ui');
(async () => {
  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 2 });
  page.on('response', r => { if (r.status() >= 400) console.log('HTTP', r.status(), r.url()); });
  await page.goto(URL, { waitUntil: 'load' });
  await page.waitForSelector('.tile');
  await page.locator('#nav button[data-view="watermark"]').click();
  await page.waitForSelector('.wm-card img', { timeout: 30000 });
  await page.waitForTimeout(600);
  const cards = page.locator('.wm-card');
  console.log('cards:', await cards.count());
  for (let i = 0; i < await cards.count(); i++) {
    const box = await cards.nth(i).boundingBox();
    console.log('card', i, box && { w: box.width, h: box.height });
    await cards.nth(i).screenshot({ path: path.join(shots, `card-${i}.png`) });
  }
  const info = await page.evaluate(() => {
    const imgs = Array.from(document.querySelectorAll('.wm-card img'));
    return imgs.map(i => ({ nw: i.naturalWidth, nh: i.naturalHeight, cw: i.clientWidth, ch: i.clientHeight }));
  });
  console.log(info);
  await browser.close();
})();
