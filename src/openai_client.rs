use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use thirtyfour::prelude::*;
use std::env;
use std::time::Duration;
use crate::overlay::{overlay_grid_with_coords, GridOptions};

// --- drawing + saving imports ---
use image::{DynamicImage, ImageOutputFormat, Rgba, RgbaImage};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::task::JoinSet;
use std::sync::{Mutex, OnceLock};


// NEW: simple in-process memory of previously chosen points

// Rate limit tracking to prevent crashes from excessive rate limiting
struct RateLimitTracker {
    consecutive_failures: usize,
    last_failure_time: Option<SystemTime>,
}

impl RateLimitTracker {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: None,
        }
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure_time = Some(SystemTime::now());
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_time = None;
    }

    fn should_pause(&self) -> bool {
        let threshold = env::var("OPENAI_RATE_LIMIT_PAUSE_THRESHOLD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        self.consecutive_failures >= threshold
    }

    fn get_pause_duration(&self) -> Duration {
        let pause_minutes = env::var("OPENAI_RATE_LIMIT_PAUSE_MINUTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        Duration::from_secs(pause_minutes * 60)
    }
}

// Global rate limit tracker (thread-safe)
static RATE_LIMIT_TRACKER: OnceLock<Mutex<RateLimitTracker>> = OnceLock::new();

fn get_rate_limit_tracker() -> &'static Mutex<RateLimitTracker> {
    RATE_LIMIT_TRACKER.get_or_init(|| Mutex::new(RateLimitTracker::new()))
}

// Helper to safely access the tracker
fn with_rate_limit_tracker<F, R>(f: F) -> R
where
    F: FnOnce(&mut RateLimitTracker) -> R,
{
    let mut tracker = get_rate_limit_tracker().lock().unwrap();
    f(&mut tracker)
}

#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String, // default official; override for proxies/azure
    pub model: String,    // e.g., "gpt-4o-mini"
    pub timeout: Duration,
    pub max_retries: usize,
}

impl OpenAIConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            api_key: env::var("OPENAI_API_KEY")
                .context("Set OPENAI_API_KEY in your environment")?,
            base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            model: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            timeout: Duration::from_secs(
                env::var("OPENAI_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
            ),
            max_retries: env::var("OPENAI_MAX_RETRIES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),
        })
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: ChatContent,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseFormat {
    JsonObject,
}

#[derive(Deserialize, Debug)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Deserialize, Debug, Clone)]
struct UsageInfo {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize, Debug)]
struct ChoiceMessage {
    content: String,
}

/// Model returns JSON {x:int, y:int, double:bool} in viewport pixels.
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct ViewportPoint {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub double: bool,
}

//TODO NEW UI CLICK FUNCTIONALITY ADDENDEUM
// =====================
// DOM click refinements
// =====================

