// V1.2 界面验证：时间线分组、时间水印编辑器、网页相册入口。
// 前置：node test/watermark.js 已经跑过（同一个 --no-window 实例，ROOT 默认 %TEMP%\pvm-wmtest）。
const fs = require('fs');
const path = require('path');
const os = require('os');
const { chromium } = require('playwright-core');

const root = process.env.PVM_TEST_ROOT || path.join(os.tmpdir(), 'pvm-wmtest');
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
function countFiles(dir) {
  if (!fs.existsSync(dir)) return 0;
  let n = 0;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) n += countFiles(path.join(dir, e.name));
    else n += 1;
  }
  return n;
}

function check(name, cond, detail) {
  console.log((cond ? '✓ ' : '✗ ') + name + (detail ? ' — ' + detail : ''));
  if (!cond) process.exitCode = 1;
}

(async () => {
  const outDir = path.join(root, 'UiWmOut');
  const albumDir = path.join(root, 'UiAlbum');
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.rmSync(albumDir, { recursive: true, force: true });

  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
  page.on('console', msg => {
    if (msg.type() !== 'error') return;
    const t = msg.text();
    if (t.includes('Failed to load resource')) return; // 视频封面 404 是可接受的已知行为
    errors.push('console: ' + t);
  });
  page.on('pageerror', e => errors.push('pageerror: ' + e.message));

  await page.goto(URL, { waitUntil: 'load' });
  await page.waitForSelector('.tile', { timeout: 20000 });

  // ===== 时间线 =====
  check('导航含时间线', await page.locator('#nav button[data-view="timeline"]').count() === 1);
  await page.locator('#nav button[data-view="timeline"]').click();
  await page.waitForSelector('.tl-item img', { timeout: 20000 });
  const groups = await page.locator('.tl-group').count();
  const heads = await page.locator('.tl-head').allTextContents();
  check('时间线有分组', groups >= 2, groups + ' 组：' + heads.join(' / '));
  // 每组的第一行必须铺满整行（末行保持自然高度是正常行为）
  const justify = await page.evaluate(() => {
    return Array.from(document.querySelectorAll('.tl-group')).map(g => {
      const r = g.querySelector('.tl-row');
      // 只有一张图的组谈不上「铺满」，跳过
      if (!r || r.children.length < 2) return null;
      const kids = Array.from(r.children).map(c => c.getBoundingClientRect());
      const sum = kids.reduce((a, b) => a + b.width, 0) + (kids.length - 1) * 6;
      return { sum: Math.round(sum), w: Math.round(r.getBoundingClientRect().width) };
    }).filter(Boolean);
  });
  check('等高不等宽行首尾对齐', justify.length > 0 && justify.every(r => Math.abs(r.sum - r.w) < 10), JSON.stringify(justify));
  const sticky = await page.evaluate(() => getComputedStyle(document.querySelector('.tl-head')).position);
  check('分组标题吸顶', sticky === 'sticky', sticky);
  // 视频封面可能生成不出来（缺解码器/无 poster），此时应显示底色 + 播放标记而不是空白块
  const videoTile = await page.evaluate(() => {
    const v = document.querySelector('.tl-item.is-video');
    if (!v) return null;
    return { ph: !!v.querySelector('.tl-ph'), noimg: v.classList.contains('noimg') };
  });
  check('视频块有兜底占位', !videoTile || videoTile.ph, JSON.stringify(videoTile));
  // 截图前等缩略图真正解码完，否则会截到一屏空占位
  await page.waitForFunction(() => {
    const imgs = [...document.querySelectorAll('#tl-body img')];
    return imgs.filter(i => i.complete && i.naturalWidth > 0).length >= Math.max(1, imgs.length - 1);
  }, null, { timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(200);
  await page.screenshot({ path: path.join(shots, 'v12-01-timeline.png') });

  await page.locator('#tl-density button[data-d="year"]').click();
  await page.waitForTimeout(300);
  const yearHeads = await page.locator('.tl-head').allTextContents();
  check('密度切到按年', yearHeads.every(t => /年/.test(t)) && yearHeads.length <= heads.length, yearHeads.join(' / '));

  await page.locator('.tl-item').first().click();
  await page.waitForSelector('#detail:not([hidden])', { timeout: 8000 });
  check('时间线可打开详情', await page.locator('#detail').isVisible());
  await page.locator('#d-close').click();

  // ===== 时间水印 =====
  await page.locator('#nav button[data-view="watermark"]').click();
  await page.waitForSelector('.wm-card img', { timeout: 30000 });
  await page.waitForFunction(() => document.querySelectorAll('.wm-card img').length >= 3, null, { timeout: 30000 });
  check('三张对照预览', await page.locator('.wm-card img').count() >= 3);
  const metas = await page.locator('.wm-card .meta').allTextContents();
  check('预览显示实际字号', metas.every(t => /字号\s*\d+px/.test(t)), metas.join(' | '));
  check('范围提示可见', (await page.locator('#wm-scope').textContent()).includes('范围'));
  await page.screenshot({ path: path.join(shots, 'v12-02-watermark.png') });

  const srcBefore = await page.locator('.wm-card img').first().getAttribute('src');
  const metaBefore = await page.locator('.wm-card .meta').first().textContent();
  await page.locator('#wm-size').evaluate(el => {
    el.value = '6';
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await page.waitForFunction(prev => {
    const el = document.querySelector('.wm-card .meta');
    return el && el.textContent !== prev;
  }, metaBefore, { timeout: 25000 });
  const srcAfter = await page.locator('.wm-card img').first().getAttribute('src');
  check('改字号后预览更新', srcBefore !== srcAfter);

  await page.locator('#wm-pos button[data-pos="tl"]').click();
  await page.waitForTimeout(1200);
  const activePos = await page.locator('#wm-pos button.on').getAttribute('data-pos');
  check('九宫格定位生效', activePos === 'tl', activePos);

  await page.selectOption('#wm-preset', 'wm-frame');
  await page.waitForTimeout(800);
  check('预设切换写入参数', await page.locator('#wm-plate').isChecked());
  check('预设切换改了字号', (await page.locator('#wm-size').inputValue()) === '3',
    await page.locator('#wm-size').inputValue());
  check('相框预设启用通栏底栏', await page.locator('#wm-plate-full').isChecked());
  await page.screenshot({ path: path.join(shots, 'v12-03-watermark-frame.png') });

  await page.fill('#wm-custom', '地点：北京');
  await page.waitForTimeout(1500);
  const hasChinese = (await page.locator('.wm-card .meta').allTextContents()).some(t => t.includes('地点：北京'));
  check('中文自定义文字进入预览', hasChinese);

  // 画面方向校正：转到 90° 后预览宽高应该交换
  // 注意：卡片先渲染占位（显示原图尺寸），再换成结果（带「字号」），要等结果态再读
  const dimsOf = async () => {
    const t = (await page.locator('.wm-card .meta').first().textContent()) || '';
    if (!/字号/.test(t)) return '';
    const mm = /(\d+)×(\d+)/.exec(t);
    return mm ? mm[1] + 'x' + mm[2] : '';
  };
  let dims0 = '';
  for (let i = 0; i < 60 && !dims0; i++) {
    dims0 = await dimsOf();
    if (!dims0) await page.waitForTimeout(250);
  }
  await page.selectOption('#wm-rotimg', '90');
  let dims90 = '';
  for (let i = 0; i < 90; i++) {
    dims90 = await dimsOf();
    if (dims90 && dims90 !== dims0) break;
    await page.waitForTimeout(250);
  }
  const [w0, h0] = (dims0 || 'x').split('x');
  check('画面方向 90° 交换宽高', dims90 === h0 + 'x' + w0, dims0 + ' → ' + dims90);
  await page.selectOption('#wm-rotimg', '0');
  await page.waitForTimeout(600);

  // 覆盖原图模式：显示红色警告、锁住输出目录并改按钮文案
  await page.selectOption('#wm-mode', 'overwrite');
  await page.waitForTimeout(400);
  const warnVisible = await page.locator('#wm-warn').isVisible();
  const outDisabled = await page.locator('#wm-out').isDisabled();
  const btnText = (await page.locator('#wm-run').textContent()) || '';
  check('覆盖模式给出警告并锁住输出目录', warnVisible && outDisabled && btnText.includes('覆盖'), btnText.trim());
  await page.selectOption('#wm-mode', 'copy');
  await page.waitForTimeout(300);

  // FFmpeg 状态提示（没有装就提示不可用，装了显示版本）
  const ffText = (await page.locator('#wm-ffmpeg').textContent()) || '';
  check('显示 FFmpeg 检测结果', /FFmpeg/i.test(ffText), ffText.trim().slice(0, 40));

  // 覆盖记录弹窗
  await page.locator('#wm-backups').click();
  await page.waitForSelector('#bk-body', { timeout: 8000 });
  const bkTitle = (await page.locator('.modal-head h3').textContent()) || '';
  check('打开覆盖记录弹窗', bkTitle.includes('覆盖记录'), bkTitle.trim());
  const bkBody = (await page.locator('#bk-body').textContent()) || '';
  check('覆盖记录给出空状态或批次列表', /还没有覆盖过|张可还原|已全部还原/.test(bkBody), bkBody.trim().slice(0, 40));
  await page.locator('#bk-done').click();

  // 直接填输出目录并执行（跳过系统文件夹对话框）
  await page.fill('#wm-out', outDir);
  await page.locator('#wm-run').click();
  await page.waitForSelector('#cf-yes', { timeout: 8000 });
  await page.locator('#cf-yes').click();
  await page.waitForSelector('#view-jobs.on', { timeout: 12000 });
  await page.waitForTimeout(2500);
  const jobText = await page.locator('#jobs-list').textContent();
  check('水印任务出现在任务中心', jobText.includes('水印'), jobText.slice(0, 80));
  await page.screenshot({ path: path.join(shots, 'v12-04-jobs.png') });

  // 等任务结束再看输出
  await page.waitForTimeout(4000);
  const produced = countFiles(outDir);
  check('界面触发的任务写出文件', produced >= 1, produced + ' 项');

  // ===== 网页相册 =====
  await page.locator('#nav button[data-view="library"]').click();
  await page.waitForSelector('.tile', { timeout: 15000 });
  check('工具条有网页相册入口', await page.locator('#btn-album').count() === 1);
  await page.locator('#btn-album').click();
  await page.waitForSelector('#al-run', { timeout: 8000 });
  check('相册向导有 zip 打包选项', await page.locator('#al-zip').count() === 1);
  await page.fill('#al-out', albumDir);
  await page.locator('#al-run').click();
  await page.waitForSelector('#view-jobs.on', { timeout: 12000 });
  // 任务在后台线程里跑，别用固定等待：轮询文件出现（最多 40s）
  const albumIndex = path.join(albumDir, 'index.html');
  const t0 = Date.now();
  while (!fs.existsSync(albumIndex) && Date.now() - t0 < 40000) {
    await page.waitForTimeout(300);
  }
  check('相册 index.html 已生成', fs.existsSync(albumIndex), ((Date.now() - t0) / 1000).toFixed(1) + 's');
  // 再等缩略图落盘，避免读到半成品
  const t1 = Date.now();
  while (!(fs.existsSync(path.join(albumDir, 'thumbs')) &&
           fs.readdirSync(path.join(albumDir, 'thumbs')).length >= 3) && Date.now() - t1 < 20000) {
    await page.waitForTimeout(300);
  }
  const albumHtml = fs.existsSync(albumIndex) ? fs.readFileSync(albumIndex, 'utf8') : '';
  check('相册内嵌照片', (albumHtml.match(/thumbs\//g) || []).length >= 3,
    ((albumHtml.match(/thumbs\//g) || []).length) + ' 处引用');

  check('无 JS 运行时错误', errors.length === 0, errors.join(' | '));
  await browser.close();
  console.log('\n界面检查完成');
})().catch(e => {
  console.error('测试异常：', e);
  process.exit(1);
});
