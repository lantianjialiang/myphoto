const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-e2e');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
const shots = path.join(root, 'ui');
fs.mkdirSync(shots, { recursive: true });

const errors = [];
function check(name, cond, detail) {
  console.log((cond ? '✓ ' : '✗ ') + name + (detail ? ' — ' + detail : ''));
  if (!cond) process.exitCode = 1;
}

(async () => {
  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  page.on('console', msg => {
    if (msg.type() !== 'error') return;
    const t = msg.text();
    if (t.includes('Failed to load resource')) return; // 资源 404 是可预期的降级行为
    errors.push('console: ' + t);
  });
  page.on('pageerror', e => errors.push('pageerror: ' + e.message));

  await page.goto(URL, { waitUntil: 'load' });
  await page.waitForSelector('.tile', { timeout: 15000 });
  const tiles = await page.locator('.tile').count();
  check('媒体库渲染网格', tiles >= 5, tiles + ' 个卡片');
  await page.screenshot({ path: path.join(shots, '01-library-grid.png') });

  // 缩略图真的加载了（naturalWidth > 0）；等图片解码完再统计，避免取样太早
  await page.waitForTimeout(1500);
  const imgOk = await page.evaluate(() => {
    const imgs = Array.from(document.querySelectorAll('.tile img'));
    return imgs.filter(i => i.naturalWidth > 0).length + '/' + imgs.length;
  });
  check('缩略图可见', parseInt(imgOk) > 0, imgOk);

  // 选中两个
  await page.locator('.tile .check').first().click({ force: true });
  await page.locator('.tile').nth(1).locator('.check').click({ force: true });
  const selText = await page.locator('#sel-count').textContent();
  check('多选生效', selText.includes('2'), selText);
  await page.screenshot({ path: path.join(shots, '02-selection.png') });

  // 打开详情
  await page.locator('.tile').first().click({ position: { x: 60, y: 60 } });
  await page.waitForSelector('#detail:not([hidden])', { timeout: 8000 });
  await page.waitForSelector('#d-body .meta', { timeout: 8000 });
  const metaText = await page.locator('#d-body').innerText();
  check('详情面板显示元数据', /日期/.test(metaText) && /SHA-256/.test(metaText) && /文件位置/.test(metaText));
  await page.waitForTimeout(1200);
  await page.screenshot({ path: path.join(shots, '03-detail.png') });

  // 改日期（人工覆盖）
  await page.locator('#d-date').fill('2019-08-15');
  await page.locator('#d-time').fill('09:30:00');
  await page.locator('[data-act="set-date"]').click();
  await page.waitForTimeout(1500);
  const after = await page.locator('#d-body').innerText();
  check('人工覆盖日期生效', after.includes('2019/08/15') || after.includes('2019-08-15'), after.match(/2019[-/]0?8[-/]1?5/)?.[0] || '未找到');
  await page.screenshot({ path: path.join(shots, '04-detail-after-date.png') });

  // 清除覆盖
  await page.locator('[data-act="clear-date"]').click();
  await page.waitForTimeout(1200);
  check('清除覆盖值', !(await page.locator('#d-body').innerText()).includes('人工覆盖'));

  await page.locator('#d-close').click();
  await page.waitForTimeout(400);

  // 列表模式
  await page.locator('#viewmode').click();
  await page.waitForTimeout(800);
  const rows = await page.locator('.row').count();
  check('列表模式渲染', rows > 0, rows + ' 行');
  await page.screenshot({ path: path.join(shots, '05-list.png') });
  await page.locator('#viewmode').click();
  await page.waitForTimeout(500);

  // 筛选：只看重复
  await page.locator('#f-dup').click();
  await page.waitForTimeout(900);
  const dupCount = await page.locator('.tile').count();
  check('重复筛选生效', dupCount >= 1, dupCount + ' 项');
  await page.locator('#f-clear').click();
  await page.waitForTimeout(900);

  // 归档向导
  await page.locator('#btn-archive').click();
  await page.waitForSelector('.modal', { timeout: 6000 });
  const outDir = path.join(root, 'Archive2');
  fs.rmSync(outDir, { recursive: true, force: true });
  await page.locator('#aw-out').fill(outDir);
  await page.screenshot({ path: path.join(shots, '06-wizard.png') });
  await page.locator('#aw-plan').click();
  await page.waitForSelector('.plan-table', { timeout: 30000 });
  const planText = await page.locator('#aw-result').innerText();
  check('生成归档预览', planText.includes('文件') && planText.includes('预计写入'));
  await page.screenshot({ path: path.join(shots, '07-wizard-preview.png') });

  await page.locator('#aw-run').click();
  await page.waitForSelector('#cf-yes', { timeout: 5000 });
  await page.locator('#cf-yes').click();
  await page.waitForTimeout(3000);
  const jobsVisible = await page.locator('#view-jobs').evaluate(el => el.classList.contains('on'));
  check('复制任务进入任务中心', jobsVisible);
  await page.screenshot({ path: path.join(shots, '08-jobs-running.png') });

  // 待整理
  await page.locator('#nav button[data-view="review"]').click();
  await page.waitForTimeout(1400);
  const reviewText = await page.locator('#review-list').innerText();
  check('待整理显示文件名', !/媒体 [0-9a-f]{8}/.test(reviewText), reviewText.slice(0, 60).replace(/\n/g, ' '));
  await page.screenshot({ path: path.join(shots, '09-review.png') });

  // 重复文件
  await page.locator('#nav button[data-view="dupes"]').click();
  await page.waitForTimeout(1200);
  const dupText = await page.locator('#dup-list').innerText();
  check('重复文件页有分组', dupText.includes('份相同内容'), dupText.split('\n')[0]);
  await page.screenshot({ path: path.join(shots, '10-dupes.png') });

  // 设置
  await page.locator('#nav button[data-view="settings"]').click();
  await page.waitForTimeout(600);
  await page.locator('#btn-backup').click();
  await page.waitForTimeout(1500);
  const toastText = await page.locator('#toasts').innerText();
  check('数据库备份成功', toastText.includes('备份完成'), toastText);
  await page.screenshot({ path: path.join(shots, '11-settings.png') });

  // 搜索
  await page.locator('#nav button[data-view="library"]').click();
  await page.waitForTimeout(800);
  await page.locator('#q').fill('PXL');
  await page.waitForTimeout(1500);
  const searchCount = await page.locator('.tile').count();
  check('搜索过滤', searchCount >= 1 && searchCount < 8, searchCount + ' 项');
  await page.screenshot({ path: path.join(shots, '12-search.png') });

  const fatal = errors.filter(e => !/favicon|net::ERR_ABORTED|404/i.test(e));
  check('无 JS 运行时错误', fatal.length === 0, fatal.slice(0, 3).join(' | ') || '无');

  await browser.close();
  console.log('\n截图目录: ' + shots);
})().catch(e => { console.error('FAIL', e.message); process.exit(1); });
