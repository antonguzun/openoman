# openoman

openoman is a secure agent runner project that starts with a Rust core service and a microVM-first architecture to safely execute untrusted automation while keeping credentials and publishing actions in trusted host code.

## Build and test

Run all local quality checks from the repository root:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`

## Git Hooks

This repository ships a repo-managed `pre-commit` hook for secret scanning.

Enable it once per clone from the repository root:

- `git config core.hooksPath .githooks`

The hook requires `gitleaks` to be installed and available on `PATH`. It scans staged content only, so it blocks newly introduced secrets without rescanning the entire working tree on each commit.

You can run the scanner manually from the repository root:

- `gitleaks dir . --config .gitleaks.toml`

If the hook reports a false positive, narrow the allowlist in `.gitleaks.toml` deliberately instead of bypassing it routinely. Emergency bypass remains available through:

- `git commit --no-verify`

## CLI (Epic 3 MVP surface)

The `openoman` binary provides a minimal local workflow against SQLite:

- `openoman --config ./config.toml submit --repo kickfoss --revision main --instruction "update README"`
- `openoman --config ./config.toml submit --repo https://github.com/acme/repo.git --revision main --instruction "update README"`
- `openoman --config ./config.toml status <job_id>`
- `openoman --config ./config.toml run <job_id>`
- `openoman --config ./config.toml logs <job_id>`
- `openoman --config ./config.toml artifacts <job_id>`
- `openoman --config ./config.toml result <job_id>`

Configuration is loaded from `--config` and can be overridden with `OPENOMAN_DATABASE_PATH`.

When a submitted job uses `--publish-policy on_validation_success`, `run` applies the canonical patch to the trusted clone and either publishes (GitHub aliases with token) or records `publish_warning=...` and skips publishing (non-GitHub aliases or missing token). `result <job_id>` prints stored branch/PR metadata when publishing succeeded, and prints `publish_warning` when publishing was skipped intentionally.

Startup now validates the configured sandbox backend before any command runs. With the default Firecracker backend, `openoman` will fail fast if required host dependencies such as `firecracker`, `/dev/kvm`, or the configured guest asset paths are unavailable.

## Agent providers

The sandbox agent contract now uses provider-neutral `[agent]` keys:

- `provider = "codex"` or `provider = "cursor"`
- `bin = "..."` selects the agent binary inside the guest
- `model = "gpt-5"` is supported for Cursor and is passed as `cursor-agent --model ...`
- `auth_file = "..."` is Codex-only and stages a host auth file into `/root/.codex/auth.json`
- `api_key = "crsr_..."` is the preferred Cursor credential path and is injected into the guest as `CURSOR_API_KEY`
- `api_key_env = "OPENOMAN_CURSOR_API_KEY"` is an optional Cursor alternative that tells `openoman run` which host environment variable to read before injecting `CURSOR_API_KEY` into the guest

Existing Codex configs that still use `codex_bin` and `codex_auth_file` continue to work as compatibility aliases.

Examples:

- Codex:
  `provider = "codex"`, `bin = "/usr/local/bin/codex"`, `auth_file = "~/.codex/auth.json"`, `egress_allowed_domains = ["api.openai.com"]`
- Cursor:
  `provider = "cursor"`, `bin = "cursor-agent"`, `model = "gpt-5"`, `api_key = "crsr_..."`, `egress_allowed_domains = ["api2.cursor.sh"]`
  For debugging only, `egress_allowed_domains = ["*"]` disables hostname filtering and enables broader guest egress via the host.

## Sandbox backend

The sandbox runtime is now selected from config:

- `backend = "firecracker"`
- `sandbox.firecracker.mode = "direct"` boots Firecracker without Jailer and is the only working mode in this release
- `sandbox.firecracker.mode = "jailer"` is config-visible but intentionally rejected during startup validation because it needs a more prepared host environment

The direct backend stages the prepared workspace plus any explicitly allowlisted host-user package directories into an ext4 runtime image, boots Firecracker with a per-run writable copy of the configured rootfs, and then extracts the modified workspace plus report/log artifacts back out of that image on the host.

Guest asset notes:

- asset-pair build helper: [guest/build-assets.sh](/home/antonguzun/Work/personal/openoman/guest/build-assets.sh)
- rootfs-only build helper: [guest/build-rootfs.sh](/home/antonguzun/Work/personal/openoman/guest/build-rootfs.sh)
- guest contract documentation: [guest/README.md](/home/antonguzun/Work/personal/openoman/guest/README.md)

## Repository aliases and accounts

Repository config now supports many repositories, each with its own platform/account binding under `[git]`:

- `[[git.accounts]]` defines reusable publish credentials and trusted commit identity (`git_user_name`, `git_user_email`)
- `[[git.repos]]` defines repository alias, `repo_ref`, `platform`, optional `env_repo_name`, and GitHub publish metadata
- `submit --repo <value>` resolves `<value>` as alias first, then falls back to raw repo refs/paths
- `env_for_repo_dir` defaults to `./env_for_repo`; if `./env_for_repo/<env_repo_name>` exists, its files are copied into sandbox workspace root for that job

Alias-based jobs persist `repo_alias`, so publish/account behavior and env overlays remain deterministic at `run` time.

## Core Git workspace preparation (Epic 4)

`openoman-core` includes a trusted `GitAdapter` that can:

- clone a repository into a trusted workspace directory
- checkout a branch name or commit SHA
- export a sandbox workspace copy with sanitized `.git` metadata
- apply optional per-repo env file overlays into sandbox workspace root

See `crates/core/src/git.rs` for the adapter API and tests.

## GitHub publishing (Epic 9)

Preferred publishing config is repo-scoped under `[[git.repos]]` + `[[git.accounts]]`:

- `platform = "github"` enables trusted GitHub publish planning for that alias
- `repo_owner` and `repo_name` select the GitHub repository for pull request creation
- `base_branch` is optional and otherwise defaults to the submitted revision
- `branch_prefix` defaults to `openoman`
- `api_base_url` defaults to `https://api.github.com`
- `push_url` is optional and otherwise defaults to `https://github.com/<owner>/<repo>.git`
- bound account token comes from `[[git.accounts]].token` or `token_env`
- trusted commits use bound account identity (`git_user_name`, `git_user_email`)

Legacy fallback publishing via `[publishing]` still works for raw non-alias `submit --repo ...` flows.

For account `token_env`, use a local environment variable name. Example:

- config: `token_env = "OPENOMAN_GITHUB_TOKEN"`
- shell: `export OPENOMAN_GITHUB_TOKEN=ghp_...`

To create the token itself, use GitHub Settings -> Developer settings -> Personal access tokens. Official GitHub docs: https://docs.github.com/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens

If a job uses `--publish-policy never`, the trusted publish step is skipped. If a job uses `--publish-policy on_validation_success` but publish prerequisites are missing for the alias, the job still succeeds and `result <job_id>` includes `publish_warning=...`.
