# System Overview

The deployment target is a single bare-metal Linux host. The host runs one trusted core service and launches one disposable microVM per job attempt. The microVM contains the untrusted agent runtime and its local container runtime. Future integrations run as separate plugin processes outside the core.

## Major components

### Trusted core service

The core service is responsible for:

- accepting CLI requests
- storing job state and artifact references in SQLite
- preparing trusted repository clones and sandbox workspaces
- starting and stopping microVMs through a runner or adapter
- collecting patch, report, and log artifacts
- re-validating the proposed patch in a trusted clean clone
- pushing branches and creating GitHub pull requests
- writing integration events to an outbox for later plugin delivery

### Untrusted microVM sandbox

Each job attempt gets its own microVM with a separate guest kernel. Inside that guest, the agent can edit the repository, start Docker Compose services, run tests, and inspect the resulting logs. The guest is disposable and should be treated as hostile from the host's perspective.

### External plugin processes

Plugins are a post-MVP concern, but the architecture already reserves a place for them. They are separate executables discovered by manifest and contacted through a stable JSON-RPC protocol over standard input and output. They are intended for notifications first and, later, for task-source integrations.

## Trust boundaries

The core service is trusted. The agent and everything inside the microVM are untrusted. Plugins are trusted only for the credentials and systems they explicitly integrate with, so they should receive the smallest possible data set and should be isolated with normal operating-system controls rather than being treated as part of the core.

## High-level system flow

1. The core creates a job from CLI input and records it.
2. The core prepares a trusted clone and a sandbox workspace snapshot.
3. The core starts a dedicated microVM for the attempt.
4. The agent runs inside the guest and produces code changes plus logs.
5. The core copies back untrusted artifacts and validates them in a clean trusted clone.
6. If validation succeeds, the core publishes the result to GitHub and records the outcome.
7. The core emits integration events into the outbox and reports the final status through the CLI.

## Suggested repository layout

The architecture assumes a layout like this as implementation work begins:

- `crates/core` for the trusted service
- `crates/core/domain` for domain types such as jobs, plugins, and events
- `crates/core/application` for use cases and orchestration
- `crates/core/adapters` for SQLite, GitHub, microVM, and artifact storage implementations
- `guest/` for microVM image build scripts and guest configuration
- `docs/design-docs/` for the design set described in this directory

The exact module boundaries can move as the codebase evolves, but the trust boundary between the core and the sandbox should remain visible in the source layout.
