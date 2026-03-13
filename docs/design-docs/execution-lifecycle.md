# Execution Lifecycle

This document describes how a job moves through the system from the user's perspective and from the core service's perspective.

## User-facing use cases

The MVP operator surface is intentionally small:

- `serve` starts the HTTP control plane
- `submit` creates a job and returns a stable job identifier
- `run` executes a job to completion; blocking execution is acceptable in the MVP
- `status <job_id>` reports the current state
- `logs <job_id>` prints or tails logs
- `artifacts <job_id>` lists artifact references and their locations
- `result <job_id>` prints the final summary, including the pull request URL when publishing succeeded

## End-to-end job pipeline

### 1. Job submission

The user provides a repository reference, revision, free-form instruction, check profile, and publish policy. The core stores that request as a new queued job.

### 2. Trusted workspace preparation

The core creates a clean trusted clone on the host, then prepares a sandbox workspace snapshot for the microVM. Secrets stay on the trusted side.

### 3. Sandbox execution

The core starts a dedicated microVM and runs the agent inside it. The agent is free to edit files, launch Docker Compose services, run tests, and inspect compose logs within the guest.

### 4. Artifact collection

After the attempt finishes, the core copies back the proposed patch, the agent's report, and the relevant logs. These artifacts are stored and referenced, but they are still untrusted at this stage.

### 5. Trusted validation

The core applies the collected patch to a fresh clean clone and runs deterministic checks again. This is the mandatory gate between "the agent changed files" and "the system is willing to publish those changes."

### 6. Publishing

If the publish policy allows it and validation succeeds, the trusted core creates a branch, pushes the validated changes, and opens a GitHub pull request. Publish metadata becomes part of the job record.

### 7. Notification and result reporting

The core prints a final summary, stores the outcome, and writes integration events into the outbox for future adapters or notification workers.

## Domain events

The core can model important transitions as internal domain events such as:

- `JobSubmitted`
- `JobAttemptStarted`
- `SandboxStarted`
- `ArtifactsCollected`
- `ValidationStarted`
- `ValidationSucceeded`
- `ValidationFailed`
- `PublishStarted`
- `PullRequestCreated`
- `PublishFailed`
- `JobSucceeded`
- `JobFailed`
- `JobCanceled`

Integration events derived from these transitions are part of the stable integration contract and are described in [`control-plane-and-adapters.md`](control-plane-and-adapters.md).
