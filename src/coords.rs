// src/coords.rs
use anyhow::{bail, Result};
use std::convert::TryInto;
 
/// Read PNG width/height from IHDR (no extra crate).
pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIG {
        bail!("Not a PNG or too small");
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    Ok((w, h))
}
 
/// Inputs needed to normalize coordinates robustly.
#[derive(Debug, Clone, Copy)]
pub struct NormalizationInputs {
    /// Screenshot size (from PNG IHDR)
    pub screenshot_w: i32,
    pub screenshot_h: i32,
    /// Chrome *window* geometry (from xdotool): window's X/Y on the screen and window's width/height in screen px.
    /// If you run kiosk/fullscreen/app mode, this is typically 0,0 and ~ display size.
    pub window_x: i32,
    pub window_y: i32,
    pub window_w: i32,
    pub window_h: i32,
}
 
/// Map viewport/CSS coords (what the AI returns) to physical screen coords.
/// Strategy:
/// - Treat model coords as pixels in the *captured screenshot*.
/// - Scale from screenshot → window using (window_w/screenshot_w, window_h/screenshot_h).
/// - Offset by the window's top-left (window_x, window_y).
/// - Clamp to display bounds responsibility is left to caller (mouse module can clamp to display).
/*
pub fn viewport_to_screen(
    inputs: NormalizationInputs,
    x_view: i32,
    y_view: i32,
) -> (i32, i32) {
    let sx = (inputs.window_w as f64) / (inputs.screenshot_w as f64);
    let sy = (inputs.window_h as f64) / (inputs.screenshot_h as f64);
 
    // Scale within window
    let x_win = ((x_view as f64) * sx).round() as i32;
    let y_win = ((y_view as f64) * sy).round() as i32;
 
    // Add window offset
    let x_screen = inputs.window_x + x_win;
    let y_screen = inputs.window_y + y_win;
 
    let x_off = std::env::var("CLICK_X_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y_off = std::env::var("CLICK_Y_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);   


    (x_screen + x_off, y_screen + y_off)
}
*/
pub fn viewport_to_screen(
    inputs: NormalizationInputs,
    x_view: i32,
    y_view: i32,
) -> (i32, i32) {
    // Guard against nonsense
    if inputs.screenshot_w <= 0 || inputs.screenshot_h <= 0 || inputs.window_w <= 0 || inputs.window_h <= 0 {
        return (inputs.window_x, inputs.window_y);
    }

    // We assume the rendered overlay stretches the *viewport screenshot* to fill the window width.
    // That implies an anisotropic scale (by width). Whatever vertical space remains is the header strip.
    
    let sx = inputs.window_w as f64 / inputs.screenshot_w as f64;
    let sy = inputs.window_h as f64 / inputs.screenshot_h as f64;
    let scale = sx.min(sy);

    let drawn_w = (inputs.screenshot_w as f64 * scale).round();
    let drawn_h = (inputs.screenshot_h as f64 * scale).round();

    // Centered paddings
    let pad_x = ((inputs.window_w as f64 - drawn_w) / 2.0).round() as i32;
    let pad_y = ((inputs.window_h as f64 - drawn_h) / 2.0).round() as i32;

    // Map screenshot-space → centered image in the window
    let mut dx = ((x_view as f64) * scale).round() as i32;
    let mut dy = ((y_view as f64) * scale).round() as i32;

    // Clamp inside drawn image
    dx = dx.clamp(0, drawn_w as i32 - 1);
    dy = dy.clamp(0, drawn_h as i32 - 1);
    
    

    //let y_off = inputs.window_y - inputs.screenshot_h;

    // Optional nudges
    let x_off = std::env::var("CLICK_X_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y_off = std::env::var("CLICK_Y_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);

    (
        inputs.window_x + pad_x + dx + x_off,
        inputs.window_y + pad_y + dy + y_off,
    ) 
    
}
/*
pub fn viewport_to_screen(
    inputs: NormalizationInputs,
    x_view: i32,
    y_view: i32,
) -> (i32, i32) {
    // Guard against nonsense
    if inputs.screenshot_w <= 0 || inputs.screenshot_h <= 0 {
        return (inputs.window_x, inputs.window_y);
    }

    // Compute isotropic scale: fit screenshot inside window preserving aspect ratio
    let sx = inputs.window_w as f64 / inputs.screenshot_w as f64;
    let sy = inputs.window_h as f64 / inputs.screenshot_h as f64;
    let scale = sx.min(sy);

    // How large the screenshot appears inside the window after scaling
    let drawn_w = (inputs.screenshot_w as f64 * scale).round();
    let drawn_h = (inputs.screenshot_h as f64 * scale).round();

    // Center inside window (fixes titlebar/borders/letterboxing)
    let pad_x = ((inputs.window_w as f64 - drawn_w) / 2.0).round() as i32;
    let pad_y = ((inputs.window_h as f64 - drawn_h) / 2.0).round() as i32;

    // Scale the model coords in screenshot space → drawn window space
    let dx = ((x_view as f64) * scale).round() as i32;
    let dy = ((y_view as f64) * scale).round() as i32;

    let x_off = std::env::var("CLICK_X_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y_off = std::env::var("CLICK_Y_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);   


    // Final absolute screen coords
    (
        inputs.window_x + pad_x + dx + x_off,
        inputs.window_y + pad_y + dy + y_off,
    )
}
*/
