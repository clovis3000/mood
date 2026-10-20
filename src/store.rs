//! Boards live on disk as plain files so a human (browser), an AI (CLI) and
//! the filesystem itself all see the same thing:
//!
//!   $MOOD_HOME/<board>/board.json        board-level notes + id counter
//!   $MOOD_HOME/<board>/items/<id>.<ext>  the image, untouched
//!   $MOOD_HOME/<board>/items/<id>.json   position, source, title, notes
//!   $MOOD_HOME/<board>/.trash/           deleted items (restorable)
//!   $MOOD_HOME/<board>/.thumbs/<size>/   sized copies for the canvas (disposable)
//!   $MOOD_HOME/<board>/.views/           render cache (disposable)

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Side of one grid cell in world units. Shared with assets/index.html.
pub const CELL: f64 = 1000.0;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: String,
    pub by: String,
    #[serde(default = "default_kind")]
    pub kind: String, // note | like | dislike
    pub text: String,
    pub at: String,
}
fn default_kind() -> String {
    "note".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub file: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub iw: u32,
    pub ih: u32,
    #[serde(default)]
    pub z: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub added: String,
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub notes: Vec<Note>,
    /// Lewis's reaction, 1–9: 1 repels, 5 indifferent, 9 essential.
    /// Bipolar on purpose — below 5 steers away, above 5 steers toward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated_at: Option<String>,
}

pub const RATING_WORDS: [&str; 10] =
    ["", "repels", "dislike", "off", "meh", "indifferent", "ok", "like", "love", "essential"];

pub fn rating_label(r: Option<u8>) -> String {
    match r {
        Some(n @ 1..=9) => format!("{n}/9 {}", RATING_WORDS[n as usize]),
        _ => "unrated".into(),
    }
}

impl Item {
    pub fn h(&self) -> f64 {
        self.w * self.ih.max(1) as f64 / self.iw.max(1) as f64
    }
    pub fn cell(&self) -> String {
        cell_label(
            ((self.x + self.w / 2.0) / CELL).floor() as i64,
            ((self.y + self.h() / 2.0) / CELL).floor() as i64,
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BoardMeta {
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub next_id: u32,
    #[serde(default)]
    pub notes: Vec<Note>,
}

pub fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn note_id() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", (n / 1000) as u64 & 0xff_ffff_ffff)
}

/// "+ love this" -> like, "- too glossy" -> dislike, else note.
pub fn parse_note_text(raw: &str) -> (String, String) {
    let t = raw.trim();
    for (prefix, kind) in [("+", "like"), ("-", "dislike"), ("−", "dislike"), ("–", "dislike")] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return (kind.into(), rest.trim().into());
        }
    }
    ("note".into(), t.into())
}

// ---------- grid labels (chess-style, anchored at world origin) ----------

pub fn col_label(c: i64) -> String {
    if c < 0 {
        return format!("-{}", col_label(-c - 1));
    }
    let mut n = c + 1;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    s.iter().rev().collect()
}

pub fn row_label(r: i64) -> String {
    if r >= 0 {
        (r + 1).to_string()
    } else {
        r.to_string()
    }
}

pub fn cell_label(c: i64, r: i64) -> String {
    format!("{}{}", col_label(c), row_label(r))
}

/// "C4" -> (2, 3); "-A2" -> (-1, 1); "B-1" -> (1, -1)
pub fn parse_cell(s: &str) -> Result<(i64, i64)> {
    let s = s.trim().to_ascii_uppercase();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.as_str()),
    };
    let letters: String = rest.chars().take_while(|c| c.is_ascii_uppercase()).collect();
    let row = &rest[letters.len()..];
    if letters.is_empty() || row.is_empty() {
        bail!("not a cell: {s} (expected e.g. C4)");
    }
    let mut c: i64 = 0;
    for ch in letters.chars() {
        c = c * 26 + (ch as i64 - 'A' as i64 + 1);
    }
    c -= 1;
    if neg {
        c = -c - 1;
    }
    let r: i64 = row.parse().map_err(|_| anyhow!("bad row in {s}"))?;
    if r == 0 {
        bail!("rows start at 1 (or -1 above the origin)");
    }
    Ok((c, if r > 0 { r - 1 } else { r }))
}

