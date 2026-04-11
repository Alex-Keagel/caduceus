use caduceus_core::{CaduceusError, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

// ── Types ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrowserActionType {
    Navigate,
    Click,
    Type,
    Screenshot,
    GetText,
    WaitFor,
    Scroll,
    GetConsole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserAction {
    pub action_type: BrowserActionType,
    pub selector: Option<String>,
    pub value: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserResult {
    pub success: bool,
    pub data: Option<String>,
    pub console_logs: Vec<String>,
    pub error: Option<String>,
}

impl BrowserResult {
    pub fn ok(data: impl Into<String>) -> Self {
        Self {
            success: true,
            data: Some(data.into()),
            console_logs: Vec::new(),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            console_logs: Vec::new(),
            error: Some(message.into()),
        }
    }
}

// ── BrowserService ─────────────────────────────────────────────────────────────

pub struct BrowserService {
    pub headless: bool,
}

impl BrowserService {
    pub fn new(headless: bool) -> Self {
        Self { headless }
    }

    /// Verify playwright CLI is installed.
    pub async fn launch(&self) -> Result<()> {
        let output = Command::new("playwright")
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CaduceusError::Tool {
                tool: "browser".into(),
                message: format!(
                    "playwright CLI not found. Install with `npm install -g playwright`: {e}"
                ),
            })?;

        if !output.status.success() {
            return Err(CaduceusError::Tool {
                tool: "browser".into(),
                message: format!(
                    "playwright not usable: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(())
    }

    /// Execute a browser action via playwright CLI.
    pub async fn execute(&self, action: BrowserAction) -> BrowserResult {
        match action.action_type {
            BrowserActionType::Navigate => self.navigate(action.url.as_deref()).await,
            BrowserActionType::Click => {
                self.click(action.url.as_deref(), action.selector.as_deref())
                    .await
            }
            BrowserActionType::Type => {
                self.type_text(
                    action.url.as_deref(),
                    action.selector.as_deref(),
                    action.value.as_deref(),
                )
                .await
            }
            BrowserActionType::Screenshot => self.screenshot(action.url.as_deref()).await,
            BrowserActionType::GetText => {
                self.get_text(action.url.as_deref(), action.selector.as_deref())
                    .await
            }
            BrowserActionType::WaitFor => {
                self.wait_for(action.url.as_deref(), action.selector.as_deref())
                    .await
            }
            BrowserActionType::Scroll => {
                self.scroll(action.url.as_deref(), action.selector.as_deref())
                    .await
            }
            BrowserActionType::GetConsole => self.get_console(action.url.as_deref()).await,
        }
    }

    /// Navigate to a URL and capture a screenshot in one call. Returns base64 PNG.
    pub async fn screenshot(&self, url: Option<&str>) -> BrowserResult {
        let Some(url) = url else {
            return BrowserResult::err("url is required for Screenshot action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  const buf = await page.screenshot({{ type: 'png' }});
  console.log('SCREENSHOT_DATA:' + buf.toString('base64'));
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn navigate(&self, url: Option<&str>) -> BrowserResult {
        let Some(url) = url else {
            return BrowserResult::err("url is required for Navigate action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  const title = await page.title();
  console.log('RESULT:' + title);
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn click(&self, url: Option<&str>, selector: Option<&str>) -> BrowserResult {
        let (Some(url), Some(sel)) = (url, selector) else {
            return BrowserResult::err("url and selector are required for Click action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  await page.click('{sel}');
  console.log('RESULT:clicked');
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
            sel = sel.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn type_text(
        &self,
        url: Option<&str>,
        selector: Option<&str>,
        value: Option<&str>,
    ) -> BrowserResult {
        let (Some(url), Some(sel), Some(text)) = (url, selector, value) else {
            return BrowserResult::err("url, selector, and value are required for Type action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  await page.fill('{sel}', '{text}');
  console.log('RESULT:typed');
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
            sel = sel.replace('\'', "\\'"),
            text = text.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn get_text(&self, url: Option<&str>, selector: Option<&str>) -> BrowserResult {
        let (Some(url), Some(sel)) = (url, selector) else {
            return BrowserResult::err("url and selector are required for GetText action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  const text = await page.textContent('{sel}');
  console.log('RESULT:' + (text || ''));
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
            sel = sel.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn wait_for(&self, url: Option<&str>, selector: Option<&str>) -> BrowserResult {
        let (Some(url), Some(sel)) = (url, selector) else {
            return BrowserResult::err("url and selector are required for WaitFor action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  await page.waitForSelector('{sel}');
  console.log('RESULT:visible');
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
            sel = sel.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    async fn scroll(&self, url: Option<&str>, selector: Option<&str>) -> BrowserResult {
        let Some(url) = url else {
            return BrowserResult::err("url is required for Scroll action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let sel_js = match selector {
            Some(s) => format!(
                "await page.locator('{}').scrollIntoViewIfNeeded();",
                s.replace('\'', "\\'")
            ),
            None => "await page.evaluate(() => window.scrollBy(0, 300));".to_string(),
        };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  {sel_js}
  console.log('RESULT:scrolled');
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
            sel_js = sel_js,
        );

        self.run_node_script(&script).await
    }

    async fn get_console(&self, url: Option<&str>) -> BrowserResult {
        let Some(url) = url else {
            return BrowserResult::err("url is required for GetConsole action");
        };

        let headless_flag = if self.headless { "true" } else { "false" };
        let script = format!(
            r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const browser = await chromium.launch({{ headless: {headless_flag} }});
  const page = await browser.newPage();
  const logs = [];
  page.on('console', msg => logs.push(msg.type() + ': ' + msg.text()));
  await page.goto('{url}', {{ waitUntil: 'networkidle' }});
  console.log('CONSOLE_LOGS:' + JSON.stringify(logs));
  await browser.close();
}})().catch(e => {{ console.error(e.message); process.exit(1); }});
"#,
            headless_flag = headless_flag,
            url = url.replace('\'', "\\'"),
        );

        self.run_node_script(&script).await
    }

    /// Run a Node.js script via `node -e`. Parses RESULT: and SCREENSHOT_DATA: prefixes.
    async fn run_node_script(&self, script: &str) -> BrowserResult {
        let output = match Command::new("node")
            .arg("-e")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => return BrowserResult::err(format!("failed to run node: {e}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return BrowserResult {
                success: false,
                data: None,
                console_logs: Vec::new(),
                error: Some(if stderr.is_empty() { stdout } else { stderr }),
            };
        }

        let mut data = None;
        let mut console_logs = Vec::new();

        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("RESULT:") {
                data = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("SCREENSHOT_DATA:") {
                data = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("CONSOLE_LOGS:") {
                if let Ok(logs) = serde_json::from_str::<Vec<String>>(rest) {
                    console_logs = logs;
                }
            } else {
                console_logs.push(line.to_string());
            }
        }

        BrowserResult {
            success: true,
            data,
            console_logs,
            error: None,
        }
    }

    /// Gracefully terminate any lingering browser processes (best-effort).
    pub async fn close(&self) {
        // Node.js subprocesses spawned inline exit on their own; nothing to track
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_result_ok() {
        let r = BrowserResult::ok("hello");
        assert!(r.success);
        assert_eq!(r.data.as_deref(), Some("hello"));
        assert!(r.error.is_none());
    }

    #[test]
    fn browser_result_err() {
        let r = BrowserResult::err("something went wrong");
        assert!(!r.success);
        assert!(r.data.is_none());
        assert_eq!(r.error.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn browser_action_serializes() {
        let action = BrowserAction {
            action_type: BrowserActionType::Navigate,
            url: Some("https://example.com".into()),
            selector: None,
            value: None,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("Navigate"));
        assert!(json.contains("example.com"));
    }

    #[test]
    fn browser_service_headless_default() {
        let svc = BrowserService::new(true);
        assert!(svc.headless);
        let svc2 = BrowserService::new(false);
        assert!(!svc2.headless);
    }

    #[tokio::test]
    async fn execute_missing_url_returns_error() {
        let svc = BrowserService::new(true);
        let action = BrowserAction {
            action_type: BrowserActionType::Screenshot,
            url: None,
            selector: None,
            value: None,
        };
        let result = svc.execute(action).await;
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn execute_click_missing_selector_returns_error() {
        let svc = BrowserService::new(true);
        let action = BrowserAction {
            action_type: BrowserActionType::Click,
            url: Some("https://example.com".into()),
            selector: None,
            value: None,
        };
        let result = svc.execute(action).await;
        assert!(!result.success);
    }
}
