// src/main.rs
mod openai_client;
mod mouse;
mod coords;
mod driver;
mod plan;
mod overlay;
mod keyboard;
mod creds;
mod client;
mod sheets;

use anyhow::{Context, Result};
use openai_client::{
	OpenAIConfig, ViewportPoint, call_openai_for_point, click_by_llm_dom_first, 
	click_checkbox_for_row, click_options_menu_for_row, click_template_input, 
	click_invoice_amount_input, click_sidebar_create_button, click_stage_option,
	ask_boolean_question
};
use driver::{init_driver, cleanup_driver, screenshot_bytes};
use mouse::{ensure_xdotool, reset_zoom, get_active_window_geometry, get_display_geometry, xdotool_move_and_click};
use coords::{png_dimensions, NormalizationInputs, viewport_to_screen};
use plan::{AutomationPlan, Step, fetch_keeper_creds_sync};
use tokio::time::{sleep, Duration};
use keyboard::type_text;
use thirtyfour::By;
use sheets::{fetch_sheet_values, update_cell_value_and_color};
use std::fs;

// Extract step execution into a helper function
async fn execute_step(
    step: &Step,
    bundle: &mut driver::DriverBundle,
    display: &str,
    openai_cfg: &Option<openai_client::OpenAIConfig>,
) -> Result<()> {
    match step {
        Step::VisitUrl { url, .. } => {
            println!("🌐 Visit: {}", url);
            bundle.driver.goto(url).await?;
        }
        Step::TypeText { text, per_char_delay_ms, .. } => {
            ensure_xdotool()?;
            println!("TypeText ({} chars, {}ms/char)", text.len(), per_char_delay_ms);
            type_text(display, text, *per_char_delay_ms)?;
        }
        Step::TypeKey { key, .. } => {
            println!("Pressing key: {key}");
            keyboard::xdotool_key(display, key)?;
        }
        Step::TypeOTP { uid, .. } => {
            let (_, _, code) = match fetch_keeper_creds_sync() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Keeper could not fetch credentials: {e}");
                    (String::new(), String::new(), None)
                }
            };

            if let Some(otp) = code {
                ensure_xdotool()?;
                type_text(display, &otp, 150)?;	
                println!("Typing OTP for UID: {uid}");
            } else {
                eprintln!("No OTP found for UID: {uid}");
            }
        }
        Step::ResetZoom => {
            println!("🔎 Reset zoom → 100%");
            reset_zoom(display)?;
        }
        Step::Wait(secs) => {
            println!("⏳ Wait {}s", secs);
            sleep(Duration::from_secs(*secs)).await;
        }
        Step::SubmitForm { .. } => {
            if let Ok(el) = bundle.driver
                .find(By::Css("button[type='submit']"))
                .await
            {
                el.click().await?;
            } else {
                return Err(anyhow::anyhow!("No submit/create button found"));
            }
        }
        Step::ClickStage { name, .. } => {
            println!("Clicking Stage given {name}");
            click_stage_option(&bundle.driver, &name).await?;
        }
        Step::ClickCheckbox { name, .. } => {
            println!("Clicking checkbox for row containing {name}");
            click_checkbox_for_row(&bundle.driver, &name).await?;
        }
        Step::ClickOptionsMenu { name, .. } => {
            println!("Clicking options menu containing name: {name}");
            click_options_menu_for_row(&bundle.driver, &name).await?;
        }
        Step::ClickTemplate { .. } => {
            println!("Clicking template for invoice");
            click_template_input(&bundle.driver).await?;
        }
        Step::ClickCreate { .. } => {
            println!("Clicking create button for invoice");
            click_sidebar_create_button(&bundle.driver).await?;
        }
        Step::ClickInvoiceAmount { .. } => {
            println!("Clicking invoice amount text input box");
            click_invoice_amount_input(&bundle.driver).await?;
        }
        Step::ClickByDom { prompt, double, .. } => {
            let cfg = match openai_cfg {
                Some(c) => c,
                None => {
                    println!("❌ OPENAI_API_KEY/config not set; skipping click");
                    return Ok(());
                }
            };
            click_by_llm_dom_first(&bundle.driver, cfg, prompt, *double).await?;
        }
        Step::ClickByLlm { prompt, double, .. } => {
            let cfg = match openai_cfg {
                Some(c) => c,
                None => {
                    println!("❌ OPENAI_API_KEY/config not set; skipping LLM click step.");
                    return Ok(());
                }
            };

            // Capture screenshot of the current viewport
            let (path, bytes) = screenshot_bytes(&bundle.driver, "screenshot.png").await?;
            println!("📸 Saved {}", path);

            // Get screenshot size
            let (sw, sh) = png_dimensions(&bytes)?;
            // Query active window geometry (offset + size)
            let (wx, wy, ww, wh) = get_active_window_geometry(display)?;
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
            let (dw, dh) = get_display_geometry(display)?;
            let sx = sx.clamp(0, dw.saturating_sub(1));
            let sy = sy.clamp(0, dh.saturating_sub(1));
            println!("🖱️ Click screen mapped: ({},{})", sx, sy);
            xdotool_move_and_click(display, sx, sy, pt.double)?;
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("Warning: couldn't delete screenshot {}: {}", path, e);
            } else {
                println!("🧹 Deleted screenshot {}", path);
            }
        }
        Step::UpdateSheetCell { row, col, value, success, yellow } => {
            let color = if *yellow {
                (255, 255, 0) // Yellow
            } else if *success {
                (0, 255, 0) // Green
            } else {
                (255, 0, 0) // Red
            };
            let color_name = if *yellow { "yellow" } else if *success { "green" } else { "red" };
            println!("📝 Updating sheet cell at row {row}, col {col} to '{}' ({})", 
                value, color_name);
            if let Err(e) = update_cell_value_and_color(*row, *col, &value, color).await {
                eprintln!("⚠️ Failed to update sheet cell: {}", e);
                // Don't fail the whole automation if sheet update fails
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    
    ensure_xdotool()?;
 
    let login_url = std::env::var("LOGIN_URL")
        .context("Set LOGIN_URL (e.g. export LOGIN_URL='https://example.com')")?;
 
    // Create driver bundle (spawns chromedriver, opens Chrome, navigates)
    let mut bundle = init_driver(&login_url).await?;
    let display = bundle.display.clone();
 
    let values = fetch_sheet_values().await?;

    // Define your automation plan (replace demo() with your own steps)
    let plan = AutomationPlan::client_loop(&values)?;
 
    // OpenAI is only needed for ClickByLlm steps
    let openai_cfg = OpenAIConfig::from_env().ok();
 
    // Execute each step in order
    for (step_idx, step) in plan.steps.iter().enumerate() {
        // Execute the main step
        execute_step(step, &mut bundle, &display, &openai_cfg).await?;
        
        // After each step, check for validation question and ask it
        if let Some(ref cfg) = openai_cfg.as_ref() {
            if let Some(question) = step.validation_question() {
                // Wait for page to settle (2 seconds to capture current state)
                sleep(Duration::from_millis(2000)).await;
                
                // Take screenshot from the automation driver
                let (screenshot_path, screenshot_bytes) = screenshot_bytes(&bundle.driver, "step-validation.png").await?;
                
                // Ask the validation question
                match ask_boolean_question(cfg, &screenshot_bytes, &question).await {
                    Ok(result) => {
                        let status = if result.answer { "✅ PASSED" } else { "❌ FAILED" };
                        println!("👁️ Step {} validation: {} (confidence: {:.2})", 
                            step_idx + 1, 
                            status,
                            result.confidence.unwrap_or(0.0)
                        );
                        println!("   Question: {}", question);
                        if let Some(ref reasoning) = result.reasoning {
                            println!("   Reasoning: {}", reasoning);
                        }
                        
                        // Execute validation action steps
                        if let Some(action_steps) = step.validation_actions(result.answer) {
                            println!("   🔄 Executing validation action steps...");
                            for action_step in action_steps {
                                execute_step(&action_step, &mut bundle, &display, &openai_cfg).await?;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("⚠️ Failed to validate step {}: {}", step_idx + 1, e);
                    }
                }
                
                // Clean up screenshot unless keeping them
                if std::env::var("KEEP_OBSERVER_SCREENSHOTS").map(|v| v != "1").unwrap_or(true) {
                    let _ = fs::remove_file(&screenshot_path);
                }
            }
        }
    }
 
    // Cleanup and exit
    cleanup_driver(&mut bundle).await;
    println!("✅ Done.");
    Ok(())
}
