'use strict';

/* ============================ 基础工具 ============================ */
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
const TOKEN = (() => {
  const p = new URLSearchParams(location.search);
  const t = p.get('token');
  if (t) sessionStorage.setItem('pvm.token', t);
  return t || sessionStorage.getItem('pvm.token') || '';
})();
const AUTH = 'token=' + encodeURIComponent(TOKEN);

// Window presence, rather than the short-lived browser launcher, owns the service.
const WINDOW_ID = crypto.randomUUID();
function notifyWindow(action) {
  const body = JSON.stringify({ window_id: WINDOW_ID });
  const url = '/api/window/' + action + '?' + AUTH;
  if (action === 'close') {
    navigator.sendBeacon(url, new Blob([body], { type: 'application/json' }));
  } else {
    fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body }).catch(() => {});
  }
}
notifyWindow('heartbeat');
setInterval(() => notifyWindow('heartbeat'), 15000);
window.addEventListener('pageshow', () => notifyWindow('heartbeat'));
window.addEventListener('pagehide', e => { if (!e.persisted) notifyWindow('close'); });
document.addEventListener('visibilitychange', () => { if (!document.hidden) notifyWindow('heartbeat'); });

function esc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function fmtSize(n) {
  if (n == null) return '—';
  n = Number(n);
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0, v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? n + ' B' : v.toFixed(v < 10 ? 1 : 0) + ' ' + u[i];
}
function fmtDur(ms) {
  if (!ms) return '';
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), ss = s % 60;
  return (h ? h + ':' + String(m).padStart(2, '0') : String(m)) + ':' + String(ss).padStart(2, '0');
}
function fmtTaken(v, precision) {
  if (!v) return '日期未知';
  const d = v.slice(0, 10).replace(/-/g, '/');
  if (precision === 'second' || precision === 'minute') return d + ' ' + v.slice(11, 16);
  if (precision === 'month') return v.slice(0, 7).replace('-', '/');
  if (precision === 'year') return v.slice(0, 4);
  return d;
}
function srcLabel(s) {
  return ({ override: '人工覆盖', exif: 'EXIF', video: '视频容器', filename: '文件名', folder: '目录名', filesystem: '文件时间' })[s] || (s || '未知');
}
function confLabel(c) {
  return ({ high: '高', medium: '中', low: '低', none: '无' })[c] || c;
}
function debounce(fn, ms) {
  let t;
  return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
}
let lastConnectionToast = 0;
function toast(msg, kind = '') {
  if (msg.startsWith('本地服务暂时没有响应')) {
    if (Date.now() - lastConnectionToast < 6000) return;
    lastConnectionToast = Date.now();
  }
  const d = document.createElement('div');
  d.className = 'toast ' + kind;
  d.textContent = msg;
  $('#toasts').appendChild(d);
  setTimeout(() => {
    d.style.opacity = '0'; d.style.transition = 'opacity .3s';
    setTimeout(() => d.remove(), 320);
  }, kind === 'err' ? 5200 : 2800);
}

async function api(path, opts = {}) {
  const init = { method: opts.method || 'GET', headers: { 'X-PVM-Token': TOKEN } };
  if (opts.body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(opts.body);
  }
  let res;
  // 本地服务偶发连不上（进程刚起、后台任务占满线程、窗口从睡眠恢复）：
  // 只对 GET 自动重试一次，避免重复提交写操作。
  for (let attempt = 0; ; attempt++) {
    try {
      res = await fetch(path, init);
      break;
    } catch (e) {
      if (init.method === 'GET' && attempt < 1) {
        await new Promise(r => setTimeout(r, 400));
        continue;
      }
      throw new Error('本地服务暂时没有响应，请稍后重试（如果一直这样，请关掉窗口重新启动程序）');
    }
  }
  const ct = res.headers.get('content-type') || '';
  if (!ct.includes('application/json')) {
    if (!res.ok) throw new Error('请求失败（' + res.status + '）');
    return res;
  }
  const data = await res.json();
  if (data && data.ok === false) throw new Error(data.error || '请求失败');
  return data.result;
}
const apiGet = (p, params) => api(p + (params ? '?' + new URLSearchParams(params) : ''));

/* ============================ 全局状态 ============================ */
const S = {
  view: 'library',
  mode: 'grid',
  filter: {},
  sort: 'taken',
  order: 'desc',
  sel: new Set(),
  state: null,
  sources: [],
  settings: {},
  caps: null,
  gen: 0,
  mediaRevision: Date.now(),
  jobs: [],
  running: new Set(),
  detailId: null,
  reviewMode: 'all',
  reviewSel: new Set(),
  jobsTab: 'jobs',
};

const G = {
  items: new Map(),
  pages: new Map(),
  total: 0,
  cols: 1,
  rows: 0,
  rowH: 218,
  tileW: 180,
  tileH: 206,
  gap: 12,
  pad: 18,
  pageSize: 120,
  nodes: new Map(),
};

/* ============================ 导航 ============================ */
const VIEW_TITLES = { library: '媒体库', timeline: '时间线', review: '待整理', dupes: '重复文件', similar: '相似照片', watermark: '时间水印', edit: '图像编辑', jobs: '任务中心', sources: '媒体源', settings: '设置' };

function switchView(v) {
  if (v === 'edit') { openLibraryEditor(); return; }
  if (v !== S.view) { closeDetail(); if (!$('#photo-viewer').hidden) closeViewer(); }
  S.view = v;
  $$('.library-menu[open]').forEach(menu=>menu.open=false);
  $$('#nav button').forEach(b => b.classList.toggle('on', b.dataset.view === v));
  $$('.view').forEach(el => el.classList.toggle('on', el.id === 'view-' + v));
  $('.topbar').style.display = v === 'library' ? '' : 'none';
  $('.topbar').hidden = v !== 'library';
  $('#status-text').textContent = VIEW_TITLES[v] || '';
  updateSelBar();
  if (v === 'library') layout();
  if (v === 'timeline') loadTimeline(true);
  if (v === 'review') loadReview();
  if (v === 'dupes') loadDupes();
  if (v === 'similar') loadSimilar();
  if (v === 'watermark') loadWatermark();
  if (v === 'edit') loadEditView();
  if (v === 'jobs') loadJobs();
  if (v === 'sources') loadSourcesView();
  if (v === 'settings') renderSettings();
}

/* ============================ 状态与统计 ============================ */
async function loadState() {
  try {
    S.state = await apiGet('/api/state');
    S.settings = S.state.settings || {};
    if (!themeSaving) applyTheme(S.settings.theme || document.documentElement.dataset.theme || 'dark');
    syncOutputFields();
    renderRail();
  } catch (e) { toast(e.message, 'err'); }
}

function renderRail() {
  const s = S.state || {};
  $('#ver').textContent = 'v' + (s.version || '');
  $('#disk-info').innerHTML = '数据盘可用 <b>' + fmtSize(s.free_space) + '</b>';
  $('#rail-stats').innerHTML = `
    <div class="srow"><span>照片</span><b>${s.images || 0}</b></div>
    <div class="srow"><span>视频</span><b>${s.videos || 0}</b></div>
    <div class="srow"><span>♡ 收藏</span><b>${s.favorites || 0}</b></div>
    <div class="srow"><span>实况照片</span><b>${s.live_pairs || 0}</b></div>
    <div class="srow"><span>日期待确认</span><b>${s.needs_review || 0}</b></div>
    <div class="srow"><span>占用空间</span><b>${fmtSize(s.bytes)}</b></div>`;
  const bReview = $('#badge-review'), bDup = $('#badge-dup'), bJobs = $('#badge-jobs');
  bReview.hidden = !s.needs_review; bReview.textContent = s.needs_review || '';
  bDup.hidden = !s.dup_groups; bDup.textContent = s.dup_groups || '';
  bJobs.hidden = !(s.running_jobs > 0); bJobs.textContent = s.running_jobs || '';
  $('#status-count').textContent = `共 ${s.assets || 0} 项 · ${s.files || 0} 个文件`;
}

async function loadSources() {
  try {
    S.sources = await apiGet('/api/sources');
    const sel = $('#f-source');
    const cur = sel.value;
    sel.innerHTML = '<option value="">全部媒体源</option>' +
      S.sources.map(s => `<option value="${esc(s.id)}">${esc(s.root_path)}${s.online ? '' : '（离线）'}</option>`).join('');
    if (cur) sel.value = cur;
  } catch (e) { toast(e.message, 'err'); }
}

async function loadFacets() {
  try {
    const f = await apiGet('/api/facets');
    const sel = $('#f-year');
    const cur = sel.value;
    sel.innerHTML = '<option value="">全部年份</option>' +
      (f.years || []).map(y => `<option value="${esc(y.year)}">${esc(y.year)}（${y.count}）</option>`).join('');
    sel.value = cur;
    const tsel = $('#f-tag');
    const curTag = tsel.value;
    tsel.innerHTML = '<option value="">全部标签</option>' +
      (f.tags || []).map(t => `<option value="${esc(t.name)}">${esc(t.name)}（${t.count}）</option>`).join('');
    tsel.value = curTag;
  } catch (e) { /* 忽略 */ }
}

/** 标签下拉/编辑共用：完整标签表（含颜色与用量） */
async function fetchTags() {
  try { return await apiGet('/api/tags'); } catch (e) { return []; }
}

/* ============================ 媒体库：虚拟滚动 ============================ */
const scrollEl = () => $('#grid-scroll');

function filterParams(extra = {}) {
  const p = {};
  const f = S.filter;
  if (f.watermarked !== undefined && f.watermarked !== '') p.watermarked = String(f.watermarked);
  ['q', 'media_type', 'source_id', 'year', 'month', 'date_from', 'date_to', 'confidence', 'tag', 'job_id'].forEach(k => {
    if (f[k]) p[k] = f[k];
  });
  ['needs_review', 'dup', 'marked', 'unknown_date', 'missing', 'star', 'favorite', 'live'].forEach(k => { if (f[k]) p[k] = '1'; });
  p.sort = S.sort; p.order = S.order;
  return Object.assign(p, extra);
}

function layout() {
  if (S.view !== 'library') return;
  const el = scrollEl();
  const w = el.clientWidth || 800;
  if (S.mode === 'grid') {
    G.tileW = 178; G.tileH = 200; G.gap = 12; G.pad = 18;
    G.cols = Math.max(1, Math.floor((w - 2 * G.pad + G.gap) / (G.tileW + G.gap)));
    // 让卡片按可用宽度均分，避免右侧留出不规则的空白
    const avail = w - 2 * G.pad - (G.cols - 1) * G.gap;
    G.tileW = Math.max(120, Math.floor(avail / G.cols));
    G.rowH = G.tileH + G.gap;
  } else {
    G.tileW = Math.max(360, w - 2 * G.pad); G.tileH = 46; G.gap = 0; G.pad = 18;
    G.cols = 1; G.rowH = G.tileH;
  }
  G.rows = Math.ceil(G.total / G.cols);
  $('#grid-spacer').style.height = (G.pad * 2 + G.rows * G.rowH) + 'px';
  renderWindow(true);
}

function setMode(mode) {
  S.mode = mode;
  $('#viewmode').textContent = mode === 'grid' ? '▤' : '☰';
  const head = $('#list-head');
  if (head) head.style.display = mode === 'list' ? 'flex' : 'none';
  G.nodes.forEach(n => n.remove());
  G.nodes.clear();
  layout();
}

async function loadMedia(reset) {
  S.mediaRevision = Date.now();
  S.gen++;
  const gen = S.gen;
  G.items.clear(); G.pages.clear();
  G.nodes.forEach(n => n.remove()); G.nodes.clear();
  if (reset) scrollEl().scrollTop = 0;
  try {
    const r = await apiGet('/api/media', filterParams({ offset: 0, limit: G.pageSize }));
    if (gen !== S.gen) return;
    G.total = r.total;
    S.filter.total = r.total;
    const active=Object.keys(S.filter).filter(k=>k!=='total' && S.filter[k]!==undefined && S.filter[k]!==false && S.filter[k]!=='').length;
    $('#filter-count').textContent=active ? ' · '+active : '';
    r.items.forEach((it, i) => G.items.set(i, it));
    G.pages.set(0, 'done');
    $('#lib-empty').hidden = G.total > 0;
    const hasLibrary = (S.state && S.state.assets > 0);
    $('#lib-empty h3').textContent = hasLibrary ? '没有符合筛选条件的照片' : '媒体库还是空的';
    $('#lib-empty p').textContent = hasLibrary ? '试试清除筛选条件，或添加其他媒体源。' : '先添加照片文件夹并扫描建立索引，扫描不会修改原文件。';
    $('#lib-hint').hidden = G.total === 0;
    if (G.total > 0) $('#lib-hint').textContent = `共 ${G.total} 项 · 按${S.sort === 'taken' ? '拍摄时间' : '当前排序'}展示 · 单击打开详情，Ctrl+单击多选`;
    layout();
    updateSelBar();
  } catch (e) { toast(e.message, 'err'); }
}

async function fetchPage(p) {
  if (G.pages.has(p)) return;
  G.pages.set(p, 'loading');
  const gen = S.gen;
  try {
    const r = await apiGet('/api/media', filterParams({ offset: p * G.pageSize, limit: G.pageSize }));
    if (gen !== S.gen) return;
    r.items.forEach((it, i) => G.items.set(p * G.pageSize + i, it));
    G.pages.set(p, 'done');
    renderWindow();
  } catch (e) {
    G.pages.delete(p);
    toast(e.message, 'err');
  }
}

function ensureLoaded(start, end) {
  const firstPage = Math.max(0, Math.floor(start / G.pageSize));
  const lastPage = Math.max(0, Math.floor((end - 1) / G.pageSize));
  for (let p = firstPage; p <= lastPage; p++) fetchPage(p);
}

function renderWindow(force) {
  const el = scrollEl();
  if (!el) return;
  const top = el.scrollTop, h = el.clientHeight;
  const firstRow = Math.max(0, Math.floor((top - G.pad) / G.rowH) - 1);
  const lastRow = Math.min(Math.max(0, G.rows - 1), Math.ceil((top + h - G.pad) / G.rowH) + 1);
  const start = firstRow * G.cols;
  const end = Math.min(G.total, (lastRow + 1) * G.cols);
  ensureLoaded(start, end);

  const needed = new Set();
  for (let i = start; i < end; i++) needed.add(i);
  for (const [i, node] of Array.from(G.nodes.entries())) {
    if (!needed.has(i)) { node.remove(); G.nodes.delete(i); }
  }
  const layer = $('#grid-layer');
  for (const i of needed) {
    let node = G.nodes.get(i);
    if (!node) {
      node = createNode();
      G.nodes.set(i, node);
      layer.appendChild(node);
    }
    positionNode(node, i);
    updateNode(node, i, force);
  }
}

function positionNode(node, i) {
  const row = Math.floor(i / G.cols);
  const col = i % G.cols;
  node.style.top = (G.pad + row * G.rowH) + 'px';
  node.style.left = (G.pad + col * (G.tileW + G.gap)) + 'px';
  node.style.width = G.tileW + 'px';
  node.style.height = G.tileH + 'px';
}

function createNode() {
  const d = document.createElement('div');
  if (S.mode === 'grid') {
    d.className = 'tile';
    d.innerHTML = `<div class="thumb"><div class="ph" style="display:none;position:absolute;inset:0">▢</div></div>
      <div class="cap"><div class="n"></div><div class="d"></div></div>`;
    d.addEventListener('click', ev => {
      const it = G.items.get(Number(d.dataset.idx));
      if (!it) return;
      const rotate=ev.target.closest('[data-quick-rotate]');
      if(rotate){quickRotate([it.id],rotate.dataset.quickRotate);return;}
      if (ev.target.classList.contains('check') || ev.ctrlKey || ev.metaKey) toggleSel(it.id);
      else queueDetail(it.id);
    });
  } else {
    d.className = 'row';
    d.innerHTML = `<div class="rthumb"></div><div class="rname"></div><div class="rdate"></div>
      <div class="rsize"></div><div class="rres"></div><div class="rtag"></div>`;
    d.addEventListener('click', ev => {
      const it = G.items.get(Number(d.dataset.idx));
      if (!it) return;
      if (ev.ctrlKey || ev.metaKey) toggleSel(it.id); else queueDetail(it.id);
    });
  }
  d.addEventListener('dblclick',ev=>{
    if(ev.ctrlKey || ev.metaKey || ev.target.closest('button,.check')) return;
    clearTimeout(detailClickTimer);
    const it=G.items.get(Number(d.dataset.idx)); if(it) openDetail(it.id,true);
  });
  return d;
}

function thumbUrl(it, size) {
  return `/api/thumb/${it.id}?size=${size}&${AUTH}&v=${it.posterTs || S.mediaRevision}`;
}
function fileUrl(fileId) { return `/api/file/${fileId}?${AUTH}&v=${Date.now()}`; }
function imageUrl(fileId) { return `/api/image/${fileId}?${AUTH}&v=${Date.now()}`; }
const DECODABLE = /^(jpg|jpeg|png|webp|gif|bmp|tif|tiff)$/;
const HEIF_EXT = /^(heic|heif)$/;

/** 启动时取一次能力表：HEIC 是否可用取决于外置解码器 */
async function loadCaps() {
  try { S.caps = await apiGet('/api/capabilities'); } catch (e) { S.caps = null; }
}

function heifReady() {
  return !!(S.caps && S.caps.heic && S.caps.heic.ok);
}

/** 该扩展名能否显示预览（HEIC 需要解码器，浏览器本身也解不了 HEIC） */
function canPreviewImage(ext) {
  ext = (ext || '').toLowerCase();
  if (DECODABLE.test(ext)) return true;
  if (HEIF_EXT.test(ext)) return heifReady();
  return false;
}

function updateNode(node, i, force) {
  const it = G.items.get(i);
  node.dataset.idx = i;
  if (!it) {
    if (node.dataset.sig !== 'pending') {
      node.dataset.sig = 'pending';
      if (S.mode === 'grid') {
        node.querySelector('.cap .n').textContent = '加载中…';
        node.querySelector('.cap .d').textContent = '';
      }
    }
    return;
  }
  const sel = S.sel.has(it.id);
  node.classList.toggle('sel', sel);
  node.classList.toggle('off', it.availability === 'missing');

  if (S.mode === 'grid') {
    const sig = [it.id, it.posterTs, sel, it.availability, it.marked, it.dup, it.needs_review,
      it.star, it.favorite, it.live, it.watermarked, (it.tags || []).join(',')].join('|');
    if (sig !== node.dataset.sig || force) {
      node.dataset.sig = sig;
      const thumb = node.querySelector('.thumb');
      const isVideo = it.media_type === 'video';
      const ext = (it.ext || '').toLowerCase();
      let inner;
      if (isVideo) {
        inner = `<img alt="" data-asset="${esc(it.id)}" src="${thumbUrl(it, 'poster')}"
                   onerror="if(this.parentNode){this.style.display='none';var p=this.parentNode.querySelector('.ph');if(p)p.style.display='grid';}pvmPosterFallback(this)">
                 <div class="ph" style="display:none;position:absolute;inset:0">▶</div>`;
      } else if (!canPreviewImage(ext)) {
        inner = `<div class="ph" style="position:absolute;inset:0">${esc(ext.toUpperCase())}<br><span style="font-size:10px">无解码器</span></div>`;
      } else {
        inner = `<img alt="" loading="lazy" src="${thumbUrl(it, 256)}"
                   onerror="if(this.parentNode){this.style.display='none';var p=this.parentNode.querySelector('.ph');if(p)p.style.display='grid';}">
                 <div class="ph" style="display:none;position:absolute;inset:0">▢</div>`;
      }
      const badges = [];
      if (it.needs_review) badges.push('<span class="b warn">待确认</span>');
      if (it.dup) badges.push('<span class="b">重复</span>');
      if (it.availability === 'missing') badges.push('<span class="b">源缺失</span>');
      if (it.favorite) badges.push('<span class="b star">♥</span>');
      if (it.live) badges.push('<span class="b live">LIVE</span>');
      const tagLine = (it.tags || []).slice(0, 2).map(t => `<span class="b tag">${esc(t)}</span>`).join('');
      thumb.innerHTML = inner +
        (!isVideo ? `<div class="tile-quick"><button class="btn sm" data-quick-rotate="rot270" title="左旋当前照片">⟲</button><button class="btn sm" data-quick-rotate="rot90" title="右旋当前照片">⟳</button></div>` : '') +
        `<div class="badges"><div style="display:flex;gap:4px;flex-wrap:wrap">${badges.join('')}</div>
           <div>${isVideo ? '<span class="b vid">视频</span>' : ''}</div></div>
         <div class="check">${sel ? '✓' : ''}</div>
         <div class="tags-on-thumb">${tagLine}</div>
         <div class="dur">${isVideo ? fmtDur(it.duration_ms) : ''}</div>`;
      node.querySelector('.cap .n').textContent = it.file_name || '(未知文件)';
      node.querySelector('.cap .d').textContent = (it.watermarked ? '已有水印 · ' : '') + fmtTaken(it.taken_at, it.date_precision) + ' · ' + fmtSize(it.size_bytes);
    }
  } else {
    const sig = [it.id, sel, it.star, it.favorite, it.live].join('|');
    if (sig !== node.dataset.sig || force) {
      node.dataset.sig = sig;
      const rt = node.querySelector('.rthumb');
      const isVid = it.media_type === 'video';
      const src = isVid ? thumbUrl(it, 'poster') : thumbUrl(it, 256);
      rt.innerHTML = `<img alt="" loading="lazy" data-asset="${esc(it.id)}" src="${src}"
        onerror="${isVid ? 'this.style.visibility=\'hidden\';pvmPosterFallback(this)' : 'this.style.visibility=\'hidden\''}">`;
      node.querySelector('.rname').textContent =
        (it.star ? '★ ' : '') + (it.file_name || '') + (it.live ? '  · LIVE' : '');
      node.querySelector('.rdate').textContent = (it.watermarked ? '已有水印 · ' : '') + fmtTaken(it.taken_at, it.date_precision);
      node.querySelector('.rsize').textContent = fmtSize(it.size_bytes);
      node.querySelector('.rres').textContent =
        (it.width && it.height ? it.width + '×' + it.height : '') + (it.media_type === 'video' ? ' ' + fmtDur(it.duration_ms) : '');
      const tags = [];
      if (it.needs_review) tags.push('<span class="pill warn">待确认</span>');
      if (it.dup) tags.push('<span class="pill">重复</span>');
      if (it.availability === 'missing') tags.push('<span class="pill bad">缺失</span>');
      if (it.star) tags.push('<span class="pill star">★</span>');
      if (it.favorite) tags.push('<span class="pill">收藏</span>');
      (it.tags || []).slice(0, 2).forEach(t => tags.push(`<span class="pill">${esc(t)}</span>`));
      node.querySelector('.rtag').innerHTML = tags.join(' ');
    }
  }
}

/* ---- 视频封面：优先后端 FFmpeg 抽帧，失败再回退浏览器抽帧 ---- */
const posterQ = [];
let posterActive = 0;

/** 后端抽帧失败时（img onerror）才用浏览器抽帧兜底 */
function pvmPosterFallback(img) {
  if (!img) return;
  const id = img.getAttribute('data-asset');
  if (!id) return;
  let it = null;
  G.items.forEach(v => { if (!it && v.id === id) it = v; });
  if (!it && Array.isArray(TL.items)) TL.items.forEach(v => { if (!it && v.id === id) it = v; });
  if (it) enqueuePoster(it);
}
function enqueuePoster(it) {
  if (it.media_type !== 'video' || it._posterTried || !it.file_id) return;
  it._posterTried = true;
  posterQ.push(it);
  pumpPoster();
}
function pumpPoster() {
  while (posterActive < 2 && posterQ.length) {
    const it = posterQ.shift();
    posterActive++;
    makePoster(it).catch(() => { }).then(() => { posterActive--; pumpPoster(); });
  }
}
function makePoster(it) {
  return new Promise((resolve, reject) => {
    const v = document.createElement('video');
    v.muted = true; v.preload = 'auto'; v.src = fileUrl(it.file_id);
    let done = false;
    const finish = ok => {
      if (done) return;
      done = true;
      v.removeAttribute('src'); v.load();
      ok ? resolve() : reject(new Error('no poster'));
    };
    const timer = setTimeout(() => finish(false), 9000);
    v.addEventListener('error', () => { clearTimeout(timer); finish(false); });
    v.addEventListener('loadeddata', () => {
      const d = isFinite(v.duration) ? v.duration : 0;
      try { v.currentTime = Math.min(1.5, Math.max(0.05, d * 0.1)); } catch (e) { }
    });
    v.addEventListener('seeked', async () => {
      try {
        const w = 480;
        const vw = v.videoWidth || 16, vh = v.videoHeight || 9;
        const h = Math.max(1, Math.round(w * vh / vw));
        const c = document.createElement('canvas');
        c.width = w; c.height = h;
        c.getContext('2d').drawImage(v, 0, 0, w, h);
        await api('/api/thumb/' + it.id + '/poster', { method: 'POST', body: { data_url: c.toDataURL('image/jpeg', 0.72) } });
        it.posterTs = Date.now();
        G.nodes.forEach((node, idx) => {
          const x = G.items.get(idx);
          if (x && x.id === it.id) { node.dataset.sig = ''; updateNode(node, idx); }
        });
        // 时间线里的缩略图也用缓存键拼 URL，需要一并刷新
        if (typeof tlRender === 'function') tlRender();
        clearTimeout(timer);
        finish(true);
      } catch (e) { clearTimeout(timer); finish(false); }
    });
  });
}

