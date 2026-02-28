# Operational Model

The MVP is deliberately simple to operate. One host runs the trusted core, stores state in SQLite, and exposes a CLI for submission and inspection.

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

## CLI surface

The minimum CLI commands are:

- `submit`
- `run`
- `status <job_id>`
- `logs <job_id>` with optional follow behavior
- `artifacts <job_id>`
- `result <job_id>`

The CLI is both the operator surface for the MVP and the easiest way to demonstrate the full lifecycle before any richer UI or external integration exists.
