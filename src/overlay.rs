// src/overlay.rs
use anyhow::{Context, Result};
use image::{DynamicImage, ImageOutputFormat, Rgba, RgbaImage};
use imageproc::drawing::draw_line_segment_mut;
 
/// Config for the grid overlay.
#[derive(Debug, Clone, Copy)]
pub struct GridOptions {
    /// Grid spacing in *image pixels* (e.g., 50)
    pub step: u32,
    /// Label every Nth line (e.g., 2 = every other line)
    pub label_every: u32,
    /// Pixel size of the bitmap font (scale factor; 1 = 5x7, 2 = 10x14, etc.)
    pub font_scale: u32,
    /// If true, write a debug copy to disk as screenshot_grid.png
    pub save_debug: bool,
}
 
impl GridOptions {
    pub fn from_env() -> Self {
        let step = std::env::var("GRID_STEP").ok().and_then(|s| s.parse().ok()).unwrap_or(50);
        let label_every = std::env::var("GRID_LABEL_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        let font_scale = std::env::var("GRID_FONT_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        let save_debug = std::env::var("GRID_SAVE_DEBUG").map_or(false, |v| v == "1");
        Self { step, label_every, font_scale, save_debug }
    }
}
 
/// Overlay a green grid + yellow coordinate labels directly on the PNG bytes.
/// Returns new PNG bytes.
pub fn overlay_grid_with_coords(png_bytes: &[u8], opts: GridOptions) -> Result<Vec<u8>> {
    // Decode
    let img = image::load_from_memory(png_bytes).context("decode PNG")?;
    let mut rgba: RgbaImage = img.to_rgba8();
    let (w, h) = rgba.dimensions();
 
    let grid = Rgba([255, 0, 0, 0]);   // green lines
    let text = Rgba([255, 0, 0, 0]); // yellow text
    let pad = 2 * opts.font_scale;       // small padding for labels
 
    // Draw vertical lines and x-labels
    let mut x_tick = 0u32;
    while x_tick <= w {
        let x = x_tick.min(w.saturating_sub(1)) as f32;
        draw_line_segment_mut(&mut rgba, (x, 0.0), (x, h as f32), grid);
 
        if opts.label_every > 0 && ((x_tick / opts.step) % opts.label_every == 0) {
            // Label "x=<num>" near the top of the image at (x+pad, pad)
            let label = format!("{}", x_tick);
            let lx = x_tick.saturating_add(pad).min(w.saturating_sub(1));
            let ly = pad.min(h.saturating_sub(1));
            draw_text_bitmap(&mut rgba, lx as i32, ly as i32, &label, text, opts.font_scale);
        }
 
        match x_tick.checked_add(opts.step) {
            Some(next) if next > x_tick => x_tick = next,
            _ => break,
        }
    }
 
    // Draw horizontal lines and y-labels
    let mut y_tick = 0u32;
    while y_tick <= h {
        let y = y_tick.min(h.saturating_sub(1)) as f32;
        draw_line_segment_mut(&mut rgba, (0.0, y), (w as f32, y), grid);
 
        if opts.label_every > 0 && ((y_tick / opts.step) % opts.label_every == 0) {
            // Label "y=<num>" at the left edge at (pad, y+pad)
            let label = format!("{}", y_tick);
            let lx = pad.min(w.saturating_sub(1));
            let ly = y_tick.saturating_add(pad).min(h.saturating_sub(1));
            draw_text_bitmap(&mut rgba, lx as i32, ly as i32, &label, text, opts.font_scale);
        }
 
        match y_tick.checked_add(opts.step) {
            Some(next) if next > y_tick => y_tick = next,
            _ => break,
        }
    }
 
    // Encode back to PNG
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut out), ImageOutputFormat::Png)
        .context("encode annotated PNG")?;
 
    if opts.save_debug {
        let _ = std::fs::write("screenshot_grid.png", &out);
    }
    Ok(out)
}
 
// ---------------------- Tiny 5x7 bitmap font ----------------------
 
#[rustfmt::skip]
const BITMAP_5X7: &[(&str, [u8; 7])] = &[
    // Each row is 5 bits (LSB on the right): bit 4..0
    // Digits
    ("0", [0b01110,0b10001,0b10011,0b10101,0b11001,0b10001,0b01110]),
    ("1", [0b00100,0b01100,0b00100,0b00100,0b00100,0b00100,0b01110]),
    ("2", [0b01110,0b10001,0b00001,0b00010,0b00100,0b01000,0b11111]),
    ("3", [0b11110,0b00001,0b00001,0b01110,0b00001,0b00001,0b11110]),
    ("4", [0b00010,0b00110,0b01010,0b10010,0b11111,0b00010,0b00010]),
    ("5", [0b11111,0b10000,0b11110,0b00001,0b00001,0b10001,0b01110]),
    ("6", [0b00110,0b01000,0b10000,0b11110,0b10001,0b10001,0b01110]),
    ("7", [0b11111,0b00001,0b00010,0b00100,0b01000,0b01000,0b01000]),
    ("8", [0b01110,0b10001,0b10001,0b01110,0b10001,0b10001,0b01110]),
    ("9", [0b01110,0b10001,0b10001,0b01111,0b00001,0b00010,0b01100]),
    // Letters we need: x, y
    ("x", [0b00000,0b10001,0b01010,0b00100,0b01010,0b10001,0b00000]),
    ("y", [0b00000,0b10001,0b10001,0b01111,0b00001,0b00010,0b01100]),
    // '=' sign
    ("=", [0b00000,0b00000,0b11111,0b00000,0b11111,0b00000,0b00000]),
];
 
fn glyph_rows(ch: char) -> Option<[u8; 7]> {
    let s = &ch.to_string();
    for (k, rows) in BITMAP_5X7 {
        if k == s { return Some(*rows); }
    }
    None
}
 
/// Draw one character from the bitmap font at (x,y). Top-left origin.
/// `scale` enlarges each pixel to scale×scale block.
fn draw_char(img: &mut RgbaImage, x: i32, y: i32, ch: char, color: Rgba<u8>, scale: u32) {
    let rows = match glyph_rows(ch) {
        Some(r) => r,
        None => return, // skip unknown chars
    };
    let (w, h) = img.dimensions();
    for (row_idx, row_bits) in rows.iter().enumerate() {
        for col in 0..5 {
            let on = (row_bits >> (4 - col)) & 1 == 1;
            if on {
                let px = x + (col as i32) * (scale as i32);
                let py = y + (row_idx as i32) * (scale as i32);
                // draw scale×scale block
                for dy in 0..scale {
                    for dx in 0..scale {
                        let sx = px + dx as i32;
                        let sy = py + dy as i32;
                        if sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h {
                            img.put_pixel(sx as u32, sy as u32, color);
                        }
                    }
                }
            }
        }
    }
}
 
/// Draw simple ASCII text (allowed chars: 0-9, x, y, =)
fn draw_text_bitmap(
    img: &mut RgbaImage,
    mut x: i32,
    y: i32,
    text: &str,
    color: Rgba<u8>,
    scale: u32,
) {
    let advance = (5 * scale) as i32 + (scale as i32); // 1px (scaled) spacing
    for ch in text.chars() {
        draw_char(img, x, y, ch, color, scale);
        x += advance;
    }
}