/* ============================ 选择 ============================ */
function toggleSel(id) {
  if (S.sel.has(id)) S.sel.delete(id); else S.sel.add(id);
  G.nodes.forEach((node, idx) => updateNode(node, idx));
  updateSelBar();
}
function updateSelBar() {
  const n = S.sel.size;
  // 只有能勾选媒体的视图才显示批量操作条，否则切到设置/任务中心会留一条悬空的操作栏
  const selectable = S.view === 'library';
  $('#selbar').hidden = n === 0 || !selectable;
  $('#sel-count').textContent = '已选 ' + n + ' 项';
  $('#sel-all').textContent = S.filter.total ? `全选筛选结果（${S.filter.total}）` : '全选当前筛选结果';
  // 旋转翻转面板的范围提示随选择变化
  if (S.view === 'library' && EDIT.open) { EDIT.ids = Array.from(S.sel); loadEditView(); }
}

/* ============================ 详情 ============================ */
async function openDetail(id, fullscreen = false) {
  clearTimeout(detailClickTimer);
  S.detailId = id;
  $('.app').classList.add('with-detail');
  $('#detail').hidden = false;
  $('#d-title').textContent = '读取中…';
  $('#d-body').innerHTML = '<div class="skeleton">正在读取媒体信息…</div>';
  try {
    const a = await apiGet('/api/media/' + id);
    if (S.detailId !== id) return;
    renderDetail(a);
    if (fullscreen) openMediaViewer(a);
  } catch (e) {
    if (S.detailId !== id) return;
    $('#d-body').innerHTML = `<div class="skeleton">${esc(e.message)}</div>`;
  }
}
function closeDetail() {
  clearTimeout(detailClickTimer);
  S.detailId = null;
  $('.app').classList.remove('with-detail');
  $('#detail').hidden = true;
  $('#d-body').replaceChildren();
  setTimeout(layout, 60);
}