// ---------- notes target ----------

#[derive(Clone, Copy, Debug)]
pub enum Target {
    Board,
    Item(u32),
}

impl FromStr for Target {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.eq_ignore_ascii_case("board") {
            return Ok(Target::Board);
        }
        Ok(Target::Item(
            s.parse().map_err(|_| anyhow!("expected an item id or 'board', got {s}"))?,
        ))
    }
}

// ---------- store ----------

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

fn valid_board_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Store {
    pub fn open() -> Result<Store> {
        let root = match std::env::var_os("MOOD_HOME") {
            Some(p) => PathBuf::from(p),
            None => PathBuf::from(std::env::var("HOME").context("HOME unset")?).join("moodboards"),
        };
        fs::create_dir_all(&root)?;
        Ok(Store { root })
    }

    pub fn boards(&self) -> Result<Vec<String>> {
        let mut v: Vec<String> = fs::read_dir(&self.root)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("board.json").exists())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_board_name(n))
            .collect();
        v.sort();
        Ok(v)
    }

    pub fn current(&self) -> Result<String> {
        if let Ok(s) = fs::read_to_string(self.root.join(".current")) {
            let s = s.trim().to_string();
            if self.root.join(&s).join("board.json").exists() {
                return Ok(s);
            }
        }
        if let Some(first) = self.boards()?.into_iter().next() {
            return Ok(first);
        }
        self.create("inbox")?;
        Ok("inbox".into())
    }

    pub fn set_current(&self, name: &str) -> Result<()> {
        self.board(name)?;
        fs::write(self.root.join(".current"), name)?;
        Ok(())
    }

    pub fn create(&self, name: &str) -> Result<Board> {
        if !valid_board_name(name) {
            bail!("board names are letters, digits, - and _");
        }
        let dir = self.root.join(name);
        if dir.join("board.json").exists() {
            bail!("board {name} already exists");
        }
        fs::create_dir_all(dir.join("items"))?;
        let b = Board { name: name.into(), dir };
        b.save_meta(&BoardMeta { created: now(), next_id: 1, notes: vec![] })?;
        Ok(b)
    }

    pub fn board(&self, name: &str) -> Result<Board> {
        if !valid_board_name(name) {
            bail!("bad board name: {name}");
        }
        let dir = self.root.join(name);
        if !dir.join("board.json").exists() {
            bail!("no board named {name} (try `mood boards`)");
        }
        Ok(Board { name: name.into(), dir })
    }
}

pub struct Board {
    pub name: String,
    pub dir: PathBuf,
}

/// Where a new image should land.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// centred on this point (paste-at-cursor)
    Center(f64, f64),
    /// first free spot scanning right/down from here
    Near(f64, f64),
    /// nobody chose a spot (the Chrome extension, `mood add` without --at):
    /// beside the newest item's group, see `beside_recent`
    Beside,
}

pub struct AddOpts {
    pub place: Place,
    pub w: Option<f64>,
    pub title: String,
    pub source: String,
    pub by: String,
    /// dimension hints for formats we can't measure (svg)
    pub hint: Option<(u32, u32)>,
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp~");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Sniff the format from magic bytes. Returns the extension we store under.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let b = bytes;
    if b.starts_with(b"\x89PNG") {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.len() > 12 && &b[4..8] == b"ftyp" {
        match &b[8..12] {
            b"avif" | b"avis" => Some("avif"),
            _ => Some("heic"),
        }
    } else if b.starts_with(b"BM") {
        Some("bmp")
    } else if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        Some("tiff")
    } else {
        let head = String::from_utf8_lossy(&b[..b.len().min(2048)]).to_ascii_lowercase();
        if head.contains("<svg") {
            Some("svg")
        } else {
            None
        }
    }
}

