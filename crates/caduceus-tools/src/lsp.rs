//! Language Server Protocol (LSP) manager service.
//!
//! Provides a unified façade over per-language LSP providers so agent
//! tools can resolve symbols, references, hovers, document symbols and
//! diagnostics without each tool re-implementing JSON-RPC plumbing.
//!
//! Design:
//! - `LspProvider` trait — a single LSP server adapter (one per
//!   language). Implementations may shell out to a stdio process,
//!   embed a WASM linter, or be an in-memory mock for tests.
//! - `LspManager` — registry that maps language IDs to providers and
//!   file extensions to language IDs. Routes calls based on the file
//!   path's extension.
//!
//! Wiring (in agent tools):
//! ```ignore
//! let manager = Arc::new(LspManager::new());
//! manager.register_extension("rs", "rust");
//! manager.install_provider("rust", Arc::new(MyRustLspProvider::new())).await;
//! let defs = manager.definition(Path::new("src/lib.rs"), 10, 4).await?;
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum LspError {
    #[error("no provider registered for language '{0}'")]
    NoProvider(String),
    #[error("could not infer language for file '{0}'")]
    UnknownLanguage(PathBuf),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: PathBuf,
    pub range: Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    /// Optional source identifier (e.g., "rustc", "tsserver").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub range: Range,
    /// Optional parent symbol name for nested definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// Adapter over a single LSP server / language.
#[async_trait]
pub trait LspProvider: Send + Sync {
    fn language(&self) -> &str;

    async fn definition(&self, file: &Path, position: Position) -> Result<Vec<Location>, LspError>;

    async fn references(&self, file: &Path, position: Position) -> Result<Vec<Location>, LspError>;

    async fn hover(&self, file: &Path, position: Position) -> Result<Option<String>, LspError>;

    async fn document_symbols(&self, file: &Path) -> Result<Vec<Symbol>, LspError>;

    async fn diagnostics(&self, file: &Path) -> Result<Vec<Diagnostic>, LspError>;

    /// Best-effort cleanup. Default is a no-op.
    async fn shutdown(&self) {}
}

// ── Manager ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct ManagerState {
    /// language id → provider
    providers: HashMap<String, Arc<dyn LspProvider>>,
    /// file extension (lowercase, no dot) → language id
    extensions: HashMap<String, String>,
}

#[derive(Clone, Default)]
pub struct LspManager {
    state: Arc<RwLock<ManagerState>>,
}

impl LspManager {
    pub fn new() -> Self {
        let mut state = ManagerState::default();
        for (ext, lang) in DEFAULT_EXTENSIONS {
            state
                .extensions
                .insert((*ext).to_string(), (*lang).to_string());
        }
        Self {
            state: Arc::new(RwLock::new(state)),
        }
    }

    pub async fn register_extension(&self, ext: &str, language: &str) {
        let mut s = self.state.write().await;
        s.extensions.insert(
            ext.trim_start_matches('.').to_lowercase(),
            language.to_string(),
        );
    }

    pub async fn install_provider(&self, language: &str, provider: Arc<dyn LspProvider>) {
        let mut s = self.state.write().await;
        s.providers.insert(language.to_string(), provider);
    }

    pub async fn uninstall_provider(&self, language: &str) -> Option<Arc<dyn LspProvider>> {
        let mut s = self.state.write().await;
        s.providers.remove(language)
    }

    pub async fn languages(&self) -> Vec<String> {
        let s = self.state.read().await;
        let mut out: Vec<String> = s.providers.keys().cloned().collect();
        out.sort();
        out
    }

    pub async fn shutdown_all(&self) {
        let providers: Vec<Arc<dyn LspProvider>> = {
            let s = self.state.read().await;
            s.providers.values().cloned().collect()
        };
        for p in providers {
            p.shutdown().await;
        }
    }

    /// Resolve a file path to its language id, then to its provider.
    /// Returns `Err` if either step fails — never silently no-ops.
    async fn resolve(&self, file: &Path) -> Result<Arc<dyn LspProvider>, LspError> {
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .ok_or_else(|| LspError::UnknownLanguage(file.to_path_buf()))?;
        let s = self.state.read().await;
        let lang = s
            .extensions
            .get(&ext)
            .cloned()
            .ok_or_else(|| LspError::UnknownLanguage(file.to_path_buf()))?;
        s.providers
            .get(&lang)
            .cloned()
            .ok_or(LspError::NoProvider(lang))
    }

    pub async fn definition(
        &self,
        file: &Path,
        position: Position,
    ) -> Result<Vec<Location>, LspError> {
        self.resolve(file).await?.definition(file, position).await
    }

    pub async fn references(
        &self,
        file: &Path,
        position: Position,
    ) -> Result<Vec<Location>, LspError> {
        self.resolve(file).await?.references(file, position).await
    }

    pub async fn hover(&self, file: &Path, position: Position) -> Result<Option<String>, LspError> {
        self.resolve(file).await?.hover(file, position).await
    }