function renderDetail(a) {
  const files = a.files || [];
  const primary = files[0] || {};
  const isVideo = a.media_type === 'video';
  $('#d-delete').hidden = isVideo;
  const ext = (primary.ext || '').toLowerCase();
  $('#d-fullscreen').onclick = () => openMediaViewer(a);
  const decodable = canPreviewImage(ext);
  const heifPreview = HEIF_EXT.test(ext) && heifReady();
  $('#d-title').textContent = primary.file_name || '媒体详情';

  let preview;
  if (isVideo) {
    preview = `<video id="d-video" controls preload="metadata" src="${fileUrl(primary.id)}"></video>
      <div class="ph" id="d-vmsg" style="display:none"></div>`;
  } else if (heifPreview || /^(tif|tiff)$/.test(ext)) {
    // 浏览器内核不能解 HEIC，用后端解码出的 640 预览图代替
    preview = `<img id="d-img" src="${imageUrl(primary.id)}" alt=""
      onerror="if(this.parentNode)this.parentNode.innerHTML='<div class=&quot;ph&quot;>图片预览生成失败，请检查文件是否可访问</div>'">`;
  } else if (!decodable) {
    preview = HEIF_EXT.test(ext)
      ? `<div class="ph">HEIC / HEIF 需要外置解码器（lib\\heif.dll）<br>当前未启用，仍可索引、去重与归档</div>`
      : `<div class="ph">${esc(ext.toUpperCase())} 没有内置解码器<br>可以索引与归档，但无法在此预览</div>`;
  } else if (primary.availability === 'missing') {
    preview = `<div class="ph">源文件当前不可访问<br>（外接盘未连接或文件已被移动）</div>`;
  } else {
    preview = `<img id="d-img" src="${fileUrl(primary.id)}" alt=""
      onerror="this.onerror=null;this.src='${imageUrl(primary.id)}'">`;
  }

  let meta = {};
  try { meta = JSON.parse(a.metadata_json || '{}'); } catch (e) { }
  const err = meta.error;
  let cands = [];
  try { cands = JSON.parse(primary.candidates_json || '[]'); } catch (e) { }
  const gps = (a.gps_lat != null && a.gps_lon != null)
    ? `${Number(a.gps_lat).toFixed(6)}, ${Number(a.gps_lon).toFixed(6)}` : '—';
  const tagList = a.tags || [];
  const companion = a.live_companion || null;

  $('#d-body').innerHTML = `
    <div class="preview" id="d-preview">${preview}</div>
    <div class="meta">
      ${err ? `<div class="warnbox">${esc(err)}</div>` : ''}
      <div class="line"><div class="k">日期</div><div class="v"><b>${esc(fmtTaken(a.taken_at, a.date_precision))}</b>
        <span class="pill ${a.confidence === 'high' ? 'ok' : a.confidence === 'none' ? 'bad' : 'warn'}">${esc(confLabel(a.confidence))}</span>
        <span class="pill">${esc(srcLabel(a.date_source))}</span></div></div>
      ${a.needs_review ? `<div class="line"><div class="k">待确认</div><div class="v" style="color:#f2a93b">${esc(a.review_reason || '日期需要人工确认')}</div></div>` : ''}
      ${a.override_taken_at ? `<div class="line"><div class="k">人工确认</div><div class="v">${esc(a.override_taken_at==='__confirmed_unknown__'?'已确认日期未知':String(a.override_taken_at).replace('T', ' '))}
        <button class="btn sm" data-act="clear-date">清除覆盖</button></div></div>` : ''}
      <div class="line"><div class="k">类型</div><div class="v">${isVideo ? '视频' : '照片'} · ${esc(ext.toUpperCase())}</div></div>
      <div class="line"><div class="k">尺寸</div><div class="v">${a.width && a.height ? a.width + ' × ' + a.height : '—'}${isVideo ? ' · ' + fmtDur(a.duration_ms) : ''}</div></div>
      <div class="line"><div class="k">大小</div><div class="v">${fmtSize(primary.size_bytes)}</div></div>
      <div class="line"><div class="k">相机</div><div class="v">${esc(a.camera || '—')}</div></div>
      <div class="line"><div class="k">GPS</div><div class="v">${esc(gps)}</div></div>
      <div class="line"><div class="k">SHA-256</div><div class="v mono">${esc(primary.sha256 || '未计算')}</div></div>
      <div class="line"><div class="k">标记</div><div class="v">
        <button class="btn sm" data-act="fav">${a.favorite ? '已收藏' : '收藏'}</button>
      </div></div>

      <h4>标签</h4>
      <div class="tagbox">
        ${tagList.length
          ? tagList.map(t => `<span class="tagchip">${esc(t.name)}<b data-untag="${esc(t.name)}" title="移除">×</b></span>`).join('')
          : '<span class="tplhint">还没有标签</span>'}
        <input id="d-tag-input" type="text" placeholder="输入标签后回车" list="d-tag-list" style="width:130px">
        <datalist id="d-tag-list"></datalist>
        <button class="btn sm" data-act="add-tag">添加</button>
      </div>

      <h4>实况照片</h4>
      ${companion
        ? `<div class="file"><div class="fp">${esc(companion.file_name || '')}</div>
             <div class="ft">
               <span class="pill ok">已配对</span>
               <span class="pill">${companion.media_type === 'video' ? '视频' : '照片'}</span>
               <span class="pill">${fmtSize(companion.size_bytes)}</span>
               <button class="btn sm" data-open-media="${esc(companion.id)}">查看配对媒体</button>
               <button class="btn sm" data-act="unpair">解除配对</button>
             </div></div>`
        : `<div class="tplhint">${isVideo
            ? '视频只在照片侧显示配对信息。'
            : '未找到同名 MOV。实况照片需要照片与视频在同一目录、主名一致；可在设置里重新配对。'}</div>`}

      <h4>修改拍摄日期（只写入索引）</h4>
      <div class="datebox">
        <input type="date" id="d-date" value="${a.taken_at ? a.taken_at.slice(0, 10) : ''}">
        <input type="time" id="d-time" step="1" value="${a.taken_at && a.date_precision === 'second' ? a.taken_at.slice(11, 19) : '00:00:00'}">
        <button class="btn sm primary" data-act="set-date">保存</button>
        <button class="btn sm" data-act="set-date-null">标记为未知</button>
      </div>

      <h4>日期候选与证据</h4>
      <div class="cands">
        ${cands.length ? cands.map(c => `<div class="c">
          <span class="pill ${c.confidence === 'high' ? 'ok' : 'warn'}">${esc(confLabel(c.confidence))}</span>
          <b>${esc(String(c.value).replace('T', ' '))}</b>
          <span class="s">${esc(srcLabel(c.source))} · ${esc(c.raw || c.detail || '')}</span>
          <button class="btn sm" data-use="${esc(c.value)}">采用</button>
        </div>`).join('') : '<div class="skeleton">没有可用的日期线索</div>'}
      </div>

      <h4>文件位置（${files.length}）</h4>
      ${files.map(f => `<div class="file">
        <div class="fp">${esc(f.absolute_path)}</div>
        <div class="ft">
          <span class="pill ${f.availability === 'online' ? 'ok' : 'bad'}">${f.availability === 'online' ? '在线' : f.availability === 'missing' ? '缺失' : '离线'}</span>
          <span class="pill">${fmtSize(f.size_bytes)}</span>
          <button class="btn sm" data-reveal="${esc(f.id)}">打开所在文件夹</button>
          <button class="btn sm" data-copy="${esc(f.absolute_path)}">复制路径</button>
          <button class="btn sm" data-preview="${esc(f.id)}" data-ext="${esc(f.ext)}" data-asset="${esc(a.id)}">用此副本预览</button>
        </div>
      </div>`).join('')}

      <div class="row-actions">
        <button class="btn sm" data-act="refresh">重新读取元数据</button>
        <button class="btn sm" data-act="archive-one">加入归档任务</button>
      </div>
    </div>`;
  installDatePickers($('#d-body'));
  organizeDetail(a);

  const v = $('#d-video');
  if (v) {
    v.addEventListener('error', () => {
      const msg = $('#d-vmsg');
      if (!msg) return;
      v.style.display = 'none';
      msg.style.display = 'block';
      const codec = (meta.details && meta.details.video && meta.details.video.codec) || '未知编码';
      msg.innerHTML = '此编码在当前系统/浏览器内核中无法播放<br><span style="font-size:11px;color:#6a7387">' +
        esc(codec) + ' · 可安装 HEVC 扩展，或用「打开所在文件夹」以原始播放器查看</span>';
    });
  }

  $('#d-body').onclick = async ev => {
    const t = ev.target.closest('button');
    const un = ev.target.closest('[data-untag]');
    if (un) {
      const names = tagList.map(x => x.name).filter(n => n !== un.dataset.untag);
      try {
        await api('/api/media/' + a.id + '/tags', { method: 'POST', body: { tags: names } });
        toast('标签已更新', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
      return;
    }
    if (!t) return;
    if (t.dataset.openMedia) { openDetail(t.dataset.openMedia); return; }
    if (t.dataset.reveal) {
      try { await api('/api/reveal', { method: 'POST', body: { file_id: t.dataset.reveal } }); }
      catch (e) { toast(e.message, 'err'); }
      return;
    }
    if (t.dataset.copy) {
      try { await navigator.clipboard.writeText(t.dataset.copy); toast('路径已复制', 'ok'); }
      catch (e) { toast('复制失败，请手动选择路径', 'err'); }
      return;
    }
    if (t.dataset.use) {
      try {
        await api('/api/media/' + a.id + '/date', { method: 'POST', body: { date: t.dataset.use } });
        toast('日期已更新', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
      return;
    }
    if (t.dataset.preview) {
      const img = $('#d-img');
      if (!img) return;
      img.dataset.fileId = t.dataset.preview;
      const fext = (t.dataset.ext || '').toLowerCase();
      if (HEIF_EXT.test(fext) || /^(tif|tiff)$/.test(fext) || !DECODABLE.test(fext)) {
        // 浏览器打不开的格式（HEIC/视频等）退回后端缩略图，避免只看到一片空白
        img.onerror = () => toast('这个副本无法生成预览（解码器缺失或文件已移动）', 'err');
        img.style.visibility = '';
        img.src = imageUrl(t.dataset.preview);
        return;
      }
      img.onerror = () => toast('这个副本无法直接显示（可能已移动或没有权限）', 'err');
      img.style.visibility = '';
      img.src = fileUrl(t.dataset.preview);
      img.style.transform = 'none';
      return;
    }
    const act = t.dataset.act;
    if (act === 'set-date' || act === 'set-date-null') {
      let date = null;
      if (act === 'set-date') {
        const d = $('#d-date').value;
        if (!d) return toast('请选择日期', 'err');
        date = d + 'T' + (($('#d-time').value || '00:00:00').slice(0, 8));
        if (date.length === 10) date += 'T00:00:00';
      }
      try {
        await api('/api/media/' + a.id + '/date', { method: 'POST', body: { date, unknown:act==='set-date-null' } });
        toast('已保存', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'clear-date') {
      try {
        await api('/api/media/' + a.id + '/date', { method: 'POST', body: { date: null } });
        toast('已清除覆盖值', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'refresh') {
      try {
        await api('/api/media/' + a.id + '/refresh', { method: 'POST' });
        toast('元数据已重新读取', 'ok');
        openDetail(a.id);
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'mark') {
      try {
        await api('/api/media/' + a.id + '/mark', { method: 'POST', body: { marked: !primary.marked } });
        openDetail(a.id);
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'archive-one') {
      openArchiveWizard([a.id], null);
    } else if (act === 'star') {
      try {
        await api('/api/media/batch-flags', { method: 'POST', body: { ids: [a.id], star: !a.star } });
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'fav') {
      try {
        await api('/api/media/batch-flags', { method: 'POST', body: { ids: [a.id], favorite: !a.favorite } });
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'add-tag') {
      const input = $('#d-tag-input');
      const add = (input.value || '').split(/[,，、]/).map(s => s.trim()).filter(Boolean);
      if (!add.length) return toast('请输入标签名', 'err');
      const names = Array.from(new Set(tagList.map(x => x.name).concat(add)));
      try {
        await api('/api/media/' + a.id + '/tags', { method: 'POST', body: { tags: names } });
        toast('标签已添加', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    } else if (act === 'unpair') {
      try {
        await api('/api/media/' + a.id + '/livepair', { method: 'POST', body: { partner: null } });
        toast('已解除配对', 'ok');
        openDetail(a.id); refreshAfterChange();
      } catch (e) { toast(e.message, 'err'); }
    }
  };
  $('#d-preview').ondblclick=ev=>{
    if(ev.target.id==='d-img') openMediaViewer(a,ev.target.dataset.fileId || primary.id);
  };

  // 标签输入框：回车即添加，并补全已有标签
  const tagInput = $('#d-tag-input');
  if (tagInput) {
    tagInput.addEventListener('keydown', ev => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        const btn = $('[data-act="add-tag"]');
        if (btn) btn.click();
      }
    });
    fetchTags().then(list => {
      const dl = $('#d-tag-list');
      if (dl) dl.innerHTML = (list || []).map(t => `<option value="${esc(t.name)}"></option>`).join('');
    });
  }
}

function refreshAfterChange() {
  loadMedia(false); loadState(); loadFacets();
  if (S.view === 'review') loadReview();
}

/* ============================ 待整理 ============================ */
const REVIEW={gen:0,items:[],total:0,busy:false,processing:false};
async function loadReview(append=false) {
  if(append && REVIEW.busy)return;
  const gen=++REVIEW.gen;REVIEW.busy=true;
  if(!append){REVIEW.items=[];S.reviewSel.clear();}
  const box = $('#review-list');
  if(!append)box.innerHTML = '<div class="skeleton">读取中…</div>';
  try {
    const r = await apiGet('/api/review', { mode: S.reviewMode, limit: 80,offset:REVIEW.items.length });
    if(gen!==REVIEW.gen)return;
    REVIEW.items=append?REVIEW.items.concat(r.items):r.items;REVIEW.total=r.total;
    $('#review-total').textContent='共 '+r.total+' 项待确认';
    if (!REVIEW.items.length) {
      box.innerHTML = `<div class="empty" style="position:static"><div class="empty-ico">✓</div>
        <h3>没有需要确认的项目</h3><p>当前筛选下的日期都有可靠来源。</p></div>`;
      $('#review-bar').hidden = true;
      updateReviewBar();
      return;
    }
    box.innerHTML = REVIEW.items.map(it => {
      const cands = it.candidates || [];
      return `<div class="rv-item ${S.reviewSel.has(it.id)?'sel':''}" data-id="${esc(it.id)}">
        <input type="checkbox" data-pick="${esc(it.id)}" style="width:15px;height:15px" ${S.reviewSel.has(it.id)?'checked':''}>
        <div class="rv-thumb">${it.media_type === 'video'
          ? `<img src="${thumbUrl(it, 'poster')}" onerror="this.remove()">`
          : `<img src="${thumbUrl(it, 256)}" onerror="this.remove()">`}</div>
        <div class="rv-main">
          <div class="rv-name">${esc(it.file_name || '媒体 ' + String(it.id).slice(0, 8))}</div>
          <div class="rv-reason">${esc(it.review_reason || '日期来源不可靠')}</div>
          <div class="rv-cands">
            ${it.taken_at ? `<span class="cand ${it.confidence}"><b>当前：${esc(fmtTaken(it.taken_at, it.date_precision))}</b>（${esc(srcLabel(it.date_source))}）</span>` : ''}
            ${cands.slice(0, 5).map(c => `<span class="cand ${c.confidence}" data-use="${esc(c.value)}" data-id="${esc(it.id)}">
              <b>${esc(String(c.value).replace('T', ' '))}</b> ${esc(srcLabel(c.source))}</span>`).join('')}
          </div>
        </div>
        <div class="rv-actions"><button class="btn sm" data-open="${esc(it.id)}">详情</button></div>
      </div>`;
    }).join('') + `<div class="review-more">已加载 ${REVIEW.items.length} / 共 ${r.total} 项 ${REVIEW.items.length<r.total?'<button class="btn sm" data-review-more="1">加载更多</button>':''}</div>`;
    $('#review-bar').hidden = false;
    updateReviewBar();
  } catch (e) {
    if(gen===REVIEW.gen)box.innerHTML = `<div class="skeleton">${esc(e.message)}</div>`;
  }finally{if(gen===REVIEW.gen)REVIEW.busy=false;}
}
function updateReviewBar() {
  $('#review-count').textContent = '已选 ' + S.reviewSel.size + ' 项';
  ['review-filesystem','review-recommended','review-apply','review-unknown'].forEach(id=>$('#'+id).disabled=!S.reviewSel.size || REVIEW.processing);
}
$('#review-list').addEventListener('click', async ev => {
  const t = ev.target.closest('[data-open],[data-use],[data-review-more]');if(!t)return;
  if(t.dataset.reviewMore){loadReview(true);return;}
  if (t.dataset.open) { openDetail(t.dataset.open); return; }
  if (t.dataset.use) {
    try {
      await api('/api/media/' + t.dataset.id + '/date', { method: 'POST', body: { date: t.dataset.use } });
      toast('日期已确认', 'ok');
      await Promise.all([loadReview(),loadState(),loadFacets()]);
    } catch (e) { toast(e.message, 'err'); }
  }
});
$('#review-list').addEventListener('change', ev => {
  const t = ev.target;
  if (t.dataset.pick) {
    if (t.checked) S.reviewSel.add(t.dataset.pick); else S.reviewSel.delete(t.dataset.pick);
    t.closest('.rv-item').classList.toggle('sel', t.checked);
    updateReviewBar();
  }
});
$('#review-apply').addEventListener('click', async () => {
  const ids = Array.from(S.reviewSel);
  if (!ids.length) return toast('请先勾选条目', 'err');
  const d = $('#review-date').value;
  if (!d) return toast('请选择日期', 'err');
  const date = d + 'T' + (($('#review-time').value || '00:00:00').slice(0, 8));
  try {
    const r = await api('/api/media/batch-date', { method: 'POST', body: { ids, date } });
    toast(`已更新 ${r.updated} 项`, 'ok');
    await Promise.all([loadReview(),loadState(),loadFacets()]);
  } catch (e) { toast(e.message, 'err'); }
});
$('#review-unknown').addEventListener('click', async () => {
  const ids = Array.from(S.reviewSel);
  if (!ids.length) return toast('请先勾选条目', 'err');
  try {
    const r = await api('/api/media/batch-date', { method: 'POST', body: { ids, date: null,unknown:true } });
    toast(`已确认 ${r.updated} 项日期未知`, 'ok');
    await Promise.all([loadReview(),loadState(),loadFacets()]);
  } catch (e) { toast(e.message, 'err'); }
});

/* ============================ 重复文件 ============================ */
async function loadDupes() {
  const box = $('#dup-list');
  box.innerHTML = '<div class="skeleton">统计中…</div>';
  try {
    const r = await apiGet('/api/duplicates');
    $('#dup-stat').textContent = `${r.group_count} 组 · 可回收 ${fmtSize(r.reclaimable_bytes)} · 未计算 ${r.unhashed}`;
    if (!r.groups.length) {
      box.innerHTML = `<div class="empty" style="position:static"><div class="empty-ico">⧉</div>
        <h3>没有发现完全重复的文件</h3>
        <p>只有完整 SHA-256 一致才会被认定为重复。若刚扫描完，请先点「计算缺失哈希」。
        当前仍有 ${r.unhashed} 个文件未计算哈希。</p></div>`;
      return;
    }
    box.innerHTML = r.groups.map(g => `<div class="dup-group">
      <div class="dup-head">
        <b>${g.count} 份相同内容</b>
        <span class="hash">${esc(g.sha256.slice(0, 16))}…</span>
        <span>可回收 ${fmtSize(g.reclaimable_bytes)}</span>
      </div>
      ${g.files.map(f => `<div class="dup-file">
        <span class="pill ${f.availability === 'online' ? 'ok' : 'bad'}">${f.availability === 'online' ? '在线' : '缺失'}</span>
        <span class="p">${esc(f.path)}</span>
        <span class="m">${fmtSize(f.size_bytes)}</span>
        <span class="m">${esc((f.taken_at || '').slice(0, 10))}</span>
        <button class="btn sm" data-reveal-file="${esc(f.file_id)}">打开位置</button>
        <button class="btn sm" data-dup-mark="${esc(f.file_id)}" data-marked="${f.marked ? 1 : 0}">${f.marked ? '取消保留' : '保留此副本'}</button>
        ${f.media_type === 'image' ? `<button class="btn sm danger" data-dup-delete="${esc(f.file_id)}" data-asset-id="${esc(f.asset_id)}" ${f.availability !== 'online' ? 'disabled' : ''}>删除此副本…</button>` : ''}
      </div>`).join('')}
    </div>`).join('');
  } catch (e) { box.innerHTML = `<div class="skeleton">${esc(e.message)}</div>`; }
}
$('#dup-list').addEventListener('click', async ev => {
  const t = ev.target;
  if (t.dataset.revealFile) {
    try { await api('/api/reveal', { method: 'POST', body: { file_id: t.dataset.revealFile } }); }
    catch (e) { toast(e.message, 'err'); }
  } else if (t.dataset.dupMark) {
    const marked = t.dataset.marked !== '1';
    try {
      await api('/api/files/' + t.dataset.dupMark + '/mark', { method: 'POST', body: { marked } });
      loadDupes(); loadState();
    } catch (e) { toast(e.message, 'err'); }
  } else if (t.dataset.dupDelete) {
    openDeletePhotos([t.dataset.assetId], [t.dataset.dupDelete]).catch(e=>toast(e.message,'err'));
  }
});

/* ============================ 相似照片 ============================ */
async function loadSimilar() {
  const box = $('#similar-list');
  box.innerHTML = '<div class="skeleton">聚类中…</div>';
  try {
    const r = await apiGet('/api/similar', { threshold: $('#sim-threshold').value });
    const head = `<div class="tplhint" style="margin-bottom:10px">
      已计算 ${r.hashed} 张 · 未计算 ${r.unhashed} 张 · 阈值 ${r.threshold}/64（越小越严格）。
      这里只做候选提示，不提供“自动清理”。</div>`;
    if (!r.groups.length) {
      box.innerHTML = head + `<div class="empty" style="position:static"><div class="empty-ico">≈</div>
        <h3>没有发现相似照片</h3>
        <p>${r.unhashed > 0
          ? `还有 ${r.unhashed} 张照片没有计算相似指纹，先点右上角「计算相似指纹」。`
          : '当前阈值下没有相似候选。可以把阈值调大一些再试。'}</p></div>`;
      return;
    }
    box.innerHTML = head + r.groups.map((g, gi) => `<div class="sim-group">
      <div class="sim-head">
        <b>${g.count} 张相似</b>
        <span>最大差异 ${g.max_distance}/64</span>
        <span class="sp"></span>
        <button class="btn sm" data-sim-star="${gi}">全部收藏</button>
        <button class="btn sm danger" data-sim-delete-selected="${gi}">删除本组勾选项…</button>
      </div>
      <div class="sim-items">
        ${g.items.map(it => `<div class="sim-item" data-open="${esc(it.id)}">
          <div class="sim-thumb">
            <img loading="lazy" src="${thumbUrl(it, 256)}" alt=""
                 onerror="if(this.parentNode)this.parentNode.classList.add('noimg');this.remove();">
            ${it.star ? '<span class="b star">★</span>' : ''}
          </div>
          <div class="sim-meta">
            <span class="n" title="${esc(it.file_name || '')}">${esc(it.file_name || '')}</span>
            <span class="d">${esc(fmtTaken(it.taken_at))} · ${fmtSize(it.size_bytes)}</span>
            <span class="d">差异 ${it.distance}/64</span>
            <div class="sim-card-actions"><label><input type="checkbox" data-sim-pick="${esc(it.id)}"> 选择</label><button class="btn sm danger" data-sim-delete="${esc(it.id)}">删除这张…</button></div>
          </div>
        </div>`).join('')}
      </div>
    </div>`).join('');
    box._groups = r.groups;
  } catch (e) {
    box.innerHTML = `<div class="skeleton">${esc(e.message)}</div>`;
  }
}
$('#similar-list').addEventListener('click', async ev => {
  const remove=ev.target.closest('[data-sim-delete]');
  if(remove){openDeletePhotos([remove.dataset.simDelete]).catch(e=>toast(e.message,'err'));return;}
  const removeSelected=ev.target.closest('[data-sim-delete-selected]');
  if(removeSelected){const group=removeSelected.closest('.sim-group');const ids=$$('input[data-sim-pick]:checked',group).map(e=>e.dataset.simPick);openDeletePhotos(ids).catch(e=>toast(e.message,'err'));return;}
  if(ev.target.closest('.sim-card-actions'))return;
  const star = ev.target.closest('[data-sim-star]');
  if (star) {
    const groups = $('#similar-list')._groups || [];
    const g = groups[Number(star.dataset.simStar)];
    if (!g) return;
    try {
      await api('/api/media/batch-flags', { method: 'POST', body: { ids: g.items.map(i => i.id), favorite: true } });
      toast(`已收藏 ${g.items.length} 张`, 'ok');
      loadSimilar(); loadState();
    } catch (e) { toast(e.message, 'err'); }
    return;
  }
  const item = ev.target.closest('[data-open]');
  if (item) queueDetail(item.dataset.open);
});
$('#btn-sim-hash').addEventListener('click', async () => {
  try {
    await api('/api/similar/hash', { method: 'POST', body: {} });
    toast('已开始计算相似指纹', 'ok');
    switchView('jobs'); loadJobs();
  } catch (e) { toast(e.message, 'err'); }
});
const simRange = $('#sim-threshold');
simRange.addEventListener('input', () => { $('#sim-threshold-val').textContent = simRange.value; });
simRange.addEventListener('change', () => {
  api('/api/settings', { method: 'POST', body: { similar_threshold: simRange.value } }).catch(() => { });
  loadSimilar();
});

/* ============================ 任务中心 ============================ */
const JOB_LABEL = { scan: '扫描索引', hash: '计算哈希', archive: '复制/移动归档', undo: '撤销批次', phash: '相似指纹', watermark: '时间水印', edit: '图像编辑', album: '网页相册', slideshow: '幻灯片', trash: '移到回收站' };
const JOB_STATUS = {
  running: '进行中', completed: '已完成', failed: '失败', cancelled: '已取消',
  cancelling: '正在取消', interrupted: '已中断', partial: '部分完成', queued: '排队中',
};

async function loadJobs() {
  try {
    S.jobs = await apiGet('/api/jobs');
    if (S.view === 'jobs') {
      if (S.jobsTab === 'logs') renderLogs();
      else if (S.jobsTab === 'undo') renderUndo();
      else renderJobs();
    }
    syncRunning();
  } catch (e) { /* 忽略 */ }
}

function renderJobs() {
  const box = $('#jobs-list');
  if (!S.jobs.length) {
    box.innerHTML = '<div class="skeleton">还没有任务。扫描媒体源或复制归档后会在这里显示。</div>';
    return;
  }
  box.innerHTML = S.jobs.map(j => {
    const pct = j.total > 0 ? Math.min(100, Math.round(j.done / j.total * 100)) : (j.status === 'running' ? 8 : 100);
    const cls = j.status === 'completed' ? 'done' : (j.status === 'failed' ? 'fail' : '');
    const running = ['running', 'cancelling'].includes(j.status);
    return `<div class="job">
      <div class="job-head">
        <span class="pill ${running || j.status === 'partial' ? 'warn' : j.status === 'failed' ? 'bad' : 'ok'}">${esc(JOB_STATUS[j.status] || j.status)}</span>
        <span class="k">${esc(JOB_LABEL[j.job_type] || j.job_type)}</span>
        <span class="t">${esc(String(j.finished_at || j.created_at || '').slice(0, 19))}</span>
        <span class="sp"></span>
        ${running ? `<button class="btn sm danger" data-cancel="${esc(j.id)}">取消</button>` : ''}
        ${!running && j.has_outputs ? `<button class="btn sm" data-results="${esc(j.id)}">查看生成副本</button>` : ''}
      </div>
      <div class="bar ${cls}"><i style="width:${pct}%"></i></div>
      <div class="job-meta">
        <span>完成 ${j.succeeded || 0}</span><span>跳过 ${j.skipped || 0}</span>
        <span>失败 ${j.failed || 0}</span><span>进度 ${j.done || 0}/${j.total || 0}</span>
      </div>
      ${(j.error || j.message) ? `<div class="job-msg">${esc(j.error || j.message)}</div>` : ''}
    </div>`;
  }).join('');
}

async function renderLogs() {
  const box = $('#jobs-list');
  box.innerHTML = '<div class="skeleton">读取中…</div>';
  try {
    const rows = await apiGet('/api/logs', { limit: 200 });
    if (!rows.length) { box.innerHTML = '<div class="skeleton">暂无操作日志</div>'; return; }
    box.innerHTML = `<div class="log-row" style="color:#6f7889"><span class="ts">时间</span><span class="op">操作</span>
      <span class="p">路径 / 说明</span><span class="st">状态</span></div>` +
      rows.map(r => `<div class="log-row">
        <span class="ts">${esc(r.timestamp)}</span>
        <span class="op">${esc(r.operation)}</span>
        <span class="p" title="${esc(r.source_path || '')}">${esc(r.detail || r.target_path || r.source_path || '')}</span>
        <span class="st">${esc(r.status)}</span>
      </div>`).join('');
  } catch (e) { box.innerHTML = `<div class="skeleton">${esc(e.message)}</div>`; }
}

async function renderUndo() {
  const box = $('#jobs-list');
  box.innerHTML = '<div class="skeleton">读取中…</div>';
  try {
    const rows = await apiGet('/api/archive/plans');
    if (!rows.length) {
      box.innerHTML = '<div class="skeleton">还没有复制或移动批次。完成一次归档后会出现在这里。</div>';
      return;
    }
    box.innerHTML = rows.map(p => {
      const done = p.done_items || 0;
      const canUndo = done > 0 && !p.undone_at;
      return `<div class="job">
        <div class="job-head">
          <span class="pill ${p.mode === 'move' ? 'warn' : 'ok'}">${p.mode === 'move' ? '移动' : '复制'}</span>
          <span class="k" style="word-break:break-all">${esc(p.out_dir)}</span>
          <span class="t">${esc((p.created_at || '').slice(0, 19))}</span>
          <span class="sp"></span>
          ${canUndo ? `<button class="btn sm danger" data-undo="${esc(p.plan_id)}">撤销这批</button>` : ''}
        </div>
        <div class="job-meta">
          <span>${p.undone_at ? '已撤销 ' + (p.total || 0) + ' 项' : '已执行 ' + done + ' 项'}</span>
          <span>计划 ${p.total || 0}</span>
          <span>${p.undone_at ? '撤销时间 ' + esc(p.undone_at) : '状态 ' + esc(p.status || '')}</span>
        </div>
        ${p.undone_at ? '' : `<div class="job-msg">撤销会${p.mode === 'move' ? '把文件移回原位置' : '删除已校验的副本'}；内容与操作记录不一致的条目会被拒绝处理。</div>`}
      </div>`;
    }).join('');
  } catch (e) {
    box.innerHTML = `<div class="skeleton">${esc(e.message)}</div>`;
  }
}

$('#jobs-list').addEventListener('click', async ev => {
  const t = ev.target;
  if (t.dataset.results) {
    try {
      const ids = await apiGet('/api/media/ids', { job_id: t.dataset.results });
      if (!ids.length) return toast('这批没有生成副本（覆盖模式请在媒体库查看）');
      S.filter = { job_id: t.dataset.results }; S.sel = new Set(ids);
      $('#q').value = ''; $$('.filters select').forEach(el => el.value = '');
      $$('.filters .chip').forEach(el => el.classList.remove('on'));
      switchView('library'); await loadMedia(true);
      toast('已选中生成副本，可直接点击“网页相册…”');
    } catch (e) { toast(e.message, 'err'); }
    return;
  }
  if (t.dataset.cancel) {
    try { await api('/api/jobs/' + t.dataset.cancel + '/cancel', { method: 'POST' }); toast('已请求取消', 'ok'); loadJobs(); }
    catch (e) { toast(e.message, 'err'); }
    return;
  }
  if (t.dataset.undo) {
    const planId = t.dataset.undo;
    confirmDialog('撤销这一批操作',
      `将按操作记录逆向处理这一批文件：\n· 复制批次：校验副本 SHA-256 后删除副本（只删除身份一致的副本）\n· 移动批次：校验后移回原路径，原位置已有文件则跳过\n\n此操作会写入操作日志。继续？`,
      async () => {
        try {
          await api('/api/archive/plans/' + planId + '/undo', { method: 'POST' });
          toast('撤销任务已开始，进度见任务列表', 'ok');
          S.jobsTab = 'jobs';
          $$('#jobs-tab button').forEach(x => x.classList.toggle('on', x.dataset.tab === 'jobs'));
          loadJobs();
        } catch (e) { toast(e.message, 'err'); }
      });
  }
});

function syncRunning() {
  const now = new Set(S.jobs.filter(j => ['running', 'cancelling'].includes(j.status)).map(j => j.id));
  let finished = false;
  S.running.forEach(id => { if (!now.has(id)) finished = true; });
  S.running = now;
  const j = S.jobs.find(x => x.status === 'running');
  $('#status-job').textContent = j
    ? `${JOB_LABEL[j.job_type] || j.job_type} ${j.total ? Math.round(j.done / j.total * 100) + '%' : ''}`
    : '空闲';
  const n = S.jobs.filter(x => ['running', 'cancelling'].includes(x.status)).length;
  const b = $('#badge-jobs');
  b.hidden = !n; b.textContent = n || '';
  if (finished) {
    loadState(); loadFacets(); loadSources();
    if (S.view === 'library') loadMedia(false);
    if (S.view === 'dupes') loadDupes();
    if (S.view === 'similar') loadSimilar();
    if (S.view === 'review') loadReview();
    if (S.detailId) openDetail(S.detailId);
  }
}

/* ============================ 媒体源 ============================ */
async function loadSourcesView() {
  await loadSources();
  const box = $('#src-list');
  if (!S.sources.length) {
    box.innerHTML = `<div class="empty" style="position:static"><div class="empty-ico">⛁</div>
      <h3>还没有媒体源</h3><p>添加一个照片/视频文件夹（可以是外接硬盘或网络盘），然后开始扫描。</p>
      <button class="btn primary" id="empty-add">添加文件夹…</button></div>`;
    const b = $('#empty-add');
    if (b) b.onclick = () => $('#btn-add-source').click();
    return;
  }
  box.innerHTML = S.sources.map(s => `<div class="src">
    <div class="src-top">
      <span class="pill ${s.online ? 'ok' : 'bad'}">${s.online ? '在线' : '离线'}</span>
      <span class="src-path">${esc(s.root_path)}</span>
    </div>
    <div class="src-meta">
      <span>索引 ${s.media_count} 个文件</span>
      <span>最近扫描 ${esc(s.last_scan_at || '从未')}</span>
      ${s.options && s.options.excludes && s.options.excludes.length ? `<span>排除 ${s.options.excludes.length} 个目录</span>` : ''}
    </div>
    <div class="src-actions">
      <button class="btn sm primary" data-scan="${esc(s.id)}" ${s.online ? '' : 'disabled'}>${s.media_count ? '重新扫描' : '开始扫描'}</button>
      <button class="btn sm" data-reveal-src="${esc(s.id)}">打开文件夹</button>
      <button class="btn sm" data-edit="${esc(s.id)}">排除设置</button>
      <button class="btn sm danger" data-remove="${esc(s.id)}">移除源</button>
    </div>
    <div class="excl" id="excl-${esc(s.id)}" hidden>
      <label>排除的子目录（每行一个，可写完整路径或目录名，例如 Temp）</label>
      <textarea data-excludes="${esc(s.id)}">${esc(((s.options && s.options.excludes) || []).join('\n'))}</textarea>
<button class="btn sm" data-browse-exclude="${esc(s.id)}">浏览并添加排除目录…</button>
      <label style="margin-top:8px">跳过的扩展名（每行一个，例如 png）</label>
      <textarea data-exts="${esc(s.id)}" style="min-height:40px">${esc(((s.options && s.options.ext_excludes) || []).join('\n'))}</textarea>
      <div class="row-actions"><button class="btn sm primary" data-save-opts="${esc(s.id)}">保存</button></div>
    </div>
  </div>`).join('');
}

$('#src-list').addEventListener('click', async ev => {
  const t = ev.target;
  if (t.dataset.scan) {
    try {
      await api('/api/sources/' + t.dataset.scan + '/scan', { method: 'POST' });
      toast('已开始扫描，进度见任务中心', 'ok');
      switchView('jobs');
    } catch (e) { toast(e.message, 'err'); }
  } else if (t.dataset.revealSrc) {
    try { await api('/api/sources/' + t.dataset.revealSrc + '/reveal', { method: 'POST' }); }
    catch (e) { toast(e.message, 'err'); }
  } else if (t.dataset.edit) {
    const box = $('#excl-' + t.dataset.edit);
    box.hidden = !box.hidden;
  } else if (t.dataset.saveOpts) {
    const id = t.dataset.saveOpts;
    const excludes = $(`[data-excludes="${id}"]`).value.split('\n').map(s => s.trim()).filter(Boolean);
    const ext_excludes = $(`[data-exts="${id}"]`).value.split('\n').map(s => s.trim()).filter(Boolean);
    try {
      await api('/api/sources/' + id, { method: 'PATCH', body: { excludes, ext_excludes } });
      toast('已保存（下次扫描生效）', 'ok');
      loadSourcesView();
    } catch (e) { toast(e.message, 'err'); }
  } else if (t.dataset.remove) {
    const src = S.sources.find(x => x.id === t.dataset.remove);
    if (!src) return;
    confirmDialog('移除媒体源',
      `将从索引中移除：\n${esc(src.root_path)}\n\n共 ${src.media_count} 条索引记录会被删除。\n原始照片和视频不会被删除或修改。`,
      async () => {
        try {
          await api('/api/sources/' + src.id, { method: 'DELETE' });
          toast('已移除该源的索引', 'ok');
          loadSourcesView(); loadState(); loadFacets();
        } catch (e) { toast(e.message, 'err'); }
      });
  }
});

$('#btn-add-source').addEventListener('click', async () => {
  const path = await pickFolder();
  if (!path) return;
  try {
    await api('/api/sources', { method: 'POST', body: { path } });
    toast('已添加媒体源', 'ok');
    await loadSourcesView();
    const src = S.sources.find(s => s.root_path.toLowerCase() === path.toLowerCase());
    if (src) {
      await api('/api/sources/' + src.id + '/scan', { method: 'POST' });
      toast('开始扫描…', 'ok');
      switchView('jobs');
    }
    loadState();
  } catch (e) { toast(e.message, 'err'); }
});

$('#btn-scan-all').addEventListener('click', async () => {
  const online = S.sources.filter(s => s.online);
  if (!online.length) return toast('没有在线的媒体源', 'err');
  for (const s of online) {
    try { await api('/api/sources/' + s.id + '/scan', { method: 'POST' }); }
    catch (e) { toast(e.message, 'err'); }
  }
  toast(`已开始扫描 ${online.length} 个源`, 'ok');
  switchView('jobs');
});

let folderPickerPending = false;
async function pickFolder(initialPath = '', kind = 'folder') {
  if (folderPickerPending) { toast('请先完成或取消已经打开的文件夹选择窗口'); return null; }
  folderPickerPending = true;
  const buttons = $$('#btn-add-source, #aw-pick, #al-pick, #sl-pick, #wm-pick, #edit-pick, #wm-font-browse, #set-output-browse, [data-browse-exclude]')
    .map(el => ({ el, disabled: el.disabled }));
  buttons.forEach(({ el }) => { el.disabled = true; });
  try {
    const r = await api('/api/pick-folder', { method: 'POST', body: { initial_path: initialPath, kind } });
    if (r && r.path) return r.path;
    // 用户取消 → 不进入手动输入
    return null;
  } catch (e) {
    toast('系统文件夹对话框不可用，请手动输入路径', 'err');
    return await promptPath(initialPath);
  } finally {
    folderPickerPending = false;
    buttons.forEach(({ el, disabled }) => { if (el.isConnected) el.disabled = disabled; });
  }
}

async function pickInto(selector) {
  const input = $(selector);
  if (!input) return;
  const path = await pickFolder(input.value);
  if (path && input.isConnected) { input.value = path; input.dispatchEvent(new Event('input', { bubbles: true })); }
}

/** 手动输入目录路径（对话框不可用时的兜底）。 */
function promptPath(initialPath = '') {
  return new Promise(resolve => {
    const previousFocus = document.activeElement;
    const overlay = document.createElement('div');
    overlay.className = 'modal-root'; overlay.style.zIndex = '61';
    overlay.innerHTML = `<div class="modal narrow"><div class="modal-head"><h3>输入文件夹路径</h3></div>
      <div class="modal-body">
        <p style="color:#98a1b3;margin-top:0">支持本机磁盘、外接硬盘与 UNC 路径，例如 D:\\Photos 或 \\\\NAS\\照片。</p>
        <div class="field"><input type="text" id="pp-path" value="${esc(initialPath)}" placeholder="D:\\Photos"></div>
      </div>
      <div class="modal-foot"><button class="btn" id="pp-cancel">取消</button>
        <button class="btn primary" id="pp-ok">确定</button></div></div>`;
    document.body.appendChild(overlay);
    const finish = value => {
      overlay.remove();
      if (previousFocus && previousFocus.isConnected) previousFocus.focus();
      resolve(value);
    };
    $('#pp-cancel', overlay).onclick = () => finish(null);
    const submit = () => finish($('#pp-path', overlay).value.trim() || null);
    $('#pp-ok', overlay).onclick = submit;
    overlay.onclick = e => { if (e.target === overlay) finish(null); };
    overlay.addEventListener('keydown', e => {
      if (e.key === 'Escape' || e.key === 'Enter') {
        e.preventDefault(); e.stopPropagation();
        if (e.key === 'Escape') finish(null); else submit();
      }
    });
    $('#pp-path', overlay).focus();
  });
}

/* ============================ 设置 ============================ */
function renderSettings() {
  const s = S.settings || {};
  const st = S.state || {};
  $('#settings-body').innerHTML = `
<div class="card"><h3>外观</h3><label>主题 <select id="set-theme"><option value="dark">深色</option><option value="light">浅色</option></select></label></div>
<div class="card"><h3>便携存储与统一输出</h3><p>数据库、设置和预览缓存保存在程序目录的 data 下；所有输出和原件备份保存在 exports 的各自子目录。退出程序后，复制整个程序文件夹即可迁移。外部媒体源仍从原位置读取。</p><div class="kv"><span>程序目录</span><b class="mono">${esc(st.program_dir || '')}</b></div><div class="field"><label>输出目录（只能选择程序目录内的文件夹，也可填写 exports 等相对路径）</label><div class="row-actions"><input id="set-output-root" type="text" value="${esc(outputRoot())}" style="flex:1"><button class="btn" id="set-output-browse">浏览目录…</button></div></div><p id="set-output-summary">${esc(outputRoot())}</p><button class="btn primary" id="set-output-save">保存输出位置</button></div>

    <div class="card">
      <h3>归档默认值</h3>
      <p>模板变量：{YYYY} {YY} {MM} {DD} {HH} {MI} {YYYY-MM} {YYYY-MM-DD} {original_name} {name} {ext} {type} {camera}</p>
      <div class="field"><label>命名模板</label>
        <input type="text" id="set-template" value="${esc(s.template || '{YYYY}/{YYYY-MM}/{original_name}')}"></div>
      <div class="tplpreview" id="tpl-preview"></div>
      <div class="field" style="margin-top:10px"><label>同名冲突策略</label>
        <select id="set-conflict">
          <option value="skip" ${s.conflict !== 'suffix' ? 'selected' : ''}>跳过并记录（推荐）</option>
          <option value="suffix" ${s.conflict === 'suffix' ? 'selected' : ''}>自动加后缀（1）（2）</option>
        </select></div>
      <div class="row-actions"><button class="btn primary" id="set-save">保存设置</button></div>
    </div>
    <div class="card">
      <h3>数据与备份</h3>
      <p>索引数据库只保存在本机。备份使用 SQLite 一致性快照，可在需要时替换 index 数据库恢复索引（不含媒体文件本身）。</p>
      <div class="kv"><span>数据目录</span><b class="mono">${esc(st.data_dir || '')}</b></div>
      <div class="kv"><span>数据库</span><b class="mono">${esc(st.db_path || '')}</b></div>
      <div class="kv"><span>索引文件数</span><b>${st.files || 0}</b></div>
      <div class="row-actions">
        <button class="btn" id="btn-backup">备份数据库</button>
      </div>
    </div>
    <div class="card">
      <h3>导出清单</h3>
      <p>按当前媒体库生成清单。CSV 便于在 Excel 中查看；JSON 为版本化 manifest，包含稳定 ID、校正后的拍摄时间与 SHA-256。</p>
      <div class="row-actions">
        <button class="btn" id="btn-export-csv">导出 CSV</button>
        <button class="btn" id="btn-export-json">导出 JSON manifest</button>
      </div>
    </div>
    <div class="card">
      <h3>格式支持</h3>
      <p>图片：JPEG / PNG / WebP / GIF / BMP / TIFF 内置解码；HEIC / HEIF 通过外置解码器（heif.dll）解码，
      缺失时仍可索引、去重与归档，只是没有缩略图。视频容器 MP4 / MOV / M4V / AVI / MKV / WMV / 3GP，
      能否播放取决于浏览器内核。</p>
      <div class="kv"><span>HEIC / HEIF 解码器</span><b id="caps-heic"><span class="pill">检测中…</span></b></div>
      <div class="kv"><span>明细</span><b class="mono" id="caps-heic-detail">—</b></div>
    </div>
    <div class="card">
      <h3>目录监控</h3>
      <p>开启后会定时检查每个媒体源的根目录是否变化，变化时自动排队一次增量扫描（仍然只读）。
      这里用轮询而不是系统文件钩子：外接盘、网络盘和没有管理员权限的机器上同样可靠，代价是最长一个周期的延迟。</p>
      <div class="kv"><span>启用监控</span><b><input type="checkbox" id="set-watch" ${s.watch_enabled === '1' ? 'checked' : ''}></b></div>
      <div class="field" style="margin-top:8px"><label>检查间隔（秒，最小 30）</label>
        <input type="text" id="set-watch-interval" value="${esc(s.watch_interval || '300')}"></div>
      <div class="row-actions"><button class="btn primary" id="set-watch-save">保存监控设置</button></div>
    </div>
    <div class="card">
      <h3>相似照片 · 实况照片</h3>
      <p>相似阈值越小越严格（0 表示几乎完全一致，16 很宽松）。实况照片按「同目录 + 同主名」一对一配对，
      多对多一律不猜，可在详情页手动解除。</p>
      <div class="field"><label>相似阈值 <b id="set-sim-val">${esc(s.similar_threshold || '6')}</b> / 64</label>
        <input type="range" id="set-sim" min="0" max="16" step="1" value="${esc(s.similar_threshold || '6')}" style="width:100%"></div>
      <div class="row-actions">
        <button class="btn" id="btn-livepair">重新配对实况照片</button>
        <button class="btn" id="btn-simhash">计算相似指纹</button>
      </div>
      <div class="kv" style="margin-top:10px"><span>标签管理</span><b id="tag-count-label">${st.tags || 0} 个</b></div>
      <div class="tagbox" id="tag-manage-box"><span class="tplhint">读取中…</span></div>
    </div>
    <div class="card">
      <h3>安全边界</h3>
      <p>本程序不联网、不需要账号。扫描、读取元数据、生成缩略图与计算哈希全部只读；
      只有「归档整理」会写入你指定的输出目录，且必须先生成预览再确认。</p>
      <div class="kv"><span>版本</span><b>v${esc(st.version || '')}</b></div>
      <div class="kv"><span>索引引擎</span><b>SQLite（WAL，事务可恢复）</b></div>
      <div class="kv"><span>去重依据</span><b>完整文件 SHA-256</b></div>
    </div>`;

  $('#set-theme').value=document.documentElement.dataset.theme || 'dark';
  $('#set-theme').onchange=e=>setTheme(e.target.value);
  $('#set-output-browse').onclick = () => pickInto('#set-output-root');
  $('#set-output-save').onclick = async () => { try { S.settings = await api('/api/settings',{method:'POST',body:{output_root:$('#set-output-root').value.trim()}}); syncOutputFields(); $('#set-output-summary').textContent = outputRoot(); toast('输出位置已保存，所有功能自动使用','ok'); } catch(e) { toast(e.message,'err'); } };
  const upd = () => { $('#tpl-preview').textContent = tplPreview($('#set-template').value); };
  $('#set-template').addEventListener('input', upd);
  upd();
  loadCapabilities();
  renderTagManager();

  const sim = $('#set-sim');
  if (sim) {
    sim.addEventListener('input', () => { $('#set-sim-val').textContent = sim.value; });
    sim.addEventListener('change', async () => {
      try {
        await api('/api/settings', { method: 'POST', body: { similar_threshold: sim.value } });
        toast('相似阈值已保存', 'ok');
      } catch (e) { toast(e.message, 'err'); }
    });
  }
  const watchSave = $('#set-watch-save');
  if (watchSave) {
    watchSave.addEventListener('click', async () => {
      const enabled = $('#set-watch').checked ? '1' : '0';
      const raw = Number($('#set-watch-interval').value) || 300;
      const interval = Math.max(30, Math.min(86400, Math.round(raw)));
      try {
        S.settings = await api('/api/settings', { method: 'POST', body: { watch_enabled: enabled, watch_interval: String(interval) } });
        $('#set-watch-interval').value = interval;
        toast(enabled === '1' ? `监控已开启，每 ${interval} 秒检查一次` : '监控已关闭', 'ok');
      } catch (e) { toast(e.message, 'err'); }
    });
  }
  const liveBtn = $('#btn-livepair');
  if (liveBtn) {
    liveBtn.addEventListener('click', async () => {
      try {
        const r = await api('/api/livepairs/run', { method: 'POST' });
        toast(`已配对 ${r.pairs} 组${r.ambiguous ? '，多对多跳过 ' + r.ambiguous + ' 组' : ''}`, 'ok');
        loadState();
      } catch (e) { toast(e.message, 'err'); }
    });
  }
  const simHashBtn = $('#btn-simhash');
  if (simHashBtn) {
    simHashBtn.addEventListener('click', async () => {
      try {
        await api('/api/similar/hash', { method: 'POST', body: {} });
        toast('已开始计算相似指纹，进度见任务中心', 'ok');
        switchView('jobs'); loadJobs();
      } catch (e) { toast(e.message, 'err'); }
    });
  }
  $('#set-save').addEventListener('click', async () => {
    try {
      S.settings = await api('/api/settings', {
        method: 'POST',
        body: { template: $('#set-template').value, conflict: $('#set-conflict').value },
      });
      toast('设置已保存', 'ok');
    } catch (e) { toast(e.message, 'err'); }
  });
  $('#btn-backup').addEventListener('click', async () => {
    try {
      const r = await api('/api/backup', { method: 'POST' });
      toast('备份完成：' + r.path, 'ok');
    } catch (e) { toast(e.message, 'err'); }
  });
  $('#btn-export-csv').onclick = async () => { try { const r=await api('/api/export/save',{method:'POST',body:filterParams({format:'csv'})});toast('清单已保存：'+r.path,'ok'); } catch(e) {toast(e.message,'err');} };
  $('#btn-export-json').onclick = async () => { try { const r=await api('/api/export/save',{method:'POST',body:filterParams({format:'json'})});toast('清单已保存：'+r.path,'ok'); } catch(e) {toast(e.message,'err');} };
}

async function loadCapabilities() {
  const label = $('#caps-heic');
  const detail = $('#caps-heic-detail');
  if (!label) return;
  try {
    const c = await api('/api/capabilities');
    if (!$('#caps-heic')) return; // 用户已切走页面
    if (c.heic.ok) {
      label.innerHTML = '<span class="pill ok">已启用</span>';
      detail.textContent = c.heic.detail;
    } else {
      label.innerHTML = '<span class="pill warn">未启用</span>';
      detail.textContent = c.heic.detail + (c.heic.hint ? ' —— ' + c.heic.hint : '');
    }
  } catch (e) {
    label.innerHTML = '<span class="pill bad">检测失败</span>';
    if (detail) detail.textContent = e.message;
  }
}

async function renderTagManager() {
  const box = $('#tag-manage-box');
  if (!box) return;
  const list = await fetchTags();
  const label = $('#tag-count-label');
  if (label) label.textContent = (list || []).length + ' 个';
  if (!list || !list.length) {
    box.innerHTML = '<span class="tplhint">还没有标签。可以在媒体详情里添加，或用媒体库的批量操作批量打标签。</span>';
    return;
  }
  box.innerHTML = list.map(t => `<span class="tagchip">${esc(t.name)}<i>${t.count}</i><b data-del-tag="${esc(t.tag_id)}" title="删除标签">×</b></span>`).join('');
  box.querySelectorAll('[data-del-tag]').forEach(el => {
    el.onclick = () => {
      const tag = list.find(x => x.tag_id === el.dataset.delTag);
      if (!tag) return;
      confirmDialog('删除标签',
        `将删除标签「${esc(tag.name)}」，并从 ${tag.count} 项媒体上移除该标签。媒体文件本身不会被修改。`,
        async () => {
          try {
            await api('/api/tags/' + tag.tag_id, { method: 'DELETE' });
            toast('标签已删除', 'ok');
            renderTagManager(); loadFacets(); loadState();
          } catch (e) { toast(e.message, 'err'); }
        });
    };
  });
}

function download(url) {
  const sep = url.includes('?') ? '&' : '?';
  window.location.href = url + sep + AUTH;
}

function tplPreview(tpl) {
  const map = {
    YYYY: '2023', YY: '23', MM: '05', DD: '01', HH: '14', MI: '32',
    'YYYY-MM': '2023-05', 'YYYY-MM-DD': '2023-05-01', YYYY_MM: '2023_05',
    original_name: 'IMG_0421.JPG', name: 'IMG_0421', ext: 'jpg', type: 'photo', camera: 'Canon EOS R6',
  };
  return tpl.replace(/\{([\w-]+)\}/g, (m, k) => (map[k] != null ? map[k] : m));
}

/* ============================ 归档向导 ============================ */
function modal(html, opts = {}) {
  const root = $('#modal-root');
  root.hidden = false;
  root.innerHTML = `<div class="modal ${opts.narrow ? 'narrow' : ''}">${html}</div>`;
  syncOutputFields();
  installDatePickers(root);
  root.onclick = ev => { if (ev.target === root && !opts.sticky) closeModal(); };
  return root;
}
function closeModal() {
  const root = $('#modal-root');
  root.hidden = true;
  root.innerHTML = '';
}
function confirmDialog(title, text, onOk) {
  modal(`<div class="modal-head"><h3>${esc(title)}</h3></div>
    <div class="modal-body"><p style="white-space:pre-wrap;line-height:1.7;color:var(--text);margin:0">${text}</p></div>
    <div class="modal-foot"><button class="btn" id="cf-no">取消</button>
    <button class="btn primary" id="cf-yes">确认</button></div>`, { narrow: true });
  $('#cf-no').onclick = closeModal;
  $('#cf-yes').onclick = async () => { closeModal(); await onOk(); };
}

async function openArchiveWizard(ids, filter) {
  const count = ids ? ids.length : (S.filter.total || 0);
  if (!count) return toast('没有可归档的媒体', 'err');
  const st = S.settings || {};
  const state = {
    ids, filter,
    out_dir: st.out_dir || '',
    template: st.template || '{YYYY}/{YYYY-MM}/{original_name}',
    conflict: st.conflict || 'skip',
    mode: st.mode === 'move' ? 'move' : 'copy',
    plan: null,
  };

  modal(`<div class="modal-head"><h3>归档整理</h3><button class="icon-btn" id="aw-close">×</button></div>
    <div class="modal-body">
      <div class="warnbox">预览确认前不会写入任何文件。写入时先落临时文件，校验大小与 SHA-256 通过后才提交为最终文件；同名目标默认跳过，绝不静默覆盖。
        选择「移动」时会先校验再移除源文件，可在任务中心按批次撤销。</div>
      <div class="field"><label>范围</label>
        <div class="tplpreview">${ids ? '已选择的 ' + count + ' 项媒体' : '当前筛选结果全部 ' + count + ' 项媒体'}（每个媒体只处理一个可用文件）</div></div>
      <div class="field"><label>操作类型</label>
        <select id="aw-mode">
          <option value="copy">复制（源文件保持不动，推荐）</option>
          <option value="move">移动（源文件移到目标目录，可撤销）</option>
        </select></div>
      <div class="field"><label>输出目录</label>
        <div style="display:flex;gap:8px">
          <input type="text" id="aw-out" value="${esc(state.out_dir)}" placeholder="例如 D:\\整理输出">
          <button class="btn" id="aw-pick">浏览…</button>
        </div></div>
      <div class="field"><label>命名模板</label>
        <input type="text" id="aw-tpl" value="${esc(state.template)}"></div>
      <div class="tplpreview" id="aw-tplprev"></div>
      <div class="tplhint">默认模板 {YYYY}/{YYYY-MM}/{original_name}。没有日期的文件进入 Unknown-Date/，不会被填成今天。若输出目录位于扫描源内，会自动加入该源排除列表，避免重复导入。</div>
      <div class="field" style="margin-top:10px"><label>同名冲突策略</label>
        <select id="aw-conflict">
          <option value="skip">跳过并列出（默认）</option>
          <option value="suffix">自动加后缀</option>
        </select></div>
      <div id="aw-result"></div>
    </div>
    <div class="modal-foot">
      <button class="btn" id="aw-cancel">取消</button>
      <button class="btn" id="aw-plan">生成预览</button>
      <button class="btn primary" id="aw-run" disabled>开始复制</button>
    </div>`);

  const tplPrev = () => { $('#aw-tplprev').textContent = tplPreview($('#aw-tpl').value); };
  $('#aw-tpl').addEventListener('input', tplPrev);
  tplPrev();
  $('#aw-conflict').value = state.conflict;
  $('#aw-mode').value = state.mode;
  const syncMode = () => {
    const move = $('#aw-mode').value === 'move';
    $('#aw-run').textContent = move ? '开始移动' : '开始复制';
    $('#aw-run').classList.toggle('danger', move);
    $('#aw-pick').disabled = false;
  };
  $('#aw-mode').addEventListener('change', syncMode);
  syncMode();
  $('#aw-close').onclick = closeModal;
  $('#aw-cancel').onclick = closeModal;
  $('#aw-pick').onclick = async () => {
    await pickInto('#aw-out');
  };

  $('#aw-plan').onclick = async () => {
    const out_dir = $('#aw-out').value.trim();
    if (!out_dir) return toast('请填写输出目录', 'err');
    const btn = $('#aw-plan');
    btn.disabled = true; btn.textContent = '生成中…';
    try {
      const body = {
        out_dir,
        template: $('#aw-tpl').value.trim(),
        conflict: $('#aw-conflict').value,
        mode: $('#aw-mode').value,
      };
      if (ids) body.asset_ids = ids; else body.filter = Object.assign({}, filter || S.filter);
      const plan = await api('/api/archive/plan', { method: 'POST', body });
      state.plan = plan;
      S.settings = Object.assign(S.settings, { out_dir, template: body.template, conflict: body.conflict, mode: body.mode });
      api('/api/settings', { method: 'POST', body: { out_dir, template: body.template, conflict: body.conflict, mode: body.mode } }).catch(() => { });
      renderPlan(plan);
      $('#aw-run').disabled = false;
      $('#aw-run').textContent = plan.mode === 'move' ? `开始移动 ${plan.total} 个文件` : `开始复制 ${plan.total} 个文件`;
    } catch (e) {
      toast(e.message, 'err');
    } finally {
      btn.disabled = false; btn.textContent = '生成预览';
    }
  };

  $('#aw-run').onclick = () => {
    if (!state.plan) return;
    const move = state.plan.mode === 'move';
    confirmDialog(move ? '确认开始移动' : '确认开始复制',
      move
        ? `即将移动 ${state.plan.total} 个文件（${fmtSize(state.plan.total_bytes)}）到：\n${state.plan.out_dir}\n\n每个文件都会先校验哈希再提交；源文件在提交成功后才会被移除。\n移动完成后可在「任务中心 → 可撤销」按批次移回。`
        : `即将复制 ${state.plan.total} 个文件（${fmtSize(state.plan.total_bytes)}）到：\n${state.plan.out_dir}\n\n原件不会被移动或删除。`,
      async () => {
        try {
          await api('/api/archive/plans/' + state.plan.plan_id + '/run', { method: 'POST' });
          closeModal();
          toast(move ? '移动任务已开始，进度见任务中心' : '复制任务已开始，进度见任务中心', 'ok');
          switchView('jobs');
          loadJobs();
        } catch (e) { toast(e.message, 'err'); }
      });
  };
}

function renderPlan(plan) {
  const box = $('#aw-result');
  const move = plan.mode === 'move';
  const warn = (plan.warnings || []).map(w => `<div class="warnbox">${esc(w)}</div>`).join('');
  box.innerHTML = `
    <h4 style="margin:16px 0 8px;font-size:12px;color:#6f7889">预览（${move ? '移动' : '复制'}）</h4>
    ${warn}
    <div class="plan-stats">
      <div><b>${plan.total}</b>文件</div>
      <div><b>${fmtSize(plan.total_bytes)}</b>预计写入</div>
      <div><b>${plan.conflicts}</b>冲突</div>
      <div><b>${fmtSize(plan.free_space)}</b>目标可用</div>
    </div>
    <div style="max-height:250px;overflow:auto;border:1px solid #232833;border-radius:8px">
      <table class="plan-table">
        <thead><tr><th>源文件</th><th>目标路径</th><th>状态</th></tr></thead>
        <tbody>${(plan.items || []).map(i => `<tr>
          <td class="path">${esc(i.source_path)}</td>
          <td class="path">${esc(i.target_path)}</td>
          <td>${i.status === 'conflict' ? `<span class="pill warn">${esc(i.note || '冲突')}</span>` : `<span class="pill ok">${move ? '待移动' : '待复制'}</span>`}</td>
        </tr>`).join('')}</tbody>
      </table>
    </div>
    ${plan.total > (plan.items || []).length ? `<div class="tplhint">仅显示前 ${(plan.items || []).length} 行，执行时处理全部 ${plan.total} 个文件。</div>` : ''}`;
}

/* ============================ 顶部工具条 ============================ */
$('#q').addEventListener('input', debounce(() => {
  const v = $('#q').value.trim();
  if (v) S.filter.q = v; else delete S.filter.q;
  loadMedia(true);
}, 260));
$('#f-type').addEventListener('change', e => setFilter('media_type', e.target.value));
$('#f-year').addEventListener('change', e => setFilter('year', e.target.value));
$('#f-conf').addEventListener('change', e => setFilter('confidence', e.target.value));
$('#f-source').addEventListener('change', e => setFilter('source_id', e.target.value));
function setFilter(k, v) {
  if (v) S.filter[k] = v; else delete S.filter[k];
  loadMedia(true);
}
function bindChip(btn, key) {
  btn.addEventListener('click', () => {
    const on = !btn.classList.contains('on');
    btn.classList.toggle('on', on);
    setFilter(key, on ? '1' : '');
  });
}
bindChip($('#f-review'), 'needs_review');
bindChip($('#f-dup'), 'dup');
bindChip($('#f-fav'), 'favorite');
bindChip($('#f-live'), 'live');
$('#f-tag').addEventListener('change', e => setFilter('tag', e.target.value));
$('#f-clear').addEventListener('click', () => {
  S.filter = {};
  $('#q').value = '';
  $$('.filters select').forEach(s => { s.value = ''; });
  $$('.topbar .chip').forEach(c => c.classList.remove('on'));
  loadMedia(true);
});
$('#sort').addEventListener('change', e => { S.sort = e.target.value; loadMedia(true); });
$('#order').addEventListener('click', () => {
  S.order = S.order === 'desc' ? 'asc' : 'desc';
  $('#order').textContent = S.order === 'desc' ? '↓' : '↑';
  loadMedia(true);
});
$('#viewmode').addEventListener('click', () => setMode(S.mode === 'grid' ? 'list' : 'grid'));
scrollEl().addEventListener('scroll', () => {
  if (!scrollEl()._raf) {
    scrollEl()._raf = true;
    requestAnimationFrame(() => { scrollEl()._raf = false; renderWindow(); });
  }
});
window.addEventListener('resize', debounce(layout, 120));

$('#btn-hash').addEventListener('click', startHash);
$('#btn-hash2').addEventListener('click', startHash);
async function startHash() {
  try {
    await api('/api/hash', { method: 'POST', body: { all: false } });
    toast('已开始计算哈希（仅重复候选）', 'ok');
    switchView('jobs');
    loadJobs();
  } catch (e) { toast(e.message, 'err'); }
}

$('#btn-archive').addEventListener('click', () => {
  const ids = S.sel.size ? Array.from(S.sel) : null;
  openArchiveWizard(ids, ids ? null : filterParams());
});

$('#sel-all').addEventListener('click', async () => {
  try {
    const ids = await apiGet('/api/media/ids', filterParams());
    S.sel = new Set(ids);
    G.nodes.forEach((n, i) => updateNode(n, i));
    updateSelBar();
    toast(`已全选 ${ids.length} 项`, 'ok');
  } catch (e) { toast(e.message, 'err'); }
});
$('#sel-clear').addEventListener('click', () => {
  S.sel.clear();
  G.nodes.forEach((n, i) => updateNode(n, i));
  updateSelBar();
});
$('#sel-date').addEventListener('click', () => {
  if (!S.sel.size) return;
  modal(`<div class="modal-head"><h3>批量修改拍摄日期</h3></div>
    <div class="modal-body">
      <p style="color:#98a1b3;margin-top:0">将覆盖 ${S.sel.size} 项的“显示日期”。原始 EXIF 不会被修改，只写入本地索引。</p>
      <div class="field"><label>日期</label><input type="date" id="bd-date"></div>
      <div class="field"><label>时间（可选）</label><input type="time" id="bd-time" step="1" value="00:00:00"></div>
    </div>
    <div class="modal-foot"><button class="btn" id="bd-cancel">取消</button>
      <button class="btn primary" id="bd-ok">保存</button></div>`, { narrow: true });
  $('#bd-cancel').onclick = closeModal;
  $('#bd-ok').onclick = async () => {
    const d = $('#bd-date').value;
    if (!d) return toast('请选择日期', 'err');
    const date = d + 'T' + ($('#bd-time').value || '00:00:00');
    try {
      const r = await api('/api/media/batch-date', { method: 'POST', body: { ids: Array.from(S.sel), date } });
      closeModal();
      toast(`已更新 ${r.updated} 项`, 'ok');
      refreshAfterChange();
    } catch (e) { toast(e.message, 'err'); }
  };
});
$('#sel-favorite').addEventListener('click', async () => {
  if (!S.sel.size) return;
  try {
    const ids=Array.from(S.sel), photos=await Promise.all(ids.map(id=>apiGet('/api/media/'+id)));
    const favorite=!photos.every(a=>a.favorite);
    const r = await api('/api/media/batch-flags', { method: 'POST', body: { ids, favorite } });
    toast(`${favorite ? '已收藏' : '已取消收藏'} ${r.updated} 项`, 'ok');
    refreshAfterChange();
  } catch (e) { toast(e.message, 'err'); }
});

$('#sel-shift').addEventListener('click', () => {
  if (!S.sel.size) return;
  modal(`<div class="modal-head"><h3>批量时间偏移</h3></div>
    <div class="modal-body">
      <p style="color:#98a1b3;margin-top:0">以每项当前的显示日期为基准整体平移，结果写入人工覆盖值；原始 EXIF 不会被修改。没有日期的项目会被跳过，不会被填成今天。</p>
      <div class="field"><label>偏移量（分钟，可为负数）</label>
        <input type="number" id="sh-min" value="60" step="1" style="width:100%;height:30px;background:#1b1f28;border:1px solid #2a2f3b;border-radius:7px;padding:0 9px"></div>
      <div class="row-actions">
        <button class="btn sm" data-shift="-1440">−1 天</button>
        <button class="btn sm" data-shift="-60">−1 小时</button>
        <button class="btn sm" data-shift="60">+1 小时</button>
        <button class="btn sm" data-shift="1440">+1 天</button>
      </div>
    </div>
    <div class="modal-foot"><button class="btn" id="sh-cancel">取消</button>
      <button class="btn primary" id="sh-ok">应用偏移</button></div>`, { narrow: true });
  $('#sh-cancel').onclick = closeModal;
  $$('[data-shift]').forEach(b => b.onclick = () => { $('#sh-min').value = b.dataset.shift; });
  $('#sh-ok').onclick = async () => {
    const minutes = Number($('#sh-min').value);
    if (!minutes) return toast('偏移量不能为 0', 'err');
    try {
      const r = await api('/api/media/shift', { method: 'POST', body: { ids: Array.from(S.sel), minutes } });
      closeModal();
      toast(`已更新 ${r.updated} 项${r.skipped ? '，跳过 ' + r.skipped + ' 项（无日期）' : ''}`, 'ok');
      refreshAfterChange();
    } catch (e) { toast(e.message, 'err'); }
  };
});

$('#sel-tag').addEventListener('click', async () => {
  if (!S.sel.size) return;
  const known = await fetchTags();
  modal(`<div class="modal-head"><h3>批量标签</h3></div>
    <div class="modal-body">
      <p style="color:#98a1b3;margin-top:0">将对已选的 ${S.sel.size} 项媒体写入标签。标签只保存在本地索引。</p>
      <div class="field"><label>标签（多个用逗号分隔）</label>
        <input type="text" id="bt-tags" placeholder="例如：旅行, 待打印" list="bt-list">
        <datalist id="bt-list">${(known || []).map(t => `<option value="${esc(t.name)}"></option>`).join('')}</datalist></div>
      <div class="field"><label>已有标签（点击填入）</label>
        <div class="tagbox">${(known || []).length
          ? known.slice(0, 40).map(t => `<span class="tagchip" data-pick-tag="${esc(t.name)}">${esc(t.name)}<i>${t.count}</i></span>`).join('')
          : '<span class="tplhint">还没有标签</span>'}</div></div>
      <div class="field"><label>写入方式</label>
        <select id="bt-mode">
          <option value="add">添加（保留原有标签）</option>
          <option value="remove">移除这些标签</option>
          <option value="set">替换为这些标签</option>
        </select></div>
    </div>
    <div class="modal-foot"><button class="btn" id="bt-cancel">取消</button>
      <button class="btn primary" id="bt-ok">应用</button></div>`, { narrow: true });
  $('#bt-cancel').onclick = closeModal;
  $$('[data-pick-tag]').forEach(el => el.onclick = () => {
    const input = $('#bt-tags');
    const cur = input.value.trim();
    input.value = cur ? cur + ', ' + el.dataset.pickTag : el.dataset.pickTag;
  });
  $('#bt-ok').onclick = async () => {
    const tags = $('#bt-tags').value.split(/[,，、]/).map(s => s.trim()).filter(Boolean);
    if (!tags.length) return toast('请至少输入一个标签', 'err');
    try {
      const r = await api('/api/media/batch-tags', {
        method: 'POST',
        body: { ids: Array.from(S.sel), tags, mode: $('#bt-mode').value },
      });
      closeModal();
      toast(`已更新 ${r.updated} 项`, 'ok');
      refreshAfterChange();
      loadFacets();
    } catch (e) { toast(e.message, 'err'); }
  };
});

$('#sel-archive').addEventListener('click', () => openArchiveWizard(Array.from(S.sel), null));

$('#nav').addEventListener('click', e => {
  const b = e.target.closest('button[data-view]');
  if (b) switchView(b.dataset.view);
});
document.addEventListener('click', e => {
  const b = e.target.closest('[data-goto]');
  if (b) switchView(b.dataset.goto);
});
$('#d-close').addEventListener('click', closeDetail);
$('#d-left').onclick=()=>quickRotate(S.detailId?[S.detailId]:[],'rot270');
$('#d-right').onclick=()=>quickRotate(S.detailId?[S.detailId]:[],'rot90');
$('#review-mode').addEventListener('click', e => {
  const b = e.target.closest('button[data-mode]');
  if (!b) return;
  $$('#review-mode button').forEach(x => x.classList.toggle('on', x === b));
  S.reviewMode = b.dataset.mode;
  loadReview();
});
$('#jobs-tab').addEventListener('click', e => {
  const b = e.target.closest('button[data-tab]');
  if (!b) return;
  $$('#jobs-tab button').forEach(x => x.classList.toggle('on', x === b));
  S.jobsTab = b.dataset.tab;
  if (S.jobsTab === 'logs') renderLogs();
  else if (S.jobsTab === 'undo') renderUndo();
  else renderJobs();
});
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') {
    if (!$('#photo-viewer').hidden) return;
    if (!$('#modal-root').hidden) closeModal();
    else if (S.detailId) closeDetail();
  }
  if ((e.ctrlKey || e.metaKey) && e.key === 'a' && S.view === 'library' && document.activeElement.tagName !== 'INPUT') {
    e.preventDefault();
    $('#sel-all').click();
  }
});

/* ============================ 启动 ============================ */
(async function boot() {
  const head = document.createElement('div');
  head.className = 'list-head';
  head.id = 'list-head';
  head.style.display = 'none';
  head.innerHTML = `<div class="rname">文件名</div><div class="rdate">拍摄时间</div>
    <div class="rsize">大小</div><div class="rres">分辨率</div><div class="rtag">标记</div>`;
  $('#grid-scroll').before(head);

  await loadState();
  await loadCaps();
  await loadSources();
  await loadFacets();
  await loadMedia(true);
  switchView('library');
  const simInit = (S.settings && S.settings.similar_threshold) || '6';
  $('#sim-threshold').value = simInit;
  $('#sim-threshold-val').textContent = simInit;
  await loadJobs();
  // 深链：?view=dupes / ?media=<id>
  const qp = new URLSearchParams(location.search);
  const deepView = qp.get('view');
  if (deepView && VIEW_TITLES[deepView]) switchView(deepView);
  const deepMedia = qp.get('media');
  if (deepMedia) openDetail(deepMedia);
  setInterval(async () => {
    try {
      S.jobs = await apiGet('/api/jobs');
      syncRunning();
      if (S.view === 'jobs' && S.jobsTab === 'jobs') renderJobs();
    } catch (e) { /* 忽略 */ }
  }, 1600);
})();

/* ============================ 时间线 ============================ */
const TL = { items: [], total: 0, offset: 0, pageSize: 240, density: 'month', today: false, busy: false, gen: 0 };

function tlDayKey(v) {
  return v ? String(v).slice(0, 10) : '';
}
function tlGroupOf(it, density) {
  const d = tlDayKey(it.taken_at);
  if (!d) return { key: 'unknown', label: '未知日期', sub: '没有可靠的拍摄时间' };
  const [y, m, day] = d.split('-');
  if (density === 'year') return { key: y, label: y + '年', sub: '' };
  if (density === 'day') {
    const dt = new Date(d + 'T00:00:00');
    const wd = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][dt.getDay()] || '';
    return { key: d, label: `${y}年${Number(m)}月${Number(day)}日`, sub: wd };
  }
  return { key: y + '-' + m, label: `${y}年${Number(m)}月`, sub: '' };
}

function tlJustify(items, width, targetH) {
  const rows = [];
  let cur = [], ratioSum = 0;
  const gap = 6;
  for (const it of items) {
    const ratio = it.width && it.height ? it.width / it.height : 1;
    cur.push({ it, ratio });
    ratioSum += ratio;
    if (ratioSum * targetH + gap * (cur.length - 1) >= width) {
      const h = Math.max(96, Math.min(320, (width - gap * (cur.length - 1)) / Math.max(ratioSum, 0.01)));
      rows.push({ items: cur, h });
      cur = []; ratioSum = 0;
    }
  }
  if (cur.length) {
    const h = Math.min(targetH, (width - gap * (cur.length - 1)) / Math.max(ratioSum, 0.01));
    rows.push({ items: cur, h: Math.max(110, h) });
  }
  return rows;
}

function tlRowsHtml(items, width) {
  const rows = tlJustify(items, width, 196);
  return rows.map(row => {
    const parts = row.items.map(({ it, ratio }) => {
      const w = Math.max(48, Math.round(ratio * row.h));
      const badge = it.media_type === 'video'
        ? `<span class="badge">▶ ${fmtDur(it.duration_ms)}</span>`
        : (it.live ? '<span class="badge">LIVE</span>' : (it.star ? '<span class="badge">★</span>' : ''));
      // 视频封面可能生成失败（例如缺少解码器），先铺一层底色与播放标记，图片加载成功后自然盖住
      const ph = it.media_type === 'video' ? '<span class="tl-ph">▶</span>' : '';
      const tsrc = it.media_type === 'video' ? thumbUrl(it, 'poster') : thumbUrl(it, 640);
      return `<div class="tl-item${it.media_type === 'video' ? ' is-video' : ''}" data-id="${esc(it.id)}"
        style="width:${w}px;height:${Math.round(row.h)}px"
        title="${esc(it.file_name || '')}">${ph}${badge}
        <img loading="lazy" data-asset="${esc(it.id)}" src="${tsrc}" alt=""
          onerror="var p=this.parentNode;this.remove();if(p)p.classList.add('noimg');${it.media_type === 'video' ? 'pvmPosterFallback(this)' : ''}"></div>`;
    }).join('');
    return `<div class="tl-row">${parts}</div>`;
  }).join('');
}

function tlRender() {
  const body = $('#tl-body');
  if (!body) return;
  const width = Math.max(320, ($('#tl-scroll').clientWidth || 900) - 44);
  let items = TL.items;
  let intro = '';
  if (TL.today) {
    const now = new Date();
    const mmdd = String(now.getMonth() + 1).padStart(2, '0') + '-' + String(now.getDate()).padStart(2, '0');
    items = items.filter(it => tlDayKey(it.taken_at).slice(5) === mmdd);
    intro = `<div class="tl-group"><h3 class="tl-head">那年今日 <span>${items.length} 项</span></h3>
      <p class="tl-today-mark">${items.length ? '往年今天拍下的照片。' : '还没有往年今天的照片，多点几次「加载更多」或换个筛选条件。'}</p></div>`;
    if (!items.length) { body.innerHTML = intro; $('#tl-more').hidden = true; return; }
  }
  const groups = [];
  const index = new Map();
  for (const it of items) {
    const g = tlGroupOf(it, TL.density);
    let bucket = index.get(g.key);
    if (!bucket) {
      bucket = { ...g, items: [] };
      index.set(g.key, bucket);
      groups.push(bucket);
    }
    bucket.items.push(it);
  }
  // 未知日期永远放在最后
  groups.sort((a, b) => (a.key === 'unknown' ? 1 : 0) - (b.key === 'unknown' ? 1 : 0));
  body.innerHTML = intro + groups.map(g => `
    <section class="tl-group" data-key="${esc(g.key)}">
      <h3 class="tl-head">${esc(g.label)} <span>${g.items.length} 项${g.sub ? ' · ' + g.sub : ''}</span></h3>
      <div class="tl-rows">${tlRowsHtml(g.items, width)}</div>
    </section>`).join('');
  if (!groups.length) {
    body.innerHTML = '<div class="tl-empty">这个筛选条件下没有照片。</div>';
  }
  $('#tl-more').hidden = TL.today || TL.offset >= TL.total;
  $('#status-count').textContent = `时间线 · 已加载 ${TL.items.length} / ${TL.total} 项`;
}

async function loadTimeline(reset) {
  if (reset) { TL.items = []; TL.offset = 0; TL.total = 0; }
  if (TL.busy) return;
  TL.busy = true;
  const gen = ++TL.gen;
  try {
    const params = Object.assign(filterParams(), { offset: TL.offset, limit: TL.pageSize, sort: 'taken', order: 'desc' });
    const r = await apiGet('/api/media', params);
    if (gen !== TL.gen) return;
    TL.total = r.total;
    TL.items = TL.items.concat(r.items);
    TL.offset += r.items.length;
    if (!r.items.length) TL.offset = TL.total;
    tlRender();
  } catch (e) {
    toast(e.message, 'err');
  } finally {
    TL.busy = false;
  }
}

$('#tl-density').addEventListener('click', e => {
  const b = e.target.closest('button[data-d]');
  if (!b) return;
  $$('#tl-density button').forEach(x => x.classList.toggle('on', x === b));
  TL.density = b.dataset.d;
  TL.today = false;
  $('#tl-today').classList.remove('primary');
  tlRender();
});
$('#tl-more').addEventListener('click', () => loadTimeline(false));

// 滚动到接近底部就自动续页，「加载更多」按钮保留作兜底（也用于无障碍/键盘）。
if ('IntersectionObserver' in window) {
  const tlObs = new IntersectionObserver(entries => {
    for (const en of entries) {
      if (en.isIntersecting && !TL.busy && !TL.today && TL.offset > 0 && TL.offset < TL.total) {
        loadTimeline(false);
      }
    }
  }, { root: $('#tl-scroll'), rootMargin: '700px 0px' });
  tlObs.observe($('#tl-more'));
}
$('#tl-today').addEventListener('click', () => {
  TL.today = !TL.today;
  $('#tl-today').classList.toggle('primary', TL.today);
  tlRender();
});
$('#tl-body').addEventListener('click', e => {
  const el = e.target.closest('.tl-item[data-id]');
  if (el) queueDetail(el.dataset.id);
});
window.addEventListener('resize', debounce(() => { if (S.view === 'timeline') tlRender(); }, 220));

/* ============================ 时间水印 ============================ */
const WM = {
  cfg: null,
  samples: [],
  fonts: [],
  presets: [],
  gen: 0,
  timer: null,
  loading: false,
  ffChecked: false,
};

const WM_DEFAULTS = {
  position: 'bc', pos_x: 50, pos_y: 50, margin_pct: 3, size_pct: 2.8, min_px: 12, max_px: 200, max_width_pct: 40,
  font: '微软雅黑', bold: true, color: '#FF8A1E', opacity: 1, stroke: 0.05, stroke_color: '#000000',
  shadow: 0.1, shadow_color: '#000000', plate: false, plate_color: '#000000', plate_opacity: 0.42,
  plate_radius: 0.22, plate_pad: 0.34, plate_full: false, rotate: 0, rotate_image: 0,
  date_format: '%Y-%m-%d', text: '{date}',
  custom: '', source_hint: false, require_date: true, output_format: 'same', video: true, skip_watermarked: true,
};

function parseJsonSetting(v) {
  if (v == null) return null;
  if (typeof v === 'object') return v;
  try { return JSON.parse(String(v)); } catch (e) { return null; }
}

function wmCfg() {
  if (!WM.cfg) {
    WM.cfg = Object.assign({}, WM_DEFAULTS, parseJsonSetting(S.settings && S.settings.watermark) || {});
  }
  return WM.cfg;
}

function wmSyncLabels() {
  const c = wmCfg();
  $('#wm-size-v').textContent = Number(c.size_pct).toFixed(1) + '%';
  $('#wm-margin-v').textContent = Number(c.margin_pct) + '%';
  $('#wm-rotate-v').textContent = Number(c.rotate) + '°';
  $('#wm-opacity-v').textContent = Math.round(c.opacity * 100) + '%';
  $('#wm-stroke-v').textContent = Math.round(c.stroke * 100) + '%';
  $('#wm-shadow-v').textContent = Math.round(c.shadow * 100) + '%';
  $('#wm-plate-v').textContent = Math.round(c.plate_opacity * 100) + '%';
  $('#wm-plate-row').hidden = !c.plate;
  $('#wm-plate-full-row').hidden = !c.plate;
}

function wmWriteUI() {
  const c = wmCfg();
  $('#wm-margin').value = c.margin_pct;
  $('#wm-size').value = c.size_pct;
  $('#wm-min').value = c.min_px;
  $('#wm-max').value = c.max_px;
  $('#wm-maxw').value = c.max_width_pct;
  $('#wm-bold').checked = !!c.bold;
  $('#wm-color').value = c.color || '#FF8A1E';
  $('#wm-stroke-color').value = c.stroke_color || '#000000';
  $('#wm-opacity').value = c.opacity;
  $('#wm-stroke').value = c.stroke;
  $('#wm-shadow').value = c.shadow;
  $('#wm-plate').checked = !!c.plate;
  $('#wm-plate-full').checked = !!c.plate_full;
  $('#wm-plate-opacity').value = c.plate_opacity;
  $('#wm-rotate').value = c.rotate;
  $('#wm-rotimg').value = String(c.rotate_image || 0);
  $('#wm-known-skip').checked = c.skip_watermarked !== false;
  $('#wm-suspect-skip').checked = !!c.skip_suspected;
  $('#wm-datefmt').value = c.date_format || '%Y-%m-%d';
  $('#wm-text').value = c.text || '{date}';
  $('#wm-custom').value = c.custom || '';
  $('#wm-source').checked = !!c.source_hint;
  $('#wm-video').checked = c.video !== false;
  $('#wm-requiredate').checked = c.require_date !== false;
  $('#wm-format').value = c.output_format || 'same';
  $('#wm-font-path').value = c.font_path || '';
  $$('#wm-pos button').forEach(b => b.classList.toggle('on', b.dataset.pos === (c.position || 'bc')));
  $('#wm-free-hint').hidden = (c.position || 'bc') !== 'free';
  if (c.font) {
    const has = Array.from($('#wm-font').options).some(o => o.value === c.font);
    if (has) $('#wm-font').value = c.font;
  }
  wmSyncLabels();
}

function wmReadUI() {
  const c = wmCfg();
  const posBtn = $$('#wm-pos button').find(b => b.classList.contains('on'));
  c.position = posBtn ? posBtn.dataset.pos : 'bc';
  c.margin_pct = Number($('#wm-margin').value);
  c.size_pct = Number($('#wm-size').value);
  c.min_px = Number($('#wm-min').value) || 12;
  c.max_px = Number($('#wm-max').value) || 200;
  c.max_width_pct = Number($('#wm-maxw').value) || 40;
  c.font = $('#wm-font').value || '微软雅黑';
  // 下拉框选中"自定义字体"时，字体路径由 font_path 决定
  if (String(c.font).startsWith('__file__:')) {
    c.font_path = String(c.font).slice(8);
    c.font = '微软雅黑';
  } else {
    c.font_path = $('#wm-font-path').value.trim();
  }
  // 自由定位的提示随九宫格选择联动
  $('#wm-free-hint').hidden = (c.position || 'bc') !== 'free';
  c.bold = $('#wm-bold').checked;
  c.color = $('#wm-color').value;
  c.stroke_color = $('#wm-stroke-color').value;
  c.opacity = Number($('#wm-opacity').value);
  c.stroke = Number($('#wm-stroke').value);
  c.shadow = Number($('#wm-shadow').value);
  c.plate = $('#wm-plate').checked;
  c.plate_full = $('#wm-plate-full').checked;
  c.plate_opacity = Number($('#wm-plate-opacity').value);
  c.rotate = Number($('#wm-rotate').value);
  c.rotate_image = Number($('#wm-rotimg').value) || 0;
  c.skip_watermarked = $('#wm-known-skip').checked;
  c.skip_suspected = $('#wm-suspect-skip').checked;
  c.date_format = $('#wm-datefmt').value.trim() || '%Y-%m-%d';
  c.text = $('#wm-text').value;
  c.custom = $('#wm-custom').value;
  c.source_hint = $('#wm-source').checked;
  c.require_date = $('#wm-requiredate').checked;
  c.output_format = $('#wm-format').value;
  c.video = $('#wm-video').checked;
  wmSyncLabels();
  return c;
}

function wmScope() {
  return WM.scope || {ids:[],count:0,label:'正在读取范围…'};
}

async function wmUpdateScope() {
  const request=WM.scopeGeneration=(WM.scopeGeneration || 0)+1;
  const mode=$('#wm-scope-mode').value,selected=Array.from(S.sel),video=wmCfg().video!==false;
  WM.scope={ids:[],count:0,label:'正在读取范围…'};$('#wm-scope').textContent=WM.scope.label;$('#wm-run').disabled=true;
  WM.gen++;WM.samples=[];wmRenderSampleChips();$('#wm-previews').innerHTML='<div class="wm-empty">正在读取当前范围…</div>';
  try{
    let ids;
    if(mode==='selected'){
      ids=await api('/api/watermark/scope',{method:'POST',body:{ids:selected,video}});
    }else{
      const params=filterParams();
      if(!video && params.media_type==='video')ids=[];
      else{if(!video)params.media_type='image';ids=await apiGet('/api/media/ids',params);}
    }
    if(request!==WM.scopeGeneration)return;
    const label=mode==='selected'?(selected.length?'媒体库所选可处理项目 '+ids.length+' 项':'未选择照片，请返回媒体库选择'):'当前筛选可处理项目 '+ids.length+' 项';
    WM.scope={ids,count:ids.length,label};$('#wm-scope').textContent='范围：'+label;$('#wm-run').disabled=!ids.length;
  }catch(e){if(request===WM.scopeGeneration){WM.scope={ids:[],count:0,label:'范围读取失败，请重新选择'};$('#wm-scope').textContent=WM.scope.label;toast(e.message,'err');}}
}

/** 覆盖原图模式：不需要输出目录/命名模板，并且把格式锁成原格式 */
function wmSyncMode() {
  const overwrite = $('#wm-mode').value === 'overwrite';
  $('#wm-backup-row').hidden = !overwrite;
  $('#wm-backups').hidden = !overwrite;
  $('#wm-out-row').hidden = overwrite;
  $('#wm-mode-help').textContent=overwrite?'直接修改原文件。默认先备份，可通过“还原原图备份”撤销；备份保存在程序输出目录的 backups 下。':'保留原文件，生成的新照片保存到上面的副本位置。此模式不需要原图备份。';
  $('#wm-warn').hidden = !overwrite;
  $('#wm-out').disabled = overwrite;
  $('#wm-pick').disabled = overwrite;
  $('#wm-name').disabled = overwrite;
  $('#wm-skip').disabled = overwrite;
  $('#wm-format').disabled = overwrite;
  if (overwrite) $('#wm-format').value = 'same';
  $('#wm-run').textContent = overwrite ? '覆盖原图并打水印' : '开始生成水印';
  $('#wm-run').classList.toggle('danger', overwrite);
}

async function wmCheckFfmpeg() {
  const el = $('#wm-ffmpeg');
  if (!el || WM.ffChecked) return;
  WM.ffChecked = true;
  try {
    const c = await api('/api/capabilities');
    const f = (c && c.ffmpeg) || {};
    if (f.ok) {
      el.innerHTML = `<b style="color:#8fd18f">已检测到 FFmpeg</b>：${esc(f.version || '')}<br>
        视频会重新编码为 H.264（体积与画质会有变化），音频原样复制。`;
    } else {
      el.innerHTML = `未检测到 FFmpeg，视频会被跳过。${esc(f.hint || '')}`;
      const cb = $('#wm-video');
      if (cb) cb.checked = false;
      const c2 = wmCfg();
      c2.video = false;
    }
  } catch (e) {
    el.textContent = 'FFmpeg 检测失败：' + e.message;
  }
}

async function loadWatermark() {
  wmCfg();
  wmCheckFfmpeg();
  if (!WM.fonts.length) {
    try {
      const f = await apiGet('/api/fonts');
      WM.fonts = f.fonts || [];
      let userFonts = [];
      try {
        const u = await apiGet('/api/fonts/user');
        userFonts = u.fonts || [];
        WM.userFontDir = u.dir || '';
      } catch (e) { /* 用户字体目录不可用时忽略 */ }
      const sysOpts = WM.fonts.map(x => `<option value="${esc(x.name)}">${esc(x.name)}</option>`).join('');
      const userOpts = userFonts.map(x =>
        `<option value="__file__:${esc(x.path)}">${esc(x.name)}（自定义）</option>`).join('');
      $('#wm-font').innerHTML =
        (userOpts ? `<optgroup label="我的字体">${userOpts}</optgroup>` : '') +
        `<optgroup label="系统字体">${sysOpts}</optgroup>`;
    } catch (e) { /* 字体缺失时后端会兜底 */ }
  }
  if (!WM.presets.length) await wmLoadPresets();
  wmWriteUI();
  await wmUpdateScope();
  wmSyncMode();
  await wmPickSamples();
  wmRefreshPreviews();
}

/** 校验自定义字体文件能不能用 */
async function wmValidateFont() {
  const spec = $('#wm-font-path').value.trim();
  const hint = $('#wm-font-hint');
  if (!spec) {
    hint.innerHTML = '还没有指定字体文件。留空则使用上面选中的系统字体。';
    return;
  }
  try {
    const r = await api('/api/fonts/validate', { method: 'POST', body: { path: spec } });
    hint.innerHTML = `<b style="color:#8fd18f">字体可用</b>：${esc(r.name || '')}（${esc(r.path)}）`;
    toast('字体校验通过', 'ok');
  } catch (e) {
    hint.innerHTML = `<b style="color:#ff8f8f">字体不可用</b>：${esc(e.message)}`;
    toast('字体不可用：' + e.message, 'err');
  }
}

async function wmLoadPresets() {
  try {
    const r = await apiGet('/api/watermark/presets');
    WM.presets = r.presets || [];
    $('#wm-preset').innerHTML = '<option value="">自定义（当前参数）</option>' + WM.presets.map(p =>
      `<option value="${esc(p.id)}">${esc(p.name)}${p.builtin ? '' : '（自定义）'}</option>`).join('');
  } catch (e) { /* 忽略 */ }
}

async function wmPickSamples() {
  const request = WM.scopeGeneration;
  // 先废弃旧预览，避免快速切换范围时旧样片覆盖当前范围。
  WM.gen++;
  try {
    const scope = wmScope();
    const r = scope.ids
      ? { items: await Promise.all(scope.ids.slice(0, 12).map(id => apiGet('/api/media/' + id))) }
      : await apiGet('/api/media', filterParams({ limit: 400, media_type: 'image' }));
    if (request !== WM.scopeGeneration) return;
    const items = (r.items || []).filter(i => i.media_type === 'image' && i.width && i.height);
    WM.gen++;
    if (!items.length) { WM.samples = []; wmRenderSampleChips(); return; }
    const byArea = items.slice().sort((a, b) => a.width * a.height - b.width * b.height);
    const picked = [];
    const add = (it, tag) => {
      if (it && !picked.some(p => p.item.id === it.id)) picked.push({ item: it, tag });
    };
    add(byArea[0], '最小');
    add(byArea[byArea.length - 1], '最大');
    add(items.find(i => i.height > i.width), '竖图');
    add(items[0], '最近');
    WM.samples = picked.slice(0, 3);
    wmRenderSampleChips();
  } catch (e) { if (request === WM.scopeGeneration) toast(e.message, 'err'); }
}

/** 自由定位：按预览图的实际显示尺寸算出水印框的屏幕位置 */
function wmFreeBox(r) {
  const b = r.block;
  if (!b) return '';
  return `<div class="wm-free-box" data-bw="${b.w}" data-bh="${b.h}"></div>`;
}

/**
 * 把水印框摆到正确的位置。
 * 预览图是 max-width/max-height 缩放后居中的，容器里可能有留白，
 * 所以要按图片的实际渲染尺寸和偏移换算，不能直接用容器百分比。
 */
function wmPlaceBox(box, img, ph, r, px, py) {
  const ir = img.getBoundingClientRect();
  const pr = ph.getBoundingClientRect();
  if (!ir.width || !ir.height) return;
  // 每个图像像素对应多少屏幕像素（预览图是被缩放显示的）
  const scale = ir.width / r.width;
  const bw = Number(box.dataset.bw), bh = Number(box.dataset.bh);
  const maxX = Math.max(0, r.width - bw);
  const maxY = Math.max(0, r.height - bh);
  const cx = Math.min(Math.max(0, px), maxX);
  const cy = Math.min(Math.max(0, py), maxY);
  // 用相对 ph 的偏移来定位（ph 是 position:relative 的定位上下文）
  const left = ir.left - pr.left + cx * scale;
  const top = ir.top - pr.top + cy * scale;
  box.style.left = left + 'px';
  box.style.top = top + 'px';
  box.style.width = (bw * scale) + 'px';
  box.style.height = (bh * scale) + 'px';
}

/** 绑定拖拽：按住预览图里的水印框拖动，改写 pos_x / pos_y */
function wmBindDrag(card, r, cfg) {
  const ph = card.querySelector('.ph');
  const box = card.querySelector('.wm-free-box');
  const img = card.querySelector('img');
  if (!ph || !box || !img) return;
  const bw = Number(box.dataset.bw), bh = Number(box.dataset.bh);
  let dragging = false;
  let startX = 0, startY = 0, startPx = 0, startPy = 0;

  // 图片加载完才知道实际渲染尺寸
  const place = () => {
    const px = (Number(cfg.pos_x) || 0) / 100 * r.width;
    const py = (Number(cfg.pos_y) || 0) / 100 * r.height;
    wmPlaceBox(box, img, ph, r, px, py);
  };
  if (img.complete && img.naturalWidth) place();
  else img.addEventListener('load', place);
  // 预览是懒加载的，尺寸可能还会变；窗口缩放时也要重新定位
  const onResize = () => place();
  window.addEventListener('resize', onResize);
  card.addEventListener('DOMNodeRemoved', () => window.removeEventListener('resize', onResize));

  /** 鼠标位置换算成图片像素坐标 */
  const toImage = ev => {
    const rect = img.getBoundingClientRect();
    if (!rect.width || !rect.height) return null;
    // 屏幕像素 → 图像像素
    const scale = r.width / rect.width;
    return { x: (ev.clientX - rect.left) * scale, y: (ev.clientY - rect.top) * scale };
  };

  ph.addEventListener('pointerdown', ev => {
    if (dragging) return;
    const p = toImage(ev);
    if (!p) return;
    // 只有按在水印框附近才开始拖动，避免误触
    const bx = Math.min(Math.max(0, (Number(cfg.pos_x) || 0) / 100 * r.width), Math.max(0, r.width - bw));
    const by = Math.min(Math.max(0, (Number(cfg.pos_y) || 0) / 100 * r.height), Math.max(0, r.height - bh));
    const inside = p.x >= bx - 14 && p.x <= bx + bw + 14 && p.y >= by - 14 && p.y <= by + bh + 14;
    if (!inside) return;
    dragging = true;
    startX = p.x; startY = p.y;
    startPx = Number(cfg.pos_x) || 0; startPy = Number(cfg.pos_y) || 0;
    try { ph.setPointerCapture(ev.pointerId); } catch (e) { }
    ev.preventDefault();
  });

  ph.addEventListener('pointermove', ev => {
    if (!dragging) return;
    const p = toImage(ev);
    if (!p) return;
    const dx = p.x - startX, dy = p.y - startY;
    const nx = Math.min(100, Math.max(0, startPx + dx / r.width * 100));
    const ny = Math.min(100, Math.max(0, startPy + dy / r.height * 100));
    // 写回全局配置，重新渲染时才能带上新位置
    wmCfg().pos_x = nx; wmCfg().pos_y = ny;
    cfg.pos_x = nx; cfg.pos_y = ny;
    // 拖动时先就地更新框，避免每动一下就重渲染
    wmPlaceBox(box, img, ph, r, nx / 100 * r.width, ny / 100 * r.height);
  });

  const end = ev => {
    if (!dragging) return;
    dragging = false;
    try { ph.releasePointerCapture(ev.pointerId); } catch (e) { }
    // 松手后用新位置重新渲染，拿到真实的水印外观
    wmSchedulePreview();
  };
  ph.addEventListener('pointerup', end);
  ph.addEventListener('pointercancel', end);
}

function wmRenderSampleChips() {
  const box = $('#wm-samples');
  if (!WM.samples.length) {
    box.innerHTML = '<span>还没有可预览的照片，先扫描媒体源。</span>';
    return;
  }
  box.innerHTML = '<span style="color:#6f7889">对照样本：</span>' + WM.samples.map((s, i) =>
    `<span class="wm-sample-chip on" title="${esc(s.item.file_name || (s.item.files && s.item.files[0] && s.item.files[0].file_name) || '')}">${esc(s.tag)} ${esc(s.item.file_name || (s.item.files && s.item.files[0] && s.item.files[0].file_name) || '')} ${s.item.width}×${s.item.height}</span>`).join('');
}

function wmSchedulePreview() {
  clearTimeout(WM.timer);
  WM.timer = setTimeout(wmRefreshPreviews, 260);
}

async function wmRefreshPreviews() {
  const box = $('#wm-previews');
  box.classList.toggle('free', (wmCfg().position || '') === 'free');
  if (!WM.samples.length) {
    box.innerHTML = '<div class="wm-empty">'+esc(wmScope().label)+'。当前范围没有可预览的照片。</div>';
    return;
  }
  if (WM.loading) { wmSchedulePreview(); return; }
  WM.loading = true;
  const gen = ++WM.gen;
  // 浅拷贝一份发给后端；拖拽时改的是 WM.cfg 本体，所以这里要把 pos_x/pos_y 同步过来
  const cfg = Object.assign({}, wmCfg(), { pos_x: Number(wmCfg().pos_x) || 0, pos_y: Number(wmCfg().pos_y) || 0 });
  box.innerHTML = WM.samples.map(s => `
    <div class="wm-card">
      <div class="ph"><div class="wm-preview-loading">正在渲染 ${s.item.width}×${s.item.height} 的预览…</div></div>
      <div class="meta"><span>${esc(s.tag)}</span><span>${s.item.width}×${s.item.height}</span></div>
    </div>`).join('');
  const cards = Array.from(box.children);
  await Promise.all(WM.samples.map(async (s, i) => {
    try {
      const r = await api('/api/watermark/preview', {
        method: 'POST',
        body: { id: s.item.id, config: cfg, max_side: 1400 },
      });
      if (gen !== WM.gen) return;
      const card = cards[i];
      if (!card) return;
      const isFree = (cfg.position || '') === 'free';
      card.innerHTML = `
        <div class="ph"><img src="${r.image}" alt="水印效果，点击全屏查看" class="wm-preview-open" ${!isFree ? `data-wm-full="${esc(s.item.id)}"` : ''}>${isFree && r.block ? wmFreeBox(r) : ''}</div>
        <div class="meta">
          <button class="btn sm" data-wm-full="${esc(s.item.id)}">全屏查看水印效果</button>
          <span>${esc(s.tag)} ${esc(r.file_name || '')}</span><span><b>${r.width}×${r.height}</b></span>
          ${!r.watermarked ? `<label>仅此照片方向（预览与输出）<select data-wm-rotation="${esc(s.item.id)}">${[0,90,180,270].map(deg => `<option value="${deg}" ${deg === Number(r.correction || 0) ? 'selected' : ''}>${deg === 0 ? '按 EXIF' : deg + '°'}</option>`).join('')}</select></label>` : ''}
          <span>字号 <b>${r.font_px}px</b></span>
          <span>${esc(r.text || '')}</span>
          <span title="照片自带的 EXIF 方向；6/8 表示竖拍">${r.exif_orientation > 1 ? `EXIF 方向 ${r.exif_orientation}` : '无方向标记'}${
            r.rotate_image ? ` · 手动 ${r.rotate_image}°` : ''}</span>
          ${isFree && r.block ? `<span>位置 <b>${Number(cfg.pos_x).toFixed(1)}%, ${Number(cfg.pos_y).toFixed(1)}%</b></span>` : ''}
          ${r.warning ? `<span class="warn-line">${esc(r.warning)}</span>` : ''}
          ${r.detection?.suspected ? `<span class="wm-detection">本地检测特征分 ${r.detection.score}/100 <button class="btn sm" data-confirm-watermark="${esc(s.item.id)}">确认为已有水印</button></span>` : ''}
        </div>`;
      if (isFree && r.block) wmBindDrag(card, r, cfg);
    } catch (e) {
      if (gen !== WM.gen) return;
      const card = cards[i];
      if (card) card.innerHTML = `<div class="ph"><div class="wm-empty">${esc(e.message)}</div></div>
        <div class="meta"><span>${esc(s.tag)}</span></div>`;
    }
  }));
  WM.loading = false;
  if (gen !== WM.gen) wmSchedulePreview();
}

$('#wm-panel').addEventListener('input', e => {
  // 下拉框在改预设时会先派发 input、再派发 change；这里不能清空选择，否则预设应用不上
  if (e.target && e.target.id === 'wm-preset') return;
  $('#wm-preset').value = '';
  wmReadUI();
  wmSchedulePreview();
});
$('#wm-panel').addEventListener('change', () => { wmReadUI(); wmSchedulePreview(); });
$('#wm-panel').addEventListener('click', e => {
  const b = e.target.closest('#wm-pos button');
  if (b) {
    $$('#wm-pos button').forEach(x => x.classList.toggle('on', x === b));
    $('#wm-preset').value = '';
    wmReadUI();
    wmSchedulePreview();
  }
});
$('#wm-resample').addEventListener('click', async () => {
  await wmPickSamples();
  wmRefreshPreviews();
});
$('#wm-preset').addEventListener('change', () => {
  const p = WM.presets.find(x => x.id === $('#wm-preset').value);
  if (p && p.config) {
    WM.cfg = Object.assign({}, WM_DEFAULTS, p.config);
    wmWriteUI();
    wmSchedulePreview();
  }
});
$('#wm-preset-save').addEventListener('click', () => {
  askText('保存为预设', '我的水印', async name => {
    if (!name.trim()) return toast('预设名称不能为空', 'err');
    try {
      await api('/api/watermark/presets', { method: 'POST', body: { name: name.trim(), config: wmCfg() } });
      WM.presets = [];
      await wmLoadPresets();
      toast('预设已保存', 'ok');
    } catch (e) { toast(e.message, 'err'); }
  });
});
$('#wm-preset-del').addEventListener('click', async () => {
  const id = $('#wm-preset').value;
  if (!id) return;
  try {
    await api('/api/watermark/presets/' + encodeURIComponent(id), { method: 'DELETE' });
    WM.presets = [];
    await wmLoadPresets();
    toast('已删除预设', 'ok');
  } catch (e) { toast(e.message, 'err'); }
});
$('#wm-pick').addEventListener('click', async () => {
  await pickInto('#wm-out');
});
$('#wm-mode').addEventListener('change', () => {
  wmSyncMode();
  if ($('#wm-mode').value === 'overwrite') {
    toast('覆盖模式会直接改写原文件，建议先选 1 张试一张', 'err');
  }
});
$('#wm-run').addEventListener('click', async () => {
  const overwrite = $('#wm-mode').value === 'overwrite';
  const out_dir = $('#wm-out').value.trim();
  if (!overwrite && !out_dir) return toast('请先选择输出目录', 'err');
  const scope = wmScope();
  if (!scope.count) return toast('没有可处理的照片', 'err');
  const cfg = wmReadUI();
  if (overwrite) cfg.output_format = 'same';
  const body = {
    out_dir,
    config: cfg,
    mode: overwrite ? 'overwrite' : 'copy',
    backup: $('#wm-backup').checked,
    name_template: $('#wm-name').value.trim() || '{name}_wm.{ext}',
    quality: Number($('#wm-quality').value) || 92,
    keep_exif: $('#wm-exif').checked,
    skip_existing: $('#wm-skip').checked,
  };
  if (scope.ids) body.ids = scope.ids; else body.filter = filterParams();
  const vidNote = cfg.video ? '视频会用 FFmpeg 重新编码' : '视频会被跳过';
  const okText = overwrite
    ? `将直接改写 ${scope.label} 的原文件（HEIC 会被跳过，${
        cfg.video ? '视频需要 FFmpeg' : '视频会被跳过'}；没有可靠拍摄时间的照片会被跳过）。\n\n${
        $('#wm-backup').checked ? '覆盖前会把原件备份到 数据目录\\backups\\wm-<任务ID>\\，可在「覆盖记录」里一键还原。' : '没有勾选备份，原图将无法恢复！'}`
    : `将对 ${scope.label} 生成水印副本到：\n${out_dir}\n\n原图不会被修改或删除；${vidNote}；没有可靠拍摄时间的照片会被跳过（可在「待整理」里先指定日期）。`;
  confirmDialog(overwrite ? '确认覆盖原图' : '确认生成时间水印', okText,
    async () => {
      try {
        const r = await api('/api/watermark/run', { method: 'POST', body });
        S.settings.watermark = cfg;
        api('/api/settings', { method: 'POST', body: { watermark: cfg } }).catch(() => { });
        toast(`已开始处理 ${r.count} 项，进度见任务中心`, 'ok');
        switchView('jobs');
        loadJobs();
      } catch (e) { toast(e.message, 'err'); }
    });
});

function askText(title, def, onOk) {
  modal(`<div class="modal-head"><h3>${esc(title)}</h3></div>
    <div class="modal-body"><div class="field"><label>名称</label>
      <input type="text" id="ask-in" value="${esc(def || '')}" style="width:100%"></div></div>
    <div class="modal-foot"><button class="btn" id="ask-cancel">取消</button>
      <button class="btn primary" id="ask-ok">确定</button></div>`, { narrow: true });
  $('#ask-cancel').onclick = closeModal;
  $('#ask-ok').onclick = () => { const v = $('#ask-in').value; closeModal(); onOk(v); };
  setTimeout(() => { const el = $('#ask-in'); if (el) { el.focus(); el.select(); } }, 30);
}

/* ============================ 网页相册 ============================ */
$('#btn-album').addEventListener('click', () => {
  const ids = S.sel.size ? Array.from(S.sel) : null;
  openAlbumWizard(ids);
});

function openAlbumWizard(ids) {
  const count = ids ? ids.length : (S.filter.total || 0);
  if (!count) return toast('没有可导出的照片', 'err');
  modal(`<div class="modal-head"><h3>导出网页相册</h3><button class="icon-btn" id="al-close">×</button></div>
    <div class="modal-body">
      <div class="warnbox">导出的是一个自包含文件夹：双击 <b>index.html</b> 即可离线浏览，不联网、不上传。
        页面里的图片会重新编码（不带 EXIF/GPS）；只有勾选「附带原图」时才会复制原文件。</div>
      <div class="field"><label>范围</label>
        <div class="tplpreview">${ids ? '已选择的 ' + count + ' 项' : '当前筛选结果 ' + count + ' 项'}（视频会被跳过）</div></div>
      <div class="field"><label>相册标题</label>
        <input type="text" id="al-title" value="${esc((S.settings && S.settings.album_title) || '我的相册')}" style="width:100%"></div>
      <div class="field"><label>输出目录</label>
        <div style="display:flex;gap:8px">
          <input type="text" id="al-out" value="${esc((S.settings && S.settings.album_out) || '')}" placeholder="例如 D://相册导出">
          <button class="btn" id="al-pick">浏览…</button>
        </div></div>
      <div class="field"><label>大图长边（像素）</label>
        <select id="al-size">
          <option value="1200">1200（省空间）</option>
          <option value="1600" selected>1600（推荐）</option>
          <option value="2048">2048（高清）</option>
        </select></div>
      <div class="field"><label>页面主题</label>
        <select id="al-theme"></select>
        <div class="tplpreview" id="al-theme-desc"></div></div>
      <label class="wm-check"><input type="checkbox" id="al-watermark"> 导出时应用“时间水印”页的当前设置（不修改原图）</label>
      <div class="tplpreview">已有目录会自动另建带序号的新目录；实际保存位置显示在任务中心。</div>
      <label class="wm-check"><input type="checkbox" id="al-orig"> 附带未处理的原图（包含 GPS 等元数据，不带本次导出水印）</label>
      <label class="wm-check" style="margin-top:6px"><input type="checkbox" id="al-zip"> 顺便打包成 zip（放在输出目录旁边，方便直接分享）</label>
      <div class="field" id="al-pw-wrap" style="margin-top:6px"><label>相册密码（加密页面中的照片；ZIP 本身不加密）</label>
        <input type="password" id="al-password" placeholder="留空 = 不加密" style="width:100%">
        <div class="tplpreview">设置密码后不附带原图，可独立于 ZIP 使用。</div></div>
    </div>
    <div class="modal-foot"><button class="btn" id="al-cancel">取消</button>
      <button class="btn primary" id="al-run">开始导出</button></div>`);
  $('#al-close').onclick = closeModal;
  $('#al-cancel').onclick = closeModal;
  // 主题列表从后端取，取不到就用默认的那套
  api('/api/album/themes').then(r => {
    const themes = (r && r.themes) || [];
    if (!themes.length) return;
    const sel = $('#al-theme');
    if (!sel) return;
    sel.innerHTML = themes.map(t =>
      `<option value="${esc(t.key)}"${t.key === 'dark' ? ' selected' : ''}>${esc(t.name)}</option>`).join('');
    const desc = $('#al-theme-desc');
    const paint = () => {
      const t = themes.find(x => x.key === sel.value);
      desc.textContent = t ? t.desc : '';
    };
    sel.addEventListener('change', paint);
    paint();
  }).catch(() => { });
  $('#al-pick').onclick = async () => {
    await pickInto('#al-out');
  };
  $('#al-password').addEventListener('input', () => {
    const encrypted = $('#al-password').value.length > 0;
    $('#al-orig').disabled = encrypted;
    if (encrypted) $('#al-orig').checked = false;
  });
  $('#al-run').onclick = async () => {
    const out_dir = $('#al-out').value.trim();
    if (!out_dir) return toast('请先选择输出目录', 'err');
    const body = {
      out_dir,
      title: $('#al-title').value.trim() || '我的相册',
      max_side: Number($('#al-size').value),
      include_originals: $('#al-orig').checked,
      zip: $('#al-zip').checked,
      password: $('#al-password').value || '',
      theme: $('#al-theme').value || 'dark',
      watermark: $('#al-watermark').checked ? Object.assign({}, wmCfg()) : null,
    };
    if (ids) body.ids = ids; else body.filter = filterParams();
    try {
      const r = await api('/api/album/run', { method: 'POST', body });
      S.settings.album_out = out_dir;
      S.settings.album_title = body.title;
      api('/api/settings', { method: 'POST', body: { album_out: out_dir, album_title: body.title } }).catch(() => { });
      closeModal();
      toast(`已开始导出 ${r.count} 项`, 'ok');
      switchView('jobs');
      loadJobs();
    } catch (e) { toast(e.message, 'err'); }
  };
}

/* ============================ 幻灯片合成 ============================ */
$('#btn-slideshow').addEventListener('click', () => {
  const ids = S.sel.size ? Array.from(S.sel) : null;
  openSlideshowWizard(ids);
});

function openSlideshowWizard(ids) {
  const count = ids ? ids.length : (S.filter.total || 0);
  if (!count) return toast('没有可合成的素材', 'err');
  modal(`<div class="modal-head"><h3>合成幻灯片</h3><button class="icon-btn" id="sl-close">×</button></div>
    <div class="modal-body">
      <div class="warnbox">把选中的照片与视频拼成一段视频或动图。照片按顺序静止停留，视频取前几秒。<br>
        需要程序目录（或 <code>PVM_FFMPEG</code> 指定）有 <b>ffmpeg.exe</b>；没有的话这个功能不可用。</div>
      <div class="field"><label>范围</label>
        <div class="tplpreview">${ids ? '已选择的 ' + count + ' 项' : '当前筛选结果 ' + count + ' 项'}</div></div>
      <div class="field"><label>输出格式</label>
        <select id="sl-fmt" style="width:100%">
          <option value="mp4">MP4 视频（H.264，兼容性最好）</option>
          <option value="gif">GIF 动图（体积大，适合直接贴聊天窗口）</option>
        </select></div>
      <div class="field"><label>文件名</label>
        <input type="text" id="sl-name" value="幻灯片" style="width:100%"></div>
      <div class="field"><label>输出目录</label>
        <div style="display:flex;gap:8px">
          <input type="text" id="sl-out" value="${esc((S.settings && S.settings.slideshow_out) || '')}" placeholder="例如 D:\幻灯片">
          <button class="btn" id="sl-pick">浏览…</button>
        </div></div>
      <div class="field"><label>每张照片停留（秒）</label>
        <input type="number" id="sl-photo" min="0.5" max="60" step="0.5" value="3" style="width:140px"></div>
      <div class="field"><label>每段视频截取（秒，0 表示整段）</label>
        <input type="number" id="sl-video" min="0" max="120" step="1" value="5" style="width:140px"></div>
      <div class="field"><label>画布短边（像素）</label>
        <select id="sl-size" style="width:140px">
          <option value="480">480（快）</option>
          <option value="720" selected>720（推荐）</option>
          <option value="1080">1080（高清）</option>
        </select></div>
      <div class="field"><label>补边背景色</label>
        <input type="color" id="sl-bg" value="#000000"></div>
    </div>
    <div class="modal-foot"><button class="btn" id="sl-cancel">取消</button>
      <button class="btn primary" id="sl-run">开始合成</button></div>`);
  $('#sl-close').onclick = closeModal;
  $('#sl-cancel').onclick = closeModal;
  $('#sl-pick').onclick = async () => {
    await pickInto('#sl-out');
  };
  // 没有 FFmpeg 就提前说明，避免用户跑完才发现
  if (S.caps && S.caps.ffmpeg && !S.caps.ffmpeg.ok) {
    $('#sl-run').disabled = true;
    $('#sl-run').title = '未检测到 FFmpeg';
    const warn = document.createElement('div');
    warn.className = 'warnbox';
    warn.style.marginTop = '10px';
    warn.innerHTML = `<b style="color:#ff8f8f">未检测到 FFmpeg，无法合成。</b><br>${esc((S.caps.ffmpeg && S.caps.ffmpeg.hint) || '')}`;
    $('.modal-body').appendChild(warn);
  }
  $('#sl-run').onclick = async () => {
    const out_dir = $('#sl-out').value.trim();
    if (!out_dir) return toast('请先选择输出目录', 'err');
    const body = {
      out_dir,
      format: $('#sl-fmt').value,
      file_name: $('#sl-name').value.trim() || '幻灯片',
      photo_seconds: Number($('#sl-photo').value) || 3,
      video_seconds: Number($('#sl-video').value) || 0,
      size: Number($('#sl-size').value) || 720,
      background: $('#sl-bg').value || '#000000',
    };
    if (ids) body.ids = ids; else body.filter = filterParams();
    try {
      const r = await api('/api/slideshow/run', { method: 'POST', body });
      S.settings.slideshow_out = out_dir;
      api('/api/settings', { method: 'POST', body: { slideshow_out: out_dir } }).catch(() => { });
      closeModal();
      toast(`已开始合成 ${r.count} 项`, 'ok');
      switchView('jobs');
      loadJobs();
    } catch (e) { toast(e.message, 'err'); }
  };
}

/* ============================ 覆盖记录（水印备份） ============================ */
async function openBackupList() {
  modal(`<div class="modal-head"><h3>覆盖记录</h3><button class="icon-btn" id="bk-close">×</button></div>
    <div class="modal-body" id="bk-body"><div class="bk-empty">读取中…</div></div>
    <div class="modal-foot"><button class="btn" id="bk-done">关闭</button></div>`);
  $('#bk-close').onclick = closeModal;
  $('#bk-done').onclick = closeModal;
  const body = $('#bk-body');

  const paint = list => {
    if (!list.length) {
      body.innerHTML = `<div class="bk-empty">还没有覆盖过原图。<br>
        用「覆盖原图」模式生成水印后，这里会出现可以一键还原的批次（备份默认放在数据目录的 <code>backups\\</code> 下）。</div>`;
      return;
    }
    body.innerHTML = `<p class="bk-tip">按批次列出。点「还原」会把该批次里的照片恢复成打水印之前的样子，
      并刷新缩略图；「删除备份」只删备份文件，照片保持当前状态。</p>` +
      list.map(b => {
        const when = String(b.created_at || '').replace('T', ' ').slice(0, 16);
        const state = b.pending > 0
          ? `${b.pending} 张可还原`
          : (b.restored > 0 ? '已全部还原' : '备份缺失');
        const extra = [b.restored ? `已还原 ${b.restored}` : '', b.missing ? `缺失 ${b.missing}` : '']
          .filter(Boolean).join(' · ');
        return `<div class="bk-item" data-job="${esc(b.job_id)}">
          <div class="bk-main">
            <div class="t"><b>${b.total} 张</b><span class="st">${esc(when)} · ${esc(state)}${extra ? ' · ' + esc(extra) : ''} · ${fmtSize(b.bytes)}</span></div>
            <div class="bk-files">${esc((b.samples || []).join('、'))}${b.total > (b.samples || []).length ? ' …' : ''}</div>
          </div>
          <div class="bk-acts">
            <button class="btn sm primary" data-restore="${esc(b.job_id)}" ${b.pending > 0 ? '' : 'disabled'}>还原</button>
            <button class="btn sm" data-drop="${esc(b.job_id)}">删除备份</button>
          </div>
        </div>`;
      }).join('');
  };

  const load = async () => {
    try {
      const r = await api('/api/watermark/backups');
      paint(r.batches || []);
    } catch (e) {
      body.innerHTML = `<div class="bk-empty">${esc(e.message)}</div>`;
    }
  };

  body.addEventListener('click', async ev => {
    const rBtn = ev.target.closest('[data-restore]');
    const dBtn = ev.target.closest('[data-drop]');
    if (rBtn) {
      const job = rBtn.dataset.restore;
      const item = rBtn.closest('.bk-item');
      const n = (item && item.querySelector('.t b')) ? item.querySelector('.t b').textContent : '';
      confirmDialog('还原这一批？',
        `将把 ${n} 照片恢复成打水印之前的原件，并刷新缩略图。\n\n还原会覆盖当前的（带水印的）图片，操作后无法再撤回；备份文件会保留，可以再覆盖一次水印后重新还原。`,
        async () => {
          try {
            const res = await api('/api/watermark/backups/restore', { method: 'POST', body: { job_id: job } });
            toast(res.failed ? `已还原 ${res.restored} 张，失败 ${res.failed} 张` : `已还原 ${res.restored} 张`,
              res.failed ? 'err' : 'ok');
            if (res.errors && res.errors.length) console.warn(res.errors);
            openBackupList();
            if (S.view === 'library') loadMedia(true);
          } catch (e) { toast(e.message, 'err'); openBackupList(); }
        });
      return;
    }
    if (dBtn) {
      const job = dBtn.dataset.drop;
      confirmDialog('删除这批备份？',
        '只会删掉备份文件，照片保持当前（已打水印）的状态。删除后这一批就无法再还原了。',
        async () => {
          try {
            const res = await api('/api/watermark/backups/drop', { method: 'POST', body: { job_id: job } });
            toast(`已删除 ${res.removed} 个备份文件`, 'ok');
            openBackupList();
          } catch (e) { toast(e.message, 'err'); openBackupList(); }
        });
    }
  });

  await load();
}

$('#wm-backups').addEventListener('click', openBackupList);
$('#wm-font-validate').addEventListener('click', wmValidateFont);

/* ============================ 图像编辑 ============================ */
const EDIT_OPS = {
  rot90: '右转 90°', rot270: '左转 90°', rot180: '转 180°',
  fliph: '左右翻转', flipv: '上下翻转',
};

/** 编辑页的状态。region/maskRect 是百分比坐标 {x,y,w,h}。 */
const EDIT = {
  open: false, ids: [], busy: false,
  tab: 'rotate',
  cropMode: 'aspect',
  align: { h: 'center', v: 'center' },
  region: null,       // 裁剪区域（百分比）
  maskRect: { x: 60, y: 60, w: 25, h: 20 },  // 打码区域（百分比）
  maskMode: 'mosaic',
  maskStrength: 16,
  tone: { preset: 'custom', brightness: 0, contrast: 0, saturation: 0, warmth: 0 },
  timer: null,
  loading: false,
  gen: 0,
  sample: null,       // 用于预览的那张照片
  onResize: null,     // 当前挂着的 resize 监听器，重渲染时要先摘掉
};

const TONE_PRESET_LIST = [
  { key: 'custom', name: '原图' },
  { key: 'mono', name: '黑白' },
  { key: 'sepia', name: '棕调' },
  { key: 'cold', name: '冷调' },
  { key: 'warm', name: '暖调' },
  { key: 'fade', name: '褪色' },
  { key: 'vivid', name: '鲜艳' },
];

function editScope() { return { ids: EDIT.ids.slice(), count: EDIT.ids.length, label: '已选 ' + EDIT.ids.length + ' 项照片' }; }

function editSyncMode() {
  const overwrite = $('#edit-mode').value === 'overwrite';
  $('#edit-backup-row').hidden = !overwrite;
  $('#edit-warn').hidden = !overwrite;
  $('#edit-out-row').hidden = overwrite;
  $('#edit-out').disabled = overwrite;
  $('#edit-pick').disabled = overwrite;
  $('#edit-name').disabled = overwrite;
}

/** 切换编辑方式（旋转/裁剪/调色/打码） */
function editSetTab(tab) {
  EDIT.tab = tab;
  $$('#edit-tabs button').forEach(b => b.classList.toggle('on', b.dataset.tab === tab));
  ['rotate', 'crop', 'tone', 'mask'].forEach(t => {
    const pane = $('#edit-pane-' + t);
    if (pane) pane.hidden = (t !== tab);
  });
  // 「应用到所选照片」只在非旋转模式出现（旋转仍是点按钮即执行）
  const run = $('#edit-run');
  if (run) run.hidden = (tab === 'rotate');
  editRefreshPreview();
}

/** 进入编辑视图：刷新范围提示，并按当前选择决定按钮是否可用 */
function loadEditView() {
  EDIT.sample = null;
  syncOutputFields();
  editSyncMode();
  const s = editScope();
  $('#edit-scope').textContent = '范围：' + s.label;
  $$('#edit-ops .edit-op').forEach(b => {
    b.disabled = s.count === 0;
  });
  $('#edit-run').disabled = s.count === 0;
  editRenderTonePresets();
  editRefreshPreview();
}

function editRenderTonePresets() {
  const box = $('#edit-tone-presets');
  if (!box) return;
  if (box.dataset.built === '1') {
    $$('#edit-tone-presets .edit-op').forEach(b => {
      b.classList.toggle('on', b.dataset.preset === EDIT.tone.preset);
    });
    return;
  }
  box.dataset.built = '1';
  box.innerHTML = TONE_PRESET_LIST.map(p => `
    <button class="edit-op" data-preset="${esc(p.key)}" title="${esc(p.name)}">
      <span class="ico">${p.key === 'custom' ? '○' : '◐'}</span><span class="lb">${esc(p.name)}</span>
    </button>`).join('');
}

/* ---------------- 预览 ---------------- */

/** 找一张能用来预览的照片：优先已选中的第一张。 */
async function editPickSample() {
 if (EDIT.sample) return EDIT.sample;
 for (const id of EDIT.ids) { const a = await apiGet('/api/media/' + id); if (a.media_type === 'image') { EDIT.sample = { id }; return EDIT.sample; } }
 return null;
}

/** 当前编辑动作对应的请求参数（发给后端 preview / run）。 */
function editParams() {
  switch (EDIT.tab) {
    case 'crop':
      if (EDIT.cropMode === 'region') {
        return { action: 'crop', params: { rect: EDIT.region || { x: 0, y: 0, w: 100, h: 100 } } };
      }
      return {
        action: 'crop',
        params: { aspect: Number($('#edit-aspect').value), h: EDIT.align.h, v: EDIT.align.v },
      };
    case 'tone':
      return {
        action: 'tone',
        params: {
          preset: EDIT.tone.preset,
          brightness: EDIT.tone.brightness,
          contrast: EDIT.tone.contrast,
          saturation: EDIT.tone.saturation,
          warmth: EDIT.tone.warmth,
        },
      };
    case 'mask':
      return {
        action: 'mask',
        params: {
          rect: EDIT.maskRect || { x: 60, y: 60, w: 25, h: 20 },
          mode: EDIT.maskMode,
          strength: EDIT.maskStrength,
        },
      };
    default:
      return null;
  }
}

function editSchedulePreview() {
  clearTimeout(EDIT.timer);
  EDIT.timer = setTimeout(editRefreshPreview, 280);
}

async function editRefreshPreview() {
  const area = $('#edit-preview-area');
  if (!area) return;
  if (EDIT.tab === 'rotate') {
    const sample = await editPickSample();
    if (sample) {
      area.innerHTML = `<div class="wm-card"><div class="ph"><img src="${thumbUrl(sample,640)}" alt="当前照片"></div><div class="meta">当前照片。点击旋转或翻转即执行；默认修改当前照片并备份，可在覆盖记录中还原。</div></div>`;
      return;
    }
    area.innerHTML = `<div class="wm-empty">
      旋转与翻转不需要预览。<br>
      批量变换不会改动拍摄时间、EXIF 里的其他信息（除了方向标记会归正）。<br>
      视频会被跳过；HEIC 不支持原地编辑（没有 HEVC 编码器），会明确报错。
    </div>`;
    return;
  }
  const sample = await editPickSample();
  if (!sample) {
    area.innerHTML = `<div class="wm-empty">没有可预览的照片。先在「媒体源」里扫描照片。</div>`;
    return;
  }
  const p = editParams();
  if (!p) return;
  if (EDIT.loading) { editSchedulePreview(); return; }
  EDIT.loading = true;
  const gen = ++EDIT.gen;
  // 重渲染会换掉整个预览区，先把上一轮的 resize 监听摘掉
  if (EDIT.onResize) { window.removeEventListener('resize', EDIT.onResize); EDIT.onResize = null; }
  area.innerHTML = `<div class="wm-card"><div class="ph">
    <div class="wm-preview-loading">正在渲染预览…</div></div>
    <div class="meta"><span>预览中</span></div></div>`;
  try {
    // 裁剪（按区域）与打码要框选区域，而框是按**原图**百分比记录的。
    // 如果只在处理后的结果图上框，坐标会和实际处理错位，
    // 所以另外取一张原图当参照，用户在参照图上框选。
    const needBox = (EDIT.tab === 'crop' && EDIT.cropMode === 'region') || EDIT.tab === 'mask';
    const rawReq = needBox
      ? api('/api/edit/preview', {
        method: 'POST',
        body: { id: sample.id, action: 'raw', params: {}, max_side: 1200 },
      })
      : Promise.resolve(null);
    const [r, raw] = await Promise.all([
      api('/api/edit/preview', {
        method: 'POST',
        body: { id: sample.id, action: p.action, params: p.params, max_side: 1200 },
      }),
      rawReq,
    ]);
    if (gen !== EDIT.gen) return;
    area.innerHTML = needBox
      ? `<div class="wm-card">
          <div class="ph" id="edit-ph"><img id="edit-ph-img" src="${raw.image}" alt=""></div>
          <div class="meta">
            <span><b>原图</b>（在图上拖动框选）</span>
            <span>${esc(raw.file_name || '')}</span>
            <span>${esc(raw.width)}×${esc(raw.height)}</span>
          </div></div>
        <div class="wm-card">
          <div class="ph"><img src="${r.image}" alt=""></div>
          <div class="meta">
            <span><b>效果预览</b></span><span>输出 <b>${r.width}×${r.height}</b></span>
          </div></div>`
      : `<div class="wm-card">
          <div class="ph" id="edit-ph"><img id="edit-ph-img" src="${r.image}" alt=""></div>
          <div class="meta">
            <span>效果预览</span><span>输出 <b>${r.width}×${r.height}</b></span>
            <span>${esc(r.file_name || '')}</span>
          </div></div>`;
    if (needBox) editBindRegionDrag();
  } catch (e) {
    if (gen !== EDIT.gen) return;
    area.innerHTML = `<div class="wm-card"><div class="ph"><div class="wm-empty">${esc(e.message)}</div></div></div>`;
  }
  EDIT.loading = false;
  if (gen !== EDIT.gen) editSchedulePreview();
}

/* ---------------- 区域拖拽（裁剪 / 打码共用） ---------------- */

/** 把百分比矩形画到预览图上。 */
function editPlaceRegion(rect, img, ph) {
  const ir = img.getBoundingClientRect();
  const pr = ph.getBoundingClientRect();
  if (!ir.width || !ir.height) return;
  const scale = ir.width / 100;   // 1% 对应多少屏幕像素
  const box = ph.querySelector('.edit-region');
  if (!box) return;
  box.style.left = (ir.left - pr.left + rect.x * scale) + 'px';
  box.style.top = (ir.top - pr.top + rect.y * scale) + 'px';
  box.style.width = (rect.w * scale) + 'px';
  box.style.height = (rect.h * scale) + 'px';
}

/** 在预览图上拖出一个矩形，松手后写入百分比坐标。 */
function editBindRegionDrag() {
  const ph = $('#edit-ph');
  const img = $('#edit-ph-img');
  if (!ph || !img) return;
  const isMask = EDIT.tab === 'mask';

  const box = document.createElement('div');
  box.className = 'edit-region';
  ph.appendChild(box);

  const place = () => {
    const r = isMask ? EDIT.maskRect : EDIT.region;
    if (r) editPlaceRegion(r, img, ph);
  };
  place();
  if (!img.complete || !img.naturalWidth) img.addEventListener('load', place);
  // 预览每次重渲染都会换掉整个 ph，之前的 resize 监听器要一起撤掉，否则会越积越多
  if (EDIT.onResize) window.removeEventListener('resize', EDIT.onResize);
  EDIT.onResize = place;
  window.addEventListener('resize', place);

  let dragging = false, sx = 0, sy = 0;

  /** 屏幕坐标 → 百分比坐标 */
  const toPct = ev => {
    const r = img.getBoundingClientRect();
    if (!r.width || !r.height) return null;
    return {
      x: (ev.clientX - r.left) / r.width * 100,
      y: (ev.clientY - r.top) / r.height * 100,
    };
  };

  ph.addEventListener('pointerdown', ev => {
    const p = toPct(ev);
    if (!p) return;
    dragging = true;
    sx = p.x; sy = p.y;
    try { ph.setPointerCapture(ev.pointerId); } catch (e) { }
    ev.preventDefault();
  });

  ph.addEventListener('pointermove', ev => {
    if (!dragging) return;
    const p = toPct(ev);
    if (!p) return;
    let x = Math.min(sx, p.x), y = Math.min(sy, p.y);
    let w = Math.abs(p.x - sx), h = Math.abs(p.y - sy);
    // 太小就不更新，避免误触留下一个点
    if (w < 1 || h < 1) return;
    x = Math.max(0, Math.min(100 - w, x));
    y = Math.max(0, Math.min(100 - h, y));
    const rect = { x, y, w, h };
    if (isMask) EDIT.maskRect = rect; else EDIT.region = rect;
    editPlaceRegion(rect, img, ph);
    editSyncRectLabel();
  });

  const end = ev => {
    if (!dragging) return;
    dragging = false;
    try { ph.releasePointerCapture(ev.pointerId); } catch (e) { }
    editSyncRectLabel();
    editSchedulePreview();
  };
  ph.addEventListener('pointerup', end);
  ph.addEventListener('pointercancel', end);
}

/** 把当前区域写进界面上的「区域」标签。 */
function editSyncRectLabel() {
  const isMask = EDIT.tab === 'mask';
  const r = isMask ? EDIT.maskRect : EDIT.region;
  // 两个标签都在 DOM 里（面板只是 hidden），必须按当前标签页挑，
  // 否则用 || 短路永远只会写到打码那个上。
  const label = isMask ? $('#edit-mask-rect') : $('#edit-crop-rect');
  if (!label) return;
  label.textContent = r
    ? `${r.x.toFixed(0)}%, ${r.y.toFixed(0)}% · ${r.w.toFixed(0)}×${r.h.toFixed(0)}%`
    : '—';
}

/* ---------------- 覆盖记录 ---------------- */

/** 覆盖记录：把水印与编辑两个来源的批次合并展示 */
async function openEditBackupList() {
  modal(`<div class="modal-head"><h3>覆盖记录（图像编辑）</h3><button class="icon-btn" id="bk-close">×</button></div>
    <div class="modal-body" id="bk-body"><div class="bk-empty">读取中…</div></div>
    <div class="modal-foot"><button class="btn" id="bk-done">关闭</button></div>`);
  $('#bk-close').onclick = closeModal;
  $('#bk-done').onclick = closeModal;
  const body = $('#bk-body');

  const paint = list => {
    if (!list.length) {
      body.innerHTML = `<div class="bk-empty">还没有用「覆盖原图」做过编辑。<br>
        修改前的原件会备份到统一输出位置的 <code>backups\\edit-&lt;任务ID&gt;\\</code> 下，
        随时可以在这里一键还原。</div>`;
      return;
    }
    body.innerHTML = `<p class="bk-tip">按批次列出。点「还原」会把该批照片恢复到编辑之前的样子，并刷新缩略图；
      「删除备份」只删备份文件，照片保持当前状态。</p>` +
      list.map(b => {
        const when = String(b.created_at || '').replace('T', ' ').slice(0, 16);
        const pending = (b.total || 0) - (b.restored || 0);
        const state = pending > 0 ? `${pending} 张可还原` : '已全部还原';
        return `<div class="bk-item" data-job="${esc(b.job_id)}">
          <div class="bk-main">
            <div class="t"><b>${b.total} 张</b><span class="st">${esc(when)} · ${esc(state)}</span></div>
            <div class="bk-files">批次 ${esc(b.job_id)}</div>
          </div>
          <div class="bk-acts">
            <button class="btn sm primary" data-restore="${esc(b.job_id)}" ${pending > 0 ? '' : 'disabled'}>还原</button>
            <button class="btn sm" data-drop="${esc(b.job_id)}">删除备份</button>
          </div>
        </div>`;
      }).join('');
  };

  const load = async () => {
    try {
      const r = await api('/api/edit/backups');
      paint(Array.isArray(r) ? r : (r.batches || []));
    } catch (e) {
      body.innerHTML = `<div class="bk-empty">${esc(e.message)}</div>`;
    }
  };

  body.addEventListener('click', async ev => {
    const rBtn = ev.target.closest('[data-restore]');
    const dBtn = ev.target.closest('[data-drop]');
    if (rBtn) {
      const job = rBtn.dataset.restore;
      const item = rBtn.closest('.bk-item');
      const n = (item && item.querySelector('.t b')) ? item.querySelector('.t b').textContent : '';
      confirmDialog('还原这一批？',
        `将把 ${n} 张照片恢复成编辑之前的原件，并刷新缩略图。\n\n还原会覆盖当前的（已编辑过的）图片，操作后无法再撤回；备份文件会保留。`,
        async () => {
          try {
            const res = await api('/api/edit/backups/restore', { method: 'POST', body: { job_id: job } });
            toast(res.failed && res.failed.length
              ? `已还原 ${res.restored} 张，失败 ${res.failed.length} 张`
              : `已还原 ${res.restored} 张`,
              (res.failed && res.failed.length) ? 'err' : 'ok');
            openEditBackupList();
            if (S.view === 'library') loadMedia(true);
          } catch (e) { toast(e.message, 'err'); openEditBackupList(); }
        });
      return;
    }
    if (dBtn) {
      const job = dBtn.dataset.drop;
      confirmDialog('删除这批备份？',
        '只会删掉备份文件，照片保持当前状态。删除后这一批就无法再还原了。',
        async () => {
          try {
            const res = await api('/api/edit/backups/drop', { method: 'POST', body: { job_id: job } });
            toast(`已删除 ${res.removed} 个备份记录`, 'ok');
            openEditBackupList();
          } catch (e) { toast(e.message, 'err'); openEditBackupList(); }
        });
    }
  });

  await load();
}

/* ---------------- 执行 ---------------- */

/** 旋转/翻转：点按钮即执行。 */
async function runEdit(op) {
  if (EDIT.busy) return;
  const overwrite = $('#edit-mode').value === 'overwrite';
  const out_dir = $('#edit-out').value.trim();
  if (!overwrite && !out_dir) return toast('请先选择输出目录', 'err');
  const s = editScope();
  if (!s.count) return toast('当前没有可处理的照片', 'err');
  const body = {
    op,
    mode: overwrite ? 'overwrite' : 'copy',
    out_dir: overwrite ? '' : out_dir,
    name_template: $('#edit-name').value.trim(),
    keep_exif: $('#edit-exif').checked,
    backup: $('#edit-backup').checked,
  };
  if (s.ids) body.ids = s.ids; else body.filter = S.filter;
  const label = EDIT_OPS[op] || op;
  const doRun = async () => {
    EDIT.busy = true;
    try {
      const fileIds = await editFileIds(s.ids);
      const r = await api('/api/edit/run', { method: 'POST', body });
      toast(`已开始${label} ${r.count} 项`, 'ok');
      await followLibraryEdit(r.job_id, overwrite ? fileIds : []);
    } catch (e) { EDIT.busy = false; toast(e.message, 'err'); }
  };
  if (overwrite && (s.count > 1 || !body.backup)) {
    confirmDialog('覆盖原图？',
      `将对 ${s.count} 项执行「${label}」，直接改写原文件。\n\n` +
      ($('#edit-backup').checked
        ? '修改前自动备份到统一输出位置的 backups 下，可在「覆盖记录」里还原。'
        : '注意：你没有勾选备份，原图将无法恢复！'),
      doRun);
    return;
  }
  await doRun();
}

/** 裁剪/调色/打码：点「应用到所选照片」执行。 */
async function runEditAction() {
  if (EDIT.busy) return;
  const p = editParams();
  if (!p) return toast('请选择编辑方式', 'err');
  const overwrite = $('#edit-mode').value === 'overwrite';
  const out_dir = $('#edit-out').value.trim();
  if (!overwrite && !out_dir) return toast('请先选择输出目录', 'err');
  const s = editScope();
  if (!s.count) return toast('当前没有可处理的照片', 'err');
  const body = {
    action: p.action,
    params: p.params,
    mode: overwrite ? 'overwrite' : 'copy',
    out_dir: overwrite ? '' : out_dir,
    name_template: $('#edit-name').value.trim(),
    keep_exif: $('#edit-exif').checked,
    backup: $('#edit-backup').checked,
  };
  if (s.ids) body.ids = s.ids; else body.filter = S.filter;
  const label = $('#edit-pane-' + EDIT.tab).querySelector('.wm-title').textContent.trim();
  const doRun = async () => {
    EDIT.busy = true;
    try {
      const fileIds = await editFileIds(s.ids);
      const r = await api('/api/edit/run', { method: 'POST', body });
      toast(`已开始${label} ${r.count} 项`, 'ok');
      await followLibraryEdit(r.job_id, overwrite ? fileIds : []);
    } catch (e) { EDIT.busy = false; toast(e.message, 'err'); }
  };
  if (overwrite) {
    confirmDialog('覆盖原图？',
      `将对 ${s.count} 项执行「${label}」，直接改写原文件。\n\n` +
      ($('#edit-backup').checked
        ? '修改前自动备份到统一输出位置的 backups 下，可在「覆盖记录」里还原。'
        : '注意：你没有勾选备份，原图将无法恢复！'),
      doRun);
    return;
  }
  await doRun();
}

/* ---------------- 事件绑定 ---------------- */

$('#edit-ops').addEventListener('click', e => {
  const b = e.target.closest('.edit-op');
  if (!b || b.disabled) return;
  runEdit(b.dataset.op);
});

$('#edit-tabs').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  editSetTab(b.dataset.tab);
});

$('#edit-crop-mode').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  EDIT.cropMode = b.dataset.crop;
  $$('#edit-crop-mode button').forEach(x => x.classList.toggle('on', x === b));
  $('#edit-crop-aspect').hidden = (EDIT.cropMode !== 'aspect');
  $('#edit-crop-region').hidden = (EDIT.cropMode !== 'region');
  if (EDIT.cropMode === 'region' && !EDIT.region) {
    EDIT.region = { x: 0, y: 0, w: 100, h: 100 };
  }
  editSyncRectLabel();
  editRefreshPreview();
});

$('#edit-aspect').addEventListener('change', () => editRefreshPreview());

$('#edit-crop-align').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  EDIT.align = { h: b.dataset.h, v: b.dataset.v };
  $$('#edit-crop-align button').forEach(x => x.classList.toggle('on', x === b));
  editRefreshPreview();
});

$('#edit-crop-reset').addEventListener('click', () => {
  EDIT.region = { x: 0, y: 0, w: 100, h: 100 };
  editSyncRectLabel();
  editRefreshPreview();
});

$('#edit-mask-mode').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  EDIT.maskMode = b.dataset.mask;
  $$('#edit-mask-mode button').forEach(x => x.classList.toggle('on', x === b));
  editRefreshPreview();
});

