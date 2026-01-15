// sheets.rs  (service-account only, NO Arc)

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct SheetValuesResponse {
    values: Option<Vec<Vec<String>>>,
}

#[derive(Deserialize)]
struct SpreadsheetMeta {
    sheets: Option<Vec<SheetEntry>>,
}
#[derive(Deserialize)]
struct SheetEntry {
    properties: SheetProps,
}
#[derive(Deserialize)]
struct SheetProps {
    #[serde(rename = "sheetId")]
    sheet_id: i32,
    title: String,
}

pub struct SheetsClient {
    http: reqwest::Client,
    spreadsheet_id: String,
    sheet_name: String,
    sheet_id: i32,
    auth: yup_oauth2::ServiceAccountAuthenticator,
}

impl SheetsClient {
    /// Builds the service-account authenticator ONCE.
    /// Env:
    /// - SHEETS_ID
    /// - GOOGLE_SERVICE_ACCOUNT_JSON (path to SA json)
    /// - SHEETS_SHEET_NAME (recommended) OR SHEETS_RANGE (Tab!A1:T) fallback
    pub async fn new_from_env() -> Result<Self> {
        let spreadsheet_id =
            std::env::var("SHEETS_ID").context("SHEETS_ID must be set")?;

        let sa_path = std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON")
            .context("GOOGLE_SERVICE_ACCOUNT_JSON must be set (path to service account JSON)")?;

        let sheet_name = std::env::var("SHEETS_SHEET_NAME").ok().or_else(|| {
            std::env::var("SHEETS_RANGE")
                .ok()
                .and_then(|r| r.split('!').next().map(|s| s.to_string()))
        }).unwrap_or_else(|| "Sheet1".to_string());

        let key = yup_oauth2::read_service_account_key(&sa_path)
            .await
            .with_context(|| format!("Failed to read service account key at: {sa_path}"))?;

        let auth = yup_oauth2::ServiceAccountAuthenticator::builder(key)
            .build()
            .await
            .context("Failed to build ServiceAccountAuthenticator")?;

        let http = reqwest::Client::new();

        // Resolve sheetId once (DO NOT assume 0)
        let sheet_id = Self::resolve_sheet_id(&http, &auth, &spreadsheet_id, &sheet_name).await?;

        Ok(Self {
            http,
            spreadsheet_id,
            sheet_name,
            sheet_id,
            auth,
        })
    }

    async fn bearer_token(&self) -> Result<String> {
        let scopes = &["https://www.googleapis.com/auth/spreadsheets"];
        let token = self
            .auth
            .token(scopes)
            .await
            .context("Failed to obtain service account access token")?;
        Ok(token.as_ref().to_string())
    }

    async fn resolve_sheet_id(
        http: &reqwest::Client,
        auth: &yup_oauth2::ServiceAccountAuthenticator,
        spreadsheet_id: &str,
        sheet_name: &str,
    ) -> Result<i32> {
        let scopes = &["https://www.googleapis.com/auth/spreadsheets"];
        let token = auth.token(scopes).await?.as_ref().to_string();

        let url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{spreadsheet_id}?fields=sheets.properties"
        );

        let meta: SpreadsheetMeta = http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        meta.sheets
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.properties.title == sheet_name)
            .map(|s| s.properties.sheet_id)
            .with_context(|| format!("Could not find sheet tab named '{sheet_name}'"))
    }

    /// Read values using service account (no API key)
    pub async fn fetch_sheet_values(&self, range_a1: &str) -> Result<Vec<Vec<String>>> {
        let token = self.bearer_token().await?;
        let url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}",
            self.spreadsheet_id, range_a1
        );

        let body: SheetValuesResponse = self.http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(body.values.unwrap_or_default())
    }

    /// Read a single cell using service account
    #[allow(dead_code)]
    pub async fn read_cell_value(&self, row: usize, col: usize) -> Result<String> {
        let token = self.bearer_token().await?;
        let col_letter = column_index_to_letter(col);
        let cell_range = format!("{}!{}{}", self.sheet_name, col_letter, row);

        let url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}",
            self.spreadsheet_id, cell_range
        );

        let body: SheetValuesResponse = self.http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let value = body.values
            .and_then(|v| v.into_iter().next())
            .and_then(|r| r.into_iter().next())
            .unwrap_or_default();

        Ok(value)
    }

    /// Update one cell value + background color using service account
    pub async fn update_cell_value_and_color(
        &self,
        row: usize, // 1-based
        col: usize, // 1-based
        value: &str,
        color: (u8, u8, u8),
    ) -> Result<()> {
        if row == 0 || col == 0 {
            anyhow::bail!("row/col must be 1-based (>= 1)");
        }

        let token = self.bearer_token().await?;

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
                            "userEnteredValue": { "stringValue": value },
                            "userEnteredFormat": {
                                "backgroundColor": {
                                    "red":   color.0 as f64 / 255.0,
                                    "green": color.1 as f64 / 255.0,
                                    "blue":  color.2 as f64 / 255.0
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

        let resp = self.http
            .post(&url)
            .bearer_auth(token)
            .json(&batch_update)
            .send()
            .await?;

        if !resp.status().is_success() {
            // Include body: Google puts the real reason there (permissions, invalid sheetId, etc.)
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sheets batchUpdate failed: {status} body={body}");
        }

        Ok(())
    }
}

/// 1 -> A, 2 -> B, ..., 26 -> Z, 27 -> AA ...
pub fn column_index_to_letter(mut col: usize) -> String {
    let mut result = String::new();
    while col > 0 {
        col -= 1;
        result.insert(0, ((col % 26) as u8 + b'A') as char);
        col /= 26;
    }
    result
}