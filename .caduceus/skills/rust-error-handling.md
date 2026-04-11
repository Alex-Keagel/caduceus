---
name: rust-error-handling
version: "1.0"
description: Rust error handling with thiserror for library errors and anyhow for application-level context chains
categories: [rust, error-handling, quality]
triggers: ["rust error handling", "thiserror library", "anyhow context rust", "error types result rust", "custom error enum"]
tools: [read_file, edit_file, run_tests, shell]
---

# Rust Error Handling Skill

## Dependencies
```toml
[dependencies]
thiserror = "1"   # Typed errors for libraries and domain logic
anyhow = "1"      # Context-chained errors for binaries and orchestration
```

## When to Use Which
- **`thiserror`**: libraries, domain logic, public APIs — callers can match on variants
- **`anyhow`**: binaries, CLI tools, top-level orchestration — ergonomic `?` with context

## Library Errors with thiserror
```rust
#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("record not found: {id}")]
    NotFound { id: u64 },

    #[error("connection failed")]
    Connection(#[from] sqlx::Error),

    #[error("invalid input: {0}")]
    Validation(String),
}
```

## Application Errors with anyhow
```rust
use anyhow::{Context, Result};

fn load_config(path: &str) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {path}"))?;
    let config: Config = toml::from_str(&text).context("invalid TOML in config")?;
    Ok(config)
}
```

## Propagation Rules
- Always prefer `?` over `unwrap()` in production code
- Add `.context()` or `.with_context()` at I/O and FFI boundaries to preserve callsite info
- Use `map_err` when converting between typed error variants in library code
- Print `{:?}` (Debug) for full chains in binaries; `{}` (Display) for user-facing messages

## Custom Result Type Alias
```rust
pub type Result<T, E = MyError> = std::result::Result<T, E>;
```

## Handling Multiple Error Types in Libraries
- Define a top-level error enum with `#[from]` variants for each dependency error
- Avoid `Box<dyn Error>` in library return types — it erases variant info for callers
- Use `thiserror::Error` derive on every custom error type

## Testing Error Paths
```rust
#[test]
fn returns_not_found_error() {
    let result = repo.find(999);
    assert!(matches!(result, Err(DatabaseError::NotFound { .. })));
}
```
- Test every public error variant is reachable from the API
- Use `.unwrap_err()` to assert on the error without match boilerplate
- Use `assert!(result.is_err())` as a minimum check; add variant matching when meaningful
