# openoman

openoman is a secure agent runner project that starts with a Rust core service and a microVM-first architecture to safely execute untrusted automation while keeping credentials and publishing actions in trusted host code.

## Build and test

Run all local quality checks from the repository root:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
