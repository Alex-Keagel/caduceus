---
name: rust-axum-api
version: "1.0"
description: Build production-ready REST APIs with the Axum web framework in Rust
categories: [rust, backend, api]
triggers: ["build axum api", "create rest api rust", "axum endpoint", "axum router", "axum handler"]
tools: [read_file, edit_file, run_tests, shell]
---

# Rust Axum API Skill

## Setup — `Cargo.toml`
```toml
[dependencies]
axum = { version = "0.7", features = ["macros"] }
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.5", features = ["cors", "trace", "compression-br"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

## Router Structure
- Use `Router::new()` with `.route()` for each endpoint
- Nest sub-routers: `Router::nest("/api/v1", api_router)`
- Apply middleware with `.layer()`; outermost layer runs first
- Share state via `State<Arc<AppState>>`; derive `Clone` on the wrapper

## Handler Pattern
```rust
async fn create_item(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateItemRequest>,
) -> Result<Json<Item>, AppError> {
    // validate, persist, return
}
```
- Return `impl IntoResponse` or typed `Result<T, E>` where `E: IntoResponse`
- Use `axum::extract::Path`, `Query`, `Json`, `Extension` for extraction
- Apply `#[axum::debug_handler]` during development for clear compile errors

## Error Handling
- Define an `AppError` enum implementing `IntoResponse`
- Map domain errors to HTTP status codes centrally
- Use `?` propagation; avoid `unwrap()` in handlers

## Recommended Middleware Stack
1. `TraceLayer` — request/response logging
2. `CompressionLayer` — response compression
3. `CorsLayer` — CORS headers
4. `TimeoutLayer` — prevent hanging connections
5. Custom auth middleware via `from_fn`

## Testing
- Bind to port 0 via `TcpListener` for integration tests
- Test handlers by calling the router directly without a real socket
- Assert status codes and JSON body structure in each test

## Production Checklist
- Set `RUST_LOG=info` and configure `tracing_subscriber` with `EnvFilter`
- Bind to `0.0.0.0:PORT` with graceful shutdown via `tokio::signal`
- Use `RequestBodyLimitLayer` to cap request sizes
- Run `cargo clippy -- -D warnings` and `cargo fmt --check` in CI
