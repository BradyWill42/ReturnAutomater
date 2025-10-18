// src/mouse.rs
use anyhow::{bail, Context, Result};
use std::process::Command;
use which::which;
 
pub fn ensure_xdotool() -> Result<()> {
    which("xdotool").context("xdotool not found. Install it (e.g., apt-get install xdotool).")?;
    Ok(())
}
 
/// Physical X display size (px).
pub fn get_display_geometry(display: &str) -> Result<(i32, i32)> {
    let out = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(["getdisplaygeometry"])
        .output()
        .context("failed to run xdotool getdisplaygeometry")?;
 
    if !out.status.success() {
        bail!(
            "xdotool getdisplaygeometry failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let w: i32 = it.next().ok_or_else(|| anyhow::anyhow!("no width"))?.parse()?;
    let h: i32 = it.next().ok_or_else(|| anyhow::anyhow!("no height"))?.parse()?;
    Ok((w, h))
}
 
/// Active window top-left offset and size (X11 window geometry).
pub fn get_active_window_geometry(display: &str) -> Result<(i32, i32, i32, i32)> {
    // xdotool getactivewindow getwindowgeometry --shell
    let out = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(["getactivewindow", "getwindowgeometry", "--shell"])
        .output()
        .context("failed to run xdotool getactivewindow getwindowgeometry")?;
 
    if !out.status.success() {
        bail!(
            "xdotool getwindowgeometry failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut x = 0i32;
    let mut y = 0i32;
    let mut w = 0i32;
    let mut h = 0i32;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("X=") { x = v.parse()?; }
        if let Some(v) = line.strip_prefix("Y=") { y = v.parse()?; }
        if let Some(v) = line.strip_prefix("WIDTH=") { w = v.parse()?; }
        if let Some(v) = line.strip_prefix("HEIGHT=") { h = v.parse()?; }
    }
    Ok((x, y, w, h))
}
 
/// Move the OS cursor and click (optionally double).
pub fn xdotool_move_and_click(display: &str, x: i32, y: i32, double: bool) -> Result<()> {
    let status = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(["mousemove", "--sync", &x.to_string(), &y.to_string()])
        .status()
        .context("xdotool mousemove failed")?;
    if !status.success() {
        bail!("xdotool mousemove returned non-zero status");
    }
 
    let status = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(["click", "1"])
        .status()
        .context("xdotool click failed")?;
    if !status.success() {
        bail!("xdotool click returned non-zero status");
    }
 
    if double {
        let status = Command::new("xdotool")
            .env("DISPLAY", display)
            .args(["click", "1"])
            .status()
            .context("xdotool second click failed")?;
        if !status.success() {
            bail!("xdotool second click returned non-zero status");
        }
    }
    Ok(())
}
 
/// Send Ctrl+0 to reset browser zoom to 100% (no JS).
pub fn reset_zoom(display: &str) -> Result<()> {
    for _ in 0..2 {
        let st = Command::new("xdotool")
            .env("DISPLAY", display)
            .args(["key", "--clearmodifiers", "ctrl+0"])
            .status()
            .context("xdotool key ctrl+0 failed")?;
        if !st.success() {
            bail!("xdotool key returned non-zero status");
        }
    }
    Ok(())
}
 
