use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;
use crate::overlay::{overlay_grid_with_coords, GridOptions};

// --- drawing + saving imports ---
use image::{DynamicImage, ImageOutputFormat, Rgba, RgbaImage};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::task::JoinSet;

// NEW: simple in-process memory of previously chosen points
use std::sync::{Mutex, OnceLock};

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

/*


static PREV_POINTS: OnceLock<Mutex<Vec<ViewportPoint>>> = OnceLock::new();

fn prev_points() -> &'static Mutex<Vec<ViewportPoint>> {
    PREV_POINTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn format_prev_points(list: &[ViewportPoint]) -> String {
    list.iter()
        .map(|p| format!("({}, {})", p.x, p.y))
        .collect::<Vec<_>>()
        .join(", ")
}

*/

pub async fn call_openai_for_point(
    cfg: &OpenAIConfig,
    screenshot_png: &[u8],
    user_prompt: &str,
) -> Result<ViewportPoint> {
    let samples: usize = env::var("OPENAI_SAMPLES_PER_CALL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);

    
    //if samples == 1 {
    //    let pt = call_openai_once(cfg, screenshot_png, user_prompt).await?;
    //    if let Err(e) = save_dotmap_png(screenshot_png, &[pt], pt) {
    //        eprintln!("(non-fatal) failed to write dot map: {e}");
    //    }
    //    return Ok(pt);
    //}
    let (my_x, my_y) = env_offset();
    if samples == 1 {
    	let raw = call_openai_once(cfg, screenshot_png, user_prompt).await?;
    	let (x_off, y_off) = env_offset();
    	let shifted = add_offset(raw, x_off, y_off);

    	// dotmap and returned value both use shifted coords
    	if let Err(e) = save_dotmap_png(screenshot_png, &[shifted], shifted) {
        	eprintln!("(non-fatal) failed to write dot map: {e}");
    	}
    	return Ok(shifted);
    }

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
        "🤖 Sampling OpenAI {} times (IQR-filtered mean combine, concurrency={}, stagger={}ms, offset={},{})...",
        samples, max_conc, stagger_ms, my_x, my_y
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
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, Ok(pt))) => {
                println!("   → Sample {}: x={}, y={}, double={}", idx + 1, pt.x, pt.y, pt.double);
                results.push(pt);
            }
            Ok((_idx, Err(e))) => {
                eprintln!("   ⚠️ sample failed: {e}");
            }
            Err(e) => eprintln!("   ⚠️ task join error: {e}"),
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

    if results.is_empty() {
        anyhow::bail!("All OpenAI samples failed");
    }

    let agg = aggregate_points(&results);
    if let Err(e) = save_dotmap_png(screenshot_png, &results, agg) {
        eprintln!("(non-fatal) failed to write dot map: {e}");
    }

    Ok(agg)
}

fn env_offset() -> (i32, i32) {
    let x_off = std::env::var("CLICK_X_OFFSET_PX")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y_off = std::env::var("CLICK_Y_OFFSET_PX")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    (x_off, y_off)
}

fn add_offset(pt: ViewportPoint, x_off: i32, y_off: i32) -> ViewportPoint {
    ViewportPoint { x: pt.x + x_off, y: pt.y + y_off, double: pt.double }
}

async fn call_openai_once(
    cfg: &OpenAIConfig,
    screenshot_png: &[u8],
    user_prompt: &str,
) -> Result<ViewportPoint> {
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

    /*
    // NEW: build a prompt that lists prior points to avoid
    let radius_px: i32 = env::var("OPENAI_DEDUP_RADIUS")
        .ok().and_then(|s| s.parse().ok())
        .unwrap_or(40);

    let prev_snapshot: Vec<ViewportPoint> = {
        let guard = prev_points().lock().unwrap();
        guard.clone()
    };

    let used_coords_text = if prev_snapshot.is_empty() {
        String::new()
    } else {
        format!(
            "Previously selected points (avoid selecting within ~{} px of any): [{}]\n",
            radius_px,
            format_prev_points(&prev_snapshot)
        )
    };
	
  */

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
        temperature: 0.0,
        response_format: ResponseFormat::JsonObject,
        messages,
    };

    let url = format!("{}/chat/completions", cfg.base_url);
    let mut last_err: Option<anyhow::Error> = None;

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
                    // (optional) read limits for diagnostic
                    if let Some(v) = r.headers().get("x-ratelimit-limit-tokens") {
                        if let Ok(_s) = v.to_str() {
                            // println!("(diag) TPM cap reported: {}", _s);
                        }
                    }

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

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("OpenAI request failed")))
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

fn ensure_run_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RUN_DIR") {
        let p = PathBuf::from(dir);
        let _ = fs::create_dir_all(&p);
        return p;
    }
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let p = Path::new("runs").join(format!("run-{}", ms));
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

    let sample_color = Rgba([255, 0, 0, 200]);
    let agg_outline = Rgba([0, 0, 0, 255]);
    let agg_fill = Rgba([255, 255, 255, 255]);

    for p in samples {
        let x = p.x.clamp(0, (w as i32) - 1);
        let y = p.y.clamp(0, (h as i32) - 1);
        draw_filled_circle(&mut rgba, x, y, 4, sample_color);
    }

    let ax = aggregate.x.clamp(0, (w as i32) - 1);
    let ay = aggregate.y.clamp(0, (h as i32) - 1);
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
