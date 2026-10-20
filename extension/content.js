// Remembers what was under the cursor at the last right-click, so the
// background worker can add the best version of it. Chrome's own srcUrl is
// whatever size the page happens to be showing, and it's absent when an
// overlay (Instagram) or a CSS background holds the image.

let last = null;

addEventListener('contextmenu', e => {
  try { last = pick(e.clientX, e.clientY); } catch { last = null; }
}, true);

chrome.runtime.onMessage.addListener((m, _sender, reply) => {
  if (m?.mood === 'last') { reply(last); return; }
  if (m?.mood === 'blob') { dataUrl(m.url).then(reply, () => reply(null)); return true; }
  if (m?.mood === 'toast') { toast(m.text, m.ok); reply(true); }
});

const MIN = 32; // smaller than this under the cursor is an icon, not the image you meant

/** Topmost image at a point, looking through overlays: an <img>, a video poster, or a CSS background. */
function pick(x, y) {
  const stack = document.elementsFromPoint(x, y);
  for (const el of stack) {
    const r = el.getBoundingClientRect();
    if (r.width < MIN || r.height < MIN) continue;
    const found = el instanceof HTMLImageElement ? fromImg(el)
      : el instanceof HTMLVideoElement && el.poster ? { url: el.poster }
      : el instanceof SVGImageElement ? { url: abs(el.href.baseVal) }
      : null;
    if (found?.url) return { ...found, w: Math.round(r.width), h: Math.round(r.height), alt: el.alt || el.title || '', page: resultPage(el) };
  }
  // no <img> anywhere in the stack: fall back to the topmost background image
  for (const el of stack) {
    const r = el.getBoundingClientRect();
    if (r.width < MIN || r.height < MIN || el === document.documentElement || el === document.body) continue;
    const m = getComputedStyle(el).backgroundImage.match(/url\(["']?(.*?)["']?\)/);
    if (m && !m[1].startsWith('data:image/svg')) return { url: abs(m[1]), w: Math.round(r.width), h: Math.round(r.height), alt: el.getAttribute('aria-label') || '', page: resultPage(el) };
  }
  return null;
}

/** The largest candidate an <img> (and its <picture>) offers. */
function fromImg(img) {
  const cands = parseSrcset(img.getAttribute('srcset') || '');
  if (img.parentElement instanceof HTMLPictureElement) {
    for (const s of img.parentElement.querySelectorAll('source')) {
      // art direction: only sources whose media matches what's showing now
      if (s.media && !matchMedia(s.media).matches) continue;
      cands.push(...parseSrcset(s.getAttribute('srcset') || ''));
    }
  }
  const best = cands.sort((a, b) => b.w - a.w || b.x - a.x)[0];
  // with only density descriptors, a 1x candidate says nothing about size; take what's loaded
  const url = best && (best.w || best.x > 1) ? best.url : img.currentSrc || img.src || best?.url;
  return { url: url && abs(url) };
}

/** srcset per the HTML spec's shape: a URL run, then descriptors up to a comma. */
function parseSrcset(s) {
  const out = [];
  let i = 0;
  while (i < s.length) {
    while (i < s.length && /[\s,]/.test(s[i])) i++;
    let j = i;
    while (j < s.length && !/\s/.test(s[j])) j++;
    let url = s.slice(i, j), desc = '';
    i = j;
    if (url.endsWith(',')) url = url.replace(/,+$/, '');
    else {
      let k = s.indexOf(',', i);
      if (k < 0) k = s.length;
      desc = s.slice(i, k).trim();
      i = k + 1;
    }
    if (!url) continue;
    const w = parseFloat(desc.match(/([\d.]+)w/)?.[1]) || 0;
    const x = parseFloat(desc.match(/([\d.]+)x/)?.[1]) || (w ? 0 : 1);
    out.push({ url, w, x });
  }
  return out;
}

const abs = u => new URL(u, document.baseURI).href;

const isGoogle = host => /(^|\.)google\.[a-z.]+$/.test(host);

/** On Google Images the page you're on is a search; the result's own site is
 *  the attribution worth keeping. Grid tiles carry it as data-lpage; the
 *  preview panel wraps its image in a "Visit" link. Empty anywhere else, so
 *  the background worker falls back to the page URL. */
function resultPage(el) {
  if (!isGoogle(location.hostname)) return '';
  const lp = el.closest('[data-lpage]')?.getAttribute('data-lpage');
  if (lp) return lp;
  const a = el.closest('a[href]');
  if (!a) return '';
  const u = new URL(a.href);
  if (u.pathname === '/url') return u.searchParams.get('q') || u.searchParams.get('url') || ''; // redirect wrapper
  return /^https?:$/.test(u.protocol) && !isGoogle(u.hostname) ? u.href : '';
}

/** blob: URLs only resolve inside the page that made them; ship the bytes back as a data URL. */
async function dataUrl(url) {
  const b = await (await fetch(url)).blob();
  return await new Promise((ok, no) => {
    const r = new FileReader();
    r.onload = () => ok(r.result); r.onerror = no;
    r.readAsDataURL(b);
  });
}

function toast(text, ok) {
  if (window !== top) return; // one toast per tab, from the top frame
  const host = document.createElement('div');
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>
    div { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; max-width: 360px;
      font: 500 13px/1.35 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif;
      padding: 9px 13px; border-radius: 8px; box-shadow: 0 4px 16px rgba(0,0,0,.25);
      background: #1b1b19; color: #fbfbf9; transition: opacity .3s; }
    div.bad { background: #8a2b1a; }
  </style><div class="${ok ? '' : 'bad'}"></div>`;
  root.querySelector('div').textContent = text;
  document.documentElement.append(host);
  setTimeout(() => { root.querySelector('div').style.opacity = 0; }, 2400);
  setTimeout(() => host.remove(), 2800);
}
