use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct SheetValuesResponse {
    values: Option<Vec<Vec<String>>>,
}

/// Get access token from service account JSON file using yup-oauth2
async fn get_access_token_from_service_account(service_account_path: &str) -> Result<String> {
    use yup_oauth2::ServiceAccountAuthenticator;
    use std::fs;
    use std::io::Write;
    
    // #region agent log
    let log_path = "/home/pegasus/rust-project/.cursor/debug.log";
    let mut log_file = fs::OpenOptions::new().create(true).append(true).open(log_path).ok();
    let mut log_entry = |msg: &str, data: serde_json::Value| {
        if let Some(ref mut f) = log_file {
            let _ = writeln!(f, "{}", serde_json::json!({
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "A",
                "location": "sheets.rs:get_access_token_from_service_account",
                "message": msg,
                "data": data,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            }));
        }
    };
    // #endregion
    
    println!("🔐 Reading service account key from: {}", service_account_path);
    log_entry("Function entry", serde_json::json!({"path": service_account_path}));
    
    // Check if file exists
    let file_exists = std::path::Path::new(service_account_path).exists();
    log_entry("File existence check", serde_json::json!({"exists": file_exists, "path": service_account_path}));
    
    if !file_exists {
        anyhow::bail!("Service account file does not exist at: {}", service_account_path);
    }
    
    // Try to read file content (first 100 chars to verify it's JSON, not full content for security)
    let file_preview = fs::read_to_string(service_account_path)
        .ok()
        .map(|s| s.chars().take(100).collect::<String>());
    log_entry("File read attempt", serde_json::json!({"preview_length": file_preview.as_ref().map(|s| s.len()), "starts_with_brace": file_preview.as_ref().map(|s| s.starts_with('{'))}));
    
    log_entry("Calling read_service_account_key", serde_json::json!({"path": service_account_path}));
    let sa_key = match yup_oauth2::read_service_account_key(service_account_path).await {
        Ok(key) => {
            log_entry("read_service_account_key success", serde_json::json!({"client_email": key.client_email, "project_id": key.project_id}));
            key
        }
        Err(e) => {
            log_entry("read_service_account_key failed", serde_json::json!({"error": format!("{:?}", e)}));
            return Err(e).with_context(|| format!("Failed to read service account key file at: {}", service_account_path));
        }
    };
    
    println!("🔐 Building service account authenticator...");
    log_entry("Building authenticator", serde_json::json!({}));
    let auth = match ServiceAccountAuthenticator::builder(sa_key).build().await {
        Ok(auth) => {
            log_entry("Authenticator build success", serde_json::json!({}));
            auth
        }
        Err(e) => {
            log_entry("Authenticator build failed", serde_json::json!({"error": format!("{:?}", e)}));
            return Err(anyhow::anyhow!(e)).context("Failed to create service account authenticator. Check that the JSON file is valid and contains all required fields.");
        }
    };
    
    let scopes = &["https://www.googleapis.com/auth/spreadsheets"];
    println!("🔐 Requesting access token with scope: {}", scopes[0]);
    log_entry("Requesting token", serde_json::json!({"scope": scopes[0]}));
    let token = match auth.token(scopes).await {
        Ok(t) => {
            log_entry("Token request success", serde_json::json!({"token_length": t.as_ref().len()}));
            t
        }
        Err(e) => {
            log_entry("Token request failed", serde_json::json!({"error": format!("{:?}", e), "error_type": format!("{}", std::any::type_name_of_val(&e))}));
            return Err(anyhow::anyhow!(e)).with_context(|| format!(
                "Failed to obtain access token from service account. \
                Make sure: 1) The service account JSON is valid, \
                2) The service account has the necessary permissions, \
                3) The Google Sheets API is enabled in your Google Cloud project"
            ));
        }
    };
    
    // In yup-oauth2 v7, AccessToken implements AsRef<str>
    let token_str = token.as_ref().to_string();
    println!("🔐 Successfully obtained access token (length: {})", token_str.len());
    log_entry("Function exit success", serde_json::json!({"token_length": token_str.len()}));
    Ok(token_str)
}

pub async fn fetch_sheet_values() -> Result<Vec<Vec<String>>> {
    let spreadsheet_id = std::env::var("SHEETS_ID")?;
    let range = std::env::var("SHEETS_RANGE")
        .unwrap_or_else(|_| "Sheet1!A1:T".to_string());
    let api_key = std::env::var("SHEETS_API_KEY")?;

    let url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?key={}",
        spreadsheet_id, range, api_key
    );

    let resp = reqwest::get(&url).await?;
    let resp = resp.error_for_status()?;

    let body: SheetValuesResponse = resp.json().await?;
    Ok(body.values.unwrap_or_default())
}

