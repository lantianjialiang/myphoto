// V1.5 界面验证：旋转翻转页、水印拖拽定位、自定义字体、幻灯片入口。
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
const shots = path.join(root, 'ui15');
fs.mkdirSync(shots, { recursive: true });

const errors = [];
function check(name, cond, detail) {
  if (cond) console.log('✓ ' + name + (detail ? ' — ' + String(detail).slice(0, 120) : ''));
  else console.log('✗ ' + name + (detail ? ' — ' + String(detail).slice(0, 200) : ''));
  if (!cond) errors.push(name);
}

(async () => {
  // 先确保媒体源已扫描：有照片才能预览、才能测拖拽
  const PHOTOS = path.join(root, 'Photos');
  const H = { 'X-PVM-Token': m[2], 'Content-Type': 'application/json' };
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
  page.on('console', m => {
    // 只收真正的脚本报错；资源加载失败（404/422）是可预期的降级场景，不算脚本 bug
    if (m.type() !== 'error') return;
    const t = m.text();
    if (t.includes('Failed to load resource')) return;
    jsErrors.push(t);
  });

  await page.goto(URL, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(2500);

  // ---- 1. 导航里有「图像编辑」 ----
  section('导航');
  const navText = await page.textContent('#nav');
  check('导航里有图像编辑入口', navText.includes('图像编辑'), navText.replace(/\s+/g, ' ').slice(0, 120));

  // ---- 2. 图像编辑页 ----
  section('图像编辑页');
  await page.click('#nav button[data-view="edit"]');
  await page.waitForTimeout(600);
  const editVisible = await page.isVisible('#view-edit');
  check('图像编辑页能打开', editVisible);
  const opCount = await page.locator('#edit-ops .edit-op').count();
  check('有五个操作按钮', opCount === 5, String(opCount));
  const opText = await page.textContent('#edit-ops');
  check('操作名正确', opText.includes('右转') && opText.includes('翻转'), opText.replace(/\s+/g, ' ').slice(0, 100));
  check('有输出方式选择', await page.isVisible('#edit-mode'));
  check('有覆盖记录入口', await page.isVisible('#edit-backups'));
  await page.screenshot({ path: path.join(shots, 'edit-page.png') });

  // ---- 3. 覆盖模式警告与联动 ----
  section('覆盖模式');
  await page.selectOption('#edit-mode', 'overwrite');
  await page.waitForTimeout(300);
  check('覆盖模式显示警告', await page.isVisible('#edit-warn'));
  check('覆盖模式显示备份选项', await page.isVisible('#edit-backup-row'));
  check('覆盖模式隐藏输出目录', !(await page.isVisible('#edit-out-row')));
  await page.selectOption('#edit-mode', 'copy');
  await page.waitForTimeout(300);
  check('副本模式显示输出目录', await page.isVisible('#edit-out-row'));

  // ---- 4. 水印拖拽定位 ----
  section('水印拖拽定位');
  await page.click('#nav button[data-view="watermark"]');
  await page.waitForTimeout(3500);
  const freeBtn = page.locator('#wm-pos button[data-pos="free"]');
  check('九宫格里有自由定位按钮', await freeBtn.count() === 1);
  await freeBtn.click();
  await page.waitForTimeout(3000);
  check('自由定位提示可见', await page.isVisible('#wm-free-hint'));
  const boxCount = await page.locator('.wm-free-box').count();
  check('预览里出现可拖拽的水印框', boxCount > 0, String(boxCount));
  const previewsFree = await page.locator('#wm-previews.free').count();
  check('预览区进入自由模式', previewsFree === 1, String(previewsFree));
  await page.screenshot({ path: path.join(shots, 'freepos.png') });

  if (boxCount > 0) {
    // 拖拽水印框，检查位置百分比变化
    const before = await page.textContent('.wm-card .meta');
    const box = page.locator('.wm-free-box').first();
    const bb = await box.boundingBox();
    if (bb) {
      await page.mouse.move(bb.x + bb.width / 2, bb.y + bb.height / 2);
      await page.mouse.down();
      await page.mouse.move(bb.x + bb.width / 2 - 120, bb.y + bb.height / 2 - 80, { steps: 12 });
      await page.mouse.up();
      await page.waitForTimeout(3000);
      const after = await page.textContent('.wm-card .meta');
      check('拖拽后位置百分比已更新', after !== before,
        (after.match(/位置\s*([\d.]+%)/g) || []).join(','));
      await page.screenshot({ path: path.join(shots, 'freepos-dragged.png') });
    } else {
      check('能取到水印框位置', false);
    }
  }

  // ---- 5. 自定义字体 ----
  section('自定义字体');
  check('有自定义字体输入框', await page.isVisible('#wm-font-path'));
  check('有校验按钮', await page.isVisible('#wm-font-validate'));
  const hint = await page.textContent('#wm-font-hint');
  check('有许可证提示', hint.includes('授权') || hint.includes('许可'), hint.slice(0, 80));
  // 用一个真实字体校验
  await page.fill('#wm-font-path', 'C:/Windows/Fonts/simhei.ttf');
  await page.click('#wm-font-validate');
  await page.waitForTimeout(1500);
  const hint2 = await page.textContent('#wm-font-hint');
  check('校验通过后有提示', hint2.includes('可用'), hint2.slice(0, 100));
  await page.screenshot({ path: path.join(shots, 'customfont.png') });

  // ---- 6. 幻灯片入口 ----
  section('幻灯片');
  await page.click('#nav button[data-view="library"]');
  await page.waitForTimeout(1500);
  check('顶栏有幻灯片按钮', await page.isVisible('#btn-slideshow'));
  await page.click('#btn-slideshow');
  await page.waitForTimeout(1000);
  const slOpen = await page.isVisible('#sl-fmt');
  check('幻灯片向导打开', slOpen);
  if (slOpen) {
    const slText = await page.textContent('.modal-body');
    check('向导里有格式选择', slText.includes('MP4') && slText.includes('GIF'), slText.slice(0, 120));
    const slDisabled = await page.getAttribute('#sl-run', 'disabled');
    check('有 FFmpeg 时按钮可用', slDisabled === null, String(slDisabled));
  }
  await page.screenshot({ path: path.join(shots, 'slideshow.png') });
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);

  // ---- 7. 无 JS 错误 ----
  section('运行时');
  check('无 JS 运行时错误', jsErrors.length === 0, jsErrors.slice(0, 3).join(' | '));

  await browser.close();
  console.log(errors.length ? `\n失败 ${errors.length} 项` : '\n全部通过');
  process.exit(errors.length ? 1 : 0);
})().catch(e => { console.error('测试异常：', e); process.exit(1); });

function section(t) { console.log('\n== ' + t); }