/// Visually-even default width: every image gets roughly the same area.
pub fn default_width(iw: u32, ih: u32) -> f64 {
    let aspect = iw.max(1) as f64 / ih.max(1) as f64;
    (340.0 * aspect.sqrt()).clamp(180.0, 640.0).round()
}

impl Board {
    pub fn items_dir(&self) -> PathBuf {
        self.dir.join("items")
    }

    pub fn meta(&self) -> Result<BoardMeta> {
        let s = fs::read_to_string(self.dir.join("board.json"))?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save_meta(&self, m: &BoardMeta) -> Result<()> {
        write_atomic(&self.dir.join("board.json"), &serde_json::to_vec_pretty(m)?)
    }

    pub fn items(&self) -> Result<Vec<Item>> {
        let mut v = Vec::new();
        for e in fs::read_dir(self.items_dir())? {
            let p = e?.path();
            if p.extension().and_then(|s| s.to_str()) == Some("json") {
                match fs::read_to_string(&p)
                    .map_err(anyhow::Error::from)
                    .and_then(|s| serde_json::from_str::<Item>(&s).map_err(Into::into))
                {
                    Ok(it) => v.push(it),
                    Err(err) => eprintln!("mood: skipping {}: {err}", p.display()),
                }
            }
        }
        v.sort_by_key(|i| (i.z, i.id));
        Ok(v)
    }

    pub fn item(&self, id: u32) -> Result<Item> {
        let p = self.items_dir().join(format!("{id}.json"));
        let s = fs::read_to_string(&p).with_context(|| format!("no item #{id} on {}", self.name))?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save_item(&self, it: &Item) -> Result<()> {
        write_atomic(
            &self.items_dir().join(format!("{}.json", it.id)),
            &serde_json::to_vec_pretty(it)?,
        )
    }

    pub fn file_path(&self, file: &str) -> Result<PathBuf> {
        if file.contains('/') || file.contains("..") || file.starts_with('.') {
            bail!("bad file name");
        }
        Ok(self.items_dir().join(file))
    }

    fn take_id(&self) -> Result<u32> {
        let mut m = self.meta()?;
        let max_seen = self.items()?.iter().map(|i| i.id).max().unwrap_or(0);
        let trash_max = fs::read_dir(self.dir.join(".trash"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .and_then(|s| s.parse::<u32>().ok())
                    })
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let id = m.next_id.max(max_seen + 1).max(trash_max + 1);
        m.next_id = id + 1;
        self.save_meta(&m)?;
        Ok(id)
    }

    pub fn add_image(&self, bytes: &[u8], o: AddOpts) -> Result<Item> {
        let mut ext = sniff(bytes).ok_or_else(|| anyhow!("that doesn't look like an image"))?;
        let mut owned: Option<Vec<u8>> = None;
        // Browsers can't show heic/tiff/bmp: convert to png with macOS `sips`.
        if matches!(ext, "heic" | "tiff" | "bmp") {
            owned = Some(convert_with_sips(bytes, ext)?);
            ext = "png";
        }
        let bytes = owned.as_deref().unwrap_or(bytes);

        let (iw, ih) = if ext == "svg" {
            o.hint.unwrap_or((600, 600))
        } else {
            let s = imagesize::blob_size(bytes).context("couldn't read image dimensions")?;
            (s.width as u32, s.height as u32)
        };

        let id = self.take_id()?;
        let file = format!("{id}.{ext}");
        fs::write(self.items_dir().join(&file), bytes)?;

        let w = o.w.unwrap_or_else(|| default_width(iw, ih));
        let h = w * ih as f64 / iw.max(1) as f64;
        let existing = self.items()?;
        let (x, y) = match o.place {
            Place::Center(x, y) => (x - w / 2.0, y - h / 2.0),
            Place::Near(x, y) => find_free(&existing, w, h, x, y),
            Place::Beside => beside_recent(&existing, w, h),
        };
        let z = existing.iter().map(|i| i.z).max().unwrap_or(0) + 1;
        let it = Item {
            id,
            file,
            x: x.round(),
            y: y.round(),
            w,
            iw,
            ih,
            z,
            title: o.title,
            source: o.source,
            added: now(),
            added_by: o.by,
            notes: vec![],
            rating: None,
            rated_at: None,
        };
        self.save_item(&it)?;
        Ok(it)
    }

    pub fn remove(&self, id: u32) -> Result<()> {
        let it = self.item(id)?;
        let trash = self.dir.join(".trash");
        fs::create_dir_all(&trash)?;
        fs::rename(self.items_dir().join(&it.file), trash.join(&it.file))?;
        fs::rename(
            self.items_dir().join(format!("{id}.json")),
            trash.join(format!("{id}.json")),
        )?;
        Ok(())
    }

    pub fn restore(&self, id: u32) -> Result<Item> {
        let trash = self.dir.join(".trash");
        let s = fs::read_to_string(trash.join(format!("{id}.json")))
            .with_context(|| format!("#{id} isn't in the trash"))?;
        let it: Item = serde_json::from_str(&s)?;
        fs::rename(trash.join(&it.file), self.items_dir().join(&it.file))?;
        fs::rename(
            trash.join(format!("{id}.json")),
            self.items_dir().join(format!("{id}.json")),
        )?;
        Ok(it)
    }

    pub fn add_note(&self, t: Target, by: &str, kind: &str, text: &str) -> Result<Note> {
        if text.trim().is_empty() {
            bail!("empty note");
        }
        let kind = match kind {
            "like" | "dislike" | "note" => kind,
            _ => bail!("kind must be note, like or dislike"),
        };
        let n = Note {
            id: note_id(),
            by: by.into(),
            kind: kind.into(),
            text: text.trim().into(),
            at: now(),
        };
        match t {
            Target::Board => {
                let mut m = self.meta()?;
                m.notes.push(n.clone());
                self.save_meta(&m)?;
            }
            Target::Item(id) => {
                let mut it = self.item(id)?;
                it.notes.push(n.clone());
                self.save_item(&it)?;
            }
        }
        Ok(n)
    }

    pub fn remove_note(&self, t: Target, nid: &str) -> Result<()> {
        let drop = |v: &mut Vec<Note>| -> Result<()> {
            let before = v.len();
            v.retain(|n| n.id != nid);
            if v.len() == before {
                bail!("no note {nid}");
            }
            Ok(())
        };
        match t {
            Target::Board => {
                let mut m = self.meta()?;
                drop(&mut m.notes)?;
                self.save_meta(&m)
            }
            Target::Item(id) => {
                let mut it = self.item(id)?;
                drop(&mut it.notes)?;
                self.save_item(&it)
            }
        }
    }

    /// Cheap change-detector for polling: hash of names, sizes and mtimes.
    pub fn rev(&self) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut stamp = |p: &Path| {
            if let Ok(m) = fs::metadata(p) {
                p.file_name().hash(&mut h);
                m.len().hash(&mut h);
                m.modified().ok().hash(&mut h);
            }
        };
        stamp(&self.dir.join("board.json"));
        if let Ok(rd) = fs::read_dir(self.items_dir()) {
            let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                if p.extension().and_then(|s| s.to_str()) == Some("json") {
                    stamp(&p);
                }
            }
        }
        format!("{:x}", h.finish())
    }