$('#edit-mask-strength').addEventListener('input', e => {
  EDIT.maskStrength = Number(e.target.value);
  $('#edit-mask-strength-v').textContent = e.target.value;
  editSchedulePreview();
});

$('#edit-mask-reset').addEventListener('click', () => {
  EDIT.maskRect = { x: 60, y: 60, w: 25, h: 20 };
  editSyncRectLabel();
  editRefreshPreview();
});

$('#edit-tone-presets').addEventListener('click', e => {
  const b = e.target.closest('.edit-op');
  if (!b) return;
  EDIT.tone.preset = b.dataset.preset;
  editRenderTonePresets();
  editRefreshPreview();
});

const TONE_SLIDERS = [
  ['#edit-t-bright', 'brightness', '#edit-t-bright-v'],
  ['#edit-t-contrast', 'contrast', '#edit-t-contrast-v'],
  ['#edit-t-sat', 'saturation', '#edit-t-sat-v'],
  ['#edit-t-warm', 'warmth', '#edit-t-warm-v'],
];
TONE_SLIDERS.forEach(([sel, key, out]) => {
  $(sel).addEventListener('input', e => {
    EDIT.tone[key] = Number(e.target.value);
    $(out).textContent = e.target.value;
    // 手动调了就是自定义
    EDIT.tone.preset = 'custom';
    editRenderTonePresets();
    editSchedulePreview();
  });
});

