//! Pull an image from a URL. A page URL is welcome too: we follow its
//! og:image and keep the *page* as the source, which is the attribution
//! you actually want.

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::io::Read;
use std::time::Duration;

pub struct Fetched {
    pub bytes: Vec<u8>,
    pub source: String,
    pub title: String,
}

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const MAX: u64 = 60 << 20;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(25))
        .user_agent(UA)
        .redirects(8)
        .build()
}

fn get(a: &ureq::Agent, url: &str) -> Result<(String, Vec<u8>, String)> {
    let req = |ua: &str| {
        a.get(url)
            .set("User-Agent", ua)
            .set("Accept", "image/avif,image/webp,image/png,image/*,text/html;q=0.9,*/*;q=0.8")
            .call()
    };
    // Rate-limited (Wikimedia does this to rapid requests): wait, retry once
    // identifying ourselves honestly.
    let r = match req(UA) {
        Err(ureq::Error::Status(429, _)) => {
            std::thread::sleep(Duration::from_millis(1500));
            req("mood/0.1 (personal moodboard tool)")
        }
        other => other,
    }
    .map_err(|e| anyhow!("fetch failed: {e}"))?;
    let final_url = r.get_url().to_string();
    let ct = r.content_type().to_ascii_lowercase();
    let mut buf = Vec::new();
    r.into_reader().take(MAX).read_to_end(&mut buf)?;
    Ok((ct, buf, final_url))
}

pub fn fetch(url: &str) -> Result<Fetched> {
    let a = agent();
    let (ct, body, final_url) = get(&a, url)?;
    if ct.starts_with("image/") || !looks_like_html(&ct, &body) {
        return Ok(Fetched { bytes: body, source: url.into(), title: String::new() });
    }
    let html = String::from_utf8_lossy(&body);
    let img = meta(&html, &["og:image:secure_url", "og:image", "twitter:image", "twitter:image:src"])
        .ok_or_else(|| anyhow!("that page has no og:image to grab — copy the image itself instead"))?;
    let base = url::Url::parse(&final_url)?;
    let img_url = base.join(&img)?.to_string();
    let (ict, bytes, _) = get(&a, &img_url)?;
    if looks_like_html(&ict, &bytes) {
        bail!("og:image pointed at a page, not an image");
    }
    let title = meta(&html, &["og:title", "twitter:title"])
        .or_else(|| {
            Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
                .unwrap()
                .captures(&html)
                .map(|c| unescape(c[1].trim()))
        })
        .unwrap_or_default();
    let title = strip_site(&title, meta(&html, &["og:site_name"]).as_deref());
    Ok(Fetched { bytes, source: url.into(), title })
}

fn looks_like_html(ct: &str, body: &[u8]) -> bool {
    if ct.contains("html") {
        return true;
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]).to_ascii_lowercase();
    head.trim_start().starts_with("<!doctype html") || head.trim_start().starts_with("<html")
}

fn meta(html: &str, keys: &[&str]) -> Option<String> {
    let tag = Regex::new(r"(?is)<meta\s[^>]*>").unwrap();
    let attr = Regex::new(r#"(?is)([a-z:_-]+)\s*=\s*("([^"]*)"|'([^']*)')"#).unwrap();
    for key in keys {
        for m in tag.find_iter(html) {
            let mut name = None;
            let mut content = None;
            for c in attr.captures_iter(m.as_str()) {
                let k = c[1].to_ascii_lowercase();
                let v = c.get(3).or(c.get(4)).map(|v| v.as_str()).unwrap_or("");
                match k.as_str() {
                    "property" | "name" => name = Some(v.to_ascii_lowercase()),
                    "content" => content = Some(v.to_string()),
                    _ => {}
                }
            }
            if name.as_deref() == Some(key) {
                if let Some(c) = content.filter(|c| !c.is_empty()) {
                    return Some(unescape(&c));
                }
            }
        }
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// "Idea 041 - Design Reviewed" -> "Idea 041"
fn strip_site(title: &str, site: Option<&str>) -> String {
    let mut t = title.trim().to_string();
    for s in site.into_iter().chain(["Wikipedia", "Wikimedia Commons"]) {
        for sep in [" - ", " | ", " — ", " – ", " · "] {
            if let Some(rest) = t.strip_suffix(&format!("{sep}{s}")) {
                t = rest.trim().to_string();
            }
        }
    }
    t
}
