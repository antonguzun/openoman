# Server async run API with background worker

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `/docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the HTTP API no longer keeps `POST /jobs/{job_id}/run` open for the full lifetime of a sandbox attempt. The endpoint should return quickly with an accepted response, and the server should execute queued jobs in the background while clients poll `/jobs/{job_id}`, `/jobs/{job_id}/logs`, and `/jobs/{job_id}/result` to observe progress and completion.

The user-visible effect is that long-running Firecracker jobs stop depending on reverse-proxy timeouts or a long-lived request. A human can submit a job, request its execution, immediately receive a `202 Accepted` response, and then watch the job move from `queued` to `running` to a terminal state by polling the existing status endpoints.

## Progress

- [x] (2026-03-26 13:03Z) Reviewed `crates/cli/src/server.rs`, `crates/cli/src/service.rs`, `crates/core/src/application/mod.rs`, and `crates/core/src/persistence/mod.rs` to confirm that the current API waits synchronously inside `/jobs/{job_id}/run` while the domain model already has a durable `queued` state.
- [x] (2026-03-26 13:07Z) Created this ExecPlan and locked the implementation shape: short-lived `run` endpoint plus a background worker loop inside `openoman serve`.
- [x] (2026-03-26 13:24Z) Implemented the server-side queue execution loop in `crates/cli/src/server.rs` and changed `POST /jobs/{job_id}/run` to return `202 Accepted` with polling URLs instead of waiting for sandbox completion.
- [x] (2026-03-26 13:31Z) Added regression coverage in `crates/cli/src/server.rs` for accepted `run` requests, background execution to completion, and rejection of `run` against terminal jobs.
- [x] (2026-03-26 13:34Z) Ran `cargo fmt --all`, `cargo test -p openoman-cli server -- --nocapture`, and `cargo test -p openoman-cli`.
- [ ] Follow up on pre-attempt execution failures that currently return an error before the job leaves `queued`, because a background worker can otherwise retry the same broken queued job forever.

## Surprises & Discoveries

- Observation: the existing domain and application layers already model the exact state transition the server needs.
  Evidence: `RunJobUseCase::run()` in `crates/core/src/application/mod.rs` rejects any job whose stored state is not `queued`, which means a background worker can safely pick queued jobs without a new domain state.

- Observation: the current server contract is synchronous only at the HTTP layer.
  Evidence: `crates/cli/src/server.rs` uses `spawn_blocking(move || service.run_job(&job_id_for_task))` and then waits for the join result before sending the response.

- Observation: server tests that used `https://example.com/repo.git` were only valid while the HTTP layer never executed jobs.
  Evidence: once the background worker started draining queued jobs, those tests began failing in `git clone` with TLS errors until they were switched to local fixture repositories.

- Observation: a successful sandbox run can still end in `failed` when the test fixture leaves `publish_policy = "on_validation_success"` without any publishing configuration.
  Evidence: the first async run test reached `process exited with status Some(0)` in sandbox logs and still observed final state `failed` until the test fixture explicitly used `publish_policy = "never"`.

- Observation: pre-attempt failures currently leave the job in `queued`.
  Evidence: `RunJobUseCase::run()` prepares the workspace and creates the runner before `job.start_attempt(attempt_id)`, so a clone or preflight error returns early without any persisted terminal transition.

## Decision Log

- Decision: keep the durable job state model unchanged for this redesign.
  Rationale: `queued`, `running`, and the later pipeline states already capture the execution lifecycle, so the API redesign can stay focused on request/response behavior and scheduling instead of introducing a new persisted state.
  Date/Author: 2026-03-26 / Codex

- Decision: implement the first background worker inside `openoman serve` instead of introducing a separate binary in the same patch.
  Rationale: this delivers the user-visible async API behavior with less churn, while still preserving a future path to split the worker into its own process once the launcher and jailer work mature.
  Date/Author: 2026-03-26 / Codex

- Decision: let the first worker scan persisted `queued` jobs directly instead of inventing a new scheduling state in this patch.
  Rationale: the goal of this change is to remove long-lived HTTP requests, and the existing persisted states were enough to ship that behavior safely for the current single-process server. A dedicated claimed-job state can still be added later if the worker is split from the API process.
  Date/Author: 2026-03-26 / Codex

- Decision: keep `POST /jobs/{job_id}/run` as a poll-friendly compatibility endpoint that returns `202 Accepted` for any non-terminal job, even though the embedded worker already drains queued jobs.
  Rationale: existing clients already know this endpoint, and returning an accepted payload for `queued` and `running` jobs avoids fragile races between fast worker pickup and client follow-up requests.
  Date/Author: 2026-03-26 / Codex

## Outcomes & Retrospective

`openoman serve` now behaves as a short-request control plane. `POST /jobs/{job_id}/run` returns `202 Accepted` with polling URLs, while an internal Tokio worker loop scans SQLite for queued jobs and executes them through the existing synchronous `OperatorService::run_job()` path. This matches the intended HTTP redesign and removes the long-lived request from the server contract.

The main remaining gap is failure handling before `RunJobUseCase` starts an attempt. Those errors still leave the job in `queued`, which is acceptable for the happy-path API redesign that landed here but is not yet a complete background-worker failure model. That should be the next follow-up before splitting the worker into a separate process.

## Context and Orientation

