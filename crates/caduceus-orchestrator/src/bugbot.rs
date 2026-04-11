use caduceus_core::{ModelId, Result};
use caduceus_providers::{ChatRequest, LlmAdapter, Message};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Finding types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "CRITICAL"),
            Self::High => write!(f, "HIGH"),
            Self::Medium => write!(f, "MEDIUM"),
            Self::Low => write!(f, "LOW"),
            Self::Info => write!(f, "INFO"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FindingCategory {
    Bug,
    Security,
    Performance,
    Style,
    Logic,
}

impl std::fmt::Display for FindingCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bug => write!(f, "Bug"),
            Self::Security => write!(f, "Security"),
            Self::Performance => write!(f, "Performance"),
            Self::Style => write!(f, "Style"),
            Self::Logic => write!(f, "Logic"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugBotFinding {
    pub severity: FindingSeverity,
    pub file: String,
    pub line: Option<usize>,
    pub description: String,
    pub suggestion: Option<String>,
    pub category: FindingCategory,
}

impl std::fmt::Display for BugBotFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let loc = match self.line {
            Some(l) => format!("{}:{}", self.file, l),
            None => self.file.clone(),
        };
        write!(
            f,
            "[{}][{}] {} — {}",
            self.severity, self.category, loc, self.description
        )?;
        if let Some(ref s) = self.suggestion {
            write!(f, "\n  Suggestion: {s}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugBotReport {
    pub findings: Vec<BugBotFinding>,
    pub files_reviewed: usize,
    pub summary: String,
}

impl BugBotReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn critical_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Critical)
            .count()
    }

    pub fn high_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::High)
            .count()
    }
}

impl std::fmt::Display for BugBotReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "BugBot Report — {} file(s) reviewed",
            self.files_reviewed
        )?;
        writeln!(f, "{}", self.summary)?;
        if self.findings.is_empty() {
            writeln!(f, "✓ No issues found.")?;
        } else {
            writeln!(f, "\n{} finding(s):", self.findings.len())?;
            for finding in &self.findings {
                writeln!(f, "  {finding}")?;
            }
        }
        Ok(())
    }
}

// ── BugBot ─────────────────────────────────────────────────────────────────────

const REVIEW_SYSTEM_PROMPT: &str = r#"You are BugBot, an expert code reviewer focused on finding real issues.

Your job is to identify:
1. **Bugs** — logic errors, off-by-one, null/None handling, panics
2. **Security** — injection, secrets in code, unsafe input handling, path traversal
3. **Logic** — incorrect algorithms, race conditions, deadlocks, infinite loops
4. **Edge cases** — empty inputs, overflow, underflow, out-of-bounds

DO NOT report style preferences, formatting, or cosmetic issues unless they mask bugs.
Focus on correctness and safety.

Respond with a JSON object like this:
{
  "summary": "Brief overview of what was reviewed and key concerns",
  "findings": [
    {
      "severity": "Critical|High|Medium|Low|Info",
      "category": "Bug|Security|Performance|Style|Logic",
      "file": "path/to/file.rs",
      "line": 42,
      "description": "What the issue is",
      "suggestion": "How to fix it"
    }
  ]
}

If no issues are found, return an empty findings array.
"#;

pub struct BugBot {
    provider: Arc<dyn LlmAdapter>,
    model: ModelId,
}

impl BugBot {
    pub fn new(provider: Arc<dyn LlmAdapter>) -> Self {
        Self {
            model: ModelId::new("claude-sonnet-4-5"),
            provider,
        }
    }

    pub fn with_model(mut self, model: ModelId) -> Self {
        self.model = model;
        self
    }

    /// Review a git diff and produce a BugBotReport.
    pub async fn review_diff(&self, diff_text: &str) -> Result<BugBotReport> {
        if diff_text.trim().is_empty() {
            return Ok(BugBotReport {
                findings: Vec::new(),
                files_reviewed: 0,
                summary: "No diff to review.".into(),
            });
        }

        let files_reviewed = count_files_in_diff(diff_text);
        let prompt = format!(
            "Review the following git diff for bugs, security issues, and logic errors:\n\n```diff\n{diff_text}\n```"
        );

        let report_json = self.call_llm(&prompt).await?;
        parse_report(report_json, files_reviewed)
    }

