# MVP Requirements

## 1. Goal and Scope

- Build a deterministic core service that can run an untrusted coding agent against a Git repository, while keeping host secrets safe.
- The agent must be able to run real Docker Compose (databases + application) and read compose logs inside an isolated sandbox.
- The core service remains the only component allowed to push code and create pull requests.

## 2. Deployment Constraints

- Runs on a single bare-metal Linux host.
- Minimal operational dependencies: no Redis, no message brokers.
- Persist job state in a single local SQLite database file.
- Provide a CLI interface only for MVP.

## 3. Core Concepts (Domain Model)

- **Job** is the primary aggregate and state machine.
- **Attempt** is an entity within Job (supports retries, tracks sandbox run and artifacts).
- **Plugin** is a separate aggregate describing an external integration (not implemented in MVP; interface is frozen).

## 4. MVP CLI Use Cases

- **SubmitJob**: create a job with repo reference, revision, instructions, check profile, and publish policy.
- **RunJob**: run a submitted job to completion (blocking mode is acceptable for MVP).
- **GetJobStatus**: show current state and last known progress.
- **GetJobLogs**: show or tail logs (core logs + sandbox/test logs if available).
- **GetJobArtifacts**: retrieve references to patch/report/logs artifacts (stored locally).

## 5. Job Pipeline (End-to-End Behavior)

### 5.1 Job Submission

**Inputs:**

- Repository reference (e.g., GitHub HTTPS URL or owner/name + URL)
- Revision (branch or commit SHA)
- Instruction text for the agent
- CheckProfile: `unit | integration | full` (MVP can implement one profile but must keep the field)
- PublishPolicy: `publish=true/false`, branch naming strategy (MVP: `publish=true` supported)

**Output:**

- JobId (stable identifier)

### 5.2 Workspace Preparation (Trusted)

- Clone a clean copy of the repository on the host.
- Create a sandbox workspace copy for the agent.
- No secrets are present in the sandbox workspace.
- Git credentials (tokens/SSH keys) exist only in the trusted core process.

### 5.3 Sandbox Execution (Untrusted Environment)

- For each job attempt, start one isolated microVM sandbox.
- Inside the microVM:
  - The agent can modify files in the workspace.
  - The agent can run Docker Compose to start services (DB/app), run tests, and read compose logs.
- The agent must not have access to host container runtime (no host Docker socket).
- Sandbox resource limits must exist (CPU, RAM, disk, time, process count).

### 5.4 Artifact Collection (From Sandbox)

- After sandbox execution, collect:
  - Patch (unified diff) produced by the agent’s changes.
  - Agent report (human-readable summary).
  - Logs (compose logs and test output).
- Apply strict size limits to logs and artifacts (truncate if needed).
- Artifacts are considered untrusted until validation succeeds.

### 5.5 Trusted Validation (Required Gate)

- Core applies the collected patch onto a fresh clean clone (trusted workspace).
- Core runs the selected checks/tests again in the trusted context (MVP: at least one deterministic validation step).
- Only if validation succeeds, publishing is allowed.

### 5.6 Publishing (MVP: GitHub)

- Core creates a new branch, pushes changes, and opens a GitHub pull request.
- GitHub authentication is performed only by the core (token never enters sandbox).
- Store PR URL/number in Job results.

### 5.7 Notification (MVP)

- MVP: print a final summary to stdout and store it in SQLite/job artifacts.
- The system must also emit integration events into an outbox table for future plugin delivery (delivery not implemented in MVP).

## 6. Security Requirements

- Untrusted agent runs inside a microVM boundary (separate guest kernel).
- Sandbox must not receive:
  - GitHub tokens, SSH keys, or any host secrets.
  - Access to host Docker/Podman socket.
  - Host filesystem mounts other than a dedicated job workspace.
- Network control:
  - Sandbox egress must be restricted (preferred: egress proxy + allowlist).
  - The design must support “few domains” by routing all external access through a single proxy/mirror endpoint.
- Host protection:
  - No privileged containers required on the host for sandbox execution.
  - Enforce resource limits to prevent DoS.

## 7. Data Persistence (SQLite)

- Persist at minimum:
  - Jobs (spec, state, timestamps)
  - Attempts (sandbox run metadata)
  - Artifact references (patch/report/logs)
  - Publish results (branch, PR URL)
  - Outbox events (pending/sent/failed fields reserved)
- The system must be able to resume/inspect completed jobs after restart.

## 8. Plugin Interface (Frozen for Post-MVP, Not Implemented)

### 8.1 Plugin Model

- Plugins are external processes (can be Python or Node/TS).
- Core discovers plugins via a manifest file (e.g., `plugin.yaml`) describing:
  - name/version
  - entrypoint command
  - capabilities (MVP scope: notify)
  - config schema / required fields

### 8.2 Protocol

- Communication: JSON-RPC 2.0 over stdin/stdout (or JSON Lines with equivalent semantics).
- Required methods (core → plugin):
  - `handshake(protocol_version)`
  - `get_manifest()`
  - `configure(config or config_path)`
  - `notify_event(event_envelope)`
  - `health()` (optional)
- Delivery semantics:
  - At-least-once delivery; plugin must be idempotent by `event_id`.
  - Plugin may request retry via `retry_after_seconds`.

### 8.3 Integration Event Envelope (Stable Contract)

**Fields:**

- `event_id` (unique, stable)
- `event_type` (e.g., `job.submitted`, `job.started`, `job.failed`, `job.succeeded`, `job.pr_created`, `job.validation_failed`)
- `occurred_at` (timestamp)
- `job_id`
- `severity`
- `payload` (typed per `event_type`; must not include secrets; references to artifacts instead of embedding large data)

## 9. Non-Goals for MVP (Explicit)

- No Slack/Telegram/Jira/Linear integrations implemented (protocol only).
- No GitLab support (GitHub only).
- No distributed execution across multiple machines.
- No advanced scheduling/priority queues beyond simple FIFO.
- No “core installs plugin dependencies via pip/npm” for MVP.

---

# Context Beyond MVP

## A) Additional Input Interfaces

- Add TUI, Telegram, Slack, Linear, and Jira as job/task sources.
- These will be implemented as plugins (`task_source` capability) or as adapters, but not inside the core.

## B) Additional Notification Channels

- Implement notify plugins in Python/Node:
  - `telegram-notify`, `slack-notify`, `email-notify`, etc.
- Core will deliver outbox events to enabled notify plugins.

## C) Multiple VCS Providers

- Add GitLab support behind a VCS port:
  - create branch, push, open MR, comment, set status.
- Keep domain model stable; swap adapter by configuration.

## D) Stronger Supply-Chain and Network Controls

- Introduce internal mirrors:
  - container registry mirror
  - pip/npm/apt proxy caches
- Make sandbox network depend only on a small set of internal endpoints.

## E) Performance Improvements

- Use microVM snapshots (pre-warmed base image) to reduce job startup time.
- Add concurrency: multiple worker threads/processes, bounded by resource limits.

## F) Reliability

- Implement robust retries:
  - sandbox failures vs validation failures vs publish failures
- Add structured metrics and health endpoints for operations.
