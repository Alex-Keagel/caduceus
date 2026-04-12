use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolType {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Import,
    Module,
    Other,
}

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Import => "import",
            Self::Module => "module",
            Self::Other => "other",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_type: SymbolType,
    pub language: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk: CodeChunk,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
    pub severity: ParseErrorSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub content: String,
    pub score: f64,
    pub has_errors: bool,
    pub file_path: String,
}

pub struct ParseErrorDownRanker {
    penalty_factor: f64,
}

impl ParseErrorDownRanker {
    pub fn new(penalty: f64) -> Self {
        Self {
            penalty_factor: penalty.clamp(0.0, 1.0),
        }
    }

    pub fn adjust_score(&self, base_score: f64, has_parse_error: bool) -> f64 {
        if !has_parse_error {
            return base_score;
        }

        let multiplier = 1.0 - self.penalty_factor;
        if base_score >= 0.0 {
            base_score * multiplier
        } else {
            base_score * (1.0 + self.penalty_factor)
        }
    }

    pub fn detect_parse_errors(content: &str, language: &str) -> Vec<ParseError> {
        detect_delimiter_errors(content, language)
    }

    #[allow(clippy::ptr_arg)]
    pub fn rank_chunks(&self, chunks: &mut Vec<ScoredChunk>) {
        for chunk in chunks.iter_mut() {
            let language = detect_language(Path::new(&chunk.file_path));
            let has_errors = !Self::detect_parse_errors(&chunk.content, &language).is_empty();
            chunk.has_errors = has_errors;
            chunk.score = self.adjust_score(chunk.score, has_errors);
        }

        chunks.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.content.cmp(&b.content))
        });
    }
}

fn delimiter_pair(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => open,
    }
}

fn detect_delimiter_errors(content: &str, language: &str) -> Vec<ParseError> {
    let supports_slash_comments = matches!(
        language,
        "rust" | "typescript" | "javascript" | "go" | "java"
    );
    let supports_hash_comments = language == "python";
    let mut errors = Vec::new();
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut chars = content.chars().peekable();
    let mut line = 1usize;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut string_delim: Option<char> = None;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            line += 1;
            in_line_comment = false;
            escape = false;
            continue;
        }

        if in_line_comment {
            continue;
        }

        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if let Some(delim) = string_delim {
            if escape {
                escape = false;
                continue;
            }

            if ch == '\\' {
                escape = true;
                continue;
            }

            if ch == delim {
                string_delim = None;
            }
            continue;
        }

        if supports_slash_comments && ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    in_line_comment = true;
                    continue;
                }
                Some('*') => {
                    chars.next();
                    in_block_comment = true;
                    continue;
                }
                _ => {}
            }
        }

        if supports_hash_comments && ch == '#' {
            in_line_comment = true;
            continue;
        }

        if matches!(ch, '\'' | '"' | '`') {
            string_delim = Some(ch);
            continue;
        }

        match ch {
            '(' | '[' | '{' => stack.push((ch, line)),
            ')' | ']' | '}' => {
                if let Some((open, open_line)) = stack.pop() {
                    if delimiter_pair(open) != ch {
                        errors.push(ParseError {
                            line,
                            message: format!(
                                "mismatched delimiter: expected '{}' to close '{}' from line {}",
                                delimiter_pair(open),
                                open,
                                open_line
                            ),
                            severity: ParseErrorSeverity::Error,
                        });
                    }
                } else {
                    errors.push(ParseError {
                        line,
                        message: format!("unexpected closing delimiter '{ch}'"),
                        severity: ParseErrorSeverity::Error,
                    });
                }
            }
            _ => {}
        }
    }

    if let Some(delim) = string_delim {
        errors.push(ParseError {
            line,
            message: format!("unterminated string literal starting with '{delim}'"),
            severity: ParseErrorSeverity::Warning,
        });
    }

    if in_block_comment {
        errors.push(ParseError {
            line,
            message: "unterminated block comment".to_string(),
            severity: ParseErrorSeverity::Warning,
        });
    }

    for (open, open_line) in stack {
        errors.push(ParseError {
            line: open_line,
            message: format!("unclosed delimiter '{open}'"),
            severity: ParseErrorSeverity::Error,
        });
    }

    errors
}

// ── Tool spec types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchInput {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_top_k() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchOutput {
    pub results: Vec<SearchResult>,
}

/// Returns the JSON tool specification for `semantic_search` that the
/// orchestrator can register and invoke.
pub fn semantic_search_tool_spec() -> serde_json::Value {
    serde_json::json!({
        "name": "semantic_search",
        "description": "Search code semantically using natural language queries. Returns matching code chunks ranked by relevance.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language search query"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "default": 10
                }
            },
            "required": ["query"]
        }
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn detect_language(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts") | Some("tsx") => "typescript",
        Some("js") | Some("jsx") => "javascript",
        Some("py") => "python",
        Some("go") => "go",
        Some("java") => "java",
        _ => "unknown",
    }
    .to_string()
}

fn compute_content_hash(content: &str) -> String {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn extract_ident(s: &str) -> String {
    s.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < f32::EPSILON || norm_b < f32::EPSILON {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

fn should_index_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "rb"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "swift"
            | "kt"
            | "scala"
            | "md"
            | "txt"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "xml"
            | "html"
            | "css"
            | "scss"
            | "sql"
            | "sh"
    )
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | ".git"
            | ".svn"
            | ".hg"
            | "dist"
            | "build"
            | "__pycache__"
            | ".tox"
            | "vendor"
            | ".idea"
            | ".vscode"
            | ".vs"
    )
}

fn collect_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if !should_skip_dir(&name_str) {
                collect_files_recursive(&path, files);
            }
        } else if path.is_file() && should_index_file(&path) {
            files.push(path);
        }
    }
}

// ── Language-specific symbol detection ────────────────────────────────────────

fn strip_rust_vis(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("pub(") {
        if let Some(pos) = rest.find(')') {
            return rest[pos + 1..].trim_start();
        }
    }
    s.strip_prefix("pub ").unwrap_or(s)
}

fn strip_keyword<'a>(s: &'a str, keyword: &str) -> &'a str {
    if let Some(rest) = s.strip_prefix(keyword) {
        if rest.starts_with(' ') {
            return rest.trim_start();
        }
    }
    s
}

fn strip_js_export(s: &str) -> &str {
    let s = s.strip_prefix("export ").unwrap_or(s);
    s.strip_prefix("default ").unwrap_or(s).trim_start()
}

fn strip_java_modifiers(s: &str) -> &str {
    let mut result = s;
    loop {
        let len_before = result.len();
        for modifier in &[
            "public ",
            "private ",
            "protected ",
            "static ",
            "final ",
            "abstract ",
            "synchronized ",
            "native ",
            "strictfp ",
        ] {
            if let Some(rest) = result.strip_prefix(modifier) {
                result = rest.trim_start();
            }
        }
        if result.len() == len_before {
            break;
        }
    }
    result
}

