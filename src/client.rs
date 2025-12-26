// src/client.rs

use anyhow::{anyhow, Result};

/// One client row from the sheet mapped into a strongly-typed struct.
#[derive(Debug, Clone)]
pub struct Client {
    pub me: String,                   // "ME"
    pub returns_printed: bool,        // "Returns Printed?" (Y/N)
    pub returns_sent: bool,           // "Returns Sent?" (Y/N)
    pub client_id: String,            // "ClientID"
    pub client_name: String,          // "ClientName"
    pub email_temp1: String,          // "EmailTemp1"
    pub email_temp2: String,          // "EmailTemp2"
    pub comment: String,              // "Comment"
    pub estimate_quarterlies: String, // "Estimate/Quarterlies"
    pub tax_return: String,           // "TaxReturn"
    pub signature: String,            // "Signature"
    pub signature_template: String,   // "SignatureTemplate"
    pub require_kba: bool,            // "RequireKBA" (Y/N)
    pub invoice: String,              // "Invoice"
    pub invoice_amount: String,          // "InvoiceAmount"
    pub invoice_template: String,     // "InvoiceTemplate"
    pub closer: String,               // "Closer"
    pub pipeline: String,             // "Pipeline"
    pub seal: String,                 // "Seal"
    pub year_to_seal: String,    // "YearToSeal"
    pub row_index: usize,             // 1-based row index in the sheet (header is row 1, first data row is 2)
}

impl Client {
    /// Build the portal URL for this client:
    /// USER_PORTAL_A + client_id + USER_PORTAL_B
    pub fn portal_url(&self) -> String {
        let base = std::env::var("USER_PORTAL_A").unwrap_or_default();
        let post = std::env::var("USER_PORTAL_B").unwrap_or_default();
        format!("{base}{}{post}", self.client_id)
    }

    pub fn docs_url(&self) -> String {
	let base = std::env::var("USER_PORTAL_A").unwrap_or_default();
	let post = std::env::var("DOCS_PORTAL").unwrap_or_default();
	format!("{base}{}{post}", self.client_id)
    }

    pub fn pipeline_url(&self) -> String {
	let base = std::env::var("USER_PORTAL_A").unwrap_or_default();
	let post = std::env::var("PIPELINE_PORTAL").unwrap_or_default();
	format!("{base}{}{post}", self.client_id)
    }
    
    pub fn email_template(&self) -> Vec<String> {
 	let mut templates = Vec::new();

	if !self.email_temp1.trim().is_empty() {
	    templates.push(self.email_temp1.trim().to_string());
	}	

	if !self.email_temp2.trim().is_empty() {
	    templates.push(self.email_temp2.trim().to_string());
	}
	
	templates
    }
    
    pub fn est_qtr(&self) -> Vec<String> {
	let mut estimates = Vec::new();
	
	if !self.estimate_quarterlies.trim().is_empty() {
	    estimates.push(self.estimate_quarterlies.trim().to_string());
   	}
	estimates
    }
}

/// In-memory store of all clients for the current run.
#[derive(Debug, Default)]
pub struct ClientStore {
    pub clients: Vec<Client>,
    pub seal_column_index: usize, // 1-based column index for "Seal" column
    pub me_column_index: usize,   // 1-based column index for "ME" column
}