    /// Newest item-sidecar mtime (unix seconds): when the board last changed, for ordering the home page.
    pub fn modified(&self) -> u64 {
        let stamp = |p: &Path| {
            fs::metadata(p).and_then(|m| m.modified()).ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs())
        };
        let newest = fs::read_dir(self.items_dir()).into_iter().flatten().flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .map(|e| stamp(&e.path()))
            .max()
            .unwrap_or(0);
        newest.max(stamp(&self.dir.join("board.json")))
    }

    /// Cached copy of `file` at most `size` px on its long side, made on a miss
    /// along with every smaller tier so one decode of the original serves all
    /// later zooms. None means serve the original: it's already that small, or
    /// it's a format we pass through whole (svg, gif, avif).
    pub fn tier(&self, file: &str, size: u32) -> Result<Option<PathBuf>> {
        if !TIERS.contains(&size) {
            bail!("sizes are {TIERS:?}");
        }
        let src = self.file_path(file)?;
        if !tierable(file) {
            return Ok(None);
        }
        let dim = imagesize::size(&src).with_context(|| format!("can't read {file}"))?;
        let long = dim.width.max(dim.height) as u32;
        if long <= size {
            return Ok(None);
        }
        let cached = |t: u32| -> Option<PathBuf> {
            let src_m = fs::metadata(&src).and_then(|m| m.modified()).ok()?;
            ["jpg", "png"].into_iter().map(|e| self.tier_path(file, t, e)).find(|p| {
                fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|m| m >= src_m)
            })
        };
        if let Some(p) = cached(size) {
            return Ok(Some(p));
        }
        let mut img = image::open(&src).with_context(|| format!("can't decode {file}"))?;
        // transparency survives as png; everything else is a much smaller jpeg
        let alpha = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255);
        let ext = if alpha { "png" } else { "jpg" };
        for &t in TIERS.iter().rev().filter(|&&t| t < long) {
            img = img.thumbnail(t, t);
            let out = self.tier_path(file, t, ext);
            fs::create_dir_all(out.parent().unwrap())?;
            let mut buf = Vec::new();
            if alpha {
                img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
            } else {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85).encode_image(&img.to_rgb8())?;
            }
            // unique temp name: two requests can be making the same tier at once
            let tmp = out.with_extension(format!("{}~", note_id()));
            fs::write(&tmp, &buf)?;
            fs::rename(&tmp, &out)?;
        }
        Ok(cached(size))
    }

    fn tier_path(&self, file: &str, size: u32, ext: &str) -> PathBuf {
        self.dir.join(".thumbs").join(size.to_string()).join(format!("{file}.{ext}"))
    }

    /// The smallest copy that's at least `px` on its long side, for rendering board views.
    pub fn sized(&self, it: &Item, px: u32) -> Result<image::DynamicImage> {
        let tier = match TIERS.iter().find(|&&t| t >= px) {
            Some(&t) => self.tier(&it.file, t)?,
            None => None,
        };
        let p = tier.unwrap_or(self.items_dir().join(&it.file));
        image::open(&p).with_context(|| format!("can't decode {}", it.file))
    }
}

