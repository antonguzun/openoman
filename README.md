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

When a submitted job uses `--publish-policy on_validation_success`, `run` now applies the canonical patch to the trusted clone, pushes a branch named `openoman/<job_id>` by default, opens a GitHub pull request, stores the branch plus PR metadata on the job, and emits a `job.pr_created` outbox event. `result <job_id>` prints the stored branch name, pull request number, and pull request URL when publishing succeeded.

Startup now validates the configured sandbox backend before any command runs. With the default Firecracker backend, `openoman` will fail fast if required host dependencies such as `firecracker`, `/dev/kvm`, or the configured guest asset paths are unavailable.

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

## Core Git workspace preparation (Epic 4)

`openoman-core` now includes a trusted `GitAdapter` that can:

- clone a repository into a trusted workspace directory
- checkout a branch name or commit SHA
- export a sandbox workspace copy without `.git` metadata

See `crates/core/src/git.rs` for the adapter API and tests.

## GitHub publishing (Epic 9)

Publishing is configured in the trusted host config under `[publishing]`:

- `provider = "github"`
- `repo_owner` and `repo_name` select the GitHub repository for pull request creation
- `base_branch` is optional and otherwise defaults to the submitted revision
- `branch_prefix` defaults to `openoman`
- `api_base_url` defaults to `https://api.github.com`
- `push_url` is optional and otherwise defaults to `https://github.com/<owner>/<repo>.git`
- `github_token_env` or `github_token` supplies the GitHub token used only by the trusted core
- `curl_bin` defaults to `curl`

`github_token_env` is not a GitHub-provided variable name. It is the environment variable name that OpenOMAN should read on your machine. For example:

- config: `github_token_env = "OPENOMAN_GITHUB_TOKEN"`
- shell: `export OPENOMAN_GITHUB_TOKEN=ghp_...`

To create the token itself, use GitHub Settings -> Developer settings -> Personal access tokens. Official GitHub docs: https://docs.github.com/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens

If a job uses `--publish-policy never`, the trusted publish step is skipped and `result <job_id>` remains a plain success/failure summary without PR metadata.
