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
  await p.goto(target, { waitUntil: 'load' });
  await p.waitForSelector('.tile');
  const boxes = await p.evaluate(() => {
    const g = s => { const el = document.querySelector(s); if (!el) return null; const r = el.getBoundingClientRect(); return { top: Math.round(r.top), bottom: Math.round(r.bottom), h: Math.round(r.height), text: el.innerText.replace(/\n/g, ' | ').slice(0, 80) }; };
    return { rail: g('.rail'), nav: g('.nav'), stats: g('.rail-stats'), foot: g('.rail-foot'), disk: g('.disk') };
  });
  console.log(JSON.stringify(boxes, null, 1));
  await p.screenshot({ path: path.join(root, 'rail.png'), clip: { x: 0, y: 380, width: 220, height: 300 } });
  await b.close();
})().catch(e => { console.error('FAIL', e.message); process.exit(1); });