/// Long sides of the cached copies the canvas picks between by zoom.
/// Shared with TIERS in assets/index.html.
pub const TIERS: [u32; 4] = [256, 512, 1024, 2048];

/// Formats we decode and resize; the rest are served whole. gif is here
/// because a resized copy would lose its animation. Mirrors tierable() in
/// assets/index.html.
pub fn tierable(file: &str) -> bool {
    matches!(Path::new(file).extension().and_then(|e| e.to_str()), Some("jpg" | "png" | "webp"))
}

fn convert_with_sips(bytes: &[u8], ext: &str) -> Result<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("mood-{}", note_id()));
    fs::create_dir_all(&dir)?;
    let inp = dir.join(format!("in.{ext}"));
    let out = dir.join("out.png");
    fs::write(&inp, bytes)?;
    let st = std::process::Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(&inp)
        .arg("--out")
        .arg(&out)
        .output()?;
    let res = if st.status.success() {
        fs::read(&out).map_err(Into::into)
    } else {
        Err(anyhow!("couldn't convert {ext} (sips failed)"))
    };
    fs::remove_dir_all(&dir).ok();
    res
}

/// Clear space between images, in world units.
const GAP: f64 = 30.0;

/// Would a w×h image at (x, y) keep GAP from everything?
fn fits(items: &[Item], x: f64, y: f64, w: f64, h: f64) -> bool {
    items.iter().all(|i| {
        x + w + GAP <= i.x || i.x + i.w + GAP <= x || y + h + GAP <= i.y || i.y + i.h() + GAP <= y
    })
}