//DOM TESTING BEGINS
// Small whitespace normalizer for visible text and aria labels
fn clean(s: String) -> String {
    let s = s.trim();
    let mut out = String::with_capacity(s.len());
    let mut last_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
        } else {
            out.push(ch);
            last_ws = false;
        }
        if out.len() >= 200 { break; } // hard cap
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct UiCandidate {
    pub id: usize,     // index in list
    pub tag: String,   // e.g., "BUTTON"
    pub text: String,  // visible text
    pub aria: String,  // aria-label
    // New: extra hints (not sent to LLM unless you want to)
    pub role: String,
    pub r#type: String,
    pub name: String,
    pub value: String,
    pub data_test: String,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub meta: UiCandidate,
    pub el: WebElement,
    // New: shape info for heuristic fallback
    pub rect: Option<(i32, i32, i32, i32)>, // x,y,w,h
    pub visible: bool,
    pub disabled: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClickDecision {
    id: Option<usize>,
    _reason: Option<String>,
    _confidence: Option<f32>,
}

pub async fn collect_ui_candidates(driver: &WebDriver, cap: usize) -> Result<Vec<Candidate>> {
    let selectors = [
        "button",
        "a[href]",
        "[role='button']",
        "[role='link']",
	    "input[type='submit']",
        "input[type='button']",
        "[tabindex]",
        ".btn",
        ".button",
	    "[data-test='document-tree-node-link']",
    	"input[data-test*='amount_input']", // invoice amount input box
    	"input[data-test*='title_input']",  // service name input
    	"textarea[data-test*='description_input']", // description input
        "[data-test='template-select']",    // invoice template box
        "[data-test='shared-section__button']",
        "article[data-test='shared-element__kanban-board__kanban-card']",
        "[data-test='select-trigger']",
        "[data-test='shared-section__dropdown-list-item']",
    ]
    .join(",");

    let elems = driver.find_all(By::Css(&selectors)).await?;
    let mut out = Vec::with_capacity(elems.len().min(cap));

    for (i, el) in elems.into_iter().enumerate().take(cap) {
        // Basic attributes
        let tag = el.tag_name().await.unwrap_or_default().to_uppercase();
        let text = clean(el.text().await.unwrap_or_default());
        let aria = clean(el.attr("aria-label").await?.unwrap_or_default());
        let role = clean(el.attr("role").await?.unwrap_or_default());
        let ty = clean(el.attr("type").await?.unwrap_or_default());
        let name = clean(el.attr("name").await?.unwrap_or_default());
        let value = clean(el.attr("value").await?.unwrap_or_default());



        // Some apps use many variants of data-test
        
	let data_test = {
    	    let d1 = el.attr("data-test").await?;    // Option<String>
    	    let d2 = el.attr("data-testid").await?;  // Option<String>
            let d3 = el.attr("data-qa").await?;      // Option<String>
    	    d1.or(d2).or(d3).unwrap_or_default()
	};

        // State/visibility
        let visible = el.is_displayed().await.unwrap_or(false);
        let disabled = el.attr("disabled").await?.is_some();

        // Geometry (best-effort)
        let rect = match el.rect().await {
            Ok(r) => Some((r.x as i32, r.y as i32, r.width as i32, r.height as i32)),
            Err(_) => None,
        };

        out.push(Candidate {
            meta: UiCandidate {
                id: i,
                tag,
                text,
                aria,
                role,
                r#type: ty,
                name,
                value,
                data_test,
            },
            el,
            rect,
            visible,
            disabled,
        });
    }
    Ok(out)
}

pub async fn click_checkbox_for_row(driver: &WebDriver, name: &str) -> Result<()> {
    let rows = driver
        .find_all(By::Css("[data-test='shared-section__docdir-table-row']"))
        .await?;

    for row in rows {
        let text = row.text().await?;

        if text.contains(name) {
            let checkbox = row
                .find(By::Css("label.checkbox[data-test='Checkbox']"))
                .await?;

            checkbox.click().await?;
            println!("✔ Clicked checkbox for row: {}", name);
            return Ok(());
        }
    }

    Err(anyhow::anyhow!(
	"Could not find document row with name containing: {}",
	name
    ))
}

pub async fn click_stage_option(driver: &WebDriver, name: &str) -> Result<()> {
    // All dropdown items share this selector
    let items = driver
        .find_all(By::Css("[data-test='shared-section__dropdown-list-item']"))
        .await?;

    for item in items {
        let text = item.text().await?.trim().to_string();

        if text.contains(name) {
            // Scroll into view (avoids intercepted clicks)
            let _ = driver.execute(
                r#"arguments[0].scrollIntoView({behavior: "instant", block: "center"});"#,
                vec![item.to_json()?],
            ).await;

            item.click().await?;
            println!("✔ Selected stage option: {}", name);
            return Ok(());
        }
    }

    Err(anyhow::anyhow!(
        "Could not find dropdown stage option containing: {}",
        name
    ))
}



pub async fn click_invoice_amount_input(driver: &WebDriver) -> Result<()> {
    // Find the amount input box
    let amount_input = driver
        .find(By::Css("[data-test='shared-element__invoice-line-items-form__amount_input']"))
        .await?;

    // Scroll into view just to be extra safe
    driver.execute(
        "arguments[0].scrollIntoView({behavior:'auto', block:'center'});",
        vec![amount_input.to_json()?],
    ).await?;

    // Click the input (this will auto-focus)
    amount_input.click().await?;

    Ok(())
}


pub async fn click_sidebar_create_button(driver: &WebDriver) -> Result<()> {
    // Strategy:
    // 1. Look for the primary create button inside the sidebar footer
    // 2. Fallback: any visible shared-section button with text "Create"
    // 3. Fallback: generic submit button types

    // 1. Highly specific selector: footer create button in invoice sidebar
    let candidates = driver
        .find_all(By::Css(
            "div[data-test='bill-form-sidebar'] \
             footer button[data-test='shared-section__button']",
        ))
        .await?;

    for el in candidates {
        if el.is_displayed().await.unwrap_or(false) {
            let txt = el.text().await.unwrap_or_default();
            if txt.trim().eq_ignore_ascii_case("Create") {
                el.click().await?;
                return Ok(());
            }
        }
    }

    // 2. Fallback: ANY visible shared-section__button with the label "Create"
    let fallback_buttons = driver
        .find_all(By::Css("button[data-test='shared-section__button']"))
        .await?;

    for el in fallback_buttons {
        if el.is_displayed().await.unwrap_or(false) {
            let txt = el.text().await.unwrap_or_default();
            if txt.trim().eq_ignore_ascii_case("Create") {
                el.click().await?;
                return Ok(());
            }
        }
    }

    // 3. Last fallback: HTML submit buttons
    if let Ok(el) = driver.find(By::Css("button[type='submit']")).await {
        if el.is_displayed().await.unwrap_or(false) {
            el.click().await?;
            return Ok(());
        }
    }

    Err(anyhow::anyhow!("No Create button found"))
}




pub async fn click_template_input(driver: &WebDriver) -> Result<()> {
    // Locate the main wrapper
    let wrapper = driver
        .find(By::Css("[data-test='template-select']"))
        .await
        .context("Could not find template-select container")?;

    // React-Select requires clicking the control div
    let control = wrapper
        .find(By::Css(".react-select__control"))
        .await
        .context("Could not find react-select__control")?;

    // Try a normal Selenium click
    let clicked = control.click().await;

    if clicked.is_err() {
        // JS fallback for stubborn React-Select controls
        driver
            .execute(
                "arguments[0].click();",
                vec![serde_json::to_value(&control)?],
            )
            .await
            .context("JS click fallback failed for template select")?;
    }

    Ok(())
}



pub async fn click_options_menu_for_row(driver: &WebDriver, name: &str) -> Result<()> {
    let name_lc = name.to_lowercase();

    // All rows in the document directory table
    let rows = driver
        .find_all(By::Css("[data-test='shared-section__docdir-table-row']"))
        .await?;

    println!(
        "[click_options_menu_for_row] looking for name containing {:?} in {} rows",
        name, rows.len()
    );

    for (idx, row) in rows.into_iter().enumerate() {
        // Try primary label: the folder/document link area
        let label_text = if let Ok(label_el) =
            row.find(By::Css("[data-test='document-tree-node-link']")).await
        {
            label_el.text().await.unwrap_or_default()
        } else {
            // Fallback: whole row text (a bit noisier, but robust)
            row.text().await.unwrap_or_default()
        };

        let label_trim = label_text.trim().to_string();
        let label_lc = label_trim.to_lowercase();

        println!("  row[{idx}] label='{label_trim}'");

        if !label_lc.is_empty() && label_lc.contains(&name_lc) {
            // Found the row we want: click its options/menu button
            if let Ok(btn) = row.find(By::Css("button[data-test='option-vertical']")).await {
                println!("  → clicking options menu on row[{idx}] for '{}'", label_trim);
                btn.click().await?;
                return Ok(());
            } else {
                println!(
                    "  row[{idx}] matched name but has no button[data-test='option-vertical']"
                );
            }
        }
    }

    Err(anyhow::anyhow!(
        "Could not find options menu for document row containing name: {}",
        name
    ))
}


pub async fn call_openai_for_dom_decision(
    cfg: &OpenAIConfig,
    user_prompt: &str,
    candidates: &[UiCandidate],
) -> Result<ClickDecision> {
    // Check if we should pause due to excessive rate limiting
    let should_pause = with_rate_limit_tracker(|tracker| tracker.should_pause());
    if should_pause {
        let pause_duration = with_rate_limit_tracker(|tracker| tracker.get_pause_duration());
        let pause_secs = pause_duration.as_secs();
        eprintln!("⚠️  Excessive rate limiting detected. Pausing for {} minutes to allow rate limits to reset...", pause_secs / 60);
        tokio::time::sleep(pause_duration).await;
        eprintln!("✅ Resuming after rate limit pause");
        // Reset counter after pause
        with_rate_limit_tracker(|tracker| tracker.record_success());
    }

    let client = reqwest::Client::builder().timeout(cfg.timeout).build()?;

    // Keep the message contract the same but a tad stricter about JSON
    let system = ChatMessage {
        role: "system",
        content: ChatContent::Text(
            "You are a UI clicking assistant. Choose exactly one candidate that best \
             matches the user's intent. Respond ONLY with JSON in this exact shape: \
             {\"id\": <number>, \"reason\": \"...\", \"confidence\": <number 0..1>}"
                .to_string(),
        ),
    };

    // We pass a compact list — if you want, you can add extra fields
    let user = ChatMessage {
        role: "user",
        content: ChatContent::Text(format!(
            "Task: {}\n\nCandidates (index, tag, text, aria):\n{}\n\n\
             Return ONLY JSON with fields id, reason, confidence.",
            user_prompt,
            serde_json::to_string(&candidates)?,
        )),
    };

    let req_body = ChatRequest {
        model: &cfg.model,
        temperature: 1.0, // be decisive
        response_format: ResponseFormat::JsonObject,
        messages: vec![system, user],
    };

    let url = format!("{}/chat/completions", cfg.base_url);
    let mut last_err: Option<anyhow::Error> = None;
    let mut rate_limited = false;

    for attempt in 0..cfg.max_retries {
        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api_key)
            .json(&req_body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() {
                    let headers = r.headers().clone();
                    let text = r.text().await.unwrap_or_default();
                    if status.as_u16() == 429 {
                        rate_limited = true;
                        let wait_ms = compute_rate_limit_sleep_ms(&headers, &text, attempt);
                        eprintln!("⏳ 429 rate-limited (attempt {}/{}) sleep {}ms",
                                  attempt + 1, cfg.max_retries, wait_ms);
                        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!("OpenAI HTTP {}: {}", status, text));
                } else {
                    // Success - reset rate limit tracker
                    with_rate_limit_tracker(|tracker| tracker.record_success());
                    
                    let parsed: ChatResponse = r.json().await?;
                    let content = parsed
                        .choices
                        .get(0)
                        .ok_or_else(|| anyhow::anyhow!("No choices from OpenAI"))?
                        .message
                        .content
                        .trim()
                        .to_string();

                    let cleaned = strip_code_fences(&content);
                    match serde_json::from_str::<ClickDecision>(cleaned) {
                        Ok(d) => {
                            println!(
                                "[click_by_llm_dom_first] decision raw: {}",
                                content.replace('\n', " ")
                            );
                            return Ok(d);
                        }
                        Err(e) => {
                            last_err = Some(anyhow::anyhow!(
                                "Failed to parse click decision: {}\nRaw: {}",
                                e, content
                            ));
                        }
                    }
                }
            }
            Err(e) => last_err = Some(anyhow::anyhow!(e)),
        }

        if attempt + 1 < cfg.max_retries {
            tokio::time::sleep(std::time::Duration::from_millis(350 * (attempt as u64 + 1))).await;
        }
    }

    // If we exhausted retries due to rate limiting, record the failure
    if rate_limited {
        with_rate_limit_tracker(|tracker| tracker.record_failure());
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("OpenAI decision request failed")))
}