fn find_matching_angle_bracket(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_rust_symbol(line: &str) -> Option<(SymbolType, String)> {
    let trimmed = line.trim();

    // Imports
    if trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("pub(crate) use ")
        || trimmed.starts_with("pub(super) use ")
    {
        let use_pos = trimmed.find("use ")?;
        let path = trimmed[use_pos + 4..]
            .trim_end_matches(';')
            .trim()
            .to_string();
        return Some((SymbolType::Import, path));
    }

    let s = strip_rust_vis(trimmed);
    let s = strip_keyword(s, "async");
    let s = strip_keyword(s, "unsafe");
    let s = strip_keyword(s, "const");

    if let Some(rest) = s.strip_prefix("fn ") {
        return Some((SymbolType::Function, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("struct ") {
        return Some((SymbolType::Struct, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("enum ") {
        return Some((SymbolType::Enum, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("trait ") {
        return Some((SymbolType::Trait, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("mod ") {
        return Some((SymbolType::Module, extract_ident(rest)));
    }

    // impl blocks
    if trimmed.starts_with("impl") && !trimmed.starts_with("impl!") {
        let rest = &trimmed[4..];
        let rest = if rest.starts_with('<') {
            if let Some(pos) = find_matching_angle_bracket(rest) {
                rest[pos + 1..].trim_start()
            } else {
                rest.trim_start()
            }
        } else {
            rest.trim_start()
        };
        let name = extract_ident(rest);
        if !name.is_empty() {
            return Some((SymbolType::Other, format!("impl {name}")));
        }
    }

    None
}

fn extract_js_ts_symbol(line: &str) -> Option<(SymbolType, String)> {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix("import ") {
        let name = if let Some(from_pos) = rest.find(" from ") {
            rest[..from_pos].trim().to_string()
        } else {
            rest.trim_end_matches(';').trim().to_string()
        };
        return Some((SymbolType::Import, name));
    }

    let s = strip_js_export(trimmed);
    let s = strip_keyword(s, "async");
    let s = strip_keyword(s, "declare");

    if let Some(rest) = s.strip_prefix("function ") {
        return Some((SymbolType::Function, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("class ") {
        return Some((SymbolType::Class, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("interface ") {
        return Some((SymbolType::Interface, extract_ident(rest)));
    }
    if s.starts_with("type ") && s.contains('=') {
        return Some((SymbolType::Other, extract_ident(&s[5..])));
    }
    if let Some(rest) = s.strip_prefix("enum ") {
        return Some((SymbolType::Enum, extract_ident(rest)));
    }

    // Arrow / assigned functions: const name = (...) => | const name = function
    for prefix in &["const ", "let ", "var "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let name = extract_ident(rest);
            if !name.is_empty() {
                let after = rest[name.len()..].trim();
                if after.contains("=>") || after.contains("function") {
                    return Some((SymbolType::Function, name));
                }
            }
        }
    }

    None
}

fn extract_python_symbol(line: &str) -> Option<(SymbolType, String)> {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix("async def ") {
        return Some((SymbolType::Function, extract_ident(rest)));
    }
    if let Some(rest) = trimmed.strip_prefix("def ") {
        return Some((SymbolType::Function, extract_ident(rest)));
    }
    if let Some(rest) = trimmed.strip_prefix("class ") {
        return Some((SymbolType::Class, extract_ident(rest)));
    }

    None
}

fn extract_go_symbol(line: &str) -> Option<(SymbolType, String)> {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix("func ") {
        if rest.starts_with('(') {
            // Method: func (r *Receiver) Name(
            if let Some(close) = rest.find(')') {
                let after = rest[close + 1..].trim();
                let name = extract_ident(after);
                if !name.is_empty() {
                    return Some((SymbolType::Method, name));
                }
            }
        }
        return Some((SymbolType::Function, extract_ident(rest)));
    }

    if let Some(rest) = trimmed.strip_prefix("type ") {
        let name = extract_ident(rest);
        let after = rest[name.len()..].trim();
        if after.starts_with("struct") {
            return Some((SymbolType::Struct, name));
        }
        if after.starts_with("interface") {
            return Some((SymbolType::Interface, name));
        }
        if !name.is_empty() {
            return Some((SymbolType::Other, name));
        }
    }

    if trimmed.starts_with("import ") || trimmed == "import (" {
        return Some((SymbolType::Import, "import".to_string()));
    }

    if let Some(rest) = trimmed.strip_prefix("package ") {
        return Some((SymbolType::Module, extract_ident(rest)));
    }

    None
}

fn extract_java_symbol(line: &str) -> Option<(SymbolType, String)> {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix("import ") {
        return Some((SymbolType::Import, rest.trim_end_matches(';').to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("package ") {
        return Some((SymbolType::Module, rest.trim_end_matches(';').to_string()));
    }

    let s = strip_java_modifiers(trimmed);

    if let Some(rest) = s.strip_prefix("class ") {
        return Some((SymbolType::Class, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("interface ") {
        return Some((SymbolType::Interface, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("enum ") {
        return Some((SymbolType::Enum, extract_ident(rest)));
    }
    if let Some(rest) = s.strip_prefix("record ") {
        return Some((SymbolType::Struct, extract_ident(rest)));
    }

    // Method/constructor: <return_type> <name>(
    if s.contains('(')
        && !s.starts_with("if ")
        && !s.starts_with("while ")
        && !s.starts_with("for ")
        && !s.starts_with("switch ")
        && !s.starts_with("return ")
    {
        if let Some(paren_pos) = s.find('(') {
            let before = s[..paren_pos].trim();
            if let Some(last_space) = before.rfind(' ') {
                let name = &before[last_space + 1..];
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return Some((SymbolType::Function, name.to_string()));
                }
            } else {
                let name = extract_ident(before);
                if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    return Some((SymbolType::Function, name));
                }
            }
        }
    }

    None
}

// ── Code chunker ─────────────────────────────────────────────────────────────

pub struct CodeChunker {
    pub fallback_chunk_size: usize,
    pub fallback_overlap: usize,
}

impl Default for CodeChunker {
    fn default() -> Self {
        Self::new(50, 10)
    }
}

impl CodeChunker {
    pub fn new(fallback_chunk_size: usize, fallback_overlap: usize) -> Self {
        Self {
            fallback_chunk_size,
            fallback_overlap,
        }
    }

    /// Parse a source file into semantic code chunks.
    pub fn chunk_file(&self, path: &str, content: &str) -> Vec<CodeChunk> {
        let language = detect_language(Path::new(path));
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            return Vec::new();
        }

        match language.as_str() {
            "rust" => self.chunk_brace_language(path, &lines, &language, extract_rust_symbol),
            "typescript" | "javascript" => {
                self.chunk_brace_language(path, &lines, &language, extract_js_ts_symbol)
            }
            "go" => self.chunk_brace_language(path, &lines, &language, extract_go_symbol),
            "java" => self.chunk_brace_language(path, &lines, &language, extract_java_symbol),
            "python" => self.chunk_python(path, &lines),
            _ => self.chunk_fallback(path, &lines, &language),
        }
    }

    /// Generic chunker for brace-delimited languages.
    fn chunk_brace_language(
        &self,
        path: &str,
        lines: &[&str],
        language: &str,
        detect_symbol: fn(&str) -> Option<(SymbolType, String)>,
    ) -> Vec<CodeChunk> {
        let mut chunks = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let trimmed = lines[i].trim();

            // Skip blank lines and line comments when searching for symbols
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                i += 1;
                continue;
            }

            if let Some((sym_type, sym_name)) = detect_symbol(trimmed) {
                let symbol_start = i;

                // Single-line statements (imports, use, mod without body)
                if trimmed.ends_with(';') && !trimmed.contains('{') {
                    let content = lines[symbol_start].to_string();
                    chunks.push(CodeChunk {
                        file_path: path.to_string(),
                        symbol_name: sym_name,
                        symbol_type: sym_type,
                        language: language.to_string(),
                        start_line: symbol_start + 1,
                        end_line: symbol_start + 1,
                        content: content.clone(),
                        content_hash: compute_content_hash(&content),
                    });
                    i += 1;
                    continue;
                }

                // Multi-line: count braces to find end
                let mut brace_depth: i32 = 0;
                let mut found_open_brace = false;

                loop {
                    if i >= lines.len() {
                        break;
                    }
                    for ch in lines[i].chars() {
                        match ch {
                            '{' => {
                                brace_depth += 1;
                                found_open_brace = true;
                            }
                            '}' => {
                                brace_depth -= 1;
                            }
                            _ => {}
                        }
                    }

                    if found_open_brace && brace_depth == 0 {
                        break;
                    }
                    // Forward declaration without body
                    if !found_open_brace && lines[i].trim().ends_with(';') {
                        break;
                    }
                    if i >= lines.len() - 1 {
                        break;
                    }
                    i += 1;
                }

                let content = lines[symbol_start..=i].join("\n");
                chunks.push(CodeChunk {
                    file_path: path.to_string(),
                    symbol_name: sym_name,
                    symbol_type: sym_type,
                    language: language.to_string(),
                    start_line: symbol_start + 1,
                    end_line: i + 1,
                    content: content.clone(),
                    content_hash: compute_content_hash(&content),
                });
            }

            i += 1;
        }

        if chunks.is_empty() && !lines.is_empty() {
            return self.chunk_fallback(path, lines, language);
        }

        chunks
    }

    /// Indentation-based chunker for Python.
    fn chunk_python(&self, path: &str, lines: &[&str]) -> Vec<CodeChunk> {
        let mut chunks = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let trimmed = lines[i].trim();

            // Python imports (single-line)
            if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                let name = trimmed.split_whitespace().nth(1).unwrap_or("*").to_string();
                let content = lines[i].to_string();
                chunks.push(CodeChunk {
                    file_path: path.to_string(),
                    symbol_name: name,
                    symbol_type: SymbolType::Import,
                    language: "python".to_string(),
                    start_line: i + 1,
                    end_line: i + 1,
                    content: content.clone(),
                    content_hash: compute_content_hash(&content),
                });
                i += 1;
                continue;
            }

            if let Some((sym_type, sym_name)) = extract_python_symbol(trimmed) {
                // Include preceding decorators
                let mut actual_start = i;
                while actual_start > 0 && lines[actual_start - 1].trim().starts_with('@') {
                    actual_start -= 1;
                }

                let base_indent = lines[i].len() - lines[i].trim_start().len();
                i += 1;

                // Block continues while lines are more indented or empty
                while i < lines.len() {
                    let line = lines[i];
                    if line.trim().is_empty() {
                        i += 1;
                        continue;
                    }
                    let indent = line.len() - line.trim_start().len();
                    if indent <= base_indent {
                        break;
                    }
                    i += 1;
                }

                // Trim trailing blank lines
                let mut end = i.saturating_sub(1);
                while end > actual_start && lines[end].trim().is_empty() {
                    end -= 1;
                }

                let content = lines[actual_start..=end].join("\n");
                chunks.push(CodeChunk {
                    file_path: path.to_string(),
                    symbol_name: sym_name,
                    symbol_type: sym_type,
                    language: "python".to_string(),
                    start_line: actual_start + 1,
                    end_line: end + 1,
                    content: content.clone(),
                    content_hash: compute_content_hash(&content),
                });
                continue; // i already advanced
            }

            i += 1;
        }

        if chunks.is_empty() && !lines.is_empty() {
            return self.chunk_fallback(path, lines, "python");
        }

        chunks
    }

    /// Line-based fallback: configurable window with overlap.
    pub fn chunk_fallback(&self, path: &str, lines: &[&str], language: &str) -> Vec<CodeChunk> {
        let mut chunks = Vec::new();
        let mut start = 0;

        while start < lines.len() {
            let end = (start + self.fallback_chunk_size).min(lines.len());
            let content = lines[start..end].join("\n");
            chunks.push(CodeChunk {
                file_path: path.to_string(),
                symbol_name: format!("chunk_{}", start + 1),
                symbol_type: SymbolType::Other,
                language: language.to_string(),
                start_line: start + 1,
                end_line: end,
                content: content.clone(),
                content_hash: compute_content_hash(&content),
            });
            if end >= lines.len() {
                break;
            }
            start = end.saturating_sub(self.fallback_overlap);
        }

        chunks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModelConfig {
    pub model_name: String,
    pub dimensions: usize,
    pub provider: EmbeddingProvider,
    pub batch_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingProvider {
    OpenAI { model: String },
    Local { path: String },
    HuggingFace { model_id: String },
    Mock,
}

pub struct EmbeddingSelector {
    models: HashMap<String, EmbeddingModelConfig>,
    active: String,
}

impl EmbeddingSelector {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            active: String::new(),
        }
    }

    pub fn register_model(&mut self, name: &str, config: EmbeddingModelConfig) {
        self.models.insert(name.to_string(), config);
        if self.active.is_empty() {
            self.active = name.to_string();
        }
    }

    pub fn set_active(&mut self, name: &str) -> Result<(), String> {
        if self.models.contains_key(name) {
            self.active = name.to_string();
            Ok(())
        } else {
            Err(format!("embedding model '{name}' is not registered"))
        }
    }

    pub fn get_active(&self) -> &EmbeddingModelConfig {
        static FALLBACK_MODEL: OnceLock<EmbeddingModelConfig> = OnceLock::new();

        self.models.get(&self.active).unwrap_or_else(|| {
            FALLBACK_MODEL.get_or_init(|| EmbeddingModelConfig {
                model_name: "unconfigured".to_string(),
                dimensions: 0,
                provider: EmbeddingProvider::Mock,
                batch_size: 0,
            })
        })
    }

    pub fn list_models(&self) -> Vec<&EmbeddingModelConfig> {
        let mut models: Vec<&EmbeddingModelConfig> = self.models.values().collect();
        models.sort_by(|a, b| a.model_name.cmp(&b.model_name));
        models
    }

    pub fn default_models() -> Self {
        let mut selector = Self::new();
        selector.register_model(
            "text-embedding-3-small",
            EmbeddingModelConfig {
                model_name: "text-embedding-3-small".to_string(),
                dimensions: 1536,
                provider: EmbeddingProvider::OpenAI {
                    model: "text-embedding-3-small".to_string(),
                },
                batch_size: 64,
            },
        );
        selector.register_model(
            "ada-002",
            EmbeddingModelConfig {
                model_name: "ada-002".to_string(),
                dimensions: 1536,
                provider: EmbeddingProvider::OpenAI {
                    model: "text-embedding-ada-002".to_string(),
                },
                batch_size: 64,
            },
        );
        selector.register_model(
            "local-minilm",
            EmbeddingModelConfig {
                model_name: "local-minilm".to_string(),
                dimensions: 384,
                provider: EmbeddingProvider::Local {
                    path: "models/all-MiniLM-L6-v2.onnx".to_string(),
                },
                batch_size: 32,
            },
        );
        selector.active = "text-embedding-3-small".to_string();
        selector
    }
}

impl Default for EmbeddingSelector {
    fn default() -> Self {
        Self::new()
    }
}

// ── Embedding provider ───────────────────────────────────────────────────────

#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    async fn embed(&self, texts: Vec<String>) -> caduceus_core::Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
}

/// Hash-based deterministic embedder for testing. Same text always produces the
/// same normalized vector; no external API needed.
pub struct DummyEmbedder {
    dims: usize,
}

impl DummyEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

#[async_trait]
impl EmbeddingBackend for DummyEmbedder {
    async fn embed(&self, texts: Vec<String>) -> caduceus_core::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut seed_hasher = DefaultHasher::new();
                text.hash(&mut seed_hasher);
                let seed = seed_hasher.finish();

                let raw: Vec<f32> = (0..self.dims)
                    .map(|i| {
                        let mut h = DefaultHasher::new();
                        seed.hash(&mut h);
                        i.hash(&mut h);
                        let val = h.finish();
                        (val as f64 / u64::MAX as f64 * 2.0 - 1.0) as f32
                    })
                    .collect();

                // Normalize to unit length for meaningful cosine similarity
                let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > f32::EPSILON {
                    raw.into_iter().map(|x| x / norm).collect()
                } else {
                    raw
                }
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// Calls OpenAI `/v1/embeddings` (text-embedding-3-small, 1536 dims).
pub struct OpenAiEmbedder {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiEmbedder {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "text-embedding-3-small".to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

#[async_trait]
impl EmbeddingBackend for OpenAiEmbedder {
    async fn embed(&self, texts: Vec<String>) -> caduceus_core::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let body = serde_json::json!({
            "model": &self.model,
            "input": texts,
        });

        let response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            return Err(anyhow::anyhow!("OpenAI API error ({status}): {err_body}").into());
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse OpenAI response: {e}"))?;

        let data = parsed["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing `data` array in OpenAI response"))?;

        let embeddings: Vec<Vec<f32>> = data
            .iter()
            .map(|item| {
                item["embedding"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect()
            })
            .collect();

        Ok(embeddings)
    }

    fn dimensions(&self) -> usize {
        1536
    }
}

// ── Semantic index — in-memory vector store ──────────────────────────────────

pub struct SemanticIndex {
    entries: Vec<(CodeChunk, Vec<f32>)>,
    embedder: Box<dyn EmbeddingBackend>,
    chunker: CodeChunker,
}

impl SemanticIndex {
    pub fn new(embedder: Box<dyn EmbeddingBackend>) -> Self {
        Self {
            entries: Vec::new(),
            embedder,
            chunker: CodeChunker::default(),
        }
    }

    pub fn with_chunker(mut self, chunker: CodeChunker) -> Self {
        self.chunker = chunker;
        self
    }

    /// Walk a directory, parse every source file, embed, and store.
    pub async fn index_directory(&mut self, dir: &Path) -> caduceus_core::Result<usize> {
        let files = collect_source_files(dir);
        let mut total = 0;

        for file_path in &files {
            match std::fs::read_to_string(file_path) {
                Ok(content) => {
                    let path_str = file_path.to_string_lossy().to_string();
                    total += self.index_content(&path_str, &content).await?;
                }
                Err(e) => {
                    tracing::warn!("Skipping {}: {e}", file_path.display());
                }
            }
        }

        Ok(total)
    }

    /// Embed query and return top-k chunks ranked by cosine similarity.
    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
    ) -> caduceus_core::Result<Vec<SearchResult>> {
        if self.entries.is_empty() {
            return Ok(Vec::new());
        }

        let query_vecs = self.embedder.embed(vec![query.to_string()]).await?;
        let query_vec = match query_vecs.first() {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };

        let mut scored: Vec<SearchResult> = self
            .entries
            .iter()
            .map(|(chunk, emb)| SearchResult {
                chunk: chunk.clone(),
                score: cosine_similarity(query_vec, emb),
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);

        Ok(scored)
    }

    /// Remove existing chunks for `path`, re-read from disk, chunk, embed.
    pub async fn reindex_file(&mut self, path: &Path) -> caduceus_core::Result<usize> {
        let path_str = path.to_string_lossy().to_string();
        self.entries.retain(|(c, _)| c.file_path != path_str);

        let content = std::fs::read_to_string(path)?;
        self.index_content(&path_str, &content).await
    }

    /// Index already-loaded content (no disk I/O).
    pub async fn index_content(
        &mut self,
        path: &str,
        content: &str,
    ) -> caduceus_core::Result<usize> {
        let chunks = self.chunker.chunk_file(path, content);
        let count = chunks.len();

        if chunks.is_empty() {
            return Ok(0);
        }

        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let embeddings = self.embedder.embed(texts).await?;

        for (chunk, emb) in chunks.into_iter().zip(embeddings) {
            self.entries.push((chunk, emb));
        }

        Ok(count)
    }

    pub fn chunk_count(&self) -> usize {
        self.entries.len()
    }
}

// ── #117: Cross-Project Index Federation ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IndexedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ProjectIndex {
    pub project_name: String,
    pub root_path: String,
    pub file_count: usize,
    pub last_indexed: u64,
    pub symbols: Vec<IndexedSymbol>,
}

#[derive(Debug, Default)]
pub struct FederatedIndex {
    indices: HashMap<String, ProjectIndex>,
}

impl FederatedIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_project(&mut self, index: ProjectIndex) {
        self.indices.insert(index.project_name.clone(), index);
    }

    pub fn remove_project(&mut self, name: &str) {
        self.indices.remove(name);
    }

    pub fn search_all(&self, query: &str) -> Vec<(&str, Vec<&IndexedSymbol>)> {
        let q = query.to_lowercase();
        let mut results: Vec<(&str, Vec<&IndexedSymbol>)> = self
            .indices
            .iter()
            .filter_map(|(name, idx)| {
                let matches: Vec<&IndexedSymbol> = idx
                    .symbols
                    .iter()
                    .filter(|s| s.name.to_lowercase().contains(&q))
                    .collect();
                if matches.is_empty() {
                    None
                } else {
                    Some((name.as_str(), matches))
                }
            })
            .collect();
        results.sort_by_key(|(name, _)| *name);
        results
    }

    pub fn search_project(&self, project: &str, query: &str) -> Vec<&IndexedSymbol> {
        let q = query.to_lowercase();
        match self.indices.get(project) {
            Some(idx) => idx
                .symbols
                .iter()
                .filter(|s| s.name.to_lowercase().contains(&q))
                .collect(),
            None => vec![],
        }
    }

    pub fn list_projects(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.indices.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    pub fn total_symbols(&self) -> usize {
        self.indices.values().map(|idx| idx.symbols.len()).sum()
    }
}

// ── #204: Branch Reflection ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCategory {
    CompileError,
    RuntimeError,
    TestFailure,
    Timeout,
    ResourceExhausted,
    LogicError,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ErrorAnalysis {
    pub category: ErrorCategory,
    pub root_cause: String,
    pub affected_files: Vec<String>,
    pub severity: String,
}

#[derive(Debug, Clone)]
pub struct AlternativeApproach {
    pub description: String,
    pub confidence: f64,
    pub estimated_effort: String,
}

pub struct BranchReflector;

impl BranchReflector {
    pub fn classify_error(error: &str) -> ErrorCategory {
        let lower = error.to_lowercase();
        if lower.contains("error[e")
            || lower.contains("error: cannot")
            || lower.contains("mismatched types")
            || lower.contains("expected") && lower.contains("found")
        {
            ErrorCategory::CompileError
        } else if lower.contains("panicked")
            || lower.contains("panic")
            || lower.contains("segfault")
            || lower.contains("signal")
        {
            ErrorCategory::RuntimeError
        } else if lower.contains("test failed")
            || lower.contains("assertion failed")
            || lower.contains("failures:")
            || lower.contains("failed test")
            || lower.contains("test result:")
        {
            ErrorCategory::TestFailure
        } else if lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("deadline exceeded")
        {
            ErrorCategory::Timeout
        } else if lower.contains("out of memory")
            || lower.contains("oom")
            || lower.contains("disk full")
            || lower.contains("resource exhausted")
        {
            ErrorCategory::ResourceExhausted
        } else if lower.contains("logic")
            || lower.contains("incorrect result")
            || lower.contains("wrong output")
        {
            ErrorCategory::LogicError
        } else {
            ErrorCategory::Unknown
        }
    }

    pub fn analyze_error(error_log: &str) -> ErrorAnalysis {
        let category = Self::classify_error(error_log);

        let root_cause = match &category {
            ErrorCategory::CompileError => {
                "Type or syntax error detected in source code".to_string()
            }
            ErrorCategory::RuntimeError => "Program panicked or crashed at runtime".to_string(),
            ErrorCategory::TestFailure => "One or more test assertions failed".to_string(),
            ErrorCategory::Timeout => "Operation exceeded its time limit".to_string(),
            ErrorCategory::ResourceExhausted => "System resource limit reached".to_string(),
            ErrorCategory::LogicError => "Incorrect logic producing wrong results".to_string(),
            ErrorCategory::Unknown => "Could not determine root cause".to_string(),
        };

        let severity = match &category {
            ErrorCategory::CompileError | ErrorCategory::RuntimeError => "high".to_string(),
            ErrorCategory::TestFailure | ErrorCategory::LogicError => "medium".to_string(),
            ErrorCategory::Timeout | ErrorCategory::ResourceExhausted => "high".to_string(),
            ErrorCategory::Unknown => "low".to_string(),
        };

        // Extract file paths referenced in the error log (lines with "src/" or "-->")
        let affected_files: Vec<String> = error_log
            .lines()
            .filter_map(|line| {
                // Direct "src/" match (covers most Rust compiler output)
                if let Some(start) = line.find("src/") {
                    let segment = &line[start..];
                    let path: String = segment
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ':' && *c != ')')
                        .collect();
                    if path.ends_with(".rs") || path.ends_with(".go") || path.ends_with(".ts") {
                        return Some(path);
                    }
                }
                // "--> path:line" pattern — strip the arrow prefix before extracting
                if let Some(arrow_pos) = line.find("-->") {
                    let after_arrow = line[arrow_pos + 3..].trim_start();
                    let path: String = after_arrow
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ':' && *c != ')')
                        .collect();
                    if path.ends_with(".rs") || path.ends_with(".go") || path.ends_with(".ts") {
                        return Some(path);
                    }
                }
                None
            })
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect::<Vec<_>>()
            .tap_sort();

        ErrorAnalysis {
            category,
            root_cause,
            affected_files,
            severity,
        }
    }

    pub fn suggest_alternatives(analysis: &ErrorAnalysis) -> Vec<AlternativeApproach> {
        match analysis.category {
            ErrorCategory::CompileError => vec![
                AlternativeApproach {
                    description: "Fix type annotations and ensure trait bounds are satisfied"
                        .to_string(),
                    confidence: 0.85,
                    estimated_effort: "low".to_string(),
                },
                AlternativeApproach {
                    description: "Refactor to use concrete types instead of generics".to_string(),
                    confidence: 0.60,
                    estimated_effort: "medium".to_string(),
                },
            ],
            ErrorCategory::RuntimeError => vec![
                AlternativeApproach {
                    description: "Add bounds checking and handle edge cases explicitly".to_string(),
                    confidence: 0.80,
                    estimated_effort: "medium".to_string(),
                },
                AlternativeApproach {
                    description: "Use Result/Option instead of unwrap to propagate errors"
                        .to_string(),
                    confidence: 0.90,
                    estimated_effort: "low".to_string(),
                },
            ],
            ErrorCategory::TestFailure => vec![
                AlternativeApproach {
                    description: "Review test expectations against actual implementation behavior"
                        .to_string(),
                    confidence: 0.75,
                    estimated_effort: "low".to_string(),
                },
                AlternativeApproach {
                    description: "Add additional fixtures or mock data to isolate failure"
                        .to_string(),
                    confidence: 0.65,
                    estimated_effort: "medium".to_string(),
                },
            ],
            ErrorCategory::Timeout => vec![
                AlternativeApproach {
                    description: "Profile and optimize the critical path".to_string(),
                    confidence: 0.70,
                    estimated_effort: "high".to_string(),
                },
                AlternativeApproach {
                    description: "Increase timeout threshold or add caching".to_string(),
                    confidence: 0.80,
                    estimated_effort: "low".to_string(),
                },
            ],
            ErrorCategory::ResourceExhausted => vec![
                AlternativeApproach {
                    description: "Stream data instead of loading into memory".to_string(),
                    confidence: 0.75,
                    estimated_effort: "high".to_string(),
                },
                AlternativeApproach {
                    description: "Add resource limits and backpressure mechanisms".to_string(),
                    confidence: 0.70,
                    estimated_effort: "medium".to_string(),
                },
            ],
            ErrorCategory::LogicError => vec![
                AlternativeApproach {
                    description: "Add property-based tests to surface edge cases".to_string(),
                    confidence: 0.65,
                    estimated_effort: "medium".to_string(),
                },
                AlternativeApproach {
                    description: "Step through logic with a debugger and verify invariants"
                        .to_string(),
                    confidence: 0.80,
                    estimated_effort: "medium".to_string(),
                },
            ],
            ErrorCategory::Unknown => vec![AlternativeApproach {
                description: "Increase logging verbosity and reproduce the failure".to_string(),
                confidence: 0.50,
                estimated_effort: "low".to_string(),
            }],
        }
    }
}

// Helper trait for in-place sort returning Self
trait TapSort {
    fn tap_sort(self) -> Self;
}

impl TapSort for Vec<String> {
    fn tap_sort(mut self) -> Self {
        self.sort_unstable();
        self
    }
}

// ── #205: Autonomous Error Recovery ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum RecoveryAction {
    Retry,
    RetryWithModification(String),
    Rollback,
    SkipAndContinue,
    Escalate,
}

#[derive(Debug, Clone)]
pub struct RecoveryStrategy {
    pub name: String,
    /// Substring pattern to match against an error message
    pub error_pattern: String,
    pub action: RecoveryAction,
    pub max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct RecoveryAttempt {
    pub strategy: String,
    pub success: bool,
    pub attempt_number: u32,
}

#[derive(Debug, Default)]
pub struct ErrorRecoveryEngine {
    strategies: Vec<RecoveryStrategy>,
    history: Vec<RecoveryAttempt>,
}

impl ErrorRecoveryEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_strategy(&mut self, strategy: RecoveryStrategy) {
        self.strategies.push(strategy);
    }

    pub fn find_recovery(&self, error: &str) -> Option<&RecoveryStrategy> {
        let lower = error.to_lowercase();
        self.strategies
            .iter()
            .find(|s| lower.contains(&s.error_pattern.to_lowercase()))
    }

    pub fn record_attempt(&mut self, strategy: &str, success: bool) {
        let attempt_number = self
            .history
            .iter()
            .filter(|a| a.strategy == strategy)
            .count() as u32
            + 1;
        self.history.push(RecoveryAttempt {
            strategy: strategy.to_string(),
            success,
            attempt_number,
        });
    }

    pub fn success_rate(&self, strategy: &str) -> f64 {
        let attempts: Vec<&RecoveryAttempt> = self
            .history
            .iter()
            .filter(|a| a.strategy == strategy)
            .collect();
        if attempts.is_empty() {
            return 0.0;
        }
        let successes = attempts.iter().filter(|a| a.success).count();
        successes as f64 / attempts.len() as f64
    }

    pub fn should_escalate(&self, strategy: &str) -> bool {
        let max_attempts = self
            .strategies
            .iter()
            .find(|s| s.name == strategy)
            .map(|s| s.max_attempts)
            .unwrap_or(3);
        let failures = self
            .history
            .iter()
            .filter(|a| a.strategy == strategy && !a.success)
            .count() as u32;
        failures >= max_attempts
    }
}

// ── #232: Code Property Graph ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphNodeType {
    Function,
    Class,
    Module,
    Variable,
    Import,
    Interface,
    Trait,
    Struct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphEdgeType {
    Calls,
    InheritsFrom,
    Implements,
    Imports,
    Modifies,
    References,
    Returns,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub node_type: GraphNodeType,
    pub file: String,
    pub line: usize,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: GraphEdgeType,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub components: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodePropertyGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl CodePropertyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: GraphNode) {
        self.nodes.push(node);
    }

    pub fn add_edge(&mut self, edge: GraphEdge) {
        self.edges.push(edge);
    }

    pub fn neighbors(&self, node_id: &str) -> Vec<&GraphNode> {
        let target_ids: Vec<&str> = self
            .edges
            .iter()
            .filter(|e| e.source == node_id)
            .map(|e| e.target.as_str())
            .collect();
        self.nodes
            .iter()
            .filter(|n| target_ids.contains(&n.id.as_str()))
            .collect()
    }

    /// Transitive downstream nodes reachable from `node_id`.
    pub fn affected_by(&self, node_id: &str) -> Vec<&GraphNode> {
        let mut visited: Vec<String> = Vec::new();
        let mut queue: Vec<String> = vec![node_id.to_string()];
        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.push(current.clone());
            for edge in &self.edges {
                if edge.source == current && !visited.contains(&edge.target) {
                    queue.push(edge.target.clone());
                }
            }
        }
        // Exclude the starting node itself
        self.nodes
            .iter()
            .filter(|n| n.id != node_id && visited.contains(&n.id))
            .collect()
    }

    pub fn subgraph(&self, node_ids: &[&str]) -> CodePropertyGraph {
        let id_set: std::collections::HashSet<&str> = node_ids.iter().copied().collect();
        let nodes = self
            .nodes
            .iter()
            .filter(|n| id_set.contains(n.id.as_str()))
            .cloned()
            .collect();
        let edges = self
            .edges
            .iter()
            .filter(|e| id_set.contains(e.source.as_str()) && id_set.contains(e.target.as_str()))
            .cloned()
            .collect();
        CodePropertyGraph { nodes, edges }
    }

    pub fn to_cytoscape_json(&self) -> serde_json::Value {
        let elements: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "group": "nodes",
                    "data": {
                        "id": n.id,
                        "label": n.label,
                        "type": format!("{:?}", n.node_type),
                        "file": n.file,
                        "line": n.line,
                    }
                })
            })
            .chain(self.edges.iter().map(|e| {
                serde_json::json!({
                    "group": "edges",
                    "data": {
                        "source": e.source,
                        "target": e.target,
                        "type": format!("{:?}", e.edge_type),
                        "weight": e.weight,
                    }
                })
            }))
            .collect();
        serde_json::json!({ "elements": elements })
    }

    pub fn stats(&self) -> GraphStats {
        // Connected components via union-find on node indices
        let n = self.nodes.len();
        let mut parent: Vec<usize> = (0..n).collect();

        fn find(parent: &mut Vec<usize>, x: usize) -> usize {
            if parent[x] != x {
                parent[x] = find(parent, parent[x]);
            }
            parent[x]
        }

        for edge in &self.edges {
            let si = self.nodes.iter().position(|nd| nd.id == edge.source);
            let ti = self.nodes.iter().position(|nd| nd.id == edge.target);
            if let (Some(s), Some(t)) = (si, ti) {
                let pr = find(&mut parent, s);
                let qr = find(&mut parent, t);
                if pr != qr {
                    parent[pr] = qr;
                }
            }
        }

        let components = if n == 0 {
            0
        } else {
            (0..n).filter(|&i| find(&mut parent, i) == i).count()
        };

        GraphStats {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            components,
        }
    }
}

// ── #233: Vector Space Visualizer ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpacePoint {
    pub id: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub cluster_id: Option<String>,
    pub file: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCluster {
    pub id: String,
    pub label: String,
    pub color: String,
    pub center_x: f64,
    pub center_y: f64,
    pub center_z: f64,
    pub point_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorSpaceMap {
    pub points: Vec<SpacePoint>,
    pub clusters: Vec<SpaceCluster>,
}

impl VectorSpaceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_point(&mut self, point: SpacePoint) {
        self.points.push(point);
    }

    pub fn add_cluster(&mut self, cluster: SpaceCluster) {
        self.clusters.push(cluster);
    }

    /// Set relevance scores based on distance from a query embedding placeholder.
    /// Points with `file` containing `query` get relevance 1.0; others decay by distance.
    pub fn highlight_relevant(&mut self, query: &str, threshold: f64) {
        for point in &mut self.points {
            if point.file.contains(query) || point.label.contains(query) {
                point.relevance = 1.0;
            } else if point.relevance <= threshold {
                point.relevance = 0.0;
            }
        }
    }

    /// Return the `k` points closest to (x, y, z) in 3-D space.
    pub fn nearest_to_query(&self, x: f64, y: f64, z: f64, k: usize) -> Vec<&SpacePoint> {
        let mut scored: Vec<(f64, &SpacePoint)> = self
            .points
            .iter()
            .map(|p| {
                let d = ((p.x - x).powi(2) + (p.y - y).powi(2) + (p.z - z).powi(2)).sqrt();
                (d, p)
            })
            .collect();
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(_, p)| p).collect()
    }

    pub fn to_render_json(&self) -> serde_json::Value {
        serde_json::json!({
            "points": self.points.iter().map(|p| serde_json::json!({
                "id": p.id,
                "label": p.label,
                "position": [p.x, p.y, p.z],
                "clusterId": p.cluster_id,
                "file": p.file,
                "relevance": p.relevance,
            })).collect::<Vec<_>>(),
            "clusters": self.clusters.iter().map(|c| serde_json::json!({
                "id": c.id,
                "label": c.label,
                "color": c.color,
                "center": [c.center_x, c.center_y, c.center_z],
                "pointCount": c.point_count,
            })).collect::<Vec<_>>(),
        })
    }
}

// ── #235: AST Overlay Data ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HighlightType {
    Focus,
    Scope,
    Modified,
    Referenced,
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstHighlight {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub highlight_type: HighlightType,
    pub label: String,
    pub tooltip: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AstOverlay {
    pub highlights: Vec<AstHighlight>,
}

impl AstOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_highlight(&mut self, highlight: AstHighlight) {
        self.highlights.push(highlight);
    }

    pub fn highlights_for_file(&self, file: &str) -> Vec<&AstHighlight> {
        self.highlights.iter().filter(|h| h.file == file).collect()
    }

    pub fn clear_file(&mut self, file: &str) {
        self.highlights.retain(|h| h.file != file);
    }

    /// Emit Monaco editor decoration JSON for a given file.
    pub fn to_editor_decorations(&self, file: &str) -> serde_json::Value {
        let decorations: Vec<serde_json::Value> = self
            .highlights_for_file(file)
            .iter()
            .map(|h| {
                let class_name = match h.highlight_type {
                    HighlightType::Focus => "caduceus-focus",
                    HighlightType::Scope => "caduceus-scope",
                    HighlightType::Modified => "caduceus-modified",
                    HighlightType::Referenced => "caduceus-referenced",
                    HighlightType::Error => "caduceus-error",
                    HighlightType::Warning => "caduceus-warning",
                };
                serde_json::json!({
                    "range": {
                        "startLineNumber": h.start_line,
                        "startColumn": h.start_col,
                        "endLineNumber": h.end_line,
                        "endColumn": h.end_col,
                    },
                    "options": {
                        "className": class_name,
                        "hoverMessage": { "value": h.tooltip },
                        "glyphMarginHoverMessage": { "value": h.label },
                    }
                })
            })
            .collect();
        serde_json::json!({ "decorations": decorations })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // 1. Parse Rust file into chunks
    #[test]
    fn parse_rust_file_into_chunks() {
        let src = "\
use std::io;

pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub enum Color {
    Red,
    Green,
    Blue,
}

pub trait Drawable {
    fn draw(&self);
}

pub fn distance(a: &Point, b: &Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

impl Point {
    pub fn origin() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}
";
        let chunker = CodeChunker::default();
        let chunks = chunker.chunk_file("geometry.rs", src);

        let types: Vec<(&str, &SymbolType)> = chunks
            .iter()
            .map(|c| (c.symbol_name.as_str(), &c.symbol_type))
            .collect();

        assert!(
            types.iter().any(|(_, t)| **t == SymbolType::Import),
            "missing import"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "Point" && **t == SymbolType::Struct),
            "missing struct"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "Color" && **t == SymbolType::Enum),
            "missing enum"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "Drawable" && **t == SymbolType::Trait),
            "missing trait"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "distance" && **t == SymbolType::Function),
            "missing fn"
        );
        assert!(
            types.iter().any(|(n, _)| n.starts_with("impl")),
            "missing impl"
        );
        assert!(
            chunks.len() >= 6,
            "expected >=6 chunks, got {}",
            chunks.len()
        );

        for c in &chunks {
            assert!(!c.content_hash.is_empty());
            assert_eq!(c.language, "rust");
            assert!(c.start_line <= c.end_line);
        }
    }

    // 2. Parse TypeScript file into chunks
    #[test]
    fn parse_typescript_file_into_chunks() {
        let src = "\
import { Request, Response } from 'express';

interface Config {
    host: string;
    port: number;
}

export class Server {
    private config: Config;

    constructor(config: Config) {
        this.config = config;
    }

    start() {
        console.log('Starting');
    }
}

export function createServer(config: Config): Server {
    return new Server(config);
}

const handler = async (req: Request) => {
    return { ok: true };
};
";
        let chunker = CodeChunker::default();
        let chunks = chunker.chunk_file("server.ts", src);

        let types: Vec<(&str, &SymbolType)> = chunks
            .iter()
            .map(|c| (c.symbol_name.as_str(), &c.symbol_type))
            .collect();

        assert!(
            types.iter().any(|(_, t)| **t == SymbolType::Import),
            "missing import"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "Config" && **t == SymbolType::Interface),
            "missing interface"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "Server" && **t == SymbolType::Class),
            "missing class"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "createServer" && **t == SymbolType::Function),
            "missing fn"
        );
        assert!(
            types
                .iter()
                .any(|(n, t)| *n == "handler" && **t == SymbolType::Function),
            "missing arrow fn"
        );
        assert!(
            chunks.len() >= 5,
            "expected >=5 chunks, got {}",
            chunks.len()
        );
    }

    // 3. Fallback line chunking for unknown language
    #[test]
    fn fallback_line_chunking_unknown_language() {
        let lines: Vec<String> = (1..=120).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");

        let chunker = CodeChunker::new(50, 10);
        let chunks = chunker.chunk_file("data.xyz", &content);

        assert!(
            chunks.len() >= 3,
            "expected >=3 chunks, got {}",
            chunks.len()
        );
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 50);
        assert_eq!(chunks[0].language, "unknown");
        assert_eq!(chunks[0].symbol_type, SymbolType::Other);
        // Second chunk starts at 50-10+1 = 41
        assert_eq!(chunks[1].start_line, 41);
    }

    // 4. Cosine similarity returns correct ordering
    #[test]
    fn cosine_similarity_correct_ordering() {
        let a = vec![1.0, 0.0, 0.0];
        let identical = vec![1.0, 0.0, 0.0];
        let orthogonal = vec![0.0, 1.0, 0.0];
        let opposite = vec![-1.0, 0.0, 0.0];
        let similar = vec![0.9, 0.1, 0.0];

        let sim_identical = cosine_similarity(&a, &identical);
        let sim_similar = cosine_similarity(&a, &similar);
        let sim_orthogonal = cosine_similarity(&a, &orthogonal);
        let sim_opposite = cosine_similarity(&a, &opposite);

        assert!(
            (sim_identical - 1.0).abs() < 1e-6,
            "identical should be 1.0"
        );
        assert!(sim_orthogonal.abs() < 1e-6, "orthogonal should be 0.0");
        assert!((sim_opposite + 1.0).abs() < 1e-6, "opposite should be -1.0");

        // Ordering: identical > similar > orthogonal > opposite
        assert!(sim_identical > sim_similar);
        assert!(sim_similar > sim_orthogonal);
        assert!(sim_orthogonal > sim_opposite);

        // Edge cases
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    // 5. Index directory and search
    #[tokio::test]
    async fn index_directory_and_search() {
        let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_test_index_dir");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        std::fs::write(
            test_dir.join("math.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn multiply(a: i32, b: i32) -> i32 {\n    a * b\n}\n",
        ).unwrap();

        std::fs::write(
            test_dir.join("greet.py"),
            "def greet(name):\n    return f\"Hello, {name}!\"\n\ndef farewell(name):\n    return f\"Goodbye, {name}!\"\n",
        ).unwrap();

        let embedder = Box::new(DummyEmbedder::new(32));
        let mut index = SemanticIndex::new(embedder);
        let count = index.index_directory(&test_dir).await.unwrap();

        assert!(count >= 4, "expected >=4 chunks, got {count}");

        let results = index.search("add numbers", 3).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].score != 0.0);

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    // 6. Reindex single file
    #[tokio::test]
    async fn reindex_single_file() {
        let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_test_reindex");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        let file = test_dir.join("lib.rs");
        std::fs::write(&file, "pub fn alpha() {}\n").unwrap();

        let embedder = Box::new(DummyEmbedder::new(32));
        let mut index = SemanticIndex::new(embedder);

        index.reindex_file(&file).await.unwrap();
        assert_eq!(index.chunk_count(), 1);

        // Add a second function and reindex
        std::fs::write(&file, "pub fn alpha() {}\n\npub fn beta() {}\n").unwrap();
        index.reindex_file(&file).await.unwrap();
        assert_eq!(index.chunk_count(), 2);

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    // 7. Empty directory returns empty index
    #[tokio::test]
    async fn empty_directory_returns_empty_index() {
        let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_test_empty_dir");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        let embedder = Box::new(DummyEmbedder::new(32));
        let mut index = SemanticIndex::new(embedder);
        let count = index.index_directory(&test_dir).await.unwrap();

        assert_eq!(count, 0);
        assert_eq!(index.chunk_count(), 0);

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    // 8. Search with no results
    #[tokio::test]
    async fn search_with_no_results_on_empty_index() {
        let embedder = Box::new(DummyEmbedder::new(32));
        let index = SemanticIndex::new(embedder);

        let results = index.search("anything", 10).await.unwrap();
        assert!(results.is_empty());
    }

    // 9. Python chunking with classes and decorators
    #[test]
    fn parse_python_file_into_chunks() {
        let src = "\
import os
from pathlib import Path

class Config:
    def __init__(self, path):
        self.path = path

    def load(self):
        return {}

def standalone():
    pass

@staticmethod
def decorated():
    pass
";
        let chunker = CodeChunker::default();
        let chunks = chunker.chunk_file("config.py", src);

        let names: Vec<&str> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"os"), "missing 'os' import: {names:?}");
        assert!(
            names.contains(&"Config"),
            "missing 'Config' class: {names:?}"
        );
        assert!(
            names.contains(&"standalone"),
            "missing 'standalone' fn: {names:?}"
        );

        let config_chunk = chunks.iter().find(|c| c.symbol_name == "Config").unwrap();
        assert!(config_chunk.content.contains("__init__"));
        assert!(config_chunk.content.contains("load"));
    }

    // 10. Tool spec structure
    #[test]
    fn tool_spec_is_valid_json() {
        let spec = semantic_search_tool_spec();
        assert_eq!(spec["name"], "semantic_search");
        assert!(spec["parameters"]["properties"]["query"].is_object());
        assert!(spec["parameters"]["properties"]["top_k"].is_object());
    }

    // 11. Content hash determinism
    #[test]
    fn content_hash_is_deterministic() {
        let chunker = CodeChunker::default();
        let src = "pub fn foo() {}\n";
        let c1 = chunker.chunk_file("a.rs", src);
        let c2 = chunker.chunk_file("a.rs", src);
        assert_eq!(c1[0].content_hash, c2[0].content_hash);

        let c3 = chunker.chunk_file("a.rs", "pub fn bar() {}\n");
        assert_ne!(c1[0].content_hash, c3[0].content_hash);
    }
    #[test]
    fn parse_error_downranker_reduces_scores_for_error_chunks() {
        let ranker = ParseErrorDownRanker::new(0.25);
        let adjusted = ranker.adjust_score(0.8, true);
        assert!((adjusted - 0.6).abs() < 1e-9);

        let errors = ParseErrorDownRanker::detect_parse_errors(
            "pub fn broken() {\n    println!(\"oops\");\n",
            "rust",
        );
        assert!(!errors.is_empty());
        assert_eq!(errors[0].severity, ParseErrorSeverity::Error);
    }

    #[test]
    fn parse_error_downranker_leaves_clean_chunks_unchanged() {
        let ranker = ParseErrorDownRanker::new(0.4);
        assert!((ranker.adjust_score(0.8, false) - 0.8).abs() < 1e-9);

        let errors = ParseErrorDownRanker::detect_parse_errors(
            "pub fn clean() -> i32 {\n    42\n}\n",
            "rust",
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn parse_error_downranker_ranks_clean_chunks_first() {
        let ranker = ParseErrorDownRanker::new(0.5);
        let mut chunks = vec![
            ScoredChunk {
                content: "pub fn broken() {".to_string(),
                score: 0.9,
                has_errors: false,
                file_path: "broken.rs".to_string(),
            },
            ScoredChunk {
                content: "pub fn clean() -> i32 {\n    42\n}\n".to_string(),
                score: 0.8,
                has_errors: false,
                file_path: "clean.rs".to_string(),
            },
        ];

        ranker.rank_chunks(&mut chunks);

        assert_eq!(chunks[0].file_path, "clean.rs");
        assert!(!chunks[0].has_errors);
        assert_eq!(chunks[1].file_path, "broken.rs");
        assert!(chunks[1].has_errors);
        assert!(chunks[1].score < chunks[0].score);
    }

    #[test]
    fn embedding_selector_registers_selects_and_lists_models() {
        let mut selector = EmbeddingSelector::new();
        selector.register_model(
            "mock-large",
            EmbeddingModelConfig {
                model_name: "mock-large".to_string(),
                dimensions: 768,
                provider: EmbeddingProvider::Mock,
                batch_size: 16,
            },
        );
        selector.register_model(
            "hf-small",
            EmbeddingModelConfig {
                model_name: "hf-small".to_string(),
                dimensions: 384,
                provider: EmbeddingProvider::HuggingFace {
                    model_id: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
                },
                batch_size: 8,
            },
        );

        selector.set_active("hf-small").unwrap();
        assert_eq!(selector.get_active().model_name, "hf-small");

        let names: Vec<&str> = selector
            .list_models()
            .iter()
            .map(|config| config.model_name.as_str())
            .collect();
        assert_eq!(names, vec!["hf-small", "mock-large"]);
    }

    #[test]
    fn embedding_selector_exposes_default_models() {
        let selector = EmbeddingSelector::default_models();
        let names: Vec<&str> = selector
            .list_models()
            .iter()
            .map(|config| config.model_name.as_str())
            .collect();

        assert_eq!(selector.get_active().model_name, "text-embedding-3-small");
        assert!(names.contains(&"text-embedding-3-small"));
        assert!(names.contains(&"ada-002"));
        assert!(names.contains(&"local-minilm"));
    }

    #[test]
    fn embedding_selector_rejects_unknown_active_model() {
        let mut selector = EmbeddingSelector::default_models();
        let err = selector.set_active("missing-model").unwrap_err();
        assert!(err.contains("missing-model"));
    }

    #[test]
    fn embedding_selector_returns_mock_when_unconfigured() {
        let selector = EmbeddingSelector::new();
        let active = selector.get_active();

        assert_eq!(active.model_name, "unconfigured");
        assert_eq!(active.provider, EmbeddingProvider::Mock);
    }

    // ── #117: FederatedIndex tests ────────────────────────────────────────────

    fn make_project(name: &str, symbols: Vec<(&str, &str)>) -> ProjectIndex {
        ProjectIndex {
            project_name: name.to_string(),
            root_path: format!("/projects/{name}"),
            file_count: symbols.len(),
            last_indexed: 0,
            symbols: symbols
                .into_iter()
                .map(|(sym, kind)| IndexedSymbol {
                    name: sym.to_string(),
                    kind: kind.to_string(),
                    file: format!("src/{sym}.rs"),
                    line: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn federated_index_empty() {
        let fi = FederatedIndex::new();
        assert_eq!(fi.total_symbols(), 0);
        assert!(fi.list_projects().is_empty());
    }

    #[test]
    fn federated_index_add_and_list_projects() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project("alpha", vec![("Foo", "struct")]));
        fi.add_project(make_project("beta", vec![("Bar", "fn")]));
        let projects = fi.list_projects();
        assert_eq!(projects, vec!["alpha", "beta"]);
    }

    #[test]
    fn federated_index_total_symbols() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project("a", vec![("X", "fn"), ("Y", "fn")]));
        fi.add_project(make_project("b", vec![("Z", "struct")]));
        assert_eq!(fi.total_symbols(), 3);
    }

    #[test]
    fn federated_index_remove_project() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project("a", vec![("X", "fn")]));
        fi.remove_project("a");
        assert!(fi.list_projects().is_empty());
        assert_eq!(fi.total_symbols(), 0);
    }

    #[test]
    fn federated_index_search_all_finds_across_projects() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project(
            "alpha",
            vec![("FooParser", "struct"), ("BarHelper", "fn")],
        ));
        fi.add_project(make_project("beta", vec![("FooBuilder", "struct")]));
        let results = fi.search_all("foo");
        assert_eq!(results.len(), 2);
        // Each project should have one match
        for (_, syms) in &results {
            assert!(!syms.is_empty());
            assert!(syms.iter().all(|s| s.name.to_lowercase().contains("foo")));
        }
    }

    #[test]
    fn federated_index_search_all_empty_when_no_match() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project("alpha", vec![("FooParser", "struct")]));
        assert!(fi.search_all("zzznomatch").is_empty());
    }

    #[test]
    fn federated_index_search_project() {
        let mut fi = FederatedIndex::new();
        fi.add_project(make_project(
            "alpha",
            vec![("FooParser", "struct"), ("BarHelper", "fn")],
        ));
        let results = fi.search_project("alpha", "foo");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "FooParser");
    }

    #[test]
    fn federated_index_search_project_unknown_returns_empty() {
        let fi = FederatedIndex::new();
        assert!(fi.search_project("nonexistent", "foo").is_empty());
    }

    // ── #204: BranchReflector tests ───────────────────────────────────────────

    #[test]
    fn branch_reflector_classify_compile_error() {
        assert_eq!(
            BranchReflector::classify_error("error[E0308]: mismatched types"),
            ErrorCategory::CompileError
        );
    }

    #[test]
    fn branch_reflector_classify_runtime_error() {
        assert_eq!(
            BranchReflector::classify_error("thread 'main' panicked at 'index out of bounds'"),
            ErrorCategory::RuntimeError
        );
    }

    #[test]
    fn branch_reflector_classify_test_failure() {
        assert_eq!(
            BranchReflector::classify_error("test result: FAILED. 1 passed; 2 failed"),
            ErrorCategory::TestFailure
        );
    }

    #[test]
    fn branch_reflector_classify_timeout() {
        assert_eq!(
            BranchReflector::classify_error("operation timed out after 30s"),
            ErrorCategory::Timeout
        );
    }

    #[test]
    fn branch_reflector_classify_resource_exhausted() {
        assert_eq!(
            BranchReflector::classify_error("out of memory: OOM killer invoked"),
            ErrorCategory::ResourceExhausted
        );
    }

    #[test]
    fn branch_reflector_classify_unknown() {
        assert_eq!(
            BranchReflector::classify_error("some completely unrelated message"),
            ErrorCategory::Unknown
        );
    }

    #[test]
    fn branch_reflector_analyze_error_sets_severity() {
        let analysis = BranchReflector::analyze_error("error[E0308]: mismatched types");
        assert_eq!(analysis.category, ErrorCategory::CompileError);
        assert_eq!(analysis.severity, "high");
        assert!(!analysis.root_cause.is_empty());
    }

    #[test]
    fn branch_reflector_suggest_alternatives_non_empty() {
        let analysis = BranchReflector::analyze_error("error[E0308]: mismatched types");
        let suggestions = BranchReflector::suggest_alternatives(&analysis);
        assert!(!suggestions.is_empty());
        for s in &suggestions {
            assert!(s.confidence > 0.0 && s.confidence <= 1.0);
            assert!(!s.description.is_empty());
            assert!(!s.estimated_effort.is_empty());
        }
    }

    #[test]
    fn branch_reflector_suggest_alternatives_all_categories() {
        for log in &[
            "error[E0308]: mismatched types",
            "thread 'main' panicked",
            "test failed: assertion_failed",
            "timed out",
            "out of memory",
            "logic error in computation",
            "some unknown thing",
        ] {
            let analysis = BranchReflector::analyze_error(log);
            let suggestions = BranchReflector::suggest_alternatives(&analysis);
            assert!(!suggestions.is_empty(), "No suggestions for: {log}");
        }
    }

    // ── #205: ErrorRecoveryEngine tests ───────────────────────────────────────

    #[test]
    fn recovery_engine_empty() {
        let engine = ErrorRecoveryEngine::new();
        assert!(engine.find_recovery("some error").is_none());
        assert_eq!(engine.success_rate("any"), 0.0);
    }

    #[test]
    fn recovery_engine_add_and_find_strategy() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.add_strategy(RecoveryStrategy {
            name: "retry-network".to_string(),
            error_pattern: "connection refused".to_string(),
            action: RecoveryAction::Retry,
            max_attempts: 3,
        });
        let found = engine.find_recovery("Error: connection refused");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "retry-network");
    }

    #[test]
    fn recovery_engine_no_match_returns_none() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.add_strategy(RecoveryStrategy {
            name: "s".to_string(),
            error_pattern: "specific-error".to_string(),
            action: RecoveryAction::Retry,
            max_attempts: 2,
        });
        assert!(engine.find_recovery("some other error").is_none());
    }

    #[test]
    fn recovery_engine_success_rate() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.record_attempt("retry-network", true);
        engine.record_attempt("retry-network", false);
        engine.record_attempt("retry-network", true);
        let rate = engine.success_rate("retry-network");
        assert!((rate - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn recovery_engine_should_escalate_after_max_failures() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.add_strategy(RecoveryStrategy {
            name: "flaky".to_string(),
            error_pattern: "flaky".to_string(),
            action: RecoveryAction::Retry,
            max_attempts: 2,
        });
        assert!(!engine.should_escalate("flaky"));
        engine.record_attempt("flaky", false);
        assert!(!engine.should_escalate("flaky"));
        engine.record_attempt("flaky", false);
        assert!(engine.should_escalate("flaky"));
    }

    #[test]
    fn recovery_engine_success_does_not_trigger_escalate() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.add_strategy(RecoveryStrategy {
            name: "ok".to_string(),
            error_pattern: "err".to_string(),
            action: RecoveryAction::SkipAndContinue,
            max_attempts: 1,
        });
        engine.record_attempt("ok", true);
        engine.record_attempt("ok", true);
        assert!(!engine.should_escalate("ok"));
    }

    #[test]
    fn recovery_attempt_number_increments() {
        let mut engine = ErrorRecoveryEngine::new();
        engine.record_attempt("s", false);
        engine.record_attempt("s", true);
        assert_eq!(engine.history[0].attempt_number, 1);
        assert_eq!(engine.history[1].attempt_number, 2);
    }

    // ── #232: CodePropertyGraph tests ────────────────────────────────────────

    fn make_node(id: &str, label: &str) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type: GraphNodeType::Function,
            file: "src/lib.rs".to_string(),
            line: 1,
            metadata: HashMap::new(),
        }
    }

    fn make_edge(src: &str, tgt: &str, t: GraphEdgeType) -> GraphEdge {
        GraphEdge {
            source: src.to_string(),
            target: tgt.to_string(),
            edge_type: t,
            weight: 1.0,
        }
    }

    #[test]
    fn cpg_add_nodes_and_edges() {
        let mut g = CodePropertyGraph::new();
        g.add_node(make_node("a", "A"));
        g.add_node(make_node("b", "B"));
        g.add_edge(make_edge("a", "b", GraphEdgeType::Calls));
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn cpg_neighbors() {
        let mut g = CodePropertyGraph::new();
        g.add_node(make_node("a", "A"));
        g.add_node(make_node("b", "B"));
        g.add_node(make_node("c", "C"));
        g.add_edge(make_edge("a", "b", GraphEdgeType::Calls));
        g.add_edge(make_edge("a", "c", GraphEdgeType::Imports));
        let nb = g.neighbors("a");
        assert_eq!(nb.len(), 2);
        assert!(nb.iter().any(|n| n.id == "b"));
        assert!(nb.iter().any(|n| n.id == "c"));
    }

    #[test]
    fn cpg_affected_by_traversal() {
        let mut g = CodePropertyGraph::new();
        // a -> b -> c  (chain)
        g.add_node(make_node("a", "A"));
        g.add_node(make_node("b", "B"));
        g.add_node(make_node("c", "C"));
        g.add_edge(make_edge("a", "b", GraphEdgeType::Calls));
        g.add_edge(make_edge("b", "c", GraphEdgeType::Calls));
        let affected = g.affected_by("a");
        assert_eq!(affected.len(), 2);
        assert!(affected.iter().any(|n| n.id == "b"));
        assert!(affected.iter().any(|n| n.id == "c"));
        // Starting node itself is excluded
        assert!(!affected.iter().any(|n| n.id == "a"));
    }

    #[test]
    fn cpg_subgraph() {
        let mut g = CodePropertyGraph::new();
        g.add_node(make_node("a", "A"));
        g.add_node(make_node("b", "B"));
        g.add_node(make_node("c", "C"));
        g.add_edge(make_edge("a", "b", GraphEdgeType::Calls));
        g.add_edge(make_edge("b", "c", GraphEdgeType::Calls));
        let sub = g.subgraph(&["a", "b"]);
        assert_eq!(sub.nodes.len(), 2);
        assert_eq!(sub.edges.len(), 1); // a->b, not b->c
    }

    #[test]
    fn cpg_cytoscape_json_structure() {
        let mut g = CodePropertyGraph::new();
        g.add_node(make_node("a", "A"));
        g.add_edge(make_edge("a", "b", GraphEdgeType::Returns));
        let json = g.to_cytoscape_json();
        let elems = json["elements"].as_array().unwrap();
        assert_eq!(elems.len(), 2);
        assert_eq!(elems[0]["group"], "nodes");
        assert_eq!(elems[1]["group"], "edges");
    }

    #[test]
    fn cpg_stats_components() {
        let mut g = CodePropertyGraph::new();
        g.add_node(make_node("a", "A"));
        g.add_node(make_node("b", "B"));
        // Two isolated nodes = 2 components
        let s = g.stats();
        assert_eq!(s.node_count, 2);
        assert_eq!(s.edge_count, 0);
        assert_eq!(s.components, 2);

        g.add_edge(make_edge("a", "b", GraphEdgeType::Calls));
        let s2 = g.stats();
        assert_eq!(s2.components, 1);
    }

    // ── #233: VectorSpaceMap tests ────────────────────────────────────────────

    fn make_point(id: &str, x: f64, y: f64, z: f64) -> SpacePoint {
        SpacePoint {
            id: id.to_string(),
            label: id.to_string(),
            x,
            y,
            z,
            cluster_id: None,
            file: format!("src/{id}.rs"),
            relevance: 0.5,
        }
    }

    #[test]
    fn vsm_add_points_and_clusters() {
        let mut m = VectorSpaceMap::new();
        m.add_point(make_point("p1", 0.0, 0.0, 0.0));
        m.add_point(make_point("p2", 1.0, 1.0, 1.0));
        m.add_cluster(SpaceCluster {
            id: "c1".to_string(),
            label: "cluster".to_string(),
            color: "#ff0000".to_string(),
            center_x: 0.5,
            center_y: 0.5,
            center_z: 0.5,
            point_count: 2,
        });
        assert_eq!(m.points.len(), 2);
        assert_eq!(m.clusters.len(), 1);
    }

    #[test]
    fn vsm_nearest_to_query() {
        let mut m = VectorSpaceMap::new();
        m.add_point(make_point("near", 0.1, 0.0, 0.0));
        m.add_point(make_point("far", 100.0, 100.0, 100.0));
        let nearest = m.nearest_to_query(0.0, 0.0, 0.0, 1);
        assert_eq!(nearest.len(), 1);
        assert_eq!(nearest[0].id, "near");
    }

    #[test]
    fn vsm_highlight_relevant() {
        let mut m = VectorSpaceMap::new();
        let mut p = make_point("auth", 0.0, 0.0, 0.0);
        p.file = "src/auth.rs".to_string();
        p.relevance = 0.1;
        m.add_point(p);
        m.add_point(make_point("other", 1.0, 1.0, 1.0));
        m.highlight_relevant("auth", 0.5);
        assert_eq!(m.points[0].relevance, 1.0); // matched
        assert_eq!(m.points[1].relevance, 0.0); // below threshold, set to 0
    }

    #[test]
    fn vsm_render_json_structure() {
        let mut m = VectorSpaceMap::new();
        m.add_point(make_point("p1", 1.0, 2.0, 3.0));
        let json = m.to_render_json();
        let pts = json["points"].as_array().unwrap();
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0]["id"], "p1");
        let pos = pts[0]["position"].as_array().unwrap();
        assert_eq!(pos[0].as_f64().unwrap(), 1.0);
    }

    // ── #235: AstOverlay tests ────────────────────────────────────────────────

    fn make_highlight(file: &str, t: HighlightType) -> AstHighlight {
        AstHighlight {
            file: file.to_string(),
            start_line: 10,
            end_line: 15,
            start_col: 1,
            end_col: 20,
            highlight_type: t,
            label: "test label".to_string(),
            tooltip: "test tooltip".to_string(),
        }
    }

    #[test]
    fn ast_overlay_add_and_filter() {
        let mut overlay = AstOverlay::new();
        overlay.add_highlight(make_highlight("src/foo.rs", HighlightType::Focus));
        overlay.add_highlight(make_highlight("src/bar.rs", HighlightType::Error));
        overlay.add_highlight(make_highlight("src/foo.rs", HighlightType::Modified));
        assert_eq!(overlay.highlights_for_file("src/foo.rs").len(), 2);
        assert_eq!(overlay.highlights_for_file("src/bar.rs").len(), 1);
        assert_eq!(overlay.highlights_for_file("src/other.rs").len(), 0);
    }

    #[test]
    fn ast_overlay_clear_file() {
        let mut overlay = AstOverlay::new();
        overlay.add_highlight(make_highlight("src/foo.rs", HighlightType::Scope));
        overlay.add_highlight(make_highlight("src/bar.rs", HighlightType::Warning));
        overlay.clear_file("src/foo.rs");
        assert_eq!(overlay.highlights.len(), 1);
        assert_eq!(overlay.highlights[0].file, "src/bar.rs");
    }

    #[test]
    fn ast_overlay_editor_decorations_json() {
        let mut overlay = AstOverlay::new();
        overlay.add_highlight(make_highlight("src/foo.rs", HighlightType::Error));
        let json = overlay.to_editor_decorations("src/foo.rs");
        let decs = json["decorations"].as_array().unwrap();
        assert_eq!(decs.len(), 1);
        assert_eq!(decs[0]["options"]["className"], "caduceus-error");
        assert_eq!(decs[0]["range"]["startLineNumber"], 10);
        assert_eq!(decs[0]["range"]["endLineNumber"], 15);
    }

    #[test]
    fn ast_overlay_decorations_empty_for_unknown_file() {
        let overlay = AstOverlay::new();
        let json = overlay.to_editor_decorations("nonexistent.rs");
        assert_eq!(json["decorations"].as_array().unwrap().len(), 0);
    }
}
