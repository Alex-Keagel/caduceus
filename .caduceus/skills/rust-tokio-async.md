---
name: rust-tokio-async
version: "1.0"
description: Async Rust patterns using Tokio — tasks, channels, streams, and structured cancellation
categories: [rust, async, concurrency]
triggers: ["tokio async", "async rust", "tokio task spawn", "async await rust", "tokio runtime setup"]
tools: [read_file, edit_file, run_tests, shell]
---

# Rust Tokio Async Skill

## Runtime Setup
```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
futures = "0.3"
tokio-stream = "0.1"
tokio-util = { version = "0.7", features = ["sync"] }
```
Use `#[tokio::main]` for the entry point; `#[tokio::test]` for async tests.

## Task Spawning
- `tokio::spawn` — fire-and-forget; collect `JoinHandle` if you need the result
- `tokio::task::spawn_blocking` — CPU-bound or blocking I/O work
- `tokio::task::LocalSet` — for `!Send` futures

## Channel Types
| Channel | Use case |
|---------|----------|
| `tokio::sync::mpsc` | Fan-in: many senders, one receiver |
| `tokio::sync::broadcast` | Fan-out: one sender, many receivers |
| `tokio::sync::oneshot` | Single response (request/reply pattern) |
| `tokio::sync::watch` | Latest-value shared state updates |

## Select and Racing
```rust
tokio::select! {
    result = operation_a() => { /* handle */ }
    _ = tokio::time::sleep(Duration::from_secs(5)) => { /* timeout */ }
}
```
- Every branch must be cancel-safe; prefer channels over holding locks across `.await`
- Use `tokio_util::sync::CancellationToken` for structured cancellation trees

## Streams
- `tokio_stream::StreamExt` adds `.next()`, `.map()`, `.filter()`, `.take_while()`
- `futures::stream::FuturesUnordered` drives many futures concurrently without ordering
- Convert `mpsc::Receiver` to a stream via `tokio_stream::wrappers::ReceiverStream`

## Common Pitfalls
- Never hold `std::sync::Mutex` across `.await` — use `tokio::sync::Mutex`
- Avoid blocking calls in async context — wrap with `spawn_blocking`
- Use `tokio::time::timeout` instead of manual sleep-based timeouts
- Profile with `tokio-console` (`tokio = { features = ["tracing"] }`)

## Testing Pattern
```rust
#[tokio::test]
async fn test_channel_roundtrip() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    tokio::spawn(async move { tx.send(42).await.unwrap(); });
    assert_eq!(rx.recv().await, Some(42));
}
```
Use `tokio::time::pause()` and `tokio::time::advance()` for time-dependent tests.