$('#edit-run').addEventListener('click', runEditAction);

$('#edit-mode').addEventListener('change', () => {
  editSyncMode();
});
$('#edit-pick').addEventListener('click', async () => {
  await pickInto('#edit-out');
});
$('#edit-backups').addEventListener('click', openEditBackupList);

function outputRoot() {
  return S.settings.output_root || (S.state && S.state.output_root) || ((S.state && S.state.program_dir) || '.') + '/exports';
}
function outputPath(kind) { return outputRoot().replace(/[\\/]+$/, '') + '/' + kind; }
function syncOutputFields() {
  for (const [id, kind, pick] of [['aw-out','archive','aw-pick'],['wm-out','watermark','wm-pick'],['edit-out','edited','edit-pick'],['al-out','albums','al-pick'],['sl-out','slideshows','sl-pick']]) {
    const input = $('#' + id); if (!input) continue;
    input.value = outputPath(kind); input.readOnly = true; input.title = '在设置 → 统一输出位置中修改';
    const browse = $('#' + pick); if (browse) browse.hidden = true;
    if (!input.parentNode.querySelector('[data-output-settings]')) {
      const button = document.createElement('button'); button.className = 'btn sm'; button.dataset.outputSettings = '1'; button.textContent = '输出设置';
      button.onclick = () => { closeModal(); switchView('settings'); };
      input.parentNode.appendChild(button);
    }
  }
}

