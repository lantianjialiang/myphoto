const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');
const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-wm');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
(async () => {
  const b = await chromium.launch({ channel: 'msedge', headless: true });
  const p = await b.newPage({ viewport: { width: 1600, height: 1000 } });
  await p.goto(URL, { waitUntil: 'load' });
  await p.waitForSelector('.tile');
  await p.locator('#nav button[data-view="watermark"]').click();
  await p.waitForTimeout(2500);
  const info = await p.evaluate(() => {
    const pick = s => {
      const e = document.querySelector(s);
      if (!e) return null;
      const r = e.getBoundingClientRect();
      return { x: Math.round(r.x), w: Math.round(r.width), cls: e.className };
    };
    return {
      main: pick('.main'), views: pick('.views'), view: pick('#view-watermark'),
      wm: pick('.wm-layout'), panel: pick('.wm-panel'), stage: pick('.wm-stage'),
      prev: pick('.wm-previews'), run: pick('.wm-runbar'),
      app: document.querySelector('.app').className,
      imgs: Array.from(document.querySelectorAll('.wm-card img')).length,
      cards: Array.from(document.querySelectorAll('.wm-card')).map(c => Math.round(c.getBoundingClientRect().height)),
    };
  });
  console.log(info);
  await b.close();
})();
