# openoman

openoman is a secure agent runner project that starts with a Rust core service and a microVM-first architecture to safely execute untrusted automation while keeping credentials and publishing actions in trusted host code.

## Build and test

Run all local quality checks from the repository root:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`

## CLI (Epic 3 MVP surface)

The `openoman` binary provides a minimal local workflow against SQLite:

- `openoman --config ./config.toml submit --repo github.com/acme/repo --revision main`
- `openoman --config ./config.toml status <job_id>`
- `openoman --config ./config.toml run <job_id>`
- `openoman --config ./config.toml logs <job_id>`
- `openoman --config ./config.toml artifacts <job_id>`
- `openoman --config ./config.toml result <job_id>`

Configuration is loaded from `--config` and can be overridden with `OPENOMAN_DATABASE_PATH`.
