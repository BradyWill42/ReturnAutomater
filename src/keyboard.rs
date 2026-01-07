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

/// Press one or more keys synchronously using xdotool.
/// Supports both single keys (e.g. "Return") and combinations like "ctrl+shift+p".
pub fn xdotool_key(display: &str, key: &str) -> Result<()> {
    // Split key combos like "ctrl+shift+p" or "Ctrl + Alt + Delete"
    let combo = key
        .split(|c| c == '+' || c == ' ')
        .filter(|s| !s.is_empty())
        .map(|k| normalize_key_name(k))
        .collect::<Vec<_>>()
        .join("+");

    println!("[TypeKey] Pressing key combo: {combo}");

    let status = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(["key", "--clearmodifiers", &combo])
        .status()
        .context("xdotool key failed")?;

    if !status.success() {
        bail!("xdotool key failed for combo: {combo}");
    }

    Ok(())
}

/// Normalize common key names and aliases to xdotool syntax
fn normalize_key_name(k: &str) -> String {
    match k.trim().to_lowercase().as_str() {
        "ctrl" | "control" => "ctrl".to_string(),
        "alt" => "alt".to_string(),
        "shift" => "shift".to_string(),
        "cmd" | "meta" | "super" | "win" => "super".to_string(),
        // xdotool expects e.g. Return, Tab, F1 to be case-sensitive
        other => {
            if other.len() == 1 {
                other.to_string() // single letters like 'p'
            } else {
                capitalize_first(other)
            }
        }
    }
}

/// Capitalize first letter to match xdotool's Return/Tab syntax
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
