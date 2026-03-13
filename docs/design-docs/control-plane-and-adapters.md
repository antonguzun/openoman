# Control Plane and Adapters

The trusted core now exposes an HTTP control plane instead of a plugin runtime. The control plane is the stable product boundary for job submission, inspection, and future user interfaces.

## Control plane

The first release exposes an `axum` HTTP server through `openoman serve`. It runs on the same host as the trusted core, persists state in SQLite, and executes jobs in-process.

The initial endpoint set is:

- `GET /health`
- `POST /jobs`
- `GET /jobs`
- `GET /jobs/:id`
- `POST /jobs/:id/run`
- `POST /jobs/:id/retry`
- `GET /jobs/:id/logs`
- `GET /jobs/:id/artifacts`
- `GET /jobs/:id/result`

Authentication is intentionally simple in the first iteration: bind to localhost by default, or require a static bearer token configured in `[server]`.

Detailed API references live under:

- [docs/api/control-plane.md](/home/antonguzun/Work/personal/openoman/docs/api/control-plane.md)
- [docs/api/openapi.yaml](/home/antonguzun/Work/personal/openoman/docs/api/openapi.yaml)

## Adapters

An adapter is an external client or service that translates some other interface into calls to the control plane.

Examples:

- a Telegram bot that turns chat commands into `POST /jobs`
- a Slack app that posts job status updates back into a channel
- an internal web UI that lists jobs and lets operators trigger reruns
- a Jira or Linear bridge that creates jobs from issue state changes

Adapters are not loaded into the trusted core process. They own channel-specific UX, credentials, and payload mapping. The trusted core owns job state, sandbox execution, validation, artifact handling, and publishing.

## Why adapters instead of plugins

The project explicitly avoids a general plugin runtime for now because it would require lifecycle management, discovery, versioning, supervision, and compatibility guarantees before there is a stable control-plane API. The HTTP boundary gives the same extensibility for current use cases while keeping the trusted core narrow.

## Outbox and future integrations

The outbox remains part of the design because job lifecycle events are still useful for notifications, audit trails, or future push-style integrations. Those integrations should target the stable event data rather than require the core to host third-party code.