// ---------- Heuristic fallback (deterministic) ----------

fn rank_score(prompt: &str, c: &UiCandidate, rect: Option<(i32,i32,i32,i32)>) -> f32 {
    // Simple, explainable scoring
    let p = prompt.to_lowercase();
    let t = c.text.to_lowercase();
    let a = c.aria.to_lowercase();

    // exact word hits (length >= 3)
    let hits = p.split_whitespace()
        .filter(|w| w.len() >= 3)
        .filter(|w| t.contains(&w.to_lowercase()) || a.contains(&w.to_lowercase()))
        .count() as f32;

    // semantic nudges
    let mut sem = 0.0;
    if c.tag == "BUTTON" { sem += 0.6; }
    if t.contains("send") || a.contains("send") { sem += 1.0; }
    if t.contains("submit") || a.contains("submit") { sem += 0.9; }
    if t.contains("save") || a.contains("save") { sem += 0.6; }

    // size bonus
    let mut size = 0.0;
    let mut center = 0.0;
    if let Some((x, y, w, h)) = rect {
        let area = (w.max(0) * h.max(0)) as f32;
        size = (area.sqrt() / 60.0).min(1.0);
        let vw: i32 = std::env::var("VIEWPORT_W").ok().and_then(|s| s.parse().ok()).unwrap_or(1280);
        let vh: i32 = std::env::var("VIEWPORT_H").ok().and_then(|s| s.parse().ok()).unwrap_or(800);
        let cx = x + w/2;
        let cy = y + h/2;
        let dx = (cx - vw/2) as f32;
        let dy = (cy - vh/2) as f32;
        let dist = (dx*dx + dy*dy).sqrt();
        center = (1.0 - (dist / 1100.0)).clamp(0.0, 0.5);
    }

    hits * 1.0 + sem + size * 0.6 + center
}

