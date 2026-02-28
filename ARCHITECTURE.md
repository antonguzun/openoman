# Architecture: Secure Agent Runner (Bare-Metal, Rust Core, microVM Sandbox)

## 1. Overview

This system runs an untrusted coding agent against a Git repository on a single bare-metal Linux host. The agent executes inside a microVM sandbox and is allowed to run real `docker-compose` stacks (databases + application) and read compose logs. The core service remains deterministic and is the only component allowed to publish changes (push branches and create Pull Requests).

The architecture prioritizes:
- Host safety (no host secrets exposed to the agent)
- Deterministic publishing (only the core can push/PR)
- Minimal operational dependencies (no Redis/broker; SQLite only)
- Extensibility via out-of-process plugins (Python or Node/TS), with a frozen protocol

## 2. Goals

### 2.1 MVP goals
- Run an end-to-end job pipeline:
  - clone repo → prepare workspace → start microVM → agent modifies code and runs compose/tests → collect patch/logs/report → trusted validation → GitHub PR → notifications (stdout + outbox events)
- Provide a CLI interface for submitting and observing jobs
- Persist job state and artifacts references in SQLite
- Freeze plugin interface and integration event contract (plugins not implemented in MVP)

### 2.2 Safety goals
- Untrusted agent must not access:
  - GitHub tokens, SSH keys, or any host secrets
  - host container runtime socket (e.g., `/var/run/docker.sock`)
  - arbitrary host filesystem paths
- Sandbox network egress must be controllable and support allowlisting (preferably via a single proxy/mirror endpoint)
- Enforce resource limits to prevent denial of service

## 3. Non-goals (MVP)

- No Slack/Telegram/Jira/Linear integrations implemented (protocol only)
- No GitLab support (GitHub only)
- No multi-host or distributed execution
- No complex scheduling beyond a simple FIFO queue
- No “core installs plugin dependencies via pip/npm”
- No advanced policy engines for sandbox control beyond a minimal, explicit configuration

## 4. High-level architecture

### 4.1 Components

- **Core Service (Rust, trusted)**
  - CLI interface
  - Job state machine and persistence (SQLite)
  - Repo cloning and workspace preparation
  - microVM sandbox lifecycle management (via adapter/runner)
  - Artifact collection (patch/logs/report)
  - Trusted validation gate
  - GitHub publishing (branch + PR)
  - Outbox event emission (for future plugins)

- **microVM Sandbox (untrusted)**
  - Guest OS with an embedded agent runtime
  - Local container runtime inside microVM (e.g., rootless Podman/Docker)
  - `docker-compose` capability inside microVM
  - Controlled network egress (preferably through proxy/mirror)
  - Workspace mounted or copied into the guest

- **Plugin Processes (post-MVP)**
  - External processes, potentially Python or Node/TS
  - Communicate with core using JSON-RPC over stdin/stdout
  - Capabilities-based contract (MVP: notify only; later: task sources)

### 4.2 Trust boundaries

- The core service is trusted.
- The agent and everything inside the microVM is untrusted.
- Plugins are trusted to the extent they handle external credentials for integrations; they must be sandboxed by OS-level mechanisms and receive only the minimum required data.

## 5. Domain model (DDD)

### 5.1 Aggregates

#### 5.1.1 Job (Aggregate Root)
Represents a unit of work executed end-to-end.

Responsibilities:
- Own the job state machine
- Record attempts and outcomes
- Enforce invariants:
  - Only one active attempt at a time
  - Publishing is allowed only after trusted validation succeeds
  - Artifacts from sandbox are untrusted until validation succeeds

Core fields:
- `job_id`
- `spec`:
  - `repo_ref` (URL or owner/name + URL)
  - `revision` (branch or commit SHA)
  - `instruction` (free text)
  - `check_profile` (unit | integration | full)
  - `publish_policy` (publish true/false; branch naming strategy)
- `state`
- `attempts[]` (entities)
- `artifacts` (refs/hashes for patch/report/logs)
- `publish_result` (branch name, PR URL/number)
- timestamps

#### 5.1.2 Plugin (Aggregate Root) (frozen for post-MVP)
Represents an integration module.

Responsibilities:
- Declare capabilities and entrypoint
- Validate enabled/disabled lifecycle
- Maintain configuration reference (schema + secret reference)

Core fields:
- `plugin_id`, `name`, `version`
- `entrypoint` (command array)
- `capabilities` (e.g., notify; later: task_source)
- `enabled` boolean
- `config_schema` and `secrets_ref`

### 5.2 Entities and Value Objects

- Attempt (entity inside Job)
  - attempt number
  - sandbox spec (resources, network policy reference)
  - sandbox result (exit reason)
  - collected artifact references
  - stage timestamps

