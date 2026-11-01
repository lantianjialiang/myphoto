// V1.6 界面验证：图像编辑四标签页、裁剪预览、拖拽选框、调色滑块、打码、相册主题。
// 前置：已启动 --no-window 实例（同一个实例复用，ROOT 默认 %TEMP%\pvm-edittest）。
const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-edittest');
const log = fs.readFileSync(path.join(root, 'server.log'), 'utf8');
const m = log.match(/http:\/\/127\.0\.0\.1:(\d+)\/\?token=([\w-]+)/);
if (!m) {
  console.error('server.log 里没有地址，请先启动 --no-window 实例');
  process.exit(2);
}
const URL = `http://127.0.0.1:${m[1]}/?token=${m[2]}`;
const shots = path.join(root, 'ui16');
fs.mkdirSync(shots, { recursive: true });

const errors = [];
function check(name, cond, detail) {
  if (cond) console.log('✓ ' + name + (detail ? ' — ' + String(detail).slice(0, 120) : ''));
  else console.log('✗ ' + name + (detail ? ' — ' + String(detail).slice(0, 200) : ''));
  if (!cond) errors.push(name);
}
function section(t) { console.log('\n== ' + t); }

(async () => {
  const PHOTOS = path.join(root, 'Photos');
  const H = { 'X-PVM-Token': m[2], 'Content-Type': 'application/json' };
  // 确保已扫描
  try {
    const sources = await (await fetch(`http://127.0.0.1:${m[1]}/api/sources`, { headers: H })).json();
    const norm = p => p.replace(/\\/g, '/').toLowerCase();
    const tail = norm(PHOTOS).split('/').slice(-2).join('/');
    let src = (sources.result || []).find(s => norm(s.root_path).endsWith(tail));
    if (!src) {
      const r = await (await fetch(`http://127.0.0.1:${m[1]}/api/sources`, {
        method: 'POST', headers: H, body: JSON.stringify({ path: PHOTOS }),
      })).json();
      src = r.result;
    }
    if (src) {
      const sj = await (await fetch(`http://127.0.0.1:${m[1]}/api/sources/${src.id}/scan`, {
        method: 'POST', headers: H,
      })).json();
      const jid = sj.result && sj.result.job_id;
      for (let i = 0; i < 100 && jid; i++) {
        await new Promise(r => setTimeout(r, 400));
        const j = await (await fetch(`http://127.0.0.1:${m[1]}/api/jobs/${jid}`, { headers: H })).json();
        if (j.result && j.result.status !== 'running') break;
      }
      console.log('扫描完成');
    }
  } catch (e) {
    console.log('扫描步骤跳过：' + e.message);
  }

  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 950 } });
  const jsErrors = [];
  page.on('pageerror', e => jsErrors.push(String(e)));
  page.on('console', msg => {
    if (msg.type() !== 'error') return;
    const t = msg.text();
    if (t.includes('Failed to load resource')) return;
    jsErrors.push(t);
  });

  await page.goto(URL, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(2500);

  // ---- 1. 导航 ----
  section('导航');
  const navText = await page.textContent('#nav');
  check('导航里有图像编辑入口', navText.includes('图像编辑'), navText.replace(/\s+/g, ' ').slice(0, 120));

  // ---- 2. 四个标签页 ----
  section('编辑页标签');
  await page.click('#nav button[data-view="edit"]');
  await page.waitForTimeout(600);
  check('编辑页能打开', await page.isVisible('#view-edit'));
  const tabs = await page.locator('#edit-tabs button').count();
  check('有四个标签页', tabs === 4, String(tabs));
  const tabText = await page.textContent('#edit-tabs');
  check('标签名齐全', ['旋转', '裁剪', '调色', '打码'].every(t => tabText.includes(t)),
    tabText.replace(/\s+/g, ' ').trim());
  check('默认显示旋转面板', await page.isVisible('#edit-pane-rotate'));
  check('旋转面板有五个操作', await page.locator('#edit-ops .edit-op').count() === 5);
  await page.screenshot({ path: path.join(shots, 'edit-rotate.png') });

  // ---- 3. 裁剪标签 ----
  section('裁剪');
  await page.click('#edit-tabs button[data-tab="crop"]');
  await page.waitForTimeout(2500);
  check('裁剪面板可见', await page.isVisible('#edit-pane-crop'));
  check('旋转面板已隐藏', !(await page.isVisible('#edit-pane-rotate')));
  check('有比例下拉', await page.isVisible('#edit-aspect'));
  check('有九宫格对齐', await page.locator('#edit-crop-align button').count() === 9);
  const previewImg = page.locator('#edit-preview-area img');
  check('出现预览图', await previewImg.count() > 0, String(await previewImg.count()));
  await page.screenshot({ path: path.join(shots, 'edit-crop.png') });

  // 换比例，预览尺寸应变化
  const sizeText1 = await page.textContent('#edit-preview-area .meta');
  await page.selectOption('#edit-aspect', '0.5625');   // 9:16
  await page.waitForTimeout(2500);
  const sizeText2 = await page.textContent('#edit-preview-area .meta');
  check('换比例后预览尺寸变化', sizeText1 !== sizeText2,
    (sizeText1.match(/(\d+)×(\d+)/) || [])[0] + ' → ' + (sizeText2.match(/(\d+)×(\d+)/) || [])[0]);

  // 切到「按区域」，应出现可拖拽的选框
  await page.click('#edit-crop-mode button[data-crop="region"]');
  await page.waitForTimeout(2500);
  check('区域模式面板可见', await page.isVisible('#edit-crop-region'));
  check('出现选框', await page.locator('.edit-region').count() > 0);
  // 参照图（带选框）与效果预览是两张卡
  const cardCount = await page.locator('#edit-preview-area .wm-card').count();
  check('有原图参照与效果预览两张图', cardCount === 2, String(cardCount));
  const metaText = await page.textContent('#edit-preview-area');
  check('标明哪张是原图', metaText.includes('原图'), metaText.replace(/\s+/g, ' ').slice(0, 100));

  // 拖拽选框（在参照图上）
  const regionBefore = await page.textContent('#edit-crop-rect');
  const img = page.locator('#edit-ph-img').first();
  const ib = await img.boundingBox();
  if (ib) {
    await page.mouse.move(ib.x + ib.width * 0.15, ib.y + ib.height * 0.20);
    await page.mouse.down();
    await page.mouse.move(ib.x + ib.width * 0.55, ib.y + ib.height * 0.70, { steps: 14 });
    await page.mouse.up();
    await page.waitForTimeout(2500);
    const regionAfter = await page.textContent('#edit-crop-rect');
    check('拖拽后区域标签更新', regionAfter !== regionBefore && regionAfter !== '—',
      regionBefore + ' → ' + regionAfter);
    check('选框仍存在', await page.locator('.edit-region').count() > 0);
    await page.screenshot({ path: path.join(shots, 'edit-crop-region.png') });
  } else {
    check('能取到参照图位置', false);
  }

  // ---- 4. 调色标签 ----
  section('调色');
  await page.click('#edit-tabs button[data-tab="tone"]');
  await page.waitForTimeout(2500);
  check('调色面板可见', await page.isVisible('#edit-pane-tone'));
  const presetCount = await page.locator('#edit-tone-presets .edit-op').count();
  check('有调色预设按钮', presetCount >= 6, String(presetCount));
  check('有四个滑块', await page.locator('#edit-tone-presets').count() === 1
    && await page.locator('#edit-pane-tone input[type="range"]').count() === 4);

  const toneBefore = await page.textContent('#edit-preview-area .meta');
  await page.click('#edit-tone-presets .edit-op[data-preset="mono"]');
  await page.waitForTimeout(2500);
  check('选中黑白后高亮', await page.locator('#edit-tone-presets .edit-op.on[data-preset="mono"]').count() === 1);
  check('调色后仍有预览图', await page.locator('#edit-preview-area img').count() > 0);
  await page.screenshot({ path: path.join(shots, 'edit-tone.png') });

  // 拖动亮度滑块
  await page.locator('#edit-t-bright').fill('40');
  await page.locator('#edit-t-bright').dispatchEvent('input');
  await page.waitForTimeout(2200);
  const brightVal = await page.textContent('#edit-t-bright-v');
  check('亮度数值已更新', brightVal === '40', brightVal);
  check('手动调参后切到自定义',
    await page.locator('#edit-tone-presets .edit-op.on[data-preset="custom"]').count() === 1);

  // ---- 5. 打码标签 ----
  section('打码');
  await page.click('#edit-tabs button[data-tab="mask"]');
  await page.waitForTimeout(2500);
  check('打码面板可见', await page.isVisible('#edit-pane-mask'));
  check('有马赛克/模糊切换', await page.locator('#edit-mask-mode button').count() === 2);
  check('有强度滑块', await page.isVisible('#edit-mask-strength'));
  check('出现选框', await page.locator('.edit-region').count() > 0);
  await page.click('#edit-mask-mode button[data-mask="blur"]');
  await page.waitForTimeout(2200);
  check('切到模糊后高亮', await page.locator('#edit-mask-mode button.on[data-mask="blur"]').count() === 1);
  await page.screenshot({ path: path.join(shots, 'edit-mask.png') });

  // ---- 6. 应用按钮 ----
  section('执行入口');
  check('有「应用到所选照片」按钮', await page.isVisible('#edit-run'));
  const scopeText = await page.textContent('#edit-scope');
  check('显示了处理范围', scopeText.includes('范围'), scopeText.trim());

  // ---- 7. 相册主题 ----
  section('相册主题');
  await page.click('#nav button[data-view="library"]');
  await page.waitForTimeout(800);
  await page.click('#btn-album');
  await page.waitForTimeout(1200);
  check('相册向导打开', await page.isVisible('#al-title'));
  const themeSel = page.locator('#al-theme');
  check('有主题下拉', await themeSel.count() === 1);
  const themeOpts = await themeSel.locator('option').count();
  check('下拉里有多个主题', themeOpts >= 3, String(themeOpts));
  const desc = await page.textContent('#al-theme-desc');
  check('主题有说明文字', desc.trim().length > 0, desc.trim());
  await page.screenshot({ path: path.join(shots, 'album-theme.png') });

  // ---- 8. 无脚本错误 ----
  section('脚本错误');
  check('没有 JS 报错', jsErrors.length === 0, jsErrors.slice(0, 3).join(' | '));

  await browser.close();
  console.log(`\n结果：${errors.length ? errors.length + ' 项失败' : '全部通过'}`);
  process.exit(errors.length ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(2); });