async function openLibraryEditor(ids) {
  ids = ids || (S.sel.size ? Array.from(S.sel) : (S.detailId ? [S.detailId] : []));
  if (!ids.length) return toast('请先在媒体库选择照片，或打开一张照片后点击编辑');
  const photos = [];
  for (const id of ids) { const a = await apiGet('/api/media/' + id); if (a.media_type === 'image') photos.push(id); }
  if (!photos.length) return toast('所选项目没有可编辑的照片','err');
  S.sel = new Set(photos); EDIT.ids = photos; EDIT.open = true;
  closeDetail(); switchView('library');
  $('#library-editor').hidden = false; $('#view-library').classList.add('editing');
  $('#edit-context').textContent = photos.length === 1 ? '当前照片' : '批量编辑 ' + photos.length + ' 张';
  loadEditView(); layout();
}
function closeLibraryEditor() {
  EDIT.open = false; EDIT.gen++; $('#library-editor').hidden = true;
  $('#view-library').classList.remove('editing'); layout();
}
document.addEventListener('keydown', e => { if(e.key==='Escape' && EDIT.open && $('#modal-root').hidden) closeLibraryEditor(); });
async function editFileIds(ids) {
  return Promise.all(ids.map(async id => {
    const a = await apiGet('/api/media/' + id);
    if (!a.files || !a.files.length) throw new Error('照片文件不可用，请重新选择');
    return a.files[0].id;
  }));
}
async function followLibraryEdit(jobId, fileIds = [], quiet = false) {
  EDIT.busy = true; $('#edit-context').textContent = '正在编辑…';
  try {
    for (let i = 0; i < 600; i++) {
      const job = await apiGet('/api/jobs/' + jobId);
      if (!['running','queued','cancelling'].includes(job.status)) {
        toast(quiet && !job.failed ? (job.message.includes('HEIC') ? '已更新方向，HEIC 原件保留' : '旋转完成，已自动备份') : (job.message || '编辑完成'), job.failed ? 'err' : 'ok');
        if (fileIds.length) {
          const current = await Promise.all(fileIds.map(id=>apiGet('/api/media/by-file/'+id)));
          EDIT.ids = Array.from(new Set(current.map(a=>a.id))); S.sel = new Set(EDIT.ids);
        }
        await loadState(); await loadMedia(false);
        if (EDIT.open) loadEditView(); return;
      }
      await new Promise(resolve => setTimeout(resolve,400));
    }
    toast('任务仍在后台运行，可到任务中心查看');
  } finally { EDIT.busy = false; $('#edit-context').textContent = '已选 ' + EDIT.ids.length + ' 张照片'; loadJobs(); }
}
$('#view-library').appendChild($('#library-editor'));
$('#sel-edit').onclick = () => openLibraryEditor().catch(e=>toast(e.message,'err'));
$('#d-edit').onclick = () => openLibraryEditor(S.detailId ? [S.detailId] : []).catch(e=>toast(e.message,'err'));
$('#edit-close').onclick = closeLibraryEditor;

