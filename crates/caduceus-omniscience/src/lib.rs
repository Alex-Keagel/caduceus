use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Code chunker ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file_path: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub language: String,
    pub node_type: Option<String>,
}

pub struct CodeChunker {
    chunk_size: usize,
    overlap: usize,
}

impl CodeChunker {
    pub fn new(chunk_size: usize, overlap: usize) -> Self {
        Self { chunk_size, overlap }
    }

    pub fn chunk_file(&self, path: impl Into<PathBuf>, content: &str) -> Vec<CodeChunk> {
        let path = path.into();
        let language = detect_language(&path);
        let lines: Vec<&str> = content.lines().collect();
        let mut chunks = Vec::new();
        let mut start = 0;

        while start < lines.len() {
            let end = (start + self.chunk_size).min(lines.len());
            chunks.push(CodeChunk {
                file_path: path.clone(),
                start_line: start + 1,
                end_line: end,
                content: lines[start..end].join("\n"),
                language: language.clone(),
                node_type: None,
            });
            if end == lines.len() {
                break;
            }
            start = end.saturating_sub(self.overlap);
        }

        chunks
    }
}

fn detect_language(path: &PathBuf) -> String {
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

// ── Semantic index (stub) ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk: CodeChunk,
    pub score: f32,
}

pub struct SemanticIndex {
    // TODO: Qdrant client integration
    chunks: Vec<CodeChunk>,
}

impl SemanticIndex {
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    pub async fn index_chunks(&mut self, chunks: Vec<CodeChunk>) -> caduceus_core::Result<()> {
        // TODO: Generate embeddings and upsert to Qdrant
        self.chunks.extend(chunks);
        Ok(())
    }

    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
    ) -> caduceus_core::Result<Vec<SearchResult>> {
        // TODO: Embed query and search Qdrant
        // For now, return keyword-matched chunks
        let results: Vec<SearchResult> = self
            .chunks
            .iter()
            .filter(|c| c.content.contains(query))
            .take(top_k)
            .map(|c| SearchResult { chunk: c.clone(), score: 1.0 })
            .collect();
        Ok(results)
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

impl Default for SemanticIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let chunker = CodeChunker::new(50, 10);
        let content = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunker.chunk_file("test.rs", &content);
        assert!(!chunks.is_empty());
    }

    #[tokio::test]
    async fn semantic_index_search() {
        let mut index = SemanticIndex::new();
        let chunks = vec![CodeChunk {
            file_path: "test.rs".into(),
            start_line: 1,
            end_line: 5,
            content: "fn hello_world() {}".into(),
            language: "rust".into(),
            node_type: None,
        }];
        index.index_chunks(chunks).await.unwrap();
        let results = index.search("hello", 5).await.unwrap();
        assert_eq!(results.len(), 1);
    }
}
