---
name: rust-testing-patterns
version: "1.0"
description: Unit and integration testing in Rust — rstest parametrize, mockall, insta snapshots, and coverage
categories: [rust, testing, quality]
triggers: ["rust testing", "cargo test patterns", "rstest parametrize", "mockall rust", "rust integration test"]
tools: [read_file, edit_file, run_tests, shell]
---

# Rust Testing Patterns Skill

## Dev Dependencies
```toml
[dev-dependencies]
rstest = "0.21"
mockall = "0.13"
pretty_assertions = "1"
insta = "1"
tokio = { version = "1", features = ["test-util"] }
```

## Unit Test Layout
- Place unit tests in a `#[cfg(test)]` module in the same file as the production code
- Use `#[test]` for sync tests; `#[tokio::test]` for async tests
- Name tests `<method>_<scenario>_<expectation>` or `given_<state>_when_<action>_then_<result>`

## Parameterized Tests with rstest
```rust
use rstest::rstest;

#[rstest]
#[case(1, 1)]
#[case(2, 4)]
#[case(3, 9)]
fn squares_correctly(#[case] input: u32, #[case] expected: u32) {
    assert_eq!(input * input, expected);
}
```

## Mocking with mockall
```rust
use mockall::{automock, predicate::eq};

#[automock]
pub trait UserRepository {
    fn find_by_id(&self, id: u64) -> Option<User>;
}

#[test]
fn service_returns_user_when_found() {
    let mut mock = MockUserRepository::new();
    mock.expect_find_by_id()
        .with(eq(1))
        .returning(|_| Some(User::default()));
    let svc = UserService::new(mock);
    assert!(svc.get_user(1).is_some());
}
```

## Integration Tests
- Place in `tests/` directory at crate root; each file compiles to a separate test binary
- Share fixtures and helpers via `tests/common/mod.rs`
- Start and stop real dependencies in `#[tokio::test]` setup blocks using Docker or in-process

## Snapshot Testing with insta
```rust
#[test]
fn report_output_snapshot() {
    let result = render_report(&fixture_data());
    insta::assert_snapshot!(result);
}
```
Run `cargo insta review` to interactively accept or reject snapshot changes in CI review.

## Coverage
```bash
cargo install cargo-llvm-cov
cargo llvm-cov                              # terminal report
cargo llvm-cov --lcov --output-path lcov.info
cargo llvm-cov --fail-under-lines 80       # fail CI if below threshold
```

## CI Pipeline
```yaml
- run: cargo test --all-features
- run: cargo test --doc
- run: cargo llvm-cov --fail-under-lines 80
- run: cargo clippy -- -D warnings
```
