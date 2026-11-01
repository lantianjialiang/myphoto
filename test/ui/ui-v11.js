// V1.1 界面验证：相似照片、标签、星标、时间偏移、目录监控设置、移动向导与撤销。
// 前置：先运行 node test/v11.js（同一个 --no-window 实例，数据目录 %TEMP%\pvm-v11）。
const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-v11');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) {
  console.error('server.log 里没有地址，请先启动 --no-window 实例');
  process.exit(2);
}
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
const shots = path.join(root, 'ui');
fs.mkdirSync(shots, { recursive: true });

const errors = [];
function check(name, cond, detail) {
  console.log((cond ? '✓ ' : '✗ ') + name + (detail ? ' — ' + detail : ''));
  if (!cond) process.exitCode = 1;
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

  // ===== 相似照片 =====
  await page.locator('#nav button[data-view="similar"]').click();
  await page.waitForSelector('#similar-list', { timeout: 8000 });
  await page.waitForTimeout(2500);
  const groups = await page.locator('#similar-list .sim-group').count();
  const groupItems = groups ? await page.locator('#similar-list .sim-group').first().locator('.sim-item').count() : 0;
  check('相似照片视图渲染分组', groups >= 1, groups + ' 组');
  check('相似组内含多张', groupItems >= 3, groupItems + ' 张');
  const simHead = groups ? await page.locator('#similar-list .sim-head').first().innerText() : '';
  check('相似组显示最大差异', /差异|距离/.test(simHead), simHead.replace(/\n/g, ' ').slice(0, 50));
  await page.screenshot({ path: path.join(shots, 'v11-01-similar.png') });

  // 阈值滑杆联动
  const before = await page.locator('#similar-list .sim-group').count();
  await page.locator('#sim-threshold').fill('1');
  await page.waitForTimeout(2000);
  const afterTight = await page.locator('#similar-list .sim-group').count();
  const valText = await page.locator('#sim-threshold-val').innerText();
  check('阈值滑杆生效', valText === '1' && afterTight <= before, `阈值 ${valText}，组数 ${before}→${afterTight}`);
  await page.locator('#sim-threshold').fill('6');
  await page.waitForTimeout(1500);

  // ===== 标签筛选 =====
  await page.locator('#nav button[data-view="library"]').click();
  await page.waitForTimeout(1200);
  const tagOptions = await page.locator('#f-tag option').allTextContents();
  check('标签筛选下拉有标签', tagOptions.some(t => t.includes('旅行')), tagOptions.join(',').slice(0, 60));
  await page.locator('#f-tag').selectOption('旅行');
  await page.waitForTimeout(1500);
  const taggedCount = await page.locator('.tile').count();
  check('按标签筛选生效', taggedCount === 1, taggedCount + ' 项');
  await page.screenshot({ path: path.join(shots, 'v11-02-tag-filter.png') });

  // ===== 星标筛选 =====
  await page.locator('#f-clear').click();
  await page.waitForTimeout(900);
  await page.locator('#f-star').click();
  await page.waitForTimeout(1500);
  const starCount = await page.locator('.tile').count();
  check('星标筛选生效', starCount >= 2, starCount + ' 项');
  const starBadges = await page.locator('.tile .b.star').count();
  check('卡片显示星标角标', starBadges >= 1, starBadges + ' 个');
  await page.locator('#f-clear').click();
  await page.waitForTimeout(900);

  // ===== 详情：星标切换 + 加标签 =====
  await page.locator('.tile').first().click({ position: { x: 60, y: 60 } });
  await page.waitForSelector('#detail:not([hidden])', { timeout: 8000 });
  await page.waitForTimeout(1200);
  const beforeStar = await page.locator('[data-act="star"]').innerText();
  await page.locator('[data-act="star"]').click();
  await page.waitForTimeout(1500);
  const afterStar = await page.locator('[data-act="star"]').innerText();
  check('详情可切换星标', beforeStar !== afterStar, `${beforeStar.trim()} → ${afterStar.trim()}`);

  await page.locator('#d-tag-input').fill('海边');
  await page.locator('#d-tag-input').press('Enter');
  await page.waitForTimeout(1500);
  const tagChips = await page.locator('#d-body .tagchip').count();
  check('详情可添加标签', tagChips >= 1, tagChips + ' 个标签');
  await page.screenshot({ path: path.join(shots, 'v11-03-detail-tags.png') });
  await page.locator('#d-close').click();
  await page.waitForTimeout(500);

  // ===== 批量时间偏移弹窗 =====
  await page.locator('.tile .check').first().click({ force: true });
  await page.waitForTimeout(400);
  await page.locator('#sel-shift').click();
  await page.waitForSelector('#sh-min', { timeout: 6000 });
  await page.locator('[data-shift="60"]').click();
  const shiftVal = await page.locator('#sh-min').inputValue();
  check('偏移弹窗快捷按钮生效', shiftVal === '60', '+1 小时 → ' + shiftVal);
  await page.locator('#sh-cancel').click();
  await page.waitForTimeout(400);
  await page.locator('#sel-star').click();  // 批量星标（验证接口连通）
  await page.waitForTimeout(1200);
  const starToast = await page.locator('#toasts').innerText();
  check('批量星标提示成功', /已星标|星标/.test(starToast), starToast.replace(/\n/g, ' ').slice(0, 40));
  await page.locator('#sel-clear').click();
  await page.waitForTimeout(500);

  // ===== 归档向导：移动模式提示 =====
  await page.locator('.tile .check').first().click({ force: true });
  await page.locator('#sel-archive').click();
  await page.waitForSelector('.modal', { timeout: 6000 });
  const modeOptions = await page.locator('#aw-mode option').allTextContents();
  check('向导提供移动模式', modeOptions.some(t => t.includes('移动')), modeOptions.join(' / ').slice(0, 60));
  await page.locator('#aw-mode').selectOption('move');
  await page.waitForTimeout(600);
  const moveHint = await page.locator('.modal').innerText();
  check('移动模式有风险提示', /可撤销|移到新位置|不保留/.test(moveHint), (moveHint.match(/[^\n]*移动[^\n]*/) || [''])[0].slice(0, 60));
  await page.screenshot({ path: path.join(shots, 'v11-04-wizard-move.png') });

  // 改用复制模式跑一遍，验证撤销按钮
  await page.locator('#aw-mode').selectOption('copy');
  const copyDir = path.join(root, 'UiCopy');
  fs.rmSync(copyDir, { recursive: true, force: true });
  await page.locator('#aw-out').fill(copyDir);
  await page.locator('#aw-plan').click();
  await page.waitForSelector('.plan-table', { timeout: 30000 });
  await page.locator('#aw-run').click();
  await page.waitForSelector('#cf-yes', { timeout: 6000 });
  await page.locator('#cf-yes').click();
  await page.waitForTimeout(4000);

  // ===== 任务中心：可撤销 =====
  await page.locator('#nav button[data-view="jobs"]').click();
  await page.waitForTimeout(800);
  await page.locator('#jobs-tab button[data-tab="undo"]').click();
  await page.waitForSelector('#jobs-list .job', { timeout: 10000 });
  const undoBtns = await page.locator('#jobs-list [data-undo]').count();
  check('可撤销批次有撤销按钮', undoBtns >= 1, undoBtns + ' 个');
  await page.screenshot({ path: path.join(shots, 'v11-05-undo-list.png') });

  const copiedFiles = countFiles(copyDir);
  await page.locator('#jobs-list [data-undo]').first().click();
  await page.waitForSelector('#cf-yes', { timeout: 6000 });
  await page.locator('#cf-yes').click();
  await page.waitForTimeout(4000);
  const afterUndoBtns = await page.locator('#jobs-list [data-undo]').count();
  const leftFiles = countFiles(copyDir);
  check('撤销后不再可撤销', afterUndoBtns < undoBtns, `${undoBtns} → ${afterUndoBtns}`);
  check('撤销删除了副本', copiedFiles > 0 && leftFiles === 0, `副本 ${copiedFiles} → ${leftFiles}`);
  await page.screenshot({ path: path.join(shots, 'v11-06-after-undo.png') });

  // ===== 设置：监控与相似/实况卡片 =====
  await page.locator('#nav button[data-view="settings"]').click();
  await page.waitForTimeout(900);
  const setText = await page.locator('#settings-body').innerText();
  check('设置页有目录监控', /目录监控|启用监控/.test(setText));
  check('设置页有相似照片与实况卡片', /相似照片/.test(setText) && /实况照片/.test(setText));
  const selbarHidden = await page.locator('#selbar').isHidden();
  check('非媒体视图不显示批量操作条', selbarHidden, 'hidden=' + selbarHidden);
  const watchChecked = await page.locator('#set-watch').isChecked();
  check('监控开关状态已回显', watchChecked === true, String(watchChecked));
  await page.screenshot({ path: path.join(shots, 'v11-07-settings.png') });

  await page.locator('#btn-livepair').click();
  await page.waitForTimeout(2500);
  const lpToast = await page.locator('#toasts').innerText();
  check('重新配对实况照片可用', /配对/.test(lpToast), lpToast.replace(/\n/g, ' ').slice(0, 50));

  const fatal = errors.filter(e => !/favicon|net::ERR_ABORTED|404/i.test(e));
  check('无 JS 运行时错误', fatal.length === 0, fatal.slice(0, 3).join(' | ') || '无');

  await browser.close();
  console.log('\n截图目录: ' + shots);
})().catch(e => { console.error('FAIL', e.message); process.exit(1); });
