# Design Docs Index

This directory decomposes the architecture into focused documents. Read them in order if you are new to the project, or jump directly to the topic you need when working on a specific subsystem.

## Recommended path

1. [`core-beliefs.md`](core-beliefs.md) explains what the system is trying to achieve, what it refuses to do, and which constraints drive every other decision.
2. [`system-overview.md`](system-overview.md) shows the major components, the trust boundaries between them, and how the repository is expected to be organized.
3. [`domain-model.md`](domain-model.md) defines the long-lived concepts the core manages, especially jobs, attempts, artifacts, and plugin metadata.
4. [`execution-lifecycle.md`](execution-lifecycle.md) walks through the user-facing commands and the end-to-end job pipeline.
5. [`sandbox-and-safety.md`](sandbox-and-safety.md) details the microVM model, network restrictions, validation gate, and safe publishing path.
6. [`plugin-protocol.md`](plugin-protocol.md) freezes the plugin discovery model, transport, JSON-RPC methods, and integration event envelope.
7. [`operational-model.md`](operational-model.md) covers SQLite persistence, artifact references, and the MVP CLI.
8. [`roadmap.md`](roadmap.md) captures the intended post-MVP expansion areas.

## Topic map

- Principles and constraints: [`core-beliefs.md`](core-beliefs.md)
- Runtime structure and trust boundaries: [`system-overview.md`](system-overview.md)
- Data and state transitions: [`domain-model.md`](domain-model.md)
- Pipeline behavior and events: [`execution-lifecycle.md`](execution-lifecycle.md)
- Isolation and trusted control points: [`sandbox-and-safety.md`](sandbox-and-safety.md)
- External integration contract: [`plugin-protocol.md`](plugin-protocol.md)
- Storage and operator surface: [`operational-model.md`](operational-model.md)
- Future direction: [`roadmap.md`](roadmap.md)