/// Scan row-major from (x0, y0) within a 3-cell-wide band for a gap.
pub fn find_free(items: &[Item], w: f64, h: f64, x0: f64, y0: f64) -> (f64, f64) {
    const STEP: f64 = 40.0;
    let band = (CELL * 3.0).max(w + STEP);
    for row in 0..2000 {
        let y = y0 + row as f64 * STEP;
        let mut x = x0;
        while x + w <= x0 + band {
            if fits(items, x, y, w, h) {
                return (x, y);
            }
            x += STEP;
        }
    }
    (x0, y0)
}

/// Where an image nobody placed should go: touching the group the newest
/// item belongs to, edge-aligned with a neighbour, at the spot closest to both
/// that group's middle and the newest item. The newest item is where work
/// last happened; the group's middle keeps a run of adds from trailing off
/// in a line, and the newest item keeps that run together.
pub fn beside_recent(items: &[Item], w: f64, h: f64) -> (f64, f64) {
    /// images closer than this are one group
    const LINK: f64 = 400.0;
    let Some(newest) = items.iter().max_by_key(|i| i.id) else {
        return find_free(items, w, h, 40.0, 60.0);
    };
    let apart = |a: &Item, b: &Item| {
        let dx = (a.x.max(b.x) - (a.x + a.w).min(b.x + b.w)).max(0.0);
        let dy = (a.y.max(b.y) - (a.y + a.h()).min(b.y + b.h())).max(0.0);
        dx.hypot(dy)
    };
    let mut group = vec![newest];
    let mut k = 0;
    while k < group.len() {
        let g = group[k];
        for o in items {
            if apart(g, o) < LINK && !group.iter().any(|m| m.id == o.id) {
                group.push(o);
            }
        }
        k += 1;
    }
    let mid = |i: &Item| (i.x + i.w / 2.0, i.y + i.h() / 2.0);
    let n = group.len() as f64;
    let (cx, cy) = group.iter().map(|i| mid(i)).fold((0.0, 0.0), |a, m| (a.0 + m.0 / n, a.1 + m.1 / n));
    let (nx, ny) = mid(newest);
    let mut best: Option<(f64, f64, f64)> = None;
    for g in &group {
        let (r, l) = (g.x + g.w + GAP, g.x - GAP - w);
        let (b, t) = (g.y + g.h() + GAP, g.y - GAP - h);
        let (top, bottom) = (g.y, g.y + g.h() - h);
        let (left, right) = (g.x, g.x + g.w - w);
        for (x, y) in [(r, top), (r, bottom), (l, top), (l, bottom), (left, b), (right, b), (left, t), (right, t)] {
            if !fits(items, x, y, w, h) {
                continue;
            }
            let (px, py) = (x + w / 2.0, y + h / 2.0);
            let score = (px - cx).hypot(py - cy) + (px - nx).hypot(py - ny);
            if best.is_none_or(|(s, _, _)| score < s) {
                best = Some((score, x, y));
            }
        }
    }
    // right of the rightmost image always fits, so this is only a guard
    best.map_or_else(|| find_free(items, w, h, newest.x, newest.y), |(_, x, y)| (x, y))
}

// ---------- trim: crop uniform borders off scans ----------