    pub async fn document_symbols(&self, file: &Path) -> Result<Vec<Symbol>, LspError> {
        self.resolve(file).await?.document_symbols(file).await
    }

    pub async fn diagnostics(&self, file: &Path) -> Result<Vec<Diagnostic>, LspError> {
        self.resolve(file).await?.diagnostics(file).await
    }
}

const DEFAULT_EXTENSIONS: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("py", "python"),
    ("go", "go"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("rb", "ruby"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("hpp", "cpp"),
    ("cc", "cpp"),
    ("cs", "csharp"),
    ("swift", "swift"),
    ("php", "php"),
    ("scala", "scala"),
    ("ex", "elixir"),
    ("exs", "elixir"),
    ("erl", "erlang"),
    ("hs", "haskell"),
    ("ml", "ocaml"),
    ("zig", "zig"),
    ("dart", "dart"),
    ("lua", "lua"),
];

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mock provider that records every call and returns canned data.
    struct MockProvider {
        lang: String,
        calls: Arc<Mutex<Vec<String>>>,
        defs: Vec<Location>,
        refs: Vec<Location>,
        hover: Option<String>,
        symbols: Vec<Symbol>,
        diags: Vec<Diagnostic>,
        fail_with: Option<String>,
    }

    impl MockProvider {
        fn new(lang: &str) -> Self {
            Self {
                lang: lang.to_string(),
                calls: Arc::new(Mutex::new(Vec::new())),
                defs: Vec::new(),
                refs: Vec::new(),
                hover: None,
                symbols: Vec::new(),
                diags: Vec::new(),
                fail_with: None,
            }
        }
    }

    #[async_trait]
    impl LspProvider for MockProvider {
        fn language(&self) -> &str {
            &self.lang
        }
        async fn definition(
            &self,
            _file: &Path,
            _position: Position,
        ) -> Result<Vec<Location>, LspError> {
            self.calls.lock().unwrap().push("definition".into());
            if let Some(e) = &self.fail_with {
                return Err(LspError::Provider(e.clone()));
            }
            Ok(self.defs.clone())
        }
        async fn references(
            &self,
            _file: &Path,
            _position: Position,
        ) -> Result<Vec<Location>, LspError> {
            self.calls.lock().unwrap().push("references".into());
            Ok(self.refs.clone())
        }
        async fn hover(
            &self,
            _file: &Path,
            _position: Position,
        ) -> Result<Option<String>, LspError> {
            self.calls.lock().unwrap().push("hover".into());
            Ok(self.hover.clone())
        }
        async fn document_symbols(&self, _file: &Path) -> Result<Vec<Symbol>, LspError> {
            self.calls.lock().unwrap().push("document_symbols".into());
            Ok(self.symbols.clone())
        }
        async fn diagnostics(&self, _file: &Path) -> Result<Vec<Diagnostic>, LspError> {
            self.calls.lock().unwrap().push("diagnostics".into());
            Ok(self.diags.clone())
        }
        async fn shutdown(&self) {
            self.calls.lock().unwrap().push("shutdown".into());
        }
    }

    fn pos(line: u32, col: u32) -> Position {
        Position { line, column: col }
    }
    fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Range {
        Range {
            start: pos(l1, c1),
            end: pos(l2, c2),
        }
    }

    #[tokio::test]
    async fn default_extensions_include_common_languages() {
        let mgr = LspManager::new();
        // We can't call resolve() without a provider, but we can probe
        // via install + call.
        let p = Arc::new(MockProvider::new("rust"));
        mgr.install_provider("rust", p.clone()).await;
        let res = mgr
            .definition(Path::new("foo.rs"), pos(0, 0))
            .await
            .unwrap();
        assert!(res.is_empty());
        assert_eq!(p.calls.lock().unwrap().as_slice(), ["definition"]);
    }

    #[tokio::test]
    async fn unknown_extension_returns_unknown_language() {
        let mgr = LspManager::new();
        let err = mgr
            .definition(Path::new("foo.unknown_ext_xyz"), pos(0, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, LspError::UnknownLanguage(_)));
    }

    #[tokio::test]
    async fn no_extension_returns_unknown_language() {
        let mgr = LspManager::new();
        let err = mgr
            .definition(Path::new("Makefile"), pos(0, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, LspError::UnknownLanguage(_)));
    }

    #[tokio::test]
    async fn known_language_without_provider_returns_no_provider() {
        let mgr = LspManager::new();
        let err = mgr
            .definition(Path::new("foo.rs"), pos(0, 0))
            .await
            .unwrap_err();
        match err {
            LspError::NoProvider(lang) => assert_eq!(lang, "rust"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn custom_extension_overrides_default() {
        let mgr = LspManager::new();
        // Pretend .rs files are actually a custom DSL.
        mgr.register_extension("rs", "mydsl").await;
        let p = Arc::new(MockProvider::new("mydsl"));
        mgr.install_provider("mydsl", p.clone()).await;
        let _ = mgr.hover(Path::new("foo.rs"), pos(0, 0)).await.unwrap();
        assert_eq!(p.calls.lock().unwrap().as_slice(), ["hover"]);
    }

    #[tokio::test]
    async fn extension_lookup_is_case_insensitive() {
        let mgr = LspManager::new();
        let p = Arc::new(MockProvider::new("rust"));
        mgr.install_provider("rust", p.clone()).await;
        // Upper-case extension on disk.
        let _ = mgr
            .references(Path::new("FOO.RS"), pos(1, 1))
            .await
            .unwrap();
        assert_eq!(p.calls.lock().unwrap().as_slice(), ["references"]);
    }

    #[tokio::test]
    async fn provider_error_is_propagated() {
        let mgr = LspManager::new();
        let mut prov = MockProvider::new("rust");
        prov.fail_with = Some("boom".into());
        mgr.install_provider("rust", Arc::new(prov)).await;
        let err = mgr
            .definition(Path::new("a.rs"), pos(0, 0))
            .await
            .unwrap_err();
        match err {
            LspError::Provider(m) => assert_eq!(m, "boom"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_canned_diagnostics_and_symbols() {
        let mgr = LspManager::new();
        let mut prov = MockProvider::new("rust");
        prov.diags = vec![Diagnostic {
            range: range(1, 0, 1, 5),
            severity: Severity::Error,
            message: "missing semicolon".into(),
            source: Some("rustc".into()),
        }];
        prov.symbols = vec![Symbol {
            name: "main".into(),
            kind: "function".into(),
            range: range(0, 0, 10, 0),
            container: None,
        }];
        mgr.install_provider("rust", Arc::new(prov)).await;
        let diags = mgr.diagnostics(Path::new("a.rs")).await.unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        let symbols = mgr.document_symbols(Path::new("a.rs")).await.unwrap();
        assert_eq!(symbols[0].name, "main");
    }

    #[tokio::test]
    async fn languages_lists_installed_providers_sorted() {
        let mgr = LspManager::new();
        mgr.install_provider("rust", Arc::new(MockProvider::new("rust")))
            .await;
        mgr.install_provider("python", Arc::new(MockProvider::new("python")))
            .await;
        mgr.install_provider("go", Arc::new(MockProvider::new("go")))
            .await;
        let langs = mgr.languages().await;
        assert_eq!(langs, vec!["go", "python", "rust"]);
    }

    #[tokio::test]
    async fn uninstall_removes_provider() {
        let mgr = LspManager::new();
        let p = Arc::new(MockProvider::new("rust"));
        mgr.install_provider("rust", p.clone()).await;
        let removed = mgr.uninstall_provider("rust").await;
        assert!(removed.is_some());
        let err = mgr
            .definition(Path::new("a.rs"), pos(0, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, LspError::NoProvider(_)));
    }

    #[tokio::test]
    async fn shutdown_all_invokes_each_provider() {
        let mgr = LspManager::new();
        let p1 = Arc::new(MockProvider::new("rust"));
        let p2 = Arc::new(MockProvider::new("python"));
        mgr.install_provider("rust", p1.clone()).await;
        mgr.install_provider("python", p2.clone()).await;
        mgr.shutdown_all().await;
        assert!(p1.calls.lock().unwrap().contains(&"shutdown".to_string()));
        assert!(p2.calls.lock().unwrap().contains(&"shutdown".to_string()));
    }

    #[tokio::test]
    async fn manager_clone_shares_state() {
        let mgr = LspManager::new();
        let mgr2 = mgr.clone();
        let p = Arc::new(MockProvider::new("rust"));
        mgr.install_provider("rust", p.clone()).await;
        // Provider installed via mgr should be visible via mgr2.
        let _ = mgr2.definition(Path::new("a.rs"), pos(0, 0)).await.unwrap();
        assert_eq!(p.calls.lock().unwrap().as_slice(), ["definition"]);
    }

    #[tokio::test]
    async fn concurrent_installs_no_deadlock() {
        let mgr = Arc::new(LspManager::new());
        let mut tasks = Vec::new();
        for i in 0..16 {
            let mgr = mgr.clone();
            tasks.push(tokio::spawn(async move {
                let lang = format!("lang-{i}");
                mgr.install_provider(&lang, Arc::new(MockProvider::new(&lang)))
                    .await;
                mgr.languages().await;
            }));
        }
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            futures::future::join_all(tasks),
        )
        .await;
        assert!(res.is_ok(), "concurrent installs deadlocked");
        assert_eq!(mgr.languages().await.len(), 16);
    }

    #[test]
    fn diagnostic_round_trip_serde() {
        let d = Diagnostic {
            range: range(1, 0, 1, 5),
            severity: Severity::Warning,
            message: "x".into(),
            source: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
