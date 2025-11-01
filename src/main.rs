// src/main.rs
mod openai_client;
mod mouse;
mod coords;
mod driver;
mod plan;
mod overlay;
mod keyboard;
mod creds;
 
use anyhow::{Context, Result};
use openai_client::{OpenAIConfig, ViewportPoint, call_openai_for_point};
use driver::{init_driver, cleanup_driver, screenshot_bytes};
use mouse::{ensure_xdotool, reset_zoom, get_active_window_geometry, get_display_geometry, xdotool_move_and_click};
use coords::{png_dimensions, NormalizationInputs, viewport_to_screen};
use plan::{AutomationPlan, Step};
use tokio::time::{sleep, Duration};
use keyboard::type_text;
use creds::KeeperCreds;


#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    
    let token = std::env::var("KEEPER_TOKEN")?;
    let uid = std::env::var("KEEPER_UID")?;
    let cfg_path = std::env::var("KEEPER_CONFIG_PATH").unwrap_or_else(|_| "config.json".to_string());

    // Run Keeper (blocking) on a blocking thread so Tokio won't panic.
    let (username, password, totp) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut kc = KeeperCreds::new(&token, &cfg_path)?;
        // Optional full dump first (comment out if you don't want it every run)
        //kc.dump_record(&uid)?;
        // Then fetch the standard fields (login/password/oneTimeCode)
        kc.get_fields(&uid)
    })
    .await??;

    println!("Username: {}", username);
    println!("Password: {}", password);
    match totp {
        Some(code) => println!("2FA Code: {}", code),
        None => println!("No TOTP configured."),
    }

    ensure_xdotool()?;
 
    let login_url = std::env::var("LOGIN_URL")
        .context("Set LOGIN_URL (e.g. export LOGIN_URL='https://example.com')")?;
 
    // Create driver bundle (spawns chromedriver, opens Chrome, navigates)
    let mut bundle = init_driver(&login_url).await?;
    let display = bundle.display.clone();
 
    // Define your automation plan (replace demo() with your own steps)
    let plan = AutomationPlan::demo();
 
    // OpenAI is only needed for ClickByLlm steps
    let openai_cfg = OpenAIConfig::from_env().ok();
 
    // Execute each step in order
    for step in plan.steps.iter() {
        match step {
            Step::VisitUrl(url) => {
                println!("🌐 Visit: {}", url);
                bundle.driver.goto(url).await?;
            }
	    Step::TypeText { text, per_char_delay_ms } => {
		ensure_xdotool()?;
		println!("TypeText ({} chars, {}ms/char)", text.len(), per_char_delay_ms);
		type_text(&display, text, *per_char_delay_ms)?;
	    }
	    Step::TypeKey { key } => {
		println!("Pressing key: {key}");
		keyboard::xdotool_key(&display, key)?;
	    }
            Step::ResetZoom => {
                println!("🔎 Reset zoom → 100%");
                reset_zoom(&display)?;
            }
            Step::Wait(secs) => {
                println!("⏳ Wait {}s", secs);
                sleep(Duration::from_secs(*secs)).await;
            }
	    /*
            Step::ClickScreen { x, y, double } => {
                println!("🧭 Click screen at {},{} (double={})", x, y, double);
                // Clamp to display bounds
                
		let (dw, dh) = get_display_geometry(&display)?;
                let sx = (*x).clamp(0, dw.saturating_sub(1));
                let sy = (*y).clamp(0, dh.saturating_sub(1));
				
                xdotool_move_and_click(&display, sx, sy, *double)?;
            }
	    */
            Step::ClickByLlm { prompt, double } => {
                let cfg = match &openai_cfg {
                    Some(c) => c,
                    None => {
                        println!("❌ OPENAI_API_KEY/config not set; skipping LLM click step.");
                        continue;
                    }
                };
 
                // Capture screenshot of the current viewport
                let (path, bytes) = screenshot_bytes(&bundle.driver, "screenshot.png").await?;
                println!("📸 Saved {}", path);
 
                // Get screenshot size
                let (sw, sh) = png_dimensions(&bytes)?;
                // Query active window geometry (offset + size)
                let (wx, wy, ww, wh) = get_active_window_geometry(&display)?;
                println!("🧭 Geo: screenshot={}x{}, window@({},{}) {}x{}", sw, sh, wx, wy, ww, wh);
 
                // Ask model for viewport coords
                println!("🤖 LLM prompt: {}", prompt);
                let mut pt: ViewportPoint = call_openai_for_point(cfg, &bytes, prompt).await?;
                // If caller wants to force double, override
                if let Some(force_double) = *double {
                    pt.double = force_double;
                }
                println!("↳ Model returned viewport ({},{}) double={}", pt.x, pt.y, pt.double);
 
                // Normalize viewport → screen using *window* geometry (not full display)
                let norm = NormalizationInputs {
                    screenshot_w: sw as i32,
                    screenshot_h: sh as i32,
                    window_x: wx,
                    window_y: wy,
                    window_w: ww,
                    window_h: wh,
                };
                let (sx, sy) = viewport_to_screen(norm, pt.x, pt.y);
 
                // Finally clamp to display before clicking
                let (dw, dh) = get_display_geometry(&display)?;
                let sx = sx.clamp(0, dw.saturating_sub(1));
                let sy = sy.clamp(0, dh.saturating_sub(1));
 
		//let x_off = std::env::var("CLICK_X_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
		//let y_off = std::env::var("CLICK_Y_OFFSET_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
			
		//sx += x_off;
		//sy += y_off;		
	
                println!("🖱️ Click screen mapped: ({},{})", sx, sy);
                xdotool_move_and_click(&display, sx, sy, pt.double)?;
            }
        }
    }
 
    // Cleanup and exit
    cleanup_driver(&mut bundle).await;
    println!("✅ Done.");
    Ok(())
}
