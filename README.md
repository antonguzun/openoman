# openoman

openoman runs coding agents against real repositories without handing them your Git or platform credentials. It gives each task its own disposable sandbox, keeps publishing decisions in trusted host code, and lets you build whatever task-entry workflow you want on top of the API.

## Why it exists

Use openoman when you want agent automation to touch real code, but you do not want it to mutate your day-to-day environment, hold your Git credentials, or dictate how tasks enter the system.

## Feature map

- [Keep agent work out of your manual environment](#disposable-sandbox-execution): each task runs in its own sandbox instead of your everyday clone or shell. The execution layer is sandbox-agnostic in design; the current implementation is Firecracker and currently targets Linux hosts with KVM. Details: [guest/README.md](guest/README.md)
- [Keep Git and publishing on the trusted side](#trusted-publishing): the agent can change files, but it does not get direct Git credentials or publish access. openoman applies a deterministic host-side publish flow for GitHub and GitLab. Details: [config.example.toml](config.example.toml), [docs/api/control-plane.md](docs/api/control-plane.md)
- [Bring your own task source](#cli-and-http-control-plane): use the built-in CLI, or wire your own Telegram bot, web UI, cron job, issue tracker bridge, or any other adapter against the HTTP API. Details: [docs/design-docs/control-plane-and-adapters.md](docs/design-docs/control-plane-and-adapters.md), [docs/api/control-plane.md](docs/api/control-plane.md), [docs/api/openapi.yaml](docs/api/openapi.yaml)
- [Swap agents without changing the rest of the system](#agent-provider-support): Codex and Cursor use the same sandbox and publish flow through one provider-neutral config shape. Details: [config.example.toml](config.example.toml), [guest/README.md](guest/README.md)
- [Run many repositories safely](#repository-aliases-accounts-and-env-overlays): bind each repo to its own account, env overlay, and isolated task runs without contaminating other repos or your local setup. Details: [config.example.toml](config.example.toml)
- [Control branch and commit naming centrally](#repository-aliases-accounts-env-overlays-and-git-naming): configure one trusted host-side prompt for branch and commit names, or omit the key and keep the old deterministic naming path. Details: [config.example.toml](config.example.toml)
- [Inspect what happened after the run](#artifacts-and-audit-trail): SQLite-backed job state plus patch, report, logs, and publish metadata make runs inspectable after the fact. Details: [docs/design-docs/operational-model.md](docs/design-docs/operational-model.md), [docs/api/control-plane.md](docs/api/control-plane.md)

## Near-term plans

- More agent providers on top of the same execution and publish flow
- Richer naming providers and templates beyond the initial global host-side LLM option
- Additional sandbox backends and runtime options
- Firecracker `jailer` support for a more hardened sandbox mode
- Agent pipelines tailored to different task types such as architecture checks, documentation checks, and review flows
- A broader HTTP API for richer external integrations
- Continuing or resuming a task after the first run has completed

## Getting started

1. Review and adapt [config.example.toml](config.example.toml).
2. Build guest assets with `./guest/build-assets.sh ./guest/out`.
3. Point `sandbox.firecracker.kernel_image_path` and `sandbox.firecracker.rootfs_image_path` at those assets.
4. Configure `[agent]` plus either `[[git.repos]]` and `[[git.accounts]]` or the legacy `[publishing]` fallback.
5. Start the HTTP control plane with `openoman --config ./config.toml serve` or submit jobs directly through the CLI.

Configuration is loaded from `--config` and the database path can be overridden with `OPENOMAN_DATABASE_PATH`.

## CLI and HTTP control plane

The `openoman` binary exposes the same trusted core through local commands:

- `openoman --config ./config.toml serve`
- `openoman --config ./config.toml submit --repo my_repo --revision main --instruction "update README"`
- `openoman --config ./config.toml submit --repo https://github.com/acme/repo.git --revision main --instruction "update README"`
- `openoman --config ./config.toml status <job_id>`
- `openoman --config ./config.toml run <job_id>`
- `openoman --config ./config.toml logs <job_id>`
- `openoman --config ./config.toml artifacts <job_id>`
- `openoman --config ./config.toml result <job_id>`

The HTTP control plane exposed by `serve` provides:

- `GET /health`
- `POST /jobs`
- `GET /jobs`
- `GET /jobs/:id`
- `POST /jobs/:id/run`
- `POST /jobs/:id/retry`
- `GET /jobs/:id/logs`
- `GET /jobs/:id/artifacts`
- `GET /jobs/:id/result`

By default the server binds to `127.0.0.1:8080`. Add `[server].auth_token` or `[server].auth_token_env` to require a bearer token for HTTP requests.

The HTTP API is the extension point for your own task sources and UIs. If you want tasks to come from chat, a form, cron, Jira, Linear, or an internal tool, build that outside the trusted core and talk to `openoman serve`.

API documentation:

- human-readable reference: [docs/api/control-plane.md](docs/api/control-plane.md)
- draft OpenAPI contract: [docs/api/openapi.yaml](docs/api/openapi.yaml)

## Disposable sandbox execution

Each job attempt runs in its own disposable sandbox, so agent work stays out of your manual environment. The execution model is sandbox-agnostic in design, but the current implementation uses Firecracker in `direct` mode and currently expects a Linux host with `/dev/kvm`.

Firecracker-specific details:

- `backend = "firecracker"` selects the microVM backend
- `sandbox.firecracker.mode = "direct"` is the working execution mode
- `sandbox.firecracker.mode = "jailer"` is config-visible but rejected during startup validation
- `sandbox.firecracker.network.mode = "disabled"` runs without guest networking
- `sandbox.firecracker.network.mode = "host-proxy"` routes guest HTTPS through a host-local allowlisting proxy
- `sandbox.firecracker.network.allowed_connect_ports = [443, 5050]` lets the host proxy tunnel HTTPS CONNECT traffic to non-443 ports such as private Docker registries
- the default guest rootfs build now includes `make`, Docker CLI, `dockerd`, `docker compose`, `python3`, and a `python -> python3` compatibility symlink in addition to the agent CLIs
- `sandbox.firecracker.runtime_disk_mb = 4096` explicitly sizes the per-attempt runtime disk (`/dev/vdb`) used for staged workspace data and guest Docker storage
- `sandbox.firecracker.docker_daemon = true` starts `dockerd` inside the guest, stores Docker data under `/mnt/runtime/docker`, and waits for `/var/run/docker.sock` readiness before agent execution
- `[[sandbox.firecracker.user_package_dirs]]` copies explicit host-user package directories into each run
- `sandbox.firecracker.docker_auth_config` or `sandbox.firecracker.docker_auth_config_env` explicitly stages Docker registry credentials into `/root/.docker/config.json`

Startup validates the configured backend before any command runs. With Firecracker, openoman fails fast if required host dependencies such as `firecracker`, `/dev/kvm`, or the configured guest asset paths are unavailable.

Guest asset documentation:

- asset-pair build helper: [guest/build-assets.sh](guest/build-assets.sh)
- rootfs-only build helper: [guest/build-rootfs.sh](guest/build-rootfs.sh)
- guest runtime contract: [guest/README.md](guest/README.md)

Docker registry auth is opt-in. openoman never auto-imports host `~/.docker/config.json`; if you want private image pulls inside the guest, point `sandbox.firecracker.docker_auth_config` at a specific host file or use `sandbox.firecracker.docker_auth_config_env` to pass JSON from a host environment variable. If the guest also needs to run containers, enable `sandbox.firecracker.docker_daemon = true` so the Docker CLI has a daemon to talk to. For image-heavy Docker workflows, raise `sandbox.firecracker.runtime_disk_mb` so `/mnt/runtime/docker` has enough space for pulled layers and build cache. If a private registry listens on a non-443 HTTPS port such as `5050`, add that port to `sandbox.firecracker.network.allowed_connect_ports` or the host proxy will deny the CONNECT tunnel before Docker auth is even attempted.

## Agent provider support

Agent configuration lives under `[agent]` and uses provider-neutral keys:

- `provider = "codex"` or `provider = "cursor"`
- `bin = "..."` selects the agent binary inside the guest
- `model = "gpt-5"` is supported for Cursor and passed as `cursor-agent --model ...`
- `auth_file = "..."` is Codex-only and stages a host auth file into `/root/.codex/auth.json`
- `api_key = "crsr_..."` injects a Cursor API key into the guest as `CURSOR_API_KEY`
- `api_key_env = "OPENOMAN_CURSOR_API_KEY"` reads the Cursor API key from a host environment variable at run time
- `egress_allowed_domains = ["api.openai.com"]` or `["api2.cursor.sh"]` defines the host-proxy allowlist

Existing Codex configs that still use `codex_bin` and `codex_auth_file` continue to work as compatibility aliases.

## Repository aliases, accounts, env overlays, and git naming

Repository configuration supports many repositories under `[git]`:

- `[[git.accounts]]` defines reusable publish credentials and trusted commit identity
- `[[git.repos]]` defines repository alias, `repo_ref`, `platform`, optional `env_repo_name`, optional `post_clone_command`, and provider-specific publish metadata
- `[git.naming]` optionally enables trusted host-side LLM generation for `branch_name` and `commit_message`
- `submit --repo <value>` resolves `<value>` as alias first, then falls back to a raw repo ref or path
- `env_repo_name` defaults to the repo alias
- `env_for_repo_dir` defaults to `./env_for_repo`, and `./env_for_repo/<env_repo_name>` is copied into the sandbox workspace root when present
- `post_clone_command`, when set on a repo alias, runs on the trusted host after clone plus checkout and before the trusted worktree is copied into the sandbox

Alias-based jobs persist `repo_alias`, so publish behavior and environment overlays remain deterministic at `run` time. Jobs now also persist `branch_name` and `commit_message` at submit time. `POST /jobs/:id/retry` inherits those values so later continuation flows can keep using the same branch identity.

If `[git.naming]` has a configured API key, openoman calls the naming provider on the trusted host at submit time using the global prompt template. If the key is omitted, or if the provider returns an unusable response, openoman keeps the legacy deterministic behavior and uses the repo or legacy publishing `branch_prefix` plus the job ID.

The naming prompt can currently reference `{{job_id}}`, `{{repo_alias}}`, `{{repo_ref}}`, `{{revision}}`, `{{platform}}`, and `{{instruction}}`. `commit_message` must be returned as a single line; invalid multi-line or empty values fall back to the legacy deterministic commit message.

Each job still runs in its own sandbox, so repo-specific automation does not overwrite files, shells, or tool state in the environment you use for manual work.

## Trusted publishing

The sandbox never publishes directly. The agent can modify the workspace, but it does not get direct Git credentials or permission to push branches on its own. openoman keeps the publish step in trusted host code and applies the resulting changes through a deterministic host-side flow.

Separate trusted re-validation of the patch is not implemented yet. The current safety boundary is that the agent does not get Git access and does not execute the publish step itself.

Publishing support includes:

- GitHub repository aliases with trusted branch push and pull request creation
- GitLab.com repository aliases with trusted branch push and merge request creation
- self-hosted GitLab aliases through `platform = "gitlab_self_hosted"`
- legacy raw-repo GitHub publishing through `[publishing]`
- `--publish-policy never` to skip publishing cleanly
- `--publish-policy on_validation_success` as the current publish-policy flag name for the trusted host-side publish path

For successful publish flows, `openoman result <job_id>` and `GET /jobs/:id/result` expose the persisted `branch_name`, `commit_message`, and PR/MR metadata. When publish prerequisites are missing intentionally, the job can still succeed and returns `publish_warning=...`.

## Artifacts and audit trail

Job state is stored in SQLite. The system persists enough information to inspect runs after restart, including job state, attempt count, artifact references, and publish outcome.

Important artifact classes include:

- `sandbox.patch`
- `sandbox.report`
- `sandbox.logs`
- `workspace.sandbox_result`

The operator surfaces expose those artifacts through `logs`, `artifacts`, and `result`, and the HTTP API mirrors the same data model.

## Build and test

Run local quality checks from the repository root:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`

## Git hooks

This repository ships a repo-managed `pre-commit` hook for secret scanning.

Enable it once per clone from the repository root:

- `git config core.hooksPath .githooks`

The hook requires `gitleaks` to be installed and available on `PATH`. It scans staged content only, so it blocks newly introduced secrets without rescanning the entire working tree on each commit.

Run the scanner manually from the repository root:

- `gitleaks dir . --config .gitleaks.toml`

If the hook reports a false positive, narrow the allowlist in `.gitleaks.toml` deliberately instead of bypassing it routinely. Emergency bypass remains available through `git commit --no-verify`.

## Documentation

- design docs index: [docs/design-docs/index.md](docs/design-docs/index.md)
- system overview: [docs/design-docs/system-overview.md](docs/design-docs/system-overview.md)
- execution lifecycle: [docs/design-docs/execution-lifecycle.md](docs/design-docs/execution-lifecycle.md)
- sandbox and safety: [docs/design-docs/sandbox-and-safety.md](docs/design-docs/sandbox-and-safety.md)
- control plane and adapters: [docs/design-docs/control-plane-and-adapters.md](docs/design-docs/control-plane-and-adapters.md)
- operational model: [docs/design-docs/operational-model.md](docs/design-docs/operational-model.md)
- guest runtime docs: [guest/README.md](guest/README.md)