fn choose_best_by_heuristic(prompt: &str, cands: &[Candidate]) -> usize {
    // Filter visible & enabled
    let mut scored: Vec<(usize, f32, i32)> = Vec::new(); // (idx, score, area)
    for (i, c) in cands.iter().enumerate() {
        if !c.visible || c.disabled {
            continue;
        }
        let area = c.rect.map(|(_,_,w,h)| w.max(0)*h.max(0)).unwrap_or(0);
        let s = rank_score(prompt, &c.meta, c.rect);
        scored.push((i, s, area));
    }

    if scored.is_empty() {
        // fallback to first
        return 0;
    }

    // Sort: score desc, area desc, id asc (deterministic)
    scored.sort_by(|a, b| {
        use std::cmp::Ordering::*;
        b.1.partial_cmp(&a.1).unwrap_or(Equal)
            .then(b.2.cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });

    let (best, best_s, _) = scored[0];
    println!("(fallback) chose #{best} with score {:.3}", best_s);
    best
}

// ---------- Main entry (unchanged signature) ----------

pub async fn click_by_llm_dom_first(
    driver: &WebDriver,
    cfg: &OpenAIConfig,
    user_prompt: &str,
    force_double: Option<bool>,
) -> Result<()> {
    let cands = collect_ui_candidates(driver, 200).await?;
    if cands.is_empty() {
        anyhow::bail!("No clickable candidates found on page");
    }

    // Send a slimmed list to the model (only the serializable UiCandidate)
    let ui_list: Vec<UiCandidate> = cands.iter().map(|c| c.meta.clone()).collect();
    let decision = call_openai_for_dom_decision(cfg, user_prompt, &ui_list).await;

    // Resolve index
    let idx = match decision {
        Ok(d) => {
            /*
	    println!(
                "[click_by_llm_dom_first] decision: id={:?} reason={:?} confidence={:?}",
                d.id, d.reason, d.confidence
            );
	    */
            match d.id {
                Some(i) if i < cands.len() => i,
                _ => {
                    // invalid id → heuristic
                    choose_best_by_heuristic(user_prompt, &cands)
                }
            }
        }
        Err(e) => {
            eprintln!("LLM decision failed → heuristic fallback: {e}");
            choose_best_by_heuristic(user_prompt, &cands)
        }
    };

    let el = &cands[idx].el;

    // Prefer WebDriver click first (more semantically correct)
    // If your site needs a pointer-based click at center, you can compute it from rect().
    if force_double.unwrap_or(false) {
        el.click().await?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        el.click().await?;
    } else {
        el.click().await?;
    }

    println!(
        "🖱️ clicked: idx={} tag={} text={:?} aria={:?}",
        idx, cands[idx].meta.tag, cands[idx].meta.text, cands[idx].meta.aria
    );

    Ok(())
}

//END OF DOM TESTING

pub async fn call_openai_for_point(
    cfg: &OpenAIConfig,
    screenshot_png: &[u8],
    user_prompt: &str,
) -> Result<ViewportPoint> {
    // Check if we should pause due to excessive rate limiting BEFORE spawning concurrent requests
    let should_pause = with_rate_limit_tracker(|tracker| tracker.should_pause());
    if should_pause {
        let pause_duration = with_rate_limit_tracker(|tracker| tracker.get_pause_duration());
        let pause_secs = pause_duration.as_secs();
        eprintln!("⚠️  Excessive rate limiting detected. Pausing for {} minutes to allow rate limits to reset...", pause_secs / 60);
        tokio::time::sleep(pause_duration).await;
        eprintln!("✅ Resuming after rate limit pause");
        // Reset counter after pause
        with_rate_limit_tracker(|tracker| tracker.record_success());
    }

    let samples: usize = env::var("OPENAI_SAMPLES_PER_CALL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);

    let max_conc: usize = env::var("OPENAI_MAX_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4)
        .max(1);

    // Optional per-task stagger to smooth bursts (reduces RPM/TPM spikes)
    let stagger_ms: u64 = env::var("OPENAI_STAGGER_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    println!(
        "🤖 Sampling OpenAI {} times (IQR-filtered mean combine, concurrency={}, stagger={}ms...",
        samples, max_conc, stagger_ms
    );

    let mut set = JoinSet::new();
    let cfg_cloned = cfg.clone();
    let img = screenshot_png.to_vec();
    let prompt = user_prompt.to_string();

    // spawn initial batch
    let initial = std::cmp::min(samples, max_conc);
    for i in 0..initial {
        let cfg_i = cfg_cloned.clone();
        let img_i = img.clone();
        let prompt_i = prompt.clone();
        let stagger = stagger_ms;
        set.spawn(async move {
            if stagger > 0 {
                // smear the first wave: 120, 240, ..., up to ~960ms
                let delay = stagger * ((i as u64 % 8) + 1);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let res = call_openai_once(&cfg_i, &img_i, &prompt_i).await;
            (i, res)
        });
    }
    let mut launched = initial;

    let mut results: Vec<ViewportPoint> = Vec::with_capacity(samples);
    let mut rate_limit_failures = 0;
    let mut total_failures = 0;
    
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, Ok(pt))) => {
                println!("   → Sample {}: x={}, y={}, double={}", idx + 1, pt.x, pt.y, pt.double);
                results.push(pt);
            }
            Ok((_idx, Err(e))) => {
                total_failures += 1;
                // Check if the error is related to rate limiting
                // call_openai_once now includes "(rate limited)" in the error message
                // when it encounters 429 errors, so we can detect it
                let error_str = e.to_string().to_lowercase();
                if error_str.contains("429") || error_str.contains("rate") || 
                   error_str.contains("rate limit") || error_str.contains("ratelimit") ||
                   error_str.contains("(rate limited)") {
                    rate_limit_failures += 1;
                }
                eprintln!("   ⚠️ sample failed: {e}");
            }
            Err(e) => {
                total_failures += 1;
                eprintln!("   ⚠️ task join error: {e}");
            }
        }

        if launched < samples {
            let cfg_i = cfg_cloned.clone();
            let img_i = img.clone();
            let prompt_i = prompt.clone();
            let idx = launched;
            let stagger = stagger_ms;
            set.spawn(async move {
                if stagger > 0 {
                    let delay = stagger * ((idx as u64 % 8) + 1);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let res = call_openai_once(&cfg_i, &img_i, &prompt_i).await;
                (idx, res)
            });
            launched += 1;
        }
    }

    // Track failures: if all samples failed, it's likely rate limiting
    // Record a single failure (not per sample) to avoid overwhelming the tracker
    if results.is_empty() {
        // All samples failed - if we saw rate limit errors, definitely record it
        // Even if we didn't detect them in error messages, concurrent failures
        // when making many requests is very likely rate limiting
        with_rate_limit_tracker(|tracker| tracker.record_failure());
        if rate_limit_failures > 0 {
            eprintln!("   ⚠️ All {} samples failed ({} confirmed rate-limit related)", 
                      samples, rate_limit_failures);
        } else {
            eprintln!("   ⚠️ All {} samples failed (likely rate limiting)", samples);
        }
    } else if rate_limit_failures > 0 && rate_limit_failures >= total_failures / 2 {
        // More than half of failures were rate-limit related
        // This suggests we're hitting rate limits even if some requests succeed
        with_rate_limit_tracker(|tracker| tracker.record_failure());
    } else if results.len() > 0 {
        // We got at least one successful result - reset the failure counter
        // This means we're not completely blocked
        with_rate_limit_tracker(|tracker| tracker.record_success());
    }

    if results.is_empty() {
        anyhow::bail!("All OpenAI samples failed");
    }

    let agg = aggregate_points(&results);
    if let Err(e) = save_dotmap_png(screenshot_png, &results, agg) {
        eprintln!("(non-fatal) failed to write dot map: {e}");
    }

    Ok(agg)
}

async fn call_openai_once(
    cfg: &OpenAIConfig,
    screenshot_png: &[u8],
    user_prompt: &str,
) -> Result<ViewportPoint> {
    // Check if we should pause due to excessive rate limiting
    let should_pause = with_rate_limit_tracker(|tracker| tracker.should_pause());
    if should_pause {
        let pause_duration = with_rate_limit_tracker(|tracker| tracker.get_pause_duration());
        let pause_secs = pause_duration.as_secs();
        eprintln!("⚠️  Excessive rate limiting detected. Pausing for {} minutes to allow rate limits to reset...", pause_secs / 60);
        tokio::time::sleep(pause_duration).await;
        eprintln!("✅ Resuming after rate limit pause");
        // Reset counter after pause
        with_rate_limit_tracker(|tracker| tracker.record_success());
    }

    let client = reqwest::Client::builder().timeout(cfg.timeout).build()?;

    let overlay_enabled = env::var("OPENAI_OVERLAY_GRID")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);

    let annotated_png = if overlay_enabled {
        let grid_opts = GridOptions::from_env();
        overlay_grid_with_coords(screenshot_png, grid_opts)
            .context("overlay grid on screenshot")?
    } else {
        screenshot_png.to_vec()
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&annotated_png);
    let data_url = format!("data:image/png;base64,{}", b64);
    let full_prompt = format!(
        "{}\nReturn only JSON in the exact form {{\"x\":int,\"y\":int,\"double\":bool}}.",
        user_prompt
    );

    let messages = vec![
        ChatMessage {
            role: "system",
            content: ChatContent::Text(format!(
                "You are selecting a single click target on the image. \
                 Output ONLY JSON (no markdown fences, no prose) with keys x:int,y:int,double:bool. \
                 Coordinates are CSS/viewport pixels relative to the visible page (top-left). \
		 Be specific, do not estimate."
            )),
        },
        ChatMessage {
            role: "user",
            content: ChatContent::Parts(vec![
                ContentPart::Text { text: full_prompt },
                ContentPart::ImageUrl { image_url: ImageUrl { url: data_url } },
            ]),
        },
    ];

    let req_body = ChatRequest {
        model: &cfg.model,
        temperature: 1.0,
        response_format: ResponseFormat::JsonObject,
        messages,
    };

    let url = format!("{}/chat/completions", cfg.base_url);
    let mut last_err: Option<anyhow::Error> = None;
    let mut encountered_429 = false;

    for attempt in 0..cfg.max_retries {
        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api_key)
            .json(&req_body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() {
                    // Grab headers & body for rate-limit hints
                    let headers = r.headers().clone();
			
		    let text = r.text().await.unwrap_or_default();
  		    
                    if status.as_u16() == 429 {
                        encountered_429 = true;
                        let wait_ms = compute_rate_limit_sleep_ms(&headers, &text, attempt);
                        eprintln!(
                            "⏳ 429 rate-limited (attempt {}/{}). Sleeping ~{} ms",
                            attempt + 1, cfg.max_retries, wait_ms
                        );
                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                        continue; // retry after sleeping
                    }

                    // other non-success -> record and try again with small backoff
                    last_err = Some(anyhow::anyhow!("OpenAI HTTP {}: {}", status, text));
                } else {
                    // Success - reset rate limit tracker
                    with_rate_limit_tracker(|tracker| tracker.record_success());
                    
                    // (optional) read rate limit info for diagnostic
                    let remaining_tokens = r.headers()
                        .get("x-ratelimit-remaining-tokens")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());
                    
                    let remaining_requests = r.headers()
                        .get("x-ratelimit-remaining-requests")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());
                    
                    let limit_requests = r.headers()
                        .get("x-ratelimit-limit-requests")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());

                    let parsed: ChatResponse = r.json().await?;
                    
                    // Log token usage and rate limit info from this request
                    if let Some(ref usage) = parsed.usage {
                        if let Some(remaining) = remaining_tokens {
                            if let Some(remaining_rpm) = remaining_requests {
                                if let Some(limit_rpm) = limit_requests {
                                    println!("(diag) tokens used: {} (prompt: {}, completion: {}) | remaining: {} | RPM: {}/{}",
                                        usage.total_tokens, usage.prompt_tokens, usage.completion_tokens, remaining, remaining_rpm, limit_rpm);
                                } else {
                                    println!("(diag) tokens used: {} (prompt: {}, completion: {}) | remaining: {} | RPM remaining: {}",
                                        usage.total_tokens, usage.prompt_tokens, usage.completion_tokens, remaining, remaining_rpm);
                                }
                            } else {
                                println!("(diag) tokens used: {} (prompt: {}, completion: {}) | remaining: {}",
                                    usage.total_tokens, usage.prompt_tokens, usage.completion_tokens, remaining);
                            }
                        } else {
                            println!("(diag) tokens used: {} (prompt: {}, completion: {})",
                                usage.total_tokens, usage.prompt_tokens, usage.completion_tokens);
                        }
                    }
                    let content = parsed
                        .choices
                        .get(0)
                        .ok_or_else(|| anyhow::anyhow!("No choices from OpenAI"))?
                        .message
                        .content
                        .trim()
                        .to_string();

                    let cleaned = strip_code_fences(&content);
                    
		    match serde_json::from_str::<ViewportPoint>(cleaned) {
                        Ok(pt) => return Ok(pt),
			Err(e) => {
                            last_err = Some(anyhow::anyhow!(
                                "Failed to parse JSON from OpenAI: {}\nRaw content: {}",
                                e,
                                content
                            ));
                        }
                    }
                }
            }
            Err(e) => last_err = Some(anyhow::anyhow!(e)),
        }

        if attempt + 1 < cfg.max_retries {
            // small linear backoff for non-429 errors
            tokio::time::sleep(Duration::from_millis(400 * (attempt as u64 + 1))).await;
        }
    }

    // Note: We don't record failures here because call_openai_once is only called
    // from call_openai_for_point, which handles failure tracking at a higher level
    // to avoid recording multiple failures for concurrent requests
    // However, we include rate limit info in the error message for better detection

    let error_msg = if encountered_429 {
        "OpenAI request failed (rate limited)".to_string()
    } else {
        "OpenAI request failed".to_string()
    };
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!(error_msg)))
}

