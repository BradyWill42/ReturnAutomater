use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;
use yup_oauth2::authenticator::TokenProvider;

#[derive(Deserialize)]
struct SheetValuesResponse {
    values: Option<Vec<Vec<String>>>,
}

/// ================================
/// Public API
/// ================================

/// Fetch sheet values using API key (read-only)
pub async fn fetch_sheet_values() -> Result<Vec<Vec<String>>> {
    let spreadsheet_id = std::env::var("SHEETS_ID")?;
    let range = std::env::var("SHEETS_RANGE")
        .unwrap_or_else(|_| "Sheet1!A1:T".to_string());
    let api_key = std::env::var("SHEETS_API_KEY")
        .context("SHEETS_API_KEY must be set for read operations")?;

    let url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?key={}",
        spreadsheet_id, range, api_key
    );

    let resp = reqwest::get(&url).await?;
    let resp = resp.error_for_status()?;
    let body: SheetValuesResponse = resp.json().await?;

    Ok(body.values.unwrap_or_default())
}

/// Reusable Sheets client (OAuth, write operations)
pub struct SheetsClient {
    spreadsheet_id: String,
    sheet_id: i32,
    auth: Arc<dyn TokenProvider>,
    http: reqwest::Client,
}

impl SheetsClient {
    /// Create Sheets client once (OAuth via service account)
    pub async fn new_from_env() -> Result<Self> {
        let spreadsheet_id = std::env::var("SHEETS_ID")
            .context("SHEETS_ID must be set")?;

        let sa_path = std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON")
            .context("GOOGLE_SERVICE_ACCOUNT_JSON must be set")?;

        let sa_key = yup_oauth2::read_service_account_key(&sa_path)
            .await
            .context("Failed to read service account JSON")?;

        let auth = yup_oauth2::ServiceAccountAuthenticator::builder(sa_key)
            .build()
            .await
            .context("Failed to build service account authenticator")?;

        Ok(Self {
            spreadsheet_id,
            sheet_id: 0, // first sheet (can be made dynamic later)
            auth: Arc::new(auth),
            http: reqwest::Client::new(),
        })
    }

    /// Update a cell's value and background color
    ///
    /// - row / col are 1-based (A1 style)
    pub async fn update_cell_value_and_color(
        &self,
        row: usize,
        col: usize,
        value: &str,
        color: (u8, u8, u8),
    ) -> Result<()> {
        let scopes = &["https://www.googleapis.com/auth/spreadsheets"];
        let token = self.auth.token(scopes).await?;
        let access_token = token.as_ref();

        let col_letter = column_index_to_letter(col);
        let cell_range = format!("Sheet1!{col_letter}{row}");

        let batch_update = serde_json::json!({
            "requests": [{
                "updateCells": {
                    "range": {
                        "sheetId": self.sheet_id,
                        "startRowIndex": row - 1,
                        "endRowIndex": row,
                        "startColumnIndex": col - 1,
                        "endColumnIndex": col
                    },
                    "rows": [{
                        "values": [{
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
                        }]
                    }],
                    "fields": "userEnteredValue,userEnteredFormat.backgroundColor"
                }
            }]
        });

        let url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/:batchUpdate",
            self.spreadsheet_id
        );

        self.http
            .post(&url)
            .bearer_auth(access_token)
            .json(&batch_update)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!(
                "Failed to update cell {}",
                cell_range
            ))?;

        println!(
            "✅ Updated cell {} to '{}' (RGB {},{}, {})",
            cell_range, value, color.0, color.1, color.2
        );

        Ok(())
    }
}

/// ================================
/// Helpers
/// ================================

/// Convert 1-based column index to A1 notation
/// 1 -> A, 2 -> B, 27 -> AA, etc.
fn column_index_to_letter(mut col: usize) -> String {
    let mut result = String::new();
    while col > 0 {
        col -= 1;
        result.insert(0, ((col % 26) as u8 + b'A') as char);
        col /= 26;
    }
    result
}