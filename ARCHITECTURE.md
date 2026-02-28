# Architecture

This repository describes a secure agent runner that automates small, well-scoped code changes without giving an untrusted coding agent access to host secrets or privileged publishing credentials. The system runs the agent inside a disposable microVM, collects its proposed changes and logs, re-validates them in a trusted environment, and lets only the trusted core publish code and create pull requests.

This file is intentionally the top-level overview. The detailed design now lives under [`docs/design-docs/index.md`](docs/design-docs/index.md).

## System at a glance

- The deployment target is a single bare-metal Linux host with minimal dependencies.
- The trusted Rust core owns job orchestration, persistence, validation, and GitHub publishing.
- The untrusted agent runs inside a dedicated microVM where it can modify code and run real Docker Compose workloads.
- Plugins remain out-of-process integrations behind a frozen JSON-based protocol so the core stays deterministic and language-agnostic.

## Non-negotiable invariants

- Host secrets and VCS credentials never enter the sandbox.
- The sandbox never receives the host container runtime socket or arbitrary host filesystem access.
- Publishing is allowed only after trusted validation succeeds against a clean clone.
- State, artifact references, and future integration events are persisted locally in SQLite.

## Documentation map

- [`docs/design-docs/index.md`](docs/design-docs/index.md): reading guide for the architecture set.
- [`docs/design-docs/core-beliefs.md`](docs/design-docs/core-beliefs.md): goals, constraints, non-goals, and the design choices that shape the system.
- [`docs/design-docs/system-overview.md`](docs/design-docs/system-overview.md): major components, trust boundaries, and the repository layout.
- [`docs/design-docs/domain-model.md`](docs/design-docs/domain-model.md): jobs, attempts, artifacts, plugins, and state transitions.
- [`docs/design-docs/execution-lifecycle.md`](docs/design-docs/execution-lifecycle.md): the end-to-end workflow from job submission through result reporting.
- [`docs/design-docs/sandbox-and-safety.md`](docs/design-docs/sandbox-and-safety.md): microVM isolation, network control, validation, and publishing safety.
- [`docs/design-docs/plugin-protocol.md`](docs/design-docs/plugin-protocol.md): frozen plugin protocol and integration event contract.
- [`docs/design-docs/operational-model.md`](docs/design-docs/operational-model.md): persistence, CLI surface, and artifact handling.
- [`docs/design-docs/roadmap.md`](docs/design-docs/roadmap.md): planned extensions beyond the MVP.

## Recommended reading order

Start with the design-doc index, then read core beliefs and the system overview. After that, use the domain, lifecycle, safety, and protocol documents as needed for implementation details.
