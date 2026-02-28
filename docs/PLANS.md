# Project Plan: Secure Agent Runner (Rust Core + microVM Sandbox)

## Epic 0 — Repository bootstrap and CI (Day 0)
- Initialize Rust workspace (Cargo workspace with `core` crate)
- Add basic project structure:
  - `crates/core/`
  - `docs/`
  - `guest/` (placeholder)
- Add dummy unit test(s) (e.g., `assert_eq!(2 + 2, 4)`) and `cargo test` passes
- Add formatting and linting:
  - `rustfmt` config (default is fine)
  - `clippy` in CI
- Add GitHub Actions:
  - Workflow `ci.yml`: `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test`
  - Cache Rust build artifacts
- Add minimal README:
  - One-paragraph description
  - How to build/run tests
- Add basic config skeleton file (e.g., `config.example.toml`) with placeholder fields

**Acceptance:**
- `cargo test`, `cargo fmt --check`, `cargo clippy` pass locally
- GitHub Actions runs on PR and main branch and is green

---

## Epic 1 — Core domain model (DDD) and state machine
- Define domain modules:
  - `domain/job.rs`, `domain/plugin.rs`, `domain/events.rs`
  - Value objects: `JobId`, `RepoRef`, `Revision`, `CheckProfile`, `PublishPolicy`, `ArtifactRef`
- Implement `Job` aggregate:
  - States: Queued → Running → CollectingArtifacts → Validating → Publishing → Notifying → Succeeded/Failed/Canceled
  - Attempt entity within Job
  - Invariants: one active attempt; publish only after validation success
- Define domain events (internal):
  - `JobSubmitted`, `JobAttemptStarted`, `ArtifactsCollected`, `ValidationSucceeded/Failed`, `PullRequestCreated`, `JobSucceeded/Failed`
- Add unit tests for state transitions and invariants

**Acceptance:**
- Domain layer compiles with tests covering valid/invalid transitions
- Domain events can be produced from state changes (even if not yet persisted)

---

## Epic 2 — Persistence layer (SQLite) with transactional state changes
- Add persistence adapter crate/module:
  - SQLite schema migration strategy (simple embedded migrations acceptable)
- Implement repositories:
  - `JobRepository` (create/load/update)
  - `OutboxRepository` (insert/list/update status) as placeholder for later epics
  - `ArtifactsRepository` (refs + metadata)
- Implement transactional write pattern:
  - Job state update and outbox insertion occur atomically
- Add integration tests:
  - Create job → update state → reload job and verify correctness

**Acceptance:**
- Running a minimal flow persists job and can reload it after process restart
- Schema migration works on clean database

---

## Epic 3 — CLI interface (MVP surface)
- Implement CLI commands (e.g., using `clap`):
  - `submit` (creates Job)
  - `run <job_id>` (blocking execution for MVP)
  - `status <job_id>`
  - `logs <job_id>` (initially core logs only; later sandbox logs)
  - `artifacts <job_id>`
  - `result <job_id>`
- Define config loading:
  - Config file path + env overrides
  - Validate config at startup
- Add CLI e2e tests:
  - Submit job into SQLite
  - Query status and confirm output

**Acceptance:**
- User can create a job and inspect it purely via CLI
- Config errors are readable and deterministic

---

## Epic 4 — Git operations (trusted clone/workspaces)
- Implement `GitAdapter` (trusted):
  - Clone repository into a trusted workspace directory
  - Support revision: branch or commit SHA checkout
  - Prepare sandbox workspace (copy from trusted clone)
- Ensure safety:
  - No credentials copied into sandbox workspace
  - `.git` handling strategy decided (MVP: allow `.git` in trusted clone; sandbox workspace may omit `.git`)
- Add tests:
  - Use a local fixture repo in tests (or create temp repo during test)
  - Verify revision checkout correctness

**Acceptance:**
- Core can clone/check out a repo and produce a sandbox workspace directory deterministically

---

## Epic 5 — microVM runner (sandbox lifecycle) — minimal viable implementation
- Decide microVM technology for bare metal MVP (choose one):
  - Firecracker-based runner OR Kata Containers runner
- Implement `SandboxRunner` adapter API in core:
  - `start(attempt_spec) -> sandbox_handle`
  - `wait(sandbox_handle) -> exit_status`
  - `collect_artifacts(sandbox_handle) -> patch/report/logs`
  - `stop(sandbox_handle)` (ensure cleanup)
- Create guest image build pipeline (minimal):
  - Guest OS that boots reliably
  - Includes agent runtime placeholder
  - Includes container runtime inside guest (rootless) and `compose`
- Workspace import/export mechanism (MVP):
  - Copy-in (tar) sandbox workspace to guest
  - Copy-out artifacts (patch/report/logs) to host
- Resource limits:
  - CPU/RAM
  - wall-clock timeout
  - disk quota for guest workspace
- Add a “smoke job” that runs in sandbox:
  - e.g., writes a file, produces a patch, exits

**Acceptance:**
- A sandbox can be started and stopped from core
- Artifacts can be collected from sandbox
- No host docker socket is exposed to sandbox

---

## Epic 6 — Agent execution contract inside sandbox (compose + logs)
- Define the minimal “agent contract” inside the guest:
  - Input: workspace path + instruction text
  - Output: patch (unified diff), report text, logs