let deletingPhotos = false;
async function openDeletePhotos(ids, fileIds = null) {
  if (deletingPhotos || EDIT.busy) return toast('请等待当前照片操作完成');
  ids = Array.from(new Set(ids));
  if (!ids.length) return toast('请先选择要删除的照片');
  // 删除范围是打开确认框时的选择快照；不允许退回到整个筛选结果。
  const plan = await api('/api/media/trash/plan', {method:'POST', body:fileIds ? {file_ids:fileIds} : {ids}});
  modal(`<div class="modal-head"><h3>删除照片</h3></div>
    <div class="modal-body">
      <p>将以下 <b id="delete-count">${plan.count}</b> 个文件移到 Windows 回收站，并更新媒体库。</p>
      <p class="tplhint">${fileIds ? '只删除下方列出的具体副本，其他副本保留。' : '默认只处理每张照片当前显示的文件。相同照片的其他副本、实况视频及编辑备份会保留。'}</p>
      <label ${fileIds ? 'hidden' : ''}><input type="checkbox" id="delete-copies"> 同时删除所选照片的全部重复副本</label>
      <div id="delete-paths" style="max-height:240px;overflow:auto;overflow-wrap:anywhere;margin-top:14px"></div>
      <p id="delete-error" role="alert"></p>
      <p class="tplhint">误删后可在 Windows 回收站还原，再扫描原媒体源。</p>
    </div>
    <div class="modal-foot"><button class="btn" id="delete-cancel">取消</button>
      <button class="btn danger" id="delete-confirm">移到回收站</button></div>`, {narrow:true});
  const paths = $('#delete-paths'), count = $('#delete-count'), error = $('#delete-error');
  const confirm = $('#delete-confirm'), copies = $('#delete-copies');
  let current = plan;
  const render = p => { count.textContent = p.count; paths.innerHTML = p.files.map(f=>`<div class="file">${esc(f.path)}</div>`).join(''); };
  render(plan);
  $('#delete-cancel').onclick = closeModal;
  copies.onchange = async () => {
    confirm.disabled = true; copies.disabled = true; current = null; error.textContent = '';
    try {
      const p = await api('/api/media/trash/plan', {method:'POST',body:{ids,all_copies:copies.checked}});
      if (!confirm.isConnected) return;
      current = p; render(p); confirm.disabled = false;
    } catch(e) { error.textContent = e.message; }
    finally { copies.disabled = false; }
  };
  confirm.onclick = async () => {
    if (deletingPhotos || !current || confirm.disabled) return;
    deletingPhotos = true; confirm.disabled = true;
    try {
      const r = await api('/api/media/trash/run', {method:'POST',body:{plan_id:current.plan_id}});
      closeModal();
      if (ids.includes(S.detailId)) { closeDetail(); $('#d-body').innerHTML = ''; }
      if (EDIT.open && EDIT.ids.some(id=>ids.includes(id))) closeLibraryEditor();
      toast('正在移到回收站，进度可在任务中心查看'); loadJobs();
      for (let i=0; i<1800; i++) {
        const job = await apiGet('/api/jobs/'+r.job_id);
        if (!['running','queued','cancelling'].includes(job.status)) {
          ids.forEach(id=>S.sel.delete(id));
          toast(job.message || '删除完成', job.failed ? 'err' : 'ok');
          await loadState(); await loadFacets(); await loadMedia(false); loadJobs();
          if (S.view === 'dupes') await loadDupes();
          if (S.view === 'similar') await loadSimilar();
          if (S.view === 'review') await loadReview();
          return;
        }
        await new Promise(resolve=>setTimeout(resolve,500));
      }
      toast('任务仍在后台运行，请在任务中心查看结果');
    } catch(e) { toast(e.message,'err'); if (confirm.isConnected) confirm.disabled = false; }
    finally { deletingPhotos = false; }
  };
}
$('#sel-delete').onclick = () => openDeletePhotos(Array.from(S.sel)).catch(e=>toast(e.message,'err'));
$('#d-delete').onclick = () => openDeletePhotos(S.detailId ? [S.detailId] : []).catch(e=>toast(e.message,'err'));
$('#wm-font-browse').onclick = async () => {
  const input = $('#wm-font-path');
  const path = await pickFolder(input.value,'font');
  if (path && input.isConnected) { input.value=path; wmReadUI(); await wmValidateFont(); wmSchedulePreview(); }
};
$('#f-watermark').addEventListener('change', e => setFilter('watermarked', e.target.value));
$('#sel-watermark').addEventListener('click', () => {
  $('#wm-scope-mode').value = 'selected'; switchView('watermark');
});
$('#wm-library').addEventListener('click', () => switchView('library'));
$('#wm-horizontal').addEventListener('click', () => {
  wmCfg().rotate = 0; wmWriteUI(); wmSchedulePreview();
});
$('#wm-known-skip').addEventListener('change', () => { wmReadUI(); wmSchedulePreview(); });
$('#wm-scope-mode').addEventListener('change', async () => { await wmUpdateScope(); await wmPickSamples(); wmSchedulePreview(); });
$('#wm-previews').addEventListener('change', async e => {
  const id = e.target.dataset.wmRotation;
  if (!id) return;
  try { await api('/api/watermark/rotation', { method: 'POST', body: { id, degrees: Number(e.target.value) } }); wmSchedulePreview(); }
  catch (err) { toast(err.message, 'err'); }
});
$('#sel-watermark-mark').addEventListener('click', () => {
  const ids = Array.from(S.sel); if (!ids.length) return;
  modal(`<div class="modal-head"><h3>已有水印标记 · ${ids.length} 项</h3></div><div class="modal-body">确认照片上已经有手机或其他软件的水印后，可批量标记。此操作只记录状态，不修改照片。</div><div class="modal-foot"><button class="btn" id="wm-mark-cancel">取消</button><button class="btn" id="wm-mark-no">清除标记</button><button class="btn primary" id="wm-mark-yes">标记已有水印</button></div>`);
  $('#wm-mark-cancel').onclick = closeModal;
  const save = async marked => { try { await api('/api/watermark/mark', { method: 'POST', body: { ids, marked } }); closeModal(); await loadMedia(false); toast('水印状态已更新'); } catch(e) { toast(e.message,'err'); } };
  $('#wm-mark-yes').onclick = () => save(true); $('#wm-mark-no').onclick = () => save(false);
});

