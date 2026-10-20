//! Render the board (or a chunk of it) to a PNG for an AI to look at.
//! The grid and the #ids drawn here match the browser exactly, so
//! "the warm cluster in C3" or "#12" mean the same thing to both of us.

use crate::store::{cell_label, parse_cell, Board, Item, CELL};
use ab_glyph::{FontVec, PxScale};
use anyhow::{bail, Result};
use image::{imageops, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_circle_mut, draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Region {
    pub fn label(&self) -> String {
        let a = cell_label((self.x0 / CELL).floor() as i64, (self.y0 / CELL).floor() as i64);
        let b = cell_label(
            ((self.x1 - 1.0) / CELL).floor() as i64,
            ((self.y1 - 1.0) / CELL).floor() as i64,
        );
        if a == b { a } else { format!("{a}:{b}") }
    }
    fn hits(&self, it: &Item) -> bool {
        it.x < self.x1 && it.x + it.w > self.x0 && it.y < self.y1 && it.y + it.h() > self.y0
    }
}

fn snap_out(x0: f64, y0: f64, x1: f64, y1: f64) -> Region {
    Region {
        x0: (x0 / CELL).floor() * CELL,
        y0: (y0 / CELL).floor() * CELL,
        x1: (x1 / CELL).ceil() * CELL,
        y1: (y1 / CELL).ceil() * CELL,
    }
}

pub fn bbox(items: &[Item]) -> Option<Region> {
    if items.is_empty() {
        return None;
    }
    let mut r = Region { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for i in items {
        r.x0 = r.x0.min(i.x);
        r.y0 = r.y0.min(i.y);
        r.x1 = r.x1.max(i.x + i.w);
        r.y1 = r.y1.max(i.y + i.h());
    }
    Some(r)
}

/// "" -> whole board, "C4" -> one cell, "B2:D3" -> a range, "#12"/"12" -> around an item.
pub fn parse_region(spec: Option<&str>, items: &[Item]) -> Result<Region> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        let Some(b) = bbox(items) else { bail!("board is empty") };
        return Ok(snap_out(b.x0, b.y0, b.x1, b.y1));
    };
    let id_part = spec.trim_start_matches('#');
    if let Ok(id) = id_part.parse::<u32>() {
        let Some(it) = items.iter().find(|i| i.id == id) else { bail!("no item #{id}") };
        let pad = it.w.max(it.h()) * 0.6;
        return Ok(Region {
            x0: it.x - pad,
            y0: it.y - pad,
            x1: it.x + it.w + pad,
            y1: it.y + it.h() + pad,
        });
    }
    let (a, b) = match spec.split_once(':') {
        Some((a, b)) => (parse_cell(a)?, parse_cell(b)?),
        None => {
            let c = parse_cell(spec)?;
            (c, c)
        }
    };
    let (c0, c1) = (a.0.min(b.0), a.0.max(b.0));
    let (r0, r1) = (a.1.min(b.1), a.1.max(b.1));
    Ok(Region {
        x0: c0 as f64 * CELL,
        y0: r0 as f64 * CELL,
        x1: (c1 + 1) as f64 * CELL,
        y1: (r1 + 1) as f64 * CELL,
    })
}

fn load_font() -> Option<FontVec> {
    for p in [
        "/System/Library/Fonts/Monaco.ttf",
        "/System/Library/Fonts/Geneva.ttf",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ] {
        if let Ok(b) = std::fs::read(p) {
            if let Ok(f) = FontVec::try_from_vec(b) {
                return Some(f);
            }
        }
    }
    None
}

const BG: Rgba<u8> = Rgba([243, 242, 239, 255]);
const LINE: Rgba<u8> = Rgba([222, 220, 214, 255]);
const LABEL: Rgba<u8> = Rgba([150, 148, 141, 255]);
const INK: Rgba<u8> = Rgba([27, 27, 25, 255]);
const PAPER: Rgba<u8> = Rgba([251, 251, 249, 255]);
const CLAUDE: Rgba<u8> = Rgba([194, 96, 58, 255]);

pub struct Rendered {
    pub img: RgbaImage,
    pub region: Region,
    pub scale: f64,
    pub visible: Vec<Item>,
}