    /// Review specific files by reading their content.
    pub async fn review_files(&self, paths: &[String]) -> Result<BugBotReport> {
        if paths.is_empty() {
            return Ok(BugBotReport {
                findings: Vec::new(),
                files_reviewed: 0,
                summary: "No files to review.".into(),
            });
        }

        let mut file_contents = String::new();
        let mut readable = 0usize;
        for path in paths {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    file_contents.push_str(&format!("=== {path} ===\n{content}\n\n"));
                    readable += 1;
                }
                Err(e) => {
                    file_contents.push_str(&format!("=== {path} === [ERROR: {e}]\n\n"));
                }
            }
        }

        let prompt = format!(
            "Review the following source files for bugs, security issues, and logic errors:\n\n{file_contents}"
        );

        let report_json = self.call_llm(&prompt).await?;
        parse_report(report_json, readable)
    }

    /// Attempt to generate a fix for a finding. Returns the suggested patch or None.
    pub async fn auto_fix(&self, finding: &BugBotFinding) -> Option<String> {
        let prompt = format!(
            "Generate a minimal code fix for this issue:\n\nFile: {}\nLine: {}\nIssue: {}\nCategory: {}\nSeverity: {}\n\nProvide ONLY the corrected code snippet, no explanation.",
            finding.file,
            finding.line.map(|l| l.to_string()).unwrap_or_else(|| "unknown".into()),
            finding.description,
            finding.category,
            finding.severity,
        );

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message::user(&prompt)],
            system: Some("You are a code fix generator. Output only corrected code.".into()),
            max_tokens: 1024,
            temperature: Some(0.1),
            thinking_mode: false,
        };

        self.provider.chat(request).await.ok().map(|r| r.content)
    }

    async fn call_llm(&self, prompt: &str) -> Result<String> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message::user(prompt)],
            system: Some(REVIEW_SYSTEM_PROMPT.into()),
            max_tokens: 4096,
            temperature: Some(0.1),
            thinking_mode: false,
        };

        let response = self.provider.chat(request).await?;
        Ok(response.content)
    }
}

// ── Parsing helpers ────────────────────────────────────────────────────────────

fn count_files_in_diff(diff: &str) -> usize {
    diff.lines()
        .filter(|l| l.starts_with("--- a/") || l.starts_with("+++ b/"))
        .count()
        / 2
}

fn parse_report(json_str: String, files_reviewed: usize) -> Result<BugBotReport> {
    // Extract JSON block if the LLM wrapped it in markdown
    let json_content = extract_json(&json_str);

    let parsed: serde_json::Value = serde_json::from_str(json_content).unwrap_or_else(|_| {
        serde_json::json!({
            "summary": json_str.clone(),
            "findings": []
        })
    });

    let summary = parsed["summary"]
        .as_str()
        .unwrap_or("Review complete.")
        .to_string();

    let mut findings = Vec::new();
    if let Some(arr) = parsed["findings"].as_array() {
        for item in arr {
            let severity = match item["severity"].as_str().unwrap_or("Low") {
                "Critical" => FindingSeverity::Critical,
                "High" => FindingSeverity::High,
                "Medium" => FindingSeverity::Medium,
                "Info" => FindingSeverity::Info,
                _ => FindingSeverity::Low,
            };
            let category = match item["category"].as_str().unwrap_or("Bug") {
                "Security" => FindingCategory::Security,
                "Performance" => FindingCategory::Performance,
                "Style" => FindingCategory::Style,
                "Logic" => FindingCategory::Logic,
                _ => FindingCategory::Bug,
            };
            findings.push(BugBotFinding {
                severity,
                category,
                file: item["file"].as_str().unwrap_or("unknown").to_string(),
                line: item["line"].as_u64().map(|l| l as usize),
                description: item["description"]
                    .as_str()
                    .unwrap_or("No description")
                    .to_string(),
                suggestion: item["suggestion"].as_str().map(str::to_string),
            });
        }
    }

    Ok(BugBotReport {
        findings,
        files_reviewed,
        summary,
    })
}

