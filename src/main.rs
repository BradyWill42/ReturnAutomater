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
    ask_boolean_question, get_largest_run_dir
};
use driver::{init_driver, cleanup_driver, screenshot_bytes};
use mouse::{
    ensure_xdotool, reset_zoom, get_active_window_geometry,
    get_display_geometry, xdotool_move_and_click
};
use coords::{png_dimensions, NormalizationInputs, viewport_to_screen};
use plan::{AutomationPlan, Step, fetch_keeper_creds_sync};
use tokio::time::{sleep, Duration};
use keyboard::type_text;
use thirtyfour::By;
use sheets::{fetch_sheet_values, SheetsClient};
use std::fs;

/// Control-flow signals for automation
#[derive(Debug)]
pub enum ControlFlowError {
    StopClient,
    AbortProgram,
    Other(anyhow::Error),
}

impl std::fmt::Display for ControlFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlFlowError::StopClient => write!(f, "Stop current client"),
            ControlFlowError::AbortProgram => write!(f, "Abort program"),
            ControlFlowError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ControlFlowError {}
impl From<anyhow::Error> for ControlFlowError {
    fn from(err: anyhow::Error) -> Self {
        ControlFlowError::Other(err)
    }
}

/// Execute a single automation step
async fn execute_step(
    step: &Step,
    bundle: &mut driver::DriverBundle,
    display: &str,
    openai_cfg: &Option<OpenAIConfig>,
    sheets: &SheetsClient,
) -> Result<()> {
    match step {
        Step::BeginClient { row } => {
            println!("👤 Begin client block (sheet row={row})");
        }

        Step::VisitUrl { url, .. } => {
            println!("🌐 Visit: {url}");
            bundle.driver.goto(url).await?;
        }

        Step::TypeText { text, per_char_delay_ms, .. } => {
            ensure_xdotool()?;
            type_text(display, text, *per_char_delay_ms)?;
        }

        Step::TypeKey { key, .. } => {
            keyboard::xdotool_key(display, key)?;
        }

        Step::TypeOTP { uid, .. } => {
            let (_, _, code) = fetch_keeper_creds_sync().unwrap_or_default();
            if let Some(otp) = code {
                ensure_xdotool()?;
                type_text(display, &otp, 150)?;
                println!("🔐 Typed OTP for UID {uid}");
            }
        }

        Step::ResetZoom => {
            reset_zoom(display)?;
        }

        Step::Wait(secs) => {
            sleep(Duration::from_secs(*secs)).await;
        }

        Step::SubmitForm { .. } => {
            bundle.driver
                .find(By::Css("button[type='submit']"))
                .await?
                .click()
                .await?;
        }

        Step::ClickStage { name, .. } => {
            click_stage_option(&bundle.driver, name).await?;
        }

        Step::ClickCheckbox { name, .. } => {
            click_checkbox_for_row(&bundle.driver, name).await?;
        }

        Step::ClickOptionsMenu { name, .. } => {
            click_options_menu_for_row(&bundle.driver, name).await?;
        }

        Step::ClickTemplate { .. } => {
            click_template_input(&bundle.driver).await?;
        }

        Step::ClickCreate { .. } => {
            click_sidebar_create_button(&bundle.driver).await?;
        }

        Step::ClickInvoiceAmount { .. } => {
            click_invoice_amount_input(&bundle.driver).await?;
        }

        Step::ClickByDom { prompt, double, .. } => {
            let cfg = openai_cfg.as_ref().context("OpenAI not configured")?;
            click_by_llm_dom_first(&bundle.driver, cfg, prompt, *double).await?;
        }

        Step::ClickByLlm { prompt, double, .. } => {
            let cfg = openai_cfg.as_ref().context("OpenAI not configured")?;

            let (path, bytes) = screenshot_bytes(&bundle.driver, "screenshot.png").await?;
            let (sw, sh) = png_dimensions(&bytes)?;
            let (wx, wy, ww, wh) = get_active_window_geometry(display)?;

            let mut pt: ViewportPoint =
                call_openai_for_point(cfg, &bytes, prompt).await?;
            if let Some(force) = *double {
                pt.double = force;
            }

            let norm = NormalizationInputs {
                screenshot_w: sw as i32,
                screenshot_h: sh as i32,
                window_x: wx,
                window_y: wy,
                window_w: ww,
                window_h: wh,
            };

            let (sx, sy) = viewport_to_screen(norm, pt.x, pt.y);
            let (dw, dh) = get_display_geometry(display)?;

            xdotool_move_and_click(
                display,
                sx.clamp(0, dw - 1),
                sy.clamp(0, dh - 1),
                pt.double,
            )?;

            let _ = fs::remove_file(path);
        }

        Step::UpdateSheetCell { row, col, value, success, yellow } => {
            let color = if *yellow {
                (255, 255, 0)
            } else if *success {
                (0, 255, 0)
            } else {
                (255, 0, 0)
            };

            sheets
                .update_cell_value_and_color(*row, *col, value, color)
                .await?;
        }

        Step::StopClient => {
            return Err(ControlFlowError::StopClient.into());
        }

        Step::Abort => {
            return Err(ControlFlowError::AbortProgram.into());
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    ensure_xdotool()?;

    let login_url = std::env::var("LOGIN_URL")
        .context("LOGIN_URL must be set")?;

    let mut bundle = init_driver(&login_url).await?;
    let display = bundle.display.clone();

    // 🔑 READ sheet once (API key)
    let values = fetch_sheet_values().await?;

    // 🔑 BUILD OAuth ONCE
    let sheets = SheetsClient::new_from_env().await?;

    let plan = AutomationPlan::client_loop(&values)?;
    let openai_cfg = OpenAIConfig::from_env().ok();

    let mut step_idx = 0;
    while step_idx < plan.steps.len() {
        let step = &plan.steps[step_idx];

        match execute_step(step, &mut bundle, &display, &openai_cfg, &sheets).await {
            Ok(()) => {}
            Err(e) => {
                if let Some(cf) = e.downcast_ref::<ControlFlowError>() {
                    match cf {
                        ControlFlowError::StopClient => {
                            step_idx += 1;
                            while step_idx < plan.steps.len() {
                                if matches!(plan.steps[step_idx], Step::BeginClient { .. }) {
                                    break;
                                }
                                step_idx += 1;
                            }
                            continue;
                        }
                        ControlFlowError::AbortProgram => return Err(e),
                        ControlFlowError::Other(_) => return Err(e),
                    }
                } else {
                    return Err(e);
                }
            }
        }

        step_idx += 1;
    }

    cleanup_driver(&mut bundle).await;
    println!("✅ Done.");
    Ok(())
}