$('#src-list').addEventListener('click',async e=>{const id=e.target.dataset.browseExclude;if(!id)return;const input=$('[data-excludes="'+CSS.escape(id)+'"]');const path=await pickFolder();if(path&&input&&input.isConnected)input.value=[input.value.trim(),path].filter(Boolean).join('\n');});

/* ============================ 全屏照片与水印效果 ============================ */
const PV = { gen:0, scale:1, fit:true, focus:null, source:null, ids:[],index:0,kind:'media',navBusy:false,navRequest:0 };
function viewerOpen(title, src, video = false) {
  const root=$('#photo-viewer');
  PV.gen++; if(root.hidden)PV.focus=document.activeElement; PV.fit=true; PV.source=src;
  root.hidden=false; $('#viewer-title').textContent=title; $('#viewer-status').textContent='正在加载…';
  $('#viewer-info').hidden=false;
  const media=document.createElement(video ? 'video' : 'img');
  if (video) { media.controls=true; media.preload='metadata'; }
  else { media.alt=title; media.onload=()=>{viewerResize();$('#viewer-status').textContent='滚轮缩放，放大后可滚动查看细节；Esc 返回';}; }
  media.onerror=()=>{$('#viewer-status').textContent='预览加载失败，请检查文件是否在线或解码器是否可用';};
  media.src=src; $('#viewer-canvas').replaceChildren(media); $('#viewer-stage').focus();
  updateViewerNavigation();
  return PV.gen;
}
function viewerResize() {
  const img=$('#viewer-canvas img'), stage=$('#viewer-stage');
  if (!img || !img.naturalWidth || !stage.clientWidth) return;
  if(PV.fit) PV.scale=Math.min(1, stage.clientWidth/img.naturalWidth, stage.clientHeight/img.naturalHeight);
  PV.scale=Math.max(.02,Math.min(8,PV.scale));
  img.style.width=Math.round(img.naturalWidth*PV.scale)+'px'; img.style.height='auto';
  $('#viewer-scale').textContent=Math.round(PV.scale*100)+'%';
}
function viewerZoom(scale) { PV.fit=false;PV.scale=scale;viewerResize(); }
function closeViewer() {
  PV.navRequest++; PV.navBusy=false;
  PV.gen++; $('#photo-viewer').hidden=true; $('#viewer-canvas').replaceChildren();
  if(document.fullscreenElement===$('#photo-viewer')) document.exitFullscreen().catch(()=>{});
  if(PV.focus && PV.focus.isConnected) PV.focus.focus();
}
function openMediaViewer(a, fileId, reuseScope = false) {
  const file=(a.files || []).find(f=>f.id===fileId) || (a.files || [])[0];
  if(!file) return toast('没有可预览的文件','err');
  if(!reuseScope){PV.kind='media';PV.ids=[a.id];PV.index=0;PV.navRequest++;PV.navBusy=false;}
  const gen=viewerOpen(file.file_name || '照片预览', a.media_type==='video' ? fileUrl(file.id) : imageUrl(file.id)+'&full=1', a.media_type==='video');
  if(!reuseScope){
    const params=filterParams({media_type:a.media_type,sort:S.view==='timeline'?'taken':S.sort,order:S.view==='timeline'?'desc':S.order});
    apiGet('/api/media/ids',params).then(ids=>{if(gen!==PV.gen || $('#photo-viewer').hidden)return;PV.ids=ids.includes(a.id)?ids:[a.id];PV.index=PV.ids.indexOf(a.id);updateViewerNavigation();}).catch(e=>toast(e.message,'err'));
  }
}
async function openWatermarkViewer(id,reuseScope=false) {
  if(!reuseScope){PV.kind='watermark';PV.ids=WM.samples.map(s=>s.item.id);if(!PV.ids.includes(id))PV.ids=[id];PV.index=PV.ids.indexOf(id);PV.wmConfig=JSON.parse(JSON.stringify(wmReadUI()));PV.navRequest++;PV.navBusy=false;}
  const cfg=PV.wmConfig;
  const card=$('[data-wm-full="'+CSS.escape(id)+'"]')?.closest('.wm-card');
  const low=card?.querySelector('img')?.src || '';
  const gen=viewerOpen('水印效果 · 正在加载原尺寸',low);
  $('#viewer-info').hidden=true; $('#viewer-status').textContent='正在渲染原尺寸水印效果…';
  try {
    const r=await api('/api/watermark/preview',{method:'POST',body:{id,config:cfg,max_side:0}});
    if(gen!==PV.gen || $('#photo-viewer').hidden) return;
    $('#viewer-title').textContent='水印效果 · '+(r.file_name || '')+' · '+r.width+'×'+r.height;
    const img=$('#viewer-canvas img'); img.src=r.image;
    img.onload=()=>{viewerResize();$('#viewer-status').textContent=(r.warning || '原尺寸渲染，点击 100% 检查水印细节；Esc 返回调整');};
  } catch(e) {if(gen===PV.gen) $('#viewer-status').textContent=e.message;}
}
$('#viewer-close').onclick=closeViewer;
$('#viewer-info').onclick=closeViewer;
$('#viewer-fit').onclick=()=>{PV.fit=true;viewerResize();};
$('#viewer-actual').onclick=()=>viewerZoom(1);
$('#viewer-in').onclick=()=>viewerZoom(PV.scale*1.25);
$('#viewer-out').onclick=()=>viewerZoom(PV.scale/1.25);
$('#viewer-fullscreen').onclick=()=>{
  const root=$('#photo-viewer');
  if(document.fullscreenElement===root) document.exitFullscreen().catch(e=>toast(e.message,'err'));
  else if(root.requestFullscreen) root.requestFullscreen().catch(()=>toast('系统全屏不可用，可继续在当前窗口查看'));
};
$('#viewer-stage').addEventListener('wheel',e=>{if($('#viewer-canvas img')){e.preventDefault();viewerZoom(PV.scale*(e.deltaY<0?1.15:1/1.15));}},{passive:false});
window.addEventListener('resize',()=>{if(!$('#photo-viewer').hidden) viewerResize();});
document.addEventListener('keydown',e=>{
  if($('#photo-viewer').hidden) return;
  if(e.key==='Escape'){e.preventDefault();e.stopImmediatePropagation();closeViewer();}
  else if(e.key==='+' || e.key==='='){e.preventDefault();viewerZoom(PV.scale*1.25);}
  else if(e.key==='-'){e.preventDefault();viewerZoom(PV.scale/1.25);}
  else if(e.key==='ArrowLeft'){e.preventDefault();viewerNavigate(-1);}
  else if(e.key==='ArrowRight'){e.preventDefault();viewerNavigate(1);}
},true);
$('#wm-previews').addEventListener('click',e=>{
  const button=e.target.closest('[data-wm-full]');
  if(button) openWatermarkViewer(button.dataset.wmFull);
});

/* 轻量旋转留在媒体库，进度结束后就地刷新，不展开编辑面板。 */
async function quickRotate(ids, op) {
  if(EDIT.busy || deletingPhotos) return toast('请等待当前照片操作完成');
  ids=Array.from(new Set(ids)); if(!ids.length) return toast('请先选择照片');
  EDIT.busy=true;
  try {
    const details=await Promise.all(ids.map(id=>apiGet('/api/media/'+id)));
    const photos=details.filter(a=>a.media_type==='image');
    if(!photos.length) return toast('请选择照片，视频不参与旋转');
    const fileIds=photos.map(a=>a.files[0].id), detailFile=details.find(a=>a.id===S.detailId)?.files[0].id;
    EDIT.ids=photos.map(a=>a.id);
    const r=await api('/api/edit/run',{method:'POST',body:{ids:EDIT.ids,op,mode:'overwrite',backup:true}});
    await followLibraryEdit(r.job_id,fileIds,true);
    if(detailFile && S.detailId){const current=await apiGet('/api/media/by-file/'+detailFile);await openDetail(current.id);}
  } catch(e){toast(e.message,'err');}
  finally{EDIT.busy=false;}
}
$('#sel-left').onclick=()=>quickRotate(Array.from(S.sel),'rot270');
$('#sel-right').onclick=()=>quickRotate(Array.from(S.sel),'rot90');
function installDatePickers(root=document) {
  root.querySelectorAll('input[type="date"]').forEach(input=>{
    if(input.dataset.pickerReady) return; input.dataset.pickerReady='1';
    const wrapper=document.createElement('span');wrapper.className='date-picker-field';
    input.before(wrapper); wrapper.appendChild(input);
    const button=document.createElement('button');button.type='button';button.className='btn sm';button.textContent='选择日期';button.setAttribute('aria-label','选择日期');
    button.onclick=()=>{try{if(input.showPicker) input.showPicker();else input.focus();}catch(e){input.focus();}};
    wrapper.appendChild(button);
  });
}
installDatePickers();
document.addEventListener('click',e=>{
  document.querySelectorAll('.library-menu[open]').forEach(menu=>{
    if(!menu.contains(e.target) || (e.target.closest('.actions-panel') && e.target.closest('button'))) menu.open=false;
  });
});

/* ============================ 主题与紧凑照片信息 ============================ */
let detailClickTimer;
function queueDetail(id) {clearTimeout(detailClickTimer);detailClickTimer=setTimeout(()=>openDetail(id),280);}
$('#tl-body').addEventListener('dblclick',e=>{const item=e.target.closest('.tl-item[data-id]');if(item){clearTimeout(detailClickTimer);openDetail(item.dataset.id,true);}});
let themeSaving=false;
function applyTheme(theme) {
  theme=theme==='light'?'light':'dark'; document.documentElement.dataset.theme=theme;
  try{localStorage.setItem('gx.theme',theme);}catch(e){}
  $('#theme-toggle').textContent=theme==='light'?'☾ 深色主题':'☀ 浅色主题';
  $('#theme-toggle').title=theme==='light'?'切换为深色主题':'切换为浅色主题';
  if($('#set-theme')) $('#set-theme').value=theme;
}
async function setTheme(theme) {
  if(themeSaving)return;
  const previous=document.documentElement.dataset.theme;themeSaving=true;applyTheme(theme);$('#theme-toggle').disabled=true;
  try{S.settings=await api('/api/settings',{method:'POST',body:{theme}});}
  catch(e){applyTheme(previous);toast(e.message,'err');}
  finally{themeSaving=false;$('#theme-toggle').disabled=false;}
}
$('#theme-toggle').onclick=()=>setTheme(document.documentElement.dataset.theme==='light'?'dark':'light');
const DETAIL_UI={id:null,tab:'overview'};
function organizeDetail(a) {
  const meta=$('#d-body .meta'), nodes=Array.from(meta.children);
  const tabs=document.createElement('div');tabs.className='detail-tabs';tabs.setAttribute('role','tablist');
  const panels={};
  for(const [key,label] of [['overview','概览'],['date','日期'],['files','文件与拍摄信息']]) {
    const button=document.createElement('button');button.className='btn sm';button.dataset.detailTab=key;button.textContent=label;button.setAttribute('role','tab');
    tabs.appendChild(button);const panel=document.createElement('section');panel.className='detail-tabpanel';panel.dataset.detailPanel=key;panel.setAttribute('role','tabpanel');panels[key]=panel;
  }
  let group='overview';
  for(const node of nodes) {
    if(node.tagName==='H4') {
      const label=node.textContent;
      group=label.startsWith('修改拍摄日期') || label.startsWith('日期候选') ? 'date' : label.startsWith('文件位置') || label.startsWith('实况照片') ? 'files':'overview';
    }
    const key=node.querySelector?.('.k')?.textContent;
    const target=['相机','GPS','SHA-256'].includes(key) || node.classList.contains('row-actions') ? 'files':group;
    panels[target].appendChild(node);
  }
  meta.replaceChildren(tabs,...Object.values(panels));
  if(DETAIL_UI.id!==a.id){DETAIL_UI.id=a.id;DETAIL_UI.tab='overview';}
  const select=key=>{DETAIL_UI.tab=key;for(const k of Object.keys(panels))panels[k].hidden=k!==key;tabs.querySelectorAll('button').forEach(b=>{b.classList.toggle('on',b.dataset.detailTab===key);b.setAttribute('aria-selected',String(b.dataset.detailTab===key));});};
  tabs.onclick=e=>{const b=e.target.closest('[data-detail-tab]');if(b)select(b.dataset.detailTab);};select(DETAIL_UI.tab);
  const status=document.createElement('div');status.className='watermark-status';
  status.innerHTML='<span>已有水印检测</span><button class="btn sm" data-detect-watermark="'+esc(a.id)+'">本地检测…</button>';
  if(a.media_type==='image')panels.overview.appendChild(status);
}

function updateViewerNavigation() {
  $('#viewer-position').textContent=PV.ids.length ? (PV.index+1)+' / '+PV.ids.length : '';
  $('#viewer-prev').disabled=PV.navBusy || PV.index<=0;
  $('#viewer-next').disabled=PV.navBusy || PV.index>=PV.ids.length-1;
}
async function viewerNavigate(delta) {
  const index=PV.index+delta;if(PV.navBusy || index<0 || index>=PV.ids.length)return;
  const request=++PV.navRequest,gen=PV.gen;PV.navBusy=true;updateViewerNavigation();
  try{
    if(PV.kind==='watermark'){PV.index=index;await openWatermarkViewer(PV.ids[index],true);}
    else{
      const a=await apiGet('/api/media/'+PV.ids[index]);
      if(request!==PV.navRequest || gen!==PV.gen || $('#photo-viewer').hidden)return;
      PV.index=index;S.detailId=a.id;renderDetail(a);openMediaViewer(a,null,true);
    }
  }catch(e){if(request===PV.navRequest)toast(e.message,'err');}
  finally{if(request===PV.navRequest){PV.navBusy=false;updateViewerNavigation();}}
}
$('#viewer-prev').onclick=()=>viewerNavigate(-1);
$('#viewer-next').onclick=()=>viewerNavigate(1);

/* ============================ 本地已有水印检测 ============================ */
async function openWatermarkDetection(ids) {
  ids=Array.from(new Set(ids));if(!ids.length)return toast('请先在媒体库选择照片');
  modal('<div class="modal-head"><h3>本地已有水印检测</h3></div><div class="modal-body"><p class="tplhint">利用边缘区域、局部对比度和文字行排列进行启发式判断，不联网、不使用 AI。特征分不是概率；招牌、树叶等可能误判，模糊或中央水印也可能漏检。只会在你确认后记录已有水印。</p><p id="detect-progress">正在检测…</p><div id="detect-list"></div></div><div class="modal-foot"><button class="btn" id="detect-close">关闭</button><button class="btn primary" id="detect-confirm">将选中项标记为已有水印</button></div>');
  const list=$('#detect-list'),progress=$('#detect-progress'),save=$('#detect-confirm');let done=0,suspected=0;save.disabled=true;
  $('#detect-close').onclick=closeModal;
  save.onclick=async()=>{
    const selected=$$('input[data-detect-id]:checked',list).map(e=>e.dataset.detectId);if(!selected.length)return toast('请勾选确认有水印的照片');
    save.disabled=true;
    try{await api('/api/watermark/mark',{method:'POST',body:{ids:selected,marked:true}});closeModal();await loadMedia(false);if(S.detailId)openDetail(S.detailId);if(S.view==='watermark')wmSchedulePreview();toast('已记录 '+selected.length+' 张照片的水印状态','ok');}
    catch(e){toast(e.message,'err');if(save.isConnected)save.disabled=false;}
  };
  for(const id of ids){
    if(!list.isConnected)break;
    const row=document.createElement('div');row.className='detection-row';row.textContent='正在检测…';list.appendChild(row);
    try{
      const r=await api('/api/watermark/detect',{method:'POST',body:{id}});if(!list.isConnected)break;
      const title=r.known?'已有确认记录':r.suspected?'疑似已有水印':'未检出明显水印';
      if(r.suspected)suspected++;
      row.innerHTML='<img loading="lazy" src="'+thumbUrl({id},256)+'" alt=""><div><b>'+esc(r.file_name || id)+'</b><p>'+title+(r.known?'':' · 特征分 '+r.score+'/100')+'</p><small>'+esc(r.reason)+'</small></div><label><input type="checkbox" data-detect-id="'+esc(id)+'" '+(r.suspected?'checked ':'')+(r.known?'disabled ':'')+'> 确认</label>';
      save.disabled=false;
    }catch(e){row.textContent=e.message;}
    done++;progress.textContent='已检测 '+done+' / '+ids.length+' · 疑似 '+suspected+' 张';
  }
}
$('#sel-watermark-detect').onclick=()=>openWatermarkDetection(Array.from(S.sel));
document.addEventListener('click',async e=>{
  const detect=e.target.closest('[data-detect-watermark]');if(detect){openWatermarkDetection([detect.dataset.detectWatermark]);return;}
  const confirm=e.target.closest('[data-confirm-watermark]');if(confirm){
    confirm.disabled=true;
    try{await api('/api/watermark/mark',{method:'POST',body:{ids:[confirm.dataset.confirmWatermark],marked:true}});wmSchedulePreview();toast('已确认为已有水印，后续按设置跳过','ok');}
    catch(err){confirm.disabled=false;toast(err.message,'err');}
  }
});
$('#wm-suspect-skip').onchange=()=>{wmReadUI();wmSchedulePreview();};

function syncReviewSelection(){
  $$('#review-list input[data-pick]').forEach(input=>{input.checked=S.reviewSel.has(input.dataset.pick);input.closest('.rv-item').classList.toggle('sel',input.checked);});updateReviewBar();
}
$('#review-page-all').onclick=()=>{REVIEW.items.forEach(a=>S.reviewSel.add(a.id));syncReviewSelection();};
$('#review-clear').onclick=()=>{S.reviewSel.clear();syncReviewSelection();};
$('#review-all').onclick=async()=>{
  const mode=S.reviewMode;
  try{const ids=await apiGet('/api/review/ids',{mode});if(mode!==S.reviewMode || S.view!=='review')return;S.reviewSel=new Set(ids);syncReviewSelection();}
  catch(e){toast(e.message,'err');}
};
async function confirmReviewSource(source){
  if(REVIEW.processing)return;const ids=Array.from(S.reviewSel);if(!ids.length)return toast('请先选择待整理项目');
  const label=source==='filesystem'?'扫描记录的文件修改时间':'各项目当前显示的日期';
  confirmDialog('批量确认 '+ids.length+' 项日期',esc('每个项目分别采用'+label+'，不会把所有照片设成同一时间。只修改本地索引，不改照片或 EXIF。确认后这些项目会移出待确认列表。'),async()=>{
    if(REVIEW.processing)return;REVIEW.processing=true;updateReviewBar();
    try{
      const r=await api('/api/media/batch-date-source',{method:'POST',body:{ids,source}});
      await Promise.all([loadReview(),loadState(),loadFacets()]);if(S.detailId)openDetail(S.detailId);
      toast('已确认 '+r.updated+' 项'+(r.skipped?'，跳过 '+r.skipped+' 项缺少可用日期的项目':''),'ok');
    }catch(e){toast(e.message,'err');}
    finally{REVIEW.processing=false;updateReviewBar();}
  });
}
$('#review-filesystem').onclick=()=>confirmReviewSource('filesystem');
$('#review-recommended').onclick=()=>confirmReviewSource('recommended');
$('#similar-list').addEventListener('dblclick',e=>{if(e.target.closest('button,input,.sim-card-actions'))return;const item=e.target.closest('[data-open]');if(item){clearTimeout(detailClickTimer);openDetail(item.dataset.open,true);}});

$('#wm-video').addEventListener('change',async()=>{wmReadUI();await wmUpdateScope();await wmPickSamples();wmSchedulePreview();});
