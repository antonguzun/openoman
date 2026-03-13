# Core Beliefs

The system exists to automate small, well-scoped repository changes without trusting the coding agent that proposes them. It should feel operationally simple to run, but strict about where privilege lives and when code is allowed to leave the machine.

## What the architecture optimizes for

- Host safety. The sandbox must not receive host secrets, host runtime sockets, or arbitrary filesystem access.
- Deterministic publishing. Only the trusted core can push branches or create pull requests, and only after validation succeeds.
- Minimal operational overhead. The MVP prefers one bare-metal Linux host, a CLI interface, and SQLite instead of external brokers or distributed control planes.
- Extension without core sprawl. Integrations should remain outside the trusted core and use a stable HTTP control plane instead of loading third-party logic into the runtime.

## MVP goals

The MVP should complete one end-to-end job: clone a repository, prepare a sandbox workspace, run the agent inside a microVM, collect patch and log artifacts, validate the proposed changes in a trusted clean clone, publish a GitHub pull request when configured, and emit a final summary plus outbox events.

The MVP should also provide a CLI for submitting, running, and inspecting jobs, while persisting enough state in SQLite to inspect outcomes after restart.

## Safety goals

The untrusted agent must not gain access to GitHub tokens, SSH keys, or any other host secrets. It must not receive the host Docker or Podman socket. Its filesystem view must be limited to the dedicated job workspace. Network egress must be controllable, ideally through a small set of allowlisted endpoints exposed by a proxy or mirror. Resource limits must prevent a single job attempt from exhausting the host.

## Explicit non-goals for the MVP

- No Slack, Telegram, Jira, or Linear integrations are implemented yet.
- No GitLab support is required; GitHub is the only publishing target.
- No distributed execution across multiple machines.
- No advanced scheduling beyond a simple FIFO queue.
- No package-management behavior in the core for installing third-party integration dependencies.
- No sophisticated policy engine for sandbox control beyond explicit configuration.

## Core design decisions

The project chooses microVM isolation rather than a host container boundary because the agent must be able to run realistic Docker Compose workloads while staying outside the host kernel and host container runtime. The trusted core replays and validates the sandbox output in a clean clone so that publishing depends on deterministic evidence instead of the sandbox filesystem state. SQLite is used because it keeps the MVP operationally small while still giving durable job history and an outbox for future integrations. The control plane is HTTP-first so that bots, issue bridges, and custom UIs can evolve independently of the trusted runtime.
