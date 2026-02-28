# Roadmap

This document captures the planned extensions beyond the MVP. These items are intentionally separated from the current architecture constraints so the MVP design can stay focused while still preserving the direction of travel.

## Additional input interfaces

Future versions can add richer frontends such as a TUI and task sources such as Linear or Jira. These should arrive as plugins or adapters rather than as logic folded directly into the trusted core.

## Additional notification channels

Notification plugins such as Telegram, Slack, or email are expected once the outbox dispatcher exists. The current architecture already freezes the event envelope and plugin protocol so those integrations can be added without redesigning the core.

## Multiple VCS providers

GitHub is the only MVP publishing target. A later version can introduce GitLab or other providers behind a VCS port so the domain model and job lifecycle stay stable while the publishing adapter changes.

## Stronger supply-chain and network controls

Later hardening can add internal container registry mirrors plus package-manager proxy caches for tools such as `pip`, `npm`, or `apt`. That would let the sandbox depend on a very small set of internal endpoints instead of reaching the public internet directly.

## Performance

Potential performance work includes pre-warmed microVM images, snapshot-based startup, and bounded concurrent workers.

## Reliability and operations

Later iterations can add richer retry policies, structured metrics, health endpoints, and more explicit separation between sandbox failures, validation failures, and publish failures.