pub fn render(board: &Board, items: &[Item], region: Region, px: u32) -> Result<Rendered> {
    let (rw, rh) = (region.x1 - region.x0, region.y1 - region.y0);
    let scale = (px as f64 / rw).min(px as f64 / rh);
    let (w, h) = ((rw * scale).round().max(1.0) as u32, (rh * scale).round().max(1.0) as u32);
    let mut img = RgbaImage::from_pixel(w, h, BG);
    let font = load_font();
    let to = |wx: f64, wy: f64| (((wx - region.x0) * scale), ((wy - region.y0) * scale));

    // grid
    let c0 = (region.x0 / CELL).floor() as i64;
    let c1 = (region.x1 / CELL).ceil() as i64;
    let r0 = (region.y0 / CELL).floor() as i64;
    let r1 = (region.y1 / CELL).ceil() as i64;
    for c in c0..=c1 {
        let (x, _) = to(c as f64 * CELL, 0.0);
        if x >= 0.0 && x < w as f64 {
            draw_filled_rect_mut(&mut img, Rect::at(x as i32, 0).of_size(1, h), LINE);
        }
    }
    for r in r0..=r1 {
        let (_, y) = to(0.0, r as f64 * CELL);
        if y >= 0.0 && y < h as f64 {
            draw_filled_rect_mut(&mut img, Rect::at(0, y as i32).of_size(w, 1), LINE);
        }
    }
    let label_px = (CELL * scale * 0.035).clamp(12.0, 22.0) as f32;

    // images, back to front
    let visible: Vec<Item> = items.iter().filter(|i| region.hits(i)).cloned().collect();
    for it in &visible {
        let (x, y) = to(it.x, it.y);
        let (tw, th) = ((it.w * scale).round() as u32, (it.h() * scale).round() as u32);
        if tw < 2 || th < 2 {
            continue;
        }
        match board.sized(it, tw.max(th)) {
            Ok(t) => {
                let t = imageops::resize(&t.to_rgba8(), tw, th, imageops::FilterType::Triangle);
                imageops::overlay(&mut img, &t, x.round() as i64, y.round() as i64);
            }
            Err(_) => {
                // undecodable here (svg/avif): a placeholder box keeps the layout honest
                draw_filled_rect_mut(&mut img, Rect::at(x as i32, y as i32).of_size(tw, th), LINE);
            }
        }
    }

    // cell labels sit above images so a crowded cell still says where it is
    if let Some(f) = &font {
        for c in c0..c1 {
            for r in r0..r1 {
                let (x, y) = to(c as f64 * CELL, r as f64 * CELL);
                let label = cell_label(c, r);
                let (tw, th) = text_size(PxScale::from(label_px), f, &label);
                let (lx, ly) = (x as i32 + 1, y as i32 + 1);
                draw_filled_rect_mut(&mut img, Rect::at(lx, ly).of_size(tw + 10, th + 9), BG);
                draw_text_mut(&mut img, LABEL, lx + 5, ly + 4, PxScale::from(label_px), f, &label);
            }
        }
    }

    // id tags + note dots, on top of everything
    let tag_px = (label_px * 0.95).max(12.0);
    for it in &visible {
        let (x, y) = to(it.x, it.y);
        let (x, y) = (x.round() as i32, y.round() as i32);
        if let Some(f) = &font {
            let text = match it.rating {
                Some(r) => format!("#{} · {r}", it.id),
                None => format!("#{}", it.id),
            };
            let (tw, th) = text_size(PxScale::from(tag_px), f, &text);
            let (pw, ph) = (tw + 8, th + 7);
            draw_filled_rect_mut(&mut img, Rect::at(x, y).of_size(pw, ph), INK);
            draw_text_mut(&mut img, PAPER, x + 4, y + 3, PxScale::from(tag_px), f, &text);
        }
        let lewis = it.notes.iter().any(|n| n.by != "claude");
        let claude = it.notes.iter().any(|n| n.by == "claude");
        let (ix1, _) = to(it.x + it.w, it.y);
        let mut dx = ix1 as i32 - 9;
        for (on, col) in [(claude, CLAUDE), (lewis, INK)] {
            if on {
                draw_filled_circle_mut(&mut img, (dx, y + 9), 6, PAPER);
                draw_filled_circle_mut(&mut img, (dx, y + 9), 4, col);
                dx -= 14;
            }
        }
    }

    Ok(Rendered { img, region, scale, visible })
}
