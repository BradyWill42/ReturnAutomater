use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct SheetValuesResponse {
    values: Option<Vec<Vec<String>>>,
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