fn aggregate_points(points: &[ViewportPoint]) -> ViewportPoint {
    // Compute IQR-based filtered mean
    fn filtered_mean(mut v: Vec<i32>) -> i32 {
        if v.is_empty() {
            return 0;
        }
        v.sort_unstable();
        let n = v.len();

        // If fewer than 4 points, just return mean directly
        if n < 4 {
            let sum: i32 = v.iter().sum();
            return sum / (n as i32);
        }

        // Compute quartiles (Q1, Q3)
        let q1 = v[n / 4];
        let q3 = v[(3 * n) / 4];
        let iqr = q3 - q1;

        // Define bounds: Q1 - 1.5×IQR, Q3 + 1.5×IQR
        let lower = q1 - (iqr * 3 / 2);
        let upper = q3 + (iqr * 3 / 2);

        // Filter out outliers (clone to keep v for fallback)
        let filtered: Vec<i32> = v
            .iter()
            .cloned()
            .filter(|&x| x >= lower && x <= upper)
            .collect();

        if filtered.is_empty() {
            // fallback to mean of all values if everything filtered out
            let sum: i32 = v.iter().sum();
            return sum / (n as i32);
        }

        // Compute mean of filtered
        let sum: i32 = filtered.iter().sum();
        sum / (filtered.len() as i32)
    }

    let xs: Vec<i32> = points.iter().map(|p| p.x).collect();
    let ys: Vec<i32> = points.iter().map(|p| p.y).collect();
    let doubles = points.iter().filter(|p| p.double).count();

    ViewportPoint {
        x: filtered_mean(xs),
        y: filtered_mean(ys),
        double: doubles * 2 >= points.len(),
    }
}

fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        if let Some(end) = rest.strip_suffix("```") {
            return end.trim();
        }
    }
    if let Some(rest) = s.strip_prefix("```") {
        if let Some(end) = rest.strip_suffix("```") {
            return end.trim();
        }
    }
    s
}

/* -------------------- Rate-limit helpers -------------------- */

fn parse_seconds_str_to_ms(s: &str) -> Option<u64> {
    // Accepts "1.686s" or "2" (seconds)
    let t = s.trim().trim_end_matches('s').trim();
    if t.is_empty() { return None; }
    if let Ok(v) = t.parse::<f64>() {
        return Some((v * 1000.0).round() as u64);
    }
    None
}

fn extract_wait_ms_from_headers(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    // Prefer explicit reset headers if present
    for key in [
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
        "retry-after",
    ] {
        if let Some(val) = headers.get(key) {
            if let Ok(s) = val.to_str() {
                if let Some(ms) = parse_seconds_str_to_ms(s) { return Some(ms); }
                // retry-after can be integer seconds
                if let Ok(sec) = s.parse::<u64>() { return Some(sec * 1000); }
            }
        }
    }
    None
}

fn extract_wait_ms_from_body(body: &str) -> Option<u64> {
    // Look for "...Please try again in 1.686s."
    if let Some(pos) = body.find("Please try again in") {
        let tail = &body[pos + "Please try again in".len()..];
        let tail = tail.trim_start();
        // read until the next space or period
        let mut num = String::new();
        for ch in tail.chars() {
            if ch.is_ascii_digit() || ch == '.' { num.push(ch); }
            else { break; }
        }
        if !num.is_empty() {
            return parse_seconds_str_to_ms(&(num + "s"));
        }
    }
    None
}