fn extract_json(text: &str) -> &str {
    // Try to find a JSON block in markdown fences
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Try raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end >= start {
                return &text[start..=end];
            }
        }
    }
    text
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::{ChatResponse, StopReason};

    fn mock_provider(response: &str) -> Arc<dyn LlmAdapter> {
        Arc::new(MockLlmAdapter::new(vec![ChatResponse {
            content: response.to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
        }]))
    }

    #[test]
    fn finding_display_with_line() {
        let f = BugBotFinding {
            severity: FindingSeverity::High,
            category: FindingCategory::Security,
            file: "src/main.rs".into(),
            line: Some(42),
            description: "SQL injection possible".into(),
            suggestion: Some("Use parameterized queries".into()),
        };
        let s = f.to_string();
        assert!(s.contains("HIGH"));
        assert!(s.contains("Security"));
        assert!(s.contains("42"));
        assert!(s.contains("SQL injection"));
        assert!(s.contains("parameterized"));
    }

    #[test]
    fn finding_display_without_line() {
        let f = BugBotFinding {
            severity: FindingSeverity::Low,
            category: FindingCategory::Style,
            file: "lib.rs".into(),
            line: None,
            description: "unused import".into(),
            suggestion: None,
        };
        let s = f.to_string();
        assert!(s.contains("LOW"));
        assert!(s.contains("lib.rs"));
        assert!(!s.contains(':'));
    }

    #[test]
    fn report_is_clean() {
        let report = BugBotReport {
            findings: Vec::new(),
            files_reviewed: 3,
            summary: "All good".into(),
        };
        assert!(report.is_clean());
        assert_eq!(report.critical_count(), 0);
    }

    #[test]
    fn report_severity_counts() {
        let report = BugBotReport {
            findings: vec![
                BugBotFinding {
                    severity: FindingSeverity::Critical,
                    category: FindingCategory::Bug,
                    file: "a.rs".into(),
                    line: None,
                    description: "bad".into(),
                    suggestion: None,
                },
                BugBotFinding {
                    severity: FindingSeverity::High,
                    category: FindingCategory::Logic,
                    file: "b.rs".into(),
                    line: Some(10),
                    description: "iffy".into(),
                    suggestion: None,
                },
            ],
            files_reviewed: 2,
            summary: "issues found".into(),
        };
        assert_eq!(report.critical_count(), 1);
        assert_eq!(report.high_count(), 1);
        assert!(!report.is_clean());
    }

    #[test]
    fn parse_report_valid_json() {
        let json = r#"{"summary":"looks good","findings":[]}"#;
        let report = parse_report(json.to_string(), 1).unwrap();
        assert_eq!(report.summary, "looks good");
        assert!(report.findings.is_empty());
    }

    #[test]
    fn parse_report_with_findings() {
        let json = r#"{
            "summary": "Found issues",
            "findings": [
                {
                    "severity": "High",
                    "category": "Security",
                    "file": "src/auth.rs",
                    "line": 55,
                    "description": "hardcoded secret",
                    "suggestion": "use env var"
                }
            ]
        }"#;
        let report = parse_report(json.to_string(), 1).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, FindingSeverity::High);
        assert_eq!(report.findings[0].category, FindingCategory::Security);
        assert_eq!(report.findings[0].line, Some(55));
    }

    #[test]
    fn count_files_in_diff_basic() {
        let diff = "--- a/foo.rs\n+++ b/foo.rs\n--- a/bar.rs\n+++ b/bar.rs\n";
        assert_eq!(count_files_in_diff(diff), 2);
    }

    #[test]
    fn extract_json_from_markdown_fence() {
        let text = "Here is the result:\n```json\n{\"key\":\"val\"}\n```\nDone.";
        let extracted = extract_json(text);
        assert_eq!(extracted, "{\"key\":\"val\"}");
    }

    #[tokio::test]
    async fn review_diff_empty_returns_clean() {
        let provider = mock_provider(r#"{"summary":"no diff","findings":[]}"#);
        let bot = BugBot::new(provider);
        let report = bot.review_diff("").await.unwrap();
        assert!(report.is_clean());
        assert_eq!(report.files_reviewed, 0);
    }

    #[tokio::test]
    async fn review_diff_parses_llm_response() {
        let llm_response = r#"{"summary":"Found 1 issue","findings":[{"severity":"Critical","category":"Bug","file":"main.rs","line":10,"description":"use after free","suggestion":"add lifetime"}]}"#;
        let provider = mock_provider(llm_response);
        let bot = BugBot::new(provider);
        let diff = "--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let report = bot.review_diff(diff).await.unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, FindingSeverity::Critical);
        assert_eq!(report.files_reviewed, 1);
    }

    #[tokio::test]
    async fn review_files_empty_returns_clean() {
        let provider = mock_provider(r#"{"summary":"nothing","findings":[]}"#);
        let bot = BugBot::new(provider);
        let report = bot.review_files(&[]).await.unwrap();
        assert!(report.is_clean());
        assert_eq!(report.files_reviewed, 0);
    }

    #[tokio::test]
    async fn auto_fix_returns_content() {
        let provider = mock_provider("let x = vec![1, 2, 3];");
        let bot = BugBot::new(provider);
        let finding = BugBotFinding {
            severity: FindingSeverity::High,
            category: FindingCategory::Bug,
            file: "src/lib.rs".into(),
            line: Some(5),
            description: "index out of bounds".into(),
            suggestion: None,
        };
        let fix = bot.auto_fix(&finding).await;
        assert!(fix.is_some());
    }
}