impl ClientStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self { 
            clients: Vec::new(),
            seal_column_index: 0, // Will be set when loading from sheet
            me_column_index: 0,   // Will be set when loading from sheet
        }
    }


    /// Clear and repopulate from a Google Sheets `values` matrix.
    ///
    /// `values` is expected to be:
    /// [
    ///   [ "ME", "Returns Printed?", "Returns Sent?", "ClientID", ... ],
    ///   [ "1", "N", "N", "SD3", ... ],
    ///   ...
    /// ]
    ///
    /// We:
    /// - read headers from the first row;
    /// - iterate rows until the first with empty "ME";
    /// - only include rows where "ME" is non-empty.
    pub fn from_sheet_values(values: &[Vec<String>]) -> Result<Self> {
        if values.is_empty() {
            return Ok(Self::new());
        }

        let header = &values[0];

        // Helper to find a column index by exact header text.
        let idx = |name: &str| -> Result<usize> {
            header
                .iter()
                .position(|h| h.trim() == name)
                .ok_or_else(|| anyhow!("Missing expected header '{name}'"))
        };

        let c_me = idx("ME")?;
        let c_returns_printed = idx("Returns Printed?")?;
        let c_returns_sent = idx("Returns Sent?")?;
        let c_client_id = idx("ClientID")?;
        let c_client_name = idx("ClientName")?;
        let c_email1 = idx("EmailTemp1")?;
        let c_email2 = idx("EmailTemp2")?;
        let c_comment = idx("Comment")?;
        let c_estimate = idx("Estimate/Quarterlies")?;
        let c_tax_return = idx("TaxReturn")?;
        let c_signature = idx("Signature")?;
        let c_sig_tmpl = idx("SignatureTemplate")?;
        let c_require_kba = idx("RequireKBA")?;
        let c_invoice = idx("Invoice")?;
        let c_invoice_amount = idx("InvoiceAmount")?;
        let c_invoice_tmpl = idx("InvoiceTemplate")?;
        let c_closer = idx("Closer")?;
        let c_pipeline = idx("Pipeline")?;
        let c_seal = idx("Seal")?;
        let c_year_to_seal = idx("YearToSeal")?;

        let mut store = ClientStore {
            clients: Vec::new(),
            seal_column_index: c_seal + 1, // Convert 0-based to 1-based
            me_column_index: c_me + 1,     // Convert 0-based to 1-based
        };

        for (row_idx, row) in values.iter().skip(1).enumerate() {
            let me = get_cell(row, c_me);
            // Stop at first empty ME (your rule).
            if me.trim().is_empty() {
                break;
            }

            // row_idx is 0-based from skip(1), so actual sheet row = row_idx + 2 (header is row 1)
            let sheet_row = row_idx + 2;

            let client = Client {
                me,
                returns_printed: parse_yn(&get_cell(row, c_returns_printed)),
                returns_sent: parse_yn(&get_cell(row, c_returns_sent)),
                client_id: get_cell(row, c_client_id),
                client_name: get_cell(row, c_client_name),
                email_temp1: get_cell(row, c_email1),
                email_temp2: get_cell(row, c_email2),
                comment: get_cell(row, c_comment),
                estimate_quarterlies: get_cell(row, c_estimate),
                tax_return: get_cell(row, c_tax_return),
                signature: get_cell(row, c_signature),
                signature_template: get_cell(row, c_sig_tmpl),
                require_kba: parse_yn(&get_cell(row, c_require_kba)),
                invoice: get_cell(row, c_invoice),
                invoice_amount: get_cell(row, c_invoice_amount),
                invoice_template: get_cell(row, c_invoice_tmpl),
                closer: get_cell(row, c_closer),
                pipeline: get_cell(row, c_pipeline),
                seal: get_cell(row, c_seal),
                year_to_seal: get_cell(row, c_year_to_seal),
                row_index: sheet_row,
            };

            store.clients.push(client);
        }

        Ok(store)
    }

    /// Convenience: clear and reload into an existing store.
    pub fn reload_from_sheet(&mut self, values: &[Vec<String>]) -> Result<()> {
        *self = ClientStore::from_sheet_values(values)?;
        Ok(())
    }
}

/* ---------- Small internal helpers ---------- */

fn get_cell(row: &[String], idx: usize) -> String {
    row.get(idx)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn parse_yn(s: &str) -> bool {
    matches!(s.trim().to_ascii_uppercase().as_str(), "Y" | "YES" | "TRUE" | "1")
}

fn parse_f64(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

fn parse_i32_opt(s: &str) -> Option<i32> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse::<i32>().ok()
    }
}