- ArtifactRef (value object)
  - typed reference (patch/report/logs)
  - storage pointer (local path, content hash, or URI-like ref)

- RepoRef / Revision / CheckProfile / PublishPolicy (value objects)

## 6. Job state machine

Recommended states (MVP):
- `Queued`
- `Running` (sandbox started; agent executing)
- `CollectingArtifacts`
- `Validating`
- `Publishing` (if publish enabled)
- `Notifying`
- Terminal:
  - `Succeeded`
  - `Failed`
  - `Canceled`

Core invariants:
- `Publishing` is reachable only from `Validating` success
- Terminal states are final (except via explicit “rerun as new job”)

## 7. Application use cases (Core)

MVP use cases:
- `SubmitJob(repo, revision, instruction, check_profile, publish_policy) -> job_id`
- `RunJob(job_id)` (blocking execution is acceptable for MVP)
- `GetJobStatus(job_id)`
- `GetJobLogs(job_id)` (and optional tail)
- `GetJobArtifacts(job_id)`

Pipeline steps inside `RunJob`:
1. Prepare trusted clone and sandbox workspace
2. Start microVM for the attempt
3. Execute agent workflow inside microVM (agent controls compose within microVM)
4. Collect artifacts (patch/report/logs) from microVM
5. Trusted validation:
   - apply patch to fresh clean clone
   - re-run checks/tests deterministically
6. Publish to GitHub (branch + PR) if configured
7. Emit integration events into outbox and print final summary

## 8. Events

### 8.1 Domain events (internal)
Examples:
- `JobSubmitted`
- `JobAttemptStarted`
- `SandboxStarted`
- `ArtifactsCollected`
- `ValidationStarted`
- `ValidationSucceeded` / `ValidationFailed`
- `PublishStarted`
- `PullRequestCreated` / `PublishFailed`
- `JobSucceeded` / `JobFailed` / `JobCanceled`

### 8.2 Integration events (outbox; for plugins)
Core emits integration events into an outbox store (SQLite). Delivery to plugins is post-MVP, but the contract is frozen in MVP.

Event envelope (stable contract):
- `event_id` (unique, stable)
- `event_type`:
  - `job.submitted`
  - `job.started`
  - `job.validation_failed`
  - `job.pr_created`
  - `job.failed`
  - `job.succeeded`
- `occurred_at` (RFC3339 timestamp)
- `job_id`
- `severity` (info | warning | error)
- `payload` (typed per event_type; no secrets; prefer artifact references over embedding large content)

Example:
```json
{
  "event_id": "01HRA0ZK8J2YJ9P4D0T2QK6N1Z",
  "event_type": "job.succeeded",
  "occurred_at": "2026-03-01T10:12:34Z",
  "job_id": "job_01HRA0YV9B1D7QJ5F8M3ZQ2X0A",
  "severity": "info",
  "payload": {
    "repo": "owner/name",
    "revision": "a1b2c3d4e5",
    "pr_url": "https://github.com/owner/name/pull/123",
    "summary": "Validation passed; PR created.",
    "artifacts": {
      "report_ref": "artifact://job_.../report.txt",
      "logs_ref": "artifact://job_.../sandbox.log"
    }
  }
}
```

Delivery semantics (post-MVP):
- At-least-once delivery
- Plugin must be idempotent using `event_id`
- Plugin may request retry with `retry_after_seconds`

## 9. Plugin system (frozen interface for post-MVP)

### 9.1 Plugin discovery model
Plugins are out-of-process executables. Core discovers plugins via a manifest file (e.g., `plugin.yaml`) stored alongside the plugin.

Manifest fields:
- `name`, `version`
- `entrypoint` (command array)
- `capabilities` (MVP: `notify`)
- `config_schema` or list of required config keys

### 9.2 Transport and protocol
- Transport: stdin/stdout
- Protocol: JSON-RPC 2.0

Required methods (core -> plugin):
- `handshake(protocol_version)`
- `get_manifest()`
- `configure(config | config_path)`
- `notify_event(event_envelope)`
- `health()` (optional)

Example `handshake` request:
```json
{"jsonrpc":"2.0","id":1,"method":"handshake","params":{"protocol_version":"1.0"}}
```

Example response:
```json
{"jsonrpc":"2.0","id":1,"result":{"ok":true,"plugin":"telegram","version":"0.1.0"}}
```

Example notification delivery:
```json
{"jsonrpc":"2.0","id":2,"method":"notify_event","params":{"event":{"event_id":"...","event_type":"job.succeeded","occurred_at":"...","job_id":"...","severity":"info","payload":{}}}}
```

Plugin response:
```json
{"jsonrpc":"2.0","id":2,"result":{"status":"sent","retry_after_seconds":null}}
```