/// Update a cell value and background color in Google Sheets.
/// 
/// - `row`: 1-based row index (header is row 1)
/// - `col`: 1-based column index (A=1, B=2, etc.)
/// - `value`: The new cell value ("Y" or "N")
/// - `color`: RGB color tuple (0-255), e.g., (0, 255, 0) for green, (255, 0, 0) for red
pub async fn update_cell_value_and_color(
    row: usize,
    col: usize,
    value: &str,
    color: (u8, u8, u8),
) -> Result<()> {
    let spreadsheet_id = std::env::var("SHEETS_ID")?;
    let sheet_name = std::env::var("SHEETS_RANGE")
        .unwrap_or_else(|_| "Sheet1!A1:T".to_string())
        .split('!')
        .next()
        .unwrap_or("Sheet1")
        .to_string();
    
    // #region agent log
    use std::fs;
    use std::io::Write;
    let log_path = "/home/pegasus/rust-project/.cursor/debug.log";
    let mut log_file = fs::OpenOptions::new().create(true).append(true).open(log_path).ok();
    let mut log_entry = |msg: &str, data: serde_json::Value| {
        if let Some(ref mut f) = log_file {
            let _ = writeln!(f, "{}", serde_json::json!({
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "B",
                "location": "sheets.rs:update_cell_value_and_color",
                "message": msg,
                "data": data,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            }));
        }
    };
    // #endregion
    
    // Try OAuth token, service account, or API key (in order of preference)
    // Note: API key won't work for write operations, but we check it for better error messages
    log_entry("Checking authentication method", serde_json::json!({
        "has_oauth_token": std::env::var("GOOGLE_ACCESS_TOKEN").is_ok(),
        "has_service_account": std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON").is_ok(),
        "service_account_path": std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON").ok()
    }));
    
    let access_token = if let Ok(token) = std::env::var("GOOGLE_ACCESS_TOKEN") {
        log_entry("Using OAuth token", serde_json::json!({"token_length": token.len()}));
        Some(token)
    } else if let Ok(sa_path) = std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON") {
        log_entry("Attempting service account auth", serde_json::json!({"path": sa_path}));
        // Get token from service account
        match get_access_token_from_service_account(&sa_path).await {
            Ok(token) => {
                log_entry("Service account auth success", serde_json::json!({"token_length": token.len()}));
                Some(token)
            }
            Err(e) => {
                log_entry("Service account auth failed", serde_json::json!({"error": format!("{:?}", e)}));
                return Err(e).context("Failed to get access token from service account");
            }
        }
    } else {
        log_entry("No authentication method found", serde_json::json!({}));
        None
    };
    
    if access_token.is_none() {
        anyhow::bail!("GOOGLE_ACCESS_TOKEN or GOOGLE_SERVICE_ACCOUNT_JSON must be set for write operations. API keys only work for read operations.");
    }

    // Convert 1-based column index to A1 notation (A=1, B=2, ..., Z=26, AA=27, etc.)
    let col_letter = column_index_to_letter(col);
    let cell_range = format!("{sheet_name}!{col_letter}{row}");

    // Build batchUpdate request
    let batch_update = serde_json::json!({
        "requests": [
            {
                "updateCells": {
                    "range": {
                        "sheetId": 0, // Assuming first sheet, may need to be configurable
                        "startRowIndex": row - 1, // Convert to 0-based
                        "endRowIndex": row,
                        "startColumnIndex": col - 1, // Convert to 0-based
                        "endColumnIndex": col
                    },
                    "rows": [
                        {
                            "values": [
                                {
                                    "userEnteredValue": {
                                        "stringValue": value
                                    },
                                    "userEnteredFormat": {
                                        "backgroundColor": {
                                            "red": color.0 as f64 / 255.0,
                                            "green": color.1 as f64 / 255.0,
                                            "blue": color.2 as f64 / 255.0
                                        }
                                    }
                                }
                            ]
                        }
                    ],
                    "fields": "userEnteredValue,userEnteredFormat.backgroundColor"
                }
            }
        ]
    });

    let url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}/:batchUpdate",
        spreadsheet_id
    );

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(access_token.as_ref().unwrap())
        .json(&batch_update)
        .send()
        .await?;
    let _resp = resp.error_for_status()
        .with_context(|| format!("Failed to update cell {cell_range}. Note: Write operations typically require OAuth (set GOOGLE_ACCESS_TOKEN), not just API key."))?;

    println!("✅ Updated cell {cell_range} to '{}' with color RGB({},{},{})", value, color.0, color.1, color.2);
    Ok(())
}

/// Convert 1-based column index to A1 notation letter(s)
/// 1 -> A, 2 -> B, ..., 26 -> Z, 27 -> AA, etc.
fn column_index_to_letter(mut col: usize) -> String {
    let mut result = String::new();
    while col > 0 {
        col -= 1;
        result.insert(0, ((col % 26) as u8 + b'A') as char);
        col /= 26;
    }
    result
}