/// Bounding box of everything that isn't the border colour, if the four
/// corners agree on a border colour. Tolerates JPEG noise.
fn content_bounds(img: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = img.dimensions();
    if w < 16 || h < 16 {
        return None;
    }
    let bg = *img.get_pixel(0, 0);
    let near = |a: &image::Rgba<u8>| (0..3).all(|i| (a[i] as i32 - bg[i] as i32).abs() <= 28);
    let corners = [(w - 1, 0), (0, h - 1), (w - 1, h - 1)];
    if !corners.iter().all(|&(x, y)| near(img.get_pixel(x, y))) {
        return None;
    }
    let row_bg = |y: u32, x0: u32, x1: u32| {
        let off = (x0..x1).filter(|&x| !near(img.get_pixel(x, y))).count();
        off as f64 <= (x1 - x0) as f64 * 0.01
    };
    let col_bg = |x: u32, y0: u32, y1: u32| {
        let off = (y0..y1).filter(|&y| !near(img.get_pixel(x, y))).count();
        off as f64 <= (y1 - y0) as f64 * 0.01
    };
    let (mut y0, mut y1) = (0, h);
    while y0 < h && row_bg(y0, 0, w) { y0 += 1; }
    while y1 > y0 && row_bg(y1 - 1, 0, w) { y1 -= 1; }
    if y1 <= y0 { return None; }
    let (mut x0, mut x1) = (0, w);
    while x0 < w && col_bg(x0, y0, y1) { x0 += 1; }
    while x1 > x0 && col_bg(x1 - 1, y0, y1) { x1 -= 1; }
    if x1 - x0 < w / 10 || y1 - y0 < h / 10 {
        return None;
    }
    Some((x0, y0, x1, y1))
}

impl Board {
    /// Crop a uniform border (black scan padding, white margins). The original
    /// is kept in .originals/. Returns None when there's nothing to trim.
    pub fn trim(&self, id: u32) -> Result<Option<Item>> {
        let mut it = self.item(id)?;
        let src = self.items_dir().join(&it.file);
        let ext = std::path::Path::new(&it.file).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "jpg" | "png" | "webp") {
            bail!("can only trim jpg/png/webp");
        }
        let img = image::open(&src).with_context(|| format!("can't decode {}", it.file))?.to_rgba8();
        let (w, h) = img.dimensions();
        let Some((x0, y0, x1, y1)) = content_bounds(&img) else { return Ok(None) };
        if x1 - x0 + 4 >= w && y1 - y0 + 4 >= h {
            return Ok(None);
        }
        let cropped = image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image();
        let orig = self.dir.join(".originals");
        fs::create_dir_all(&orig)?;
        fs::copy(&src, orig.join(&it.file))?;
        let out_ext = if ext == "png" { "png" } else { "jpg" };
        // a new filename busts the browser's immutable image cache
        let file = format!("{}-t{}.{out_ext}", it.id, note_id());
        let out = self.items_dir().join(&file);
        if out_ext == "jpg" {
            let rgb = image::DynamicImage::ImageRgba8(cropped.clone()).to_rgb8();
            let mut buf = std::io::BufWriter::new(fs::File::create(&out)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92).encode_image(&rgb)?;
        } else {
            cropped.save(&out)?;
        }
        fs::remove_file(&src).ok();
        // keep the on-canvas height roughly the same so neighbours don't get buried
        let old_h = it.h();
        it.file = file;
        it.iw = x1 - x0;
        it.ih = y1 - y0;
        it.w = (old_h * it.iw as f64 / it.ih as f64).round();
        self.save_item(&it)?;
        Ok(Some(it))
    }
}

impl Board {
    /// Set (1–9) or clear (None) the human's rating on an item.
    pub fn rate(&self, id: u32, rating: Option<u8>) -> Result<Item> {
        if let Some(r) = rating {
            if !(1..=9).contains(&r) {
                bail!("ratings run 1–9 (1 repels · 5 indifferent · 9 essential)");
            }
        }
        let mut it = self.item(id)?;
        it.rating = rating;
        it.rated_at = rating.map(|_| now());
        self.save_item(&it)?;
        Ok(it)
    }
}
