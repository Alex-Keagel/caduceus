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
}