/// Decide how long to sleep for a 429, using headers first, then body, then a fallback.
fn compute_rate_limit_sleep_ms(
    headers: &reqwest::header::HeaderMap,
    body: &str,
    attempt: usize,
) -> u64 {
    if let Some(ms) = extract_wait_ms_from_headers(headers) { return ms; }
    if let Some(ms) = extract_wait_ms_from_body(body) { return ms; }
    // fallback exponential-ish backoff with cap
    let base = 600u64; // 0.6s
    (base * (attempt as u64 + 1)).min(8_000) // cap at 8s
}

/* -------------------- Heat dotmap helpers (time-based) -------------------- */

/// Get the largest numbered run directory without creating a new one.
/// Returns the directory if found, or None if no run directories exist.
pub fn get_largest_run_dir() -> Option<PathBuf> {
    // Check if RUN_DIR is already set (current run directory)
    if let Ok(dir) = std::env::var("RUN_DIR") {
        let path = PathBuf::from(&dir);
        if path.exists() && path.is_dir() {
            return Some(path);
        }
    }
    
    // Otherwise, find the largest numbered run directory
    let base_dir = PathBuf::from("runs");
    if !base_dir.exists() {
        return None;
    }
    
    let mut max_run_num = 0;
    let mut max_run_dir: Option<PathBuf> = None;
    
    // Read the base directory and find the highest run number
    if let Ok(entries) = fs::read_dir(&base_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("run-") {
                    // Try to parse the number after "run-"
                    if let Some(num_str) = name.strip_prefix("run-") {
                        if let Ok(num) = num_str.parse::<usize>() {
                            if num > max_run_num {
                                max_run_num = num;
                                max_run_dir = Some(entry.path());
                            }
                        }
                    }
                }
            }
        }
    }
    
    max_run_dir
}

fn ensure_run_dir() -> PathBuf {
    let base_dir = if let Ok(dir) = std::env::var("RUN_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from("runs")
    };
    
    // Check if the path is already a specific run folder (contains "run-")
    let path_str = base_dir.to_string_lossy();
    if path_str.contains("run-") && base_dir.exists() && base_dir.is_dir() {
        // It's already a specific run folder, use it as-is
        let _ = fs::create_dir_all(&base_dir);
        return base_dir;
    }
    
    // It's a base directory, find or create the next sequential run folder
    let _ = fs::create_dir_all(&base_dir);
    
    let mut max_run_num = 0;
    
    // Read the base directory and find the highest run number
    if let Ok(entries) = fs::read_dir(&base_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("run-") {
                    // Try to parse the number after "run-"
                    if let Some(num_str) = name.strip_prefix("run-") {
                        if let Ok(num) = num_str.parse::<usize>() {
                            max_run_num = max_run_num.max(num);
                        }
                    }
                }
            }
        }
    }
    
    // Create the next run folder (run-001, run-002, etc.)
    let next_run_num = max_run_num + 1;
    let p = base_dir.join(format!("run-{:03}", next_run_num));
    let _ = fs::create_dir_all(&p);
    std::env::set_var("RUN_DIR", &p);
    p
}

fn dotmap_path_timebased() -> PathBuf {
    let run_dir = ensure_run_dir();
    if let Ok(step_str) = std::env::var("CURRENT_STEP_NO") {
        if let Ok(step_no) = step_str.parse::<usize>() {
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
            return run_dir.join(format!("step-{:02}-llm-dots-{}.png", step_no, ms));
        }
    }
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    run_dir.join(format!("llm-dots-{}.png", ms))
}

fn draw_filled_circle(img: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Rgba<u8>) {
    let (w, h) = img.dimensions();
    let (w, h) = (w as i32, h as i32);
    let r2 = radius * radius;
    for dy in -radius..=radius {
        let y = cy + dy;
        if y < 0 || y >= h { continue; }
        for dx in -radius..=radius {
            let x = cx + dx;
            if x < 0 || x >= w { continue; }
            if dx*dx + dy*dy <= r2 {
                img.put_pixel(x as u32, y as u32, color);
            }
        }
    }
}

fn save_dotmap_png(
    original_screenshot_png: &[u8],
    samples: &[ViewportPoint],
    aggregate: ViewportPoint,
) -> Result<()> {
    let overlay_enabled = std::env::var("OPENAI_OVERLAY_GRID")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);

    let base_png = if overlay_enabled {
        let opts = GridOptions::from_env();
        overlay_grid_with_coords(original_screenshot_png, opts)?
    } else {
        original_screenshot_png.to_vec()
    };

    let mut rgba: RgbaImage = image::load_from_memory(&base_png)?.to_rgba8();
    let (w, h) = rgba.dimensions();

    let x_off = std::env::var("CLICK_X_OFFSET_PX")
	.ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y_off = std::env::var("CLICK_Y_OFFSET_PX")
	.ok().and_then(|s| s.parse().ok()).unwrap_or(0);

    let sample_color = Rgba([255, 0, 0, 200]);
    let agg_outline = Rgba([0, 0, 0, 255]);
    let agg_fill = Rgba([255, 255, 255, 255]);

    for p in samples {
        let mut x = p.x.clamp(0, (w as i32) - 1);
        let mut y = p.y.clamp(0, (h as i32) - 1);
	x += x_off;
	y += y_off;
        draw_filled_circle(&mut rgba, x, y, 4, sample_color);
    }

    let mut ax = aggregate.x.clamp(0, (w as i32) - 1);
    let mut ay = aggregate.y.clamp(0, (h as i32) - 1);

    ax += x_off;
    ay += y_off;    

    draw_filled_circle(&mut rgba, ax, ay, 8, agg_outline);
    draw_filled_circle(&mut rgba, ax, ay, 5, agg_fill);

    let path = dotmap_path_timebased();
    if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); }
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut out), ImageOutputFormat::Png)?;
    fs::write(&path, &out)?;
    println!("🟡 Saved LLM dotmap to {}", path.display());
    Ok(())
}

