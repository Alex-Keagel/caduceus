---
name: release
description: Create a new release
triggers:
  - "create a release"
  - "ship it"
  - "prepare release"
---
## Release Steps
1. Run all tests: cargo test --workspace
2. Run clippy: cargo clippy --workspace -- -D warnings
3. Run fmt check: cargo fmt --all --check
4. Update version in Cargo.toml
5. Update CHANGELOG.md
6. Create git tag: git tag v{version}
7. Push: git push origin main --tags
8. GitHub Actions will build and create release
