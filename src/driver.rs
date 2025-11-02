// src/driver.rs (windowed, not fullscreen)
use anyhow::{bail, Context, Result};
use std::env;
use std::fs::File;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thirtyfour::prelude::*;
use serde_json::Value;
use which::which;
 
pub struct DriverBundle {
    pub driver: WebDriver,
    pub chromedriver_child: Child,
    pub user_data_dir: PathBuf,
    pub display: String,
}
 
pub async fn init_driver(login_url: &str) -> Result<DriverBundle> {
    let _ = dotenvy::dotenv();
 
    let headful = env::var("HEADFUL").map_or(true, |v| v == "1");
    if !headful {
        bail!("OS-level cursor requires headful mode/VNC. Set HEADFUL=1.");
    }
 
    let display = env::var("DISPLAY_VNC").unwrap_or_else(|_| String::from(":1"));
    let driver_port: u16 = env::var("CHROMEDRIVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9515);
 
    let chromedriver_path =
        which("chromedriver").context("chromedriver not found in PATH. Install it or add to PATH.")?;
 
    let xauth = guess_xauthority()?;
 
    let log_file = File::create(log_path()).context("cannot create chromedriver.log")?;
 
    let chromedriver = spawn_chromedriver(
        chromedriver_path.as_path(),
        driver_port,
        &display,
        xauth.as_deref(),
        log_file,
    )?;
    wait_for_port("127.0.0.1", driver_port, Duration::from_secs(10))
        .context("chromedriver did not become ready on time")?;
 
    // ---- Build Chrome caps (WINDOWED) ----
    let mut caps = DesiredCapabilities::chrome();
 
    if let Ok(bin) = env::var("CHROME_BIN") {
        caps.set_binary(&bin)?;
    } else if let Some(bin) = find_chrome_bin() {
        caps.set_binary(&bin)?;
    }
 
    // Fresh profile per run
    let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let mut user_data_dir = env::temp_dir();
    user_data_dir.push(format!("interactive-webdriver-{}", timestamp_ms));
    caps.add_arg(&format!("--user-data-dir={}", user_data_dir.to_string_lossy()))?;
 
    // IMPORTANT: windowed, not fullscreen. Keep device scale stable.
    caps.add_arg("--force-device-scale-factor=1")?;
    caps.add_arg("--high-dpi-support=1")?;
 
    // Optional window geometry from env; defaults to “almost fullscreen” feel.
    let win_w = env::var("CHROME_WINDOW_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(1200);
    let win_h = env::var("CHROME_WINDOW_HEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(800);
    let win_x = env::var("CHROME_WINDOW_X").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    let win_y = env::var("CHROME_WINDOW_Y").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
 
    caps.add_arg(&format!("--window-size={},{}", win_w, win_h))?;
    caps.add_arg(&format!("--window-position={},{}", win_x, win_y))?;
 
    // Container-friendly flags
    caps.add_arg("--disable-gpu")?;
    caps.add_arg("--no-sandbox")?;
    caps.add_arg("--disable-dev-shm-usage")?;
    caps.add_arg("--no-default-browser-check")?;
    caps.add_arg("--no-first-run")?;
    caps.add_arg("--disable-infobars")?;	
    caps.add_arg("--kiosk")?;
    
    caps.add_experimental_option("excludeSwitches", vec!["enable-automation"])?;
    caps.add_experimental_option("useAutomationExtension", false)?;

    let driver_url = format!("http://127.0.0.1:{driver_port}");
    let driver = WebDriver::new(&driver_url, caps).await?;
   
    Ok(DriverBundle {
        driver,
        chromedriver_child: chromedriver,
        user_data_dir,
        display,
    })
}
 
pub async fn screenshot_bytes(driver: &WebDriver, path: &str) -> Result<(String, Vec<u8>)> {
    let png = driver.screenshot_as_png().await?;

    let mut target = std::path::PathBuf::from(path);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // --- Added: make filename unique if it already exists ---
    if target.exists() {
        let parent = target.parent().unwrap_or(std::path::Path::new("."));
        let stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("screenshot");
        let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("png");

        for i in 1.. {
            let candidate = parent.join(format!("{stem}-{i:03}.{ext}"));
            if !candidate.exists() {
                target = candidate;
                break;
            }
        }
    }

    std::fs::write(&target, &png)?;
    println!("📸 Saved screenshot to {}", target.display());

    Ok((target.to_string_lossy().into_owned(), png))
}


 
pub async fn cleanup_driver(bundle: &mut DriverBundle) {
    let _ = bundle.driver.clone().quit().await;
    let _ = bundle.chromedriver_child.kill();
    let _ = std::fs::remove_dir_all(&bundle.user_data_dir);
}
 
fn spawn_chromedriver(
    chromedriver: &Path,
    port: u16,
    display: &str,
    xauthority: Option<&Path>,
    log_file: File,
) -> Result<Child> {
    let mut cmd = Command::new(chromedriver);
    cmd.arg(format!("--port={}", port))
        .env("DISPLAY", display)
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    if let Some(xa) = xauthority {
        cmd.env("XAUTHORITY", xa);
    }
    let child = cmd.spawn().with_context(|| "failed to spawn chromedriver")?;
    Ok(child)
}
 
fn wait_for_port(host: &str, port: u16, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect((host, port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("port {}:{} did not open within {:?}", host, port, timeout)
}
 
fn find_chrome_bin() -> Option<String> {
    for cand in [
        "google-chrome",
        "google-chrome-stable",
        "chromium-browser",
        "chromium",
    ] {
        if let Ok(p) = which(cand) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}
 
fn guess_xauthority() -> Result<Option<PathBuf>> {
    if let Ok(p) = env::var("XAUTHORITY") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(Some(pb));
        }
    }
    if let Ok(home) = env::var("HOME") {
        let pb = Path::new(&home).join(".Xauthority");
        if pb.exists() {
            return Ok(Some(pb));
        }
    }
    Ok(None)
}
 
fn log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("chromedriver.log")
}