#[derive(Deserialize, Debug)]
pub struct BooleanResponse {
    pub answer: bool,
    pub confidence: Option<f32>,
    pub reasoning: Option<String>,
}

/// Ask OpenAI a yes/no question about a screenshot from the existing automation and get a boolean response
pub async fn ask_boolean_question(
    cfg: &OpenAIConfig,
    screenshot_png: &[u8],
    question: &str,
) -> Result<BooleanResponse> {
    // Check if we should pause due to excessive rate limiting
    let should_pause = with_rate_limit_tracker(|tracker| tracker.should_pause());
    if should_pause {
        let pause_duration = with_rate_limit_tracker(|tracker| tracker.get_pause_duration());
        let pause_secs = pause_duration.as_secs();
        eprintln!("⚠️  Excessive rate limiting detected. Pausing for {} minutes to allow rate limits to reset...", pause_secs / 60);
        tokio::time::sleep(pause_duration).await;
        eprintln!("✅ Resuming after rate limit pause");
        // Reset counter after pause
        with_rate_limit_tracker(|tracker| tracker.record_success());
    }

    let client = reqwest::Client::builder().timeout(cfg.timeout).build()?;
    
    let b64 = base64::engine::general_purpose::STANDARD.encode(screenshot_png);
    let data_url = format!("data:image/png;base64,{}", b64);
    
    let full_prompt = format!(
        "{}\n\nReturn ONLY JSON in the exact form {{\"answer\":bool,\"confidence\":float,\"reasoning\":\"string\"}}. \
         answer must be true or false.",
        question
    );
    
    let messages = vec![
        ChatMessage {
            role: "system",
            content: ChatContent::Text(
                "You are analyzing screenshots from an automation and answering yes/no questions. \
                 Output ONLY JSON (no markdown fences, no prose) with keys answer:bool, confidence:float, reasoning:string. \
                 answer must be true or false. confidence should be between 0.0 and 1.0.".to_string()
            ),
        },
        ChatMessage {
            role: "user",
            content: ChatContent::Parts(vec![
                ContentPart::Text { text: full_prompt },
                ContentPart::ImageUrl { image_url: ImageUrl { url: data_url } },
            ]),
        },
    ];
    
    let req_body = ChatRequest {
        model: &cfg.model,
        temperature: 1.0, // Lower temperature for more consistent boolean answers
        response_format: ResponseFormat::JsonObject,
        messages,
    };
    
    let url = format!("{}/chat/completions", cfg.base_url);
    let mut last_err: Option<anyhow::Error> = None;
    let mut rate_limited = false;
    
    for attempt in 0..cfg.max_retries {
        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api_key)
            .json(&req_body)
            .send()
            .await;
        
        match resp {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() {
                    let headers = r.headers().clone();
                    let text = r.text().await.unwrap_or_default();
                    
                    if status.as_u16() == 429 {
                        rate_limited = true;
                        let wait_ms = compute_rate_limit_sleep_ms(&headers, &text, attempt);
                        eprintln!(
                            "⏳ 429 rate-limited (attempt {}/{}). Sleeping ~{} ms",
                            attempt + 1, cfg.max_retries, wait_ms
                        );
                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                        continue;
                    }
                    
                    last_err = Some(anyhow::anyhow!("OpenAI HTTP {}: {}", status, text));
                } else {
                    // Success - reset rate limit tracker
                    with_rate_limit_tracker(|tracker| tracker.record_success());
                    
                    let parsed: ChatResponse = r.json().await?;
                    let content = parsed
                        .choices
                        .get(0)
                        .ok_or_else(|| anyhow::anyhow!("No choices from OpenAI"))?
                        .message
                        .content
                        .trim()
                        .to_string();
                    
                    let cleaned = strip_code_fences(&content);
                    
                    match serde_json::from_str::<BooleanResponse>(cleaned) {
                        Ok(result) => {
                            println!("🤔 Question: {}", question);
                            println!("   Answer: {} (confidence: {:.2})", 
                                result.answer, 
                                result.confidence.unwrap_or(0.0)
                            );
                            if let Some(ref reasoning) = result.reasoning {
                                println!("   Reasoning: {}", reasoning);
                            }
                            return Ok(result);
                        }
                        Err(e) => {
                            last_err = Some(anyhow::anyhow!(
                                "Failed to parse JSON from OpenAI: {}\nRaw content: {}",
                                e,
                                content
                            ));
                        }
                    }
                }
            }
            Err(e) => last_err = Some(anyhow::anyhow!(e)),
        }
        
        if attempt + 1 < cfg.max_retries {
            tokio::time::sleep(Duration::from_millis(400 * (attempt as u64 + 1))).await;
        }
    }
    
    // If we exhausted retries due to rate limiting, record the failure
    if rate_limited {
        with_rate_limit_tracker(|tracker| tracker.record_failure());
    }
    
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("OpenAI boolean question request failed")))
}

