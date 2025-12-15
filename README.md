# ReturnAutomater

Rust automation that reads a Google Sheet, fetches portal credentials from Keeper, and drives a Chrome session (with real OS-level mouse/keyboard) to send emails, seal documents, request signatures, and create invoices for each client row. OpenAI vision is used to choose click targets when DOM selectors are not stable.

## What it does
- Loads client rows from Google Sheets and maps them into typed structs.
- Pulls portal credentials (username/password/TOTP) from Keeper Secrets Manager.
- Launches Chrome via Chromedriver in headful mode and controls it with Selenium + xdotool.
- Executes a plan of steps per client: login, send emails, share/seal docs, request signatures, move pipeline cards, and create invoices.
- Uses OpenAI to pick click points from screenshots or DOM candidates, with heuristic fallbacks and optional on-step visual validation.

## Requirements
- Rust toolchain (edition 2021) and `cargo`.
- Google Chrome/Chromium and `chromedriver` on PATH (matching browser version).
- X11 environment with `xdotool` installed (automation moves your real cursor).
- Internet access to Google Sheets API, Keeper, and OpenAI (or configured proxy).
- Env file (`.env`) with the variables below.

## Environment variables (all explicit)
Core run:
- `LOGIN_URL` – portal login URL (required).
- `HEADFUL` – must be `1`; headless is rejected.
- `DISPLAY_VNC` – X display to drive (default `:1`).
- `CHROMEDRIVER_PORT` – chromedriver port (default `9515`).
- `CHROME_BIN` – optional path to chrome/chromium.
- `CHROME_WINDOW_WIDTH` / `CHROME_WINDOW_HEIGHT` / `CHROME_WINDOW_X` / `CHROME_WINDOW_Y` – window geometry.
- `XAUTHORITY` – optional path if X11 auth is non-standard.

Google Sheets:
- `SHEETS_ID`
- `SHEETS_RANGE` (default `Sheet1!A1:T`)
- `SHEETS_API_KEY`

Keeper Secrets Manager:
- `KEEPER_TOKEN`
- `KEEPER_UID`
- `KEEPER_CONFIG_PATH` (default `config.json`)

Portal URL pieces (used to compose per-client URLs):
- `USER_PORTAL_A`, `USER_PORTAL_B`
- `DOCS_PORTAL`
- `PIPELINE_PORTAL`

OpenAI vision and retries:
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL` (default `https://api.openai.com/v1`)
- `OPENAI_MODEL` (default `gpt-4o-mini`)
- `OPENAI_TIMEOUT_SECS` (default `60`)
- `OPENAI_MAX_RETRIES` (default `3`)
- `OPENAI_SAMPLES_PER_CALL` (default `1`)
- `OPENAI_MAX_CONCURRENCY` (default `4`)
- `OPENAI_STAGGER_MS` (default `120`)
- `OPENAI_OVERLAY_GRID` (default on)
- `GRID_STEP`, `GRID_LABEL_EVERY`, `GRID_FONT_SCALE`, `GRID_SAVE_DEBUG`

Click and viewport tuning:
- `VIEWPORT_W`, `VIEWPORT_H`
- `CLICK_X_OFFSET_PX`, `CLICK_Y_OFFSET_PX`

Run artifacts and screenshots:
- `RUN_DIR` – override output directory for LLM dotmaps and artifacts.
- `CURRENT_STEP_NO` – tag dotmaps with the active step number.
- `KEEP_OBSERVER_SCREENSHOTS` – set to `1` to keep validation screenshots (otherwise deleted).

## How the plan works
- Steps are defined in `plan.rs` (e.g., `VisitUrl`, `ClickByDom`, `ClickByLlm`, `TypeText`, `SubmitForm`, `ClickStage`, etc.).
- `AutomationPlan::client_loop` builds a plan per client row (seal docs, send emails, move pipeline cards, request signatures, create invoices).
- Validation: Steps may include a yes/no question to OpenAI after a screenshot; follow-up `on_pass`/`on_fail` steps can run based on the answer.
- LLM clicks: `click_by_llm_dom_first` first enumerates DOM candidates, asks OpenAI to choose, and falls back to heuristics.
- Screen clicks: `call_openai_for_point` asks OpenAI for viewport coordinates on a screenshot, then maps them to screen space using window geometry and optional offsets.

## Safety notes
- The program moves your real cursor and types into the active X11 display. Run inside a dedicated VNC/desktop session.
- `HEADFUL=1` is enforced; headless is not supported.
- Screenshots may be written temporarily; set `KEEP_OBSERVER_SCREENSHOTS=1` to keep validation captures.
- Various folder directories and sensitive automation source files have been omitted

