// src/keyboard.rs
use anyhow::{bail, Context, Result};
use std::process::Command;


/// Type literal text into the active window on the given DISPLAY.
/// `per_char_delay_ms` is the inter-key delay (e.g., 6–15ms).
pub fn type_text(display: &str, text: &str, per_char_delay_ms: u64) -> Result<()> {
    let status = Command::new("xdotool")
        .env("DISPLAY", display)
        .args([
            "type",
            "--clearmodifiers",
            "--delay",
            &per_char_delay_ms.to_string(),
            "--",
            text,
        ])
        .status()
        .context("xdotool type failed")?;

    if !status.success() {
        bail!("xdotool type returned non-zero status");
    }
    Ok(())
}