Error conventions (frozen):
- `InvalidConfig`
- `TemporarilyUnavailable`
- `RateLimited`
- `PermanentFailure`

### 9.3 Plugin language support
Because plugins are external processes using JSON-RPC over stdin/stdout, plugins can be implemented in:
- Python
- Node.js / TypeScript
- Any other language

MVP does not implement plugin execution/delivery, but the interface above is fixed.

## 10. Sandbox virtualization model (Bare metal)

### 10.1 microVM requirement
- Each job attempt runs in a dedicated microVM (separate guest kernel).
- The agent runs inside the microVM and can run compose and tests there.
- The host must not expose its container runtime or secrets to the microVM.

### 10.2 Workspace transfer model (MVP acceptable options)
- Copy-in/copy-out:
  - Core copies a workspace snapshot into the microVM
  - microVM returns patch/report/logs back to the host
- Shared folder (optional post-MVP):
  - Use a VM shared filesystem only for the job workspace
  - Avoid sharing host paths beyond that workspace

### 10.3 Network control
- Preferred approach for “few domains” allowlist:
  - microVM egress must go through a single proxy/mirror endpoint on the host or LAN
  - proxy/mirror performs domain allowlisting and caching (container registry mirror, package caches)
- Direct open internet from the microVM should be disabled by default.

### 10.4 Resource limits
Per attempt:
- CPU quota/cores limit
- RAM limit
- disk quota
- process count limit
- timeouts for job/attempt phases

## 11. Trusted validation gate

Validation is mandatory before publishing:
- Apply the sandbox-produced patch to a fresh clean clone in a trusted workspace
- Re-run deterministic checks/tests (at least one in MVP)
- Only after validation succeeds may the core publish to GitHub

This ensures:
- The core does not trust the sandbox filesystem state
- The publishing step uses a clean, deterministic build context

## 12. GitHub publishing (MVP)

Core requirements:
- Authentication token exists only in the core process environment or secret store
- Create a new branch for the job
- Push the validated changes
- Create a Pull Request
- Store PR URL/number in the Job record and emit a `job.pr_created` integration event

## 13. Persistence (SQLite)

MVP must persist enough to:
- resume inspection after restart
- audit job outcomes
- store artifact references and publish results
- store outbox integration events for future plugin delivery

Recommended records (conceptual):
- jobs: spec, state, timestamps, publish policy, results
- attempts: job_id, attempt_no, sandbox metadata, stage timestamps
- artifacts: job_id, attempt_no, type (patch/report/logs), ref/path, hash, size
- outbox_events: event_id, job_id, event_type, occurred_at, payload_json, status, retry_at

Implementation detail:
- Use transactional writes for state transitions and outbox insertion to preserve consistency.

## 14. CLI (MVP)

Minimum commands:
- `submit` (create a job; prints job_id)
- `run` (run a job to completion; blocking acceptable)
- `status <job_id>`
- `logs <job_id>` (optional `--follow`)
- `artifacts <job_id>` (prints artifact references and locations)
- `result <job_id>` (prints summary including PR URL if created)

## 15. Repository layout (suggested)

- `crates/core`
  - domain:
    - `job.rs`
    - `plugin.rs`
    - `events.rs`
  - application:
    - use cases (submit/run/status/logs/artifacts)
    - orchestration pipeline
  - adapters:
    - sqlite persistence
    - github client
    - microvm runner
    - artifact store
    - (post-MVP) plugin host + outbox dispatcher
- `docs/architecture.md` (this file)
- `docs/plugin-protocol.md` (frozen JSON-RPC + event envelope)
- `guest/` (microVM guest image build scripts and configuration)

## 16. Extension roadmap (post-MVP)

- Input interfaces:
  - TUI
  - task sources: Linear/Jira (as plugins with `task_source` capability)
  - chat sources: Slack/Telegram (as plugins or adapters)
- Notification channels:
  - notify plugins in Python/Node (Slack/Telegram/email)
  - outbox dispatcher and retry strategy
- Multi-VCS providers:
  - GitLab adapter behind a VCS port interface
- Performance:
  - microVM snapshots/pre-warmed images
  - concurrency with bounded worker pool
- Security hardening:
  - stronger artifact and log sanitization
  - dedicated OS user separation for plugins
  - mandatory proxy/mirror for all external fetches

## 17. Key design decisions (summary)

- Use microVM (separate guest kernel) to allow the untrusted agent to run real compose safely.
- Keep publishing deterministic and trusted: only core can push/create PR; always validate in a clean clone.
- Persist state in SQLite for minimal ops footprint.
- Enable multi-language plugins by making them external processes and freezing a JSON-RPC protocol.
- Prefer proxy/mirror-based allowlisting to keep network control practical and stable.
