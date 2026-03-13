# Operational Model

The MVP is deliberately simple to operate. One host runs the trusted core, stores state in SQLite, and exposes both a CLI and an HTTP control plane for submission and inspection.

## Persistence

SQLite must store enough information to inspect and audit jobs after restart. At minimum, the architecture expects records for:

- jobs, including the original spec, state, timestamps, publish policy, and publish result
- attempts, including attempt number, sandbox metadata, stage timestamps, and exit information
- artifacts, including type, path or reference, hash, and size
- outbox events, including event id, event type, payload JSON, status, and retry metadata

State transitions and outbox insertion should happen transactionally so that job history and integration events remain consistent.

## Artifact handling

The core stores references to artifacts rather than treating the copied files themselves as trusted state. Important artifact classes are:

- patch
- report
- logs

The artifact store can be local files in the MVP as long as the database records point to them reliably.

## Runtime surface

The minimum runtime surfaces are:

- `openoman serve` for the HTTP control plane
- CLI commands for local/operator usage:
  - `submit`
  - `run <job_id>`
  - `status <job_id>`
  - `logs <job_id>` with optional follow behavior
  - `artifacts <job_id>`
  - `result <job_id>`

The HTTP control plane is the stable integration boundary for adapters and custom UIs. The CLI remains the simplest operator surface and the easiest way to demonstrate the full lifecycle locally.
