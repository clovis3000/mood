// Right-click → Add to mood → <board>. The browser fetches the image (with
// your cookies, so logged-in sites work) and uploads the bytes to the local
// `mood serve`, the same endpoint a paste on the canvas uses.

const BASE = 'http://localhost:7777';
const CONTEXTS = ['image', 'page', 'link', 'frame', 'video'];

// ---------- menu ----------
// Chrome can't change a menu as it opens, so the board list is rebuilt on
// startup, every minute, and after each add.

let building = Promise.resolve();
function buildMenu() {
  building = building.then(rebuild, rebuild);
  return building;
}

async function rebuild() {
  await chrome.contextMenus.removeAll();
  const item = props => chrome.contextMenus.create({ contexts: CONTEXTS, ...props }, () => void chrome.runtime.lastError);
  let d;
  try { d = await api('/api/boards'); }
  catch {
    item({ id: 'down', title: 'mood isn’t running (mood serve)', enabled: false });
    return;
  }
  item({ id: 'root', title: 'Add to mood' });
  const names = d.boards.map(b => b.name);
  // the board open in the canvas first, the rest alphabetical below a line
  if (names.includes(d.current)) item({ id: 'b:' + d.current, parentId: 'root', title: d.current });
  const rest = names.filter(n => n !== d.current);
  if (rest.length && names.includes(d.current)) item({ id: 'sep', parentId: 'root', type: 'separator' });
  for (const n of rest) item({ id: 'b:' + n, parentId: 'root', title: n });
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.alarms.create('menu', { periodInMinutes: 1 });
  buildMenu();
});
chrome.runtime.onStartup.addListener(buildMenu);
chrome.alarms.onAlarm.addListener(a => a.name === 'menu' && buildMenu());

// ---------- add ----------

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (!String(info.menuItemId).startsWith('b:')) return;
  const board = String(info.menuItemId).slice(2);
  const frame = { frameId: info.frameId ?? 0 };
  badge(tab, '…', '#666');
  try {
    // the content script saw the right-click: biggest srcset candidate, or the image under an overlay
    const seen = await chrome.tabs.sendMessage(tab.id, { mood: 'last' }, frame).catch(() => null);
    const url = seen?.url || info.srcUrl;
    if (!url) throw new Error('no image under the cursor');
    const it = await add(board, url, {
      source: seen?.page || info.pageUrl || tab.url || '',
      title: seen?.alt || tab.title || '',
      hint: seen,
      tab, frame,
    });
    badge(tab, '✓', '#2f7d4a');
    say(tab, `Added to ${board} · #${it.id}`, true);
  } catch (e) {
    badge(tab, '!', '#b3261e', e.message);
    say(tab, `mood: ${e.message}`, false);
  }
  buildMenu();
});

/** Upload the browser's copy; if the browser can't get usable bytes, let the server try the URL. */
async function add(board, url, o) {
  const b = `/api/b/${encodeURIComponent(board)}`;
  let bytes = null;
  try { bytes = await grab(url, o.tab, o.frame); } catch {}
  if (bytes) {
    const q = new URLSearchParams({ source: o.source, title: o.title });
    // svg has no pixel size to read; the size it was showing is the best guess
    if (o.hint?.w && o.hint?.h) { q.set('iw', o.hint.w); q.set('ih', o.hint.h); }
    try { return await api(`${b}/items?${q}`, { method: 'POST', body: bytes }); }
    catch (e) { if (!/look like an image/.test(e.message)) throw e; } // got a login page or similar
  }
  if (!/^https?:/.test(url)) throw new Error('couldn’t read that image');
  const it = await api(`${b}/fetch`, json('POST', { url }));
  // the server records the image URL as the source; the page is the attribution worth keeping
  return await api(`${b}/items/${it.id}`, json('PATCH', { source: o.source, title: o.title || it.title }));
}

async function grab(url, tab, frame) {
  if (url.startsWith('blob:')) {
    const d = await chrome.tabs.sendMessage(tab.id, { mood: 'blob', url }, frame);
    if (!d) throw new Error('blob gone');
    url = d;
  }
  const r = await fetch(url, { credentials: 'include' });
  if (!r.ok) throw new Error(`${r.status}`);
  const blob = await r.blob();
  if (!blob.size) throw new Error('empty');
  return blob;
}

// ---------- plumbing ----------

async function api(path, opts = {}) {
  let r;
  // the server refuses writes without x-mood, which ordinary web pages can't send
  try { r = await fetch(BASE + path, { ...opts, headers: { ...opts.headers, 'x-mood': '1' } }); }
  catch { throw new Error('mood isn’t running (start it with `mood serve`)'); }
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  return r.json();
}
const json = (method, body) => ({ method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });

function badge(tab, text, color, title = 'mood') {
  if (!tab?.id) return;
  chrome.action.setBadgeText({ tabId: tab.id, text }).catch(() => {});
  chrome.action.setBadgeBackgroundColor({ tabId: tab.id, color }).catch(() => {});
  chrome.action.setTitle({ tabId: tab.id, title }).catch(() => {});
  if (text !== '…') setTimeout(() => chrome.action.setBadgeText({ tabId: tab.id, text: '' }).catch(() => {}), 4000);
}

function say(tab, text, ok) {
  chrome.tabs.sendMessage(tab.id, { mood: 'toast', text, ok }, { frameId: 0 }).catch(() => {});
}