The current HTTP server lives in `crates/cli/src/server.rs`. It defines the `/jobs` endpoints using Axum, which is the Rust HTTP framework used by this repository. The current `run_job` handler clones `OperatorService`, calls `tokio::task::spawn_blocking`, and waits for `service.run_job()` to finish before responding. That means one HTTP request lives for the entire sandbox attempt.

`OperatorService` lives in `crates/cli/src/service.rs`. It is the CLI and server orchestration layer on top of the core application code. Its `submit_job()` method persists new jobs in the `queued` state. Its `run_job()` method resolves config, creates the execution backend, and delegates the real state machine to `openoman_core::application::RunJobUseCase`.

`RunJobUseCase` lives in `crates/core/src/application/mod.rs`. It loads a job from SQLite, requires that it is currently `queued`, prepares the workspace, runs the sandbox, collects artifacts, validates, publishes if configured, and finally persists the resulting state. This means the background worker does not need to reimplement execution semantics; it only needs to decide which queued job to run next.

SQLite persistence lives in `crates/core/src/persistence/mod.rs`. `JobRepository::list()` currently returns all jobs in reverse insertion order, which is sufficient for a first implementation that scans for the oldest or newest queued job in-process. A future worker split can replace this with a stronger claim-and-lock mechanism, but this redesign does not need to widen the persistence surface beyond what the current single-process server requires.

## Plan of Work

`crates/cli/src/service.rs` now contains a queue-facing helper `next_queued_job_id()` that returns the oldest queued job identifier without exposing persistence details to the server module. `run_job()` itself was intentionally left intact so the CLI `openoman run` command and the embedded server worker still share the same execution implementation.

`crates/cli/src/server.rs` now starts a background Tokio task from `serve()`. That task polls for queued jobs on a short interval and executes them one at a time with `spawn_blocking(move || service.run_job(...))`. The `/jobs/{job_id}/run` handler no longer executes sandbox work inline. Instead, it reads the current job state, returns `202 Accepted` for non-terminal jobs, and returns a concise JSON body with `status_url`, `logs_url`, and `result_url` so clients can poll existing read endpoints.

The same file now also carries the HTTP regression tests that prove the new contract. The async tests use a local Git fixture repository and a fake codex-compatible shell script so the worker path exercises the real process backend launch contract without depending on external network access.

## Concrete Steps

From the repository root:

1. Edit `crates/cli/src/service.rs` to add queue-oriented helpers for the server worker.
2. Edit `crates/cli/src/server.rs` to:
   - start a background worker loop from `serve()`
   - change `POST /jobs/{job_id}/run` to return `202 Accepted`
   - surface status/result/logs URLs in the response body
3. Add or update server tests in `crates/cli/src/server.rs` so they cover the new async contract with a local fixture repository.
4. Run:

       cargo test -p openoman-cli server

   Then run the full CLI package suite:

       cargo test -p openoman-cli

   If formatting changed, run:

       cargo fmt --all

## Validation and Acceptance

Acceptance is met when a human can do the following against `openoman serve`:

1. `POST /jobs` creates a job and returns `state = "queued"`.
2. `POST /jobs/{job_id}/run` returns HTTP `202 Accepted` quickly instead of waiting for sandbox completion.
3. Repeated polling of `GET /jobs/{job_id}` eventually shows the job moving out of `queued` and reaching either `succeeded` or `failed`.
4. `GET /jobs/{job_id}/result` and `GET /jobs/{job_id}/logs` still expose the final outcome and logs after the worker finishes.
5. A second `run` request against a non-queued job is rejected with a clear status code and message instead of starting another overlapping execution.
6. The server test suite proves the async path with a local repository and passes without depending on external network access.

## Idempotence and Recovery

The worker loop is safe to restart with the server process because it derives work from persisted SQLite state rather than from a transient in-memory queue. This first implementation is intentionally single-process and runs one queued job at a time per server instance. Repeating the HTTP tests is safe because they use temporary directories and isolated SQLite databases.

## Artifacts and Notes

The accepted response body should be concise and poll-friendly. A representative payload is:

    {
      "job_id": "job-123",
      "state": "queued",
      "status_url": "/jobs/job-123",
      "logs_url": "/jobs/job-123/logs",
      "result_url": "/jobs/job-123/result"
    }

The exact field names should remain stable once introduced so external adapters can rely on them.

## Interfaces and Dependencies

In `crates/cli/src/service.rs`, define small server-facing helpers rather than exposing persistence directly from `server.rs`. The exact names can be adjusted during implementation, but the final interface must let the server:

    inspect whether a job exists and is still queued
    discover the next queued job to run
    execute a queued job by reusing `OperatorService::run_job`

In `crates/cli/src/server.rs`, define a response type for accepted execution requests that serializes to JSON and includes the job identifier plus the polling URLs. Use Tokio, which is already a dependency of `openoman-cli`, for the background polling loop and for `spawn_blocking` when invoking the synchronous execution path.

Change note: Created on 2026-03-26 after deciding to move the HTTP API from synchronous `run` requests to queue-based background execution so the server contract matches long-running Firecracker jobs better.
Change note: Updated on 2026-03-26 after implementation to record the shipped embedded worker loop, the accepted-response contract, the local-fixture HTTP tests, and the remaining pre-attempt-failure gap.