- Implement initial agent stub (not LLM-based yet):
  - Example: modify a known file and run `compose` commands (or simulate)
- Add compose support in guest:
  - `compose up`, `compose logs`, `compose down`
  - Capture logs with size limits and exit codes
- Add a sample compose project for testing:
  - Minimal DB container + app container (or two trivial services)
- Ensure network controls are compatible:
  - egress via proxy/mirror (even if proxy is not yet fully implemented)

**Acceptance:**
- Sandbox job can run a compose stack and return logs + patch to host

---

## Epic 7 — Artifact pipeline and storage (patch/report/logs)
- Define artifact formats and storage strategy:
  - Patch: unified diff text + content hash
  - Report: plain text
  - Logs: plain text, truncated with clear marker
- Implement artifact store:
  - Local filesystem under a job directory
  - Store metadata in SQLite (type, size, hash, path)
- Add API in core to retrieve artifacts for CLI:
  - `logs` shows stored logs
  - `artifacts` prints refs/paths

**Acceptance:**
- After sandbox run, artifacts are stored and retrievable after restart

---

## Epic 8 — Trusted validation gate (mandatory before publish)
- Implement validation workflow:
  - Apply patch onto a fresh clean clone (trusted workspace)
  - Run deterministic validation command(s)
- Define `CheckProfile` mapping:
  - MVP: implement at least one profile end-to-end (e.g., `unit`)
  - Keep structure for `integration/full`
- Capture validation logs and store as artifacts
- Update Job transitions:
  - `Validating` → `Publishing` only on success
  - On failure: terminal `Failed` (with reason)

**Acceptance:**
- A job cannot be published without validation success
- Validation results and logs are persisted

---

## Epic 9 — GitHub publishing (branch + PR) (MVP)
- Implement `GitHubPublisher` adapter:
  - Authenticate with token (core only)
  - Create branch name (based on job id)
  - Push validated changes
  - Create Pull Request
- Persist publish result:
  - branch name
  - PR URL/number
- Emit domain event and integration event:
  - `PullRequestCreated`
  - `job.pr_created`

**Acceptance:**
- Successful job ends with a GitHub PR created and stored in job result

---

## Epic 10 — Integration outbox (events persisted for future plugins)
- Implement integration event envelope structure:
  - `event_id`, `event_type`, `occurred_at`, `job_id`, `severity`, `payload`
- Implement outbox persistence in SQLite:
  - Insert on key transitions
  - Fields for delivery status and retry scheduling (even if unused)
- Expose CLI debug command:
  - `events <job_id>` to list emitted events

**Acceptance:**
- Outbox events are stored and visible, even without plugin delivery

---

## Epic 11 — Plugin protocol specification (frozen) + compatibility test harness
- Write `docs/plugin-protocol.md`:
  - JSON-RPC 2.0 over stdin/stdout
  - Methods: `handshake`, `get_manifest`, `configure`, `notify_event`, optional `health`
  - Error conventions and delivery semantics (at-least-once, idempotency by `event_id`)
- Write `docs/integration-events.md`:
  - Event types and payload schema rules
  - Redaction rules (no secrets; prefer artifact refs)
- Provide reference “mock plugin” examples (not full delivery in MVP):
  - Python mock plugin that prints received event ids
  - Node/TS mock plugin that prints received event types
  - These can live under `examples/plugins/` and be run manually
- Add CI check that docs exist and mock plugins lint/build (optional for MVP, but helpful)

**Acceptance:**
- Plugin interface is documented and stable
- Mock plugins can be executed manually against sample JSON-RPC input

---

## Epic 12 — Operational hardening and safety controls (bare metal)
- Sandboxing hardening:
  - Ensure no host secrets in sandbox
  - Enforce cleanup of workspaces and microVM artifacts
- Network strategy for “few domains”:
  - Define proxy/mirror approach (documented)
  - Provide config fields for proxy endpoint
- Resource hardening:
  - Enforce per-job timeouts and max disk usage
  - Cap log sizes and artifact sizes
- Observability:
  - Structured logs (JSON) in core
  - Clear error codes / failure reasons stored in job record

**Acceptance:**
- System remains stable under failure scenarios (sandbox crash, validation failure, GitHub error)
- Cleanup happens reliably

---

# Post-MVP Epics (planned, not required for MVP)

## Epic P1 — Plugin delivery engine (outbox dispatcher)
- Implement plugin discovery (`plugin.yaml`)
- Implement plugin host (spawn process, JSON-RPC client)
- Implement outbox dispatcher with retries
- Implement notify plugins (Telegram, Slack) in Python/Node

## Epic P2 — Additional input interfaces
- Terminal UI (TUI)
- Telegram/Slack command ingestion (as plugins or adapters)
- Linear/Jira task ingestion (task_source capability)

## Epic P3 — GitLab support
- Implement GitLab publisher adapter behind VCS port
- Provider selection in config

## Epic P4 — Performance and concurrency
- Worker pool (bounded concurrency)
- microVM snapshots / pre-warmed images
- Better caching strategy for dependencies via mirrors

## Epic P5 — Advanced policy and security
- Stronger sandbox network policies
- Artifact sanitization and redaction rules
- Separate OS users for plugins, tighter egress allowlists per plugin
