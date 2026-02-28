# Decompose Architecture Documentation Into Linked Design Docs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` governs how this document must be maintained.

## Purpose / Big Picture

After this change, a reader can open `ARCHITECTURE.md` to understand the system at a glance, then follow links into focused design documents under `docs/design-docs/` for details about principles, runtime boundaries, domain concepts, lifecycle flow, and operational constraints. The result is easier to navigate than one long architecture file and gives the repository a stable place to grow future design documentation without overloading the top-level overview.

## Progress

- [x] (2026-02-28 23:00Z) Read `ARCHITECTURE.md`, `docs/PLANS.md`, `docs/MVP_REQUIREMENTS.md`, and `docs/PRODUCT_SENSE.md` to capture the existing architecture scope and the repository rules for documentation refactors.
- [x] (2026-02-28 23:01Z) Define the target `docs/design-docs/` file layout and assign each major section of `ARCHITECTURE.md` to a destination document.
- [x] (2026-02-28 23:03Z) Rewrite `ARCHITECTURE.md` into a short orientation document with a navigation map into `docs/design-docs/`.
- [x] (2026-02-28 23:03Z) Create the new design documents under `docs/design-docs/` and preserve the original architecture topics across focused files.
- [x] (2026-02-28 23:03Z) Verify the resulting document tree and inspect the rewritten files to confirm the decomposition is coherent.

## Surprises & Discoveries

- Observation: The repository instructions require an ExecPlan for significant refactors, and there is already a checked-in `docs/exec-plans/` area even though the architecture refactor itself is documentation-only.
  Evidence: `AGENTS.md` says to use an ExecPlan for significant refactors; `docs/exec-plans/active/` already exists.

- Observation: `docs/exec-plans/tech-debt-tracker.md` exists as an empty top-level file while the active/completed convention lives under subdirectories.
  Evidence: `ls -la docs/exec-plans` shows `active/`, `completed/`, and the empty `tech-debt-tracker.md`.

## Decision Log

- Decision: Treat this documentation decomposition as a significant refactor and record the work in an ExecPlan.
  Rationale: The change restructures a primary project document into multiple linked artifacts, and the repository instructions explicitly require an ExecPlan for significant refactors.
  Date/Author: 2026-02-28 / Codex

- Decision: Use eight focused design documents under `docs/design-docs/`: `index.md`, `core-beliefs.md`, `system-overview.md`, `domain-model.md`, `execution-lifecycle.md`, `sandbox-and-safety.md`, `plugin-protocol.md`, `operational-model.md`, and `roadmap.md`.
  Rationale: This split follows the existing architecture themes closely enough to preserve meaning, while separating principles, structure, flow, safety, integrations, and operational concerns into stable topics that can evolve independently.
  Date/Author: 2026-02-28 / Codex

## Outcomes & Retrospective

The refactor now leaves `ARCHITECTURE.md` as a landing page and moves the detailed architecture into a navigable set of focused documents under `docs/design-docs/`. The split preserved the original themes while making the repository easier to browse: principles, overview, domain model, execution lifecycle, sandbox policy, plugin contract, operations, and roadmap each have a dedicated home.

## Context and Orientation

The current repository has one large top-level architecture document at `ARCHITECTURE.md`. That file mixes high-level intent, system boundaries, domain concepts, runtime flow, integration protocol details, sandbox policy, persistence notes, and roadmap material in one place. Related context exists in `docs/MVP_REQUIREMENTS.md`, which states the minimum required behavior of the product, and `docs/PRODUCT_SENSE.md`, which states the product intent in prose. There is no existing `docs/design-docs/` directory.

For this plan, “decomposition” means splitting the single architecture narrative into multiple smaller Markdown files, each centered on one stable topic, while preserving the overall story through cross-links and a short top-level summary. The top-level summary must remain in `ARCHITECTURE.md`, because users may expect to land there first.

## Plan of Work

First, create `docs/design-docs/` and decide on a file layout that matches the current architecture themes. The target structure must include `docs/design-docs/index.md` and `docs/design-docs/core-beliefs.md`, because those names were explicitly requested. Additional files should be added only where they make navigation clearer, not just to maximize file count.

Next, rewrite `ARCHITECTURE.md` so it becomes an overview instead of the full specification. It should explain the system in a few sections, summarize the major design choices, and provide a reading path into the detailed design docs. It must no longer duplicate the entire detailed content verbatim.

Then, create the detailed documents in `docs/design-docs/`. Each document should absorb related material from the current `ARCHITECTURE.md`, using wording aligned with `docs/MVP_REQUIREMENTS.md` and `docs/PRODUCT_SENSE.md`. The set should collectively preserve the current architecture decisions: trust boundaries, deterministic publishing, sandbox model, domain model, job lifecycle, plugin protocol direction, persistence, CLI, and extension roadmap.

Finally, review the new document set for coverage and navigation. Every major section from the original `ARCHITECTURE.md` should have a clear destination in the new structure, and the index should make the reading order obvious to a new contributor.

The current section mapping is:

- Goals, safety goals, non-goals, and key design decisions move to `docs/design-docs/core-beliefs.md`.
- Components, trust boundaries, and the suggested repository layout move to `docs/design-docs/system-overview.md`.
- Aggregates, entities, value objects, and the job state machine move to `docs/design-docs/domain-model.md`.
- Core use cases and the end-to-end pipeline move to `docs/design-docs/execution-lifecycle.md`.
- Sandbox virtualization, network controls, resource limits, trusted validation, and trusted GitHub publishing move to `docs/design-docs/sandbox-and-safety.md`.
- Integration events, plugin discovery, JSON-RPC protocol, and plugin language support move to `docs/design-docs/plugin-protocol.md`.
- Persistence and CLI behavior move to `docs/design-docs/operational-model.md`.
- Extension material moves to `docs/design-docs/roadmap.md`.

## Concrete Steps

From `/home/antonguzun/Work/personal/openoman`:

1. Inspect the current architecture and related context documents.
2. Add this ExecPlan to `docs/exec-plans/active/architecture-doc-decomposition.md`.
3. Create `docs/design-docs/` and its Markdown files.
4. Replace the body of `ARCHITECTURE.md` with a concise overview and links.
5. Review the resulting document tree and skim the files to confirm the decomposition is coherent.

Expected verification is conceptual rather than executable: file listing should show the new structure, and the documents should read as a coherent, non-duplicative set.

## Validation and Acceptance

Acceptance is satisfied when all of the following are true:

1. `ARCHITECTURE.md` is a short top-level overview rather than the full detailed specification.
2. `docs/design-docs/index.md` exists and provides navigation to the detailed design documents.
3. `docs/design-docs/core-beliefs.md` exists and captures the foundational principles, goals, and constraints that guide the system.
4. The remaining architecture topics from the old `ARCHITECTURE.md` appear in focused documents under `docs/design-docs/` instead of being lost or left only in the top-level file.
5. A new contributor can follow links from `ARCHITECTURE.md` into the detailed docs and understand where to find information about the system model, runtime flow, safety boundaries, and future extensions.

## Idempotence and Recovery

This refactor is file-based and additive until the final rewrite of `ARCHITECTURE.md`. Re-running the steps is safe because the target files are Markdown documents in the repository. If a draft document ends up with the wrong scope, the safe recovery path is to edit that document and adjust the links in `ARCHITECTURE.md` and `docs/design-docs/index.md` until the navigation is clear again.

## Artifacts and Notes

Initial source material:

    ARCHITECTURE.md
    docs/MVP_REQUIREMENTS.md
    docs/PRODUCT_SENSE.md
    docs/PLANS.md

Requested target shape:

    docs/
    └── design-docs/
        ├── index.md
        ├── core-beliefs.md
        └── ...

Implemented document set:

    docs/design-docs/index.md
    docs/design-docs/core-beliefs.md
    docs/design-docs/system-overview.md
    docs/design-docs/domain-model.md
    docs/design-docs/execution-lifecycle.md
    docs/design-docs/sandbox-and-safety.md
    docs/design-docs/plugin-protocol.md
    docs/design-docs/operational-model.md
    docs/design-docs/roadmap.md

## Interfaces and Dependencies

This refactor depends only on repository Markdown files. The primary files that must exist at the end of the work are:

- `ARCHITECTURE.md`
- `docs/design-docs/index.md`
- `docs/design-docs/core-beliefs.md`

Additional Markdown files under `docs/design-docs/` should be named for stable architecture topics and linked from both `ARCHITECTURE.md` and `docs/design-docs/index.md`.

Revision note: 2026-02-28. Created the initial ExecPlan so the architecture-document decomposition can be executed and tracked in accordance with `docs/PLANS.md`.
Revision note: 2026-02-28. Recorded the target documentation layout and section mapping so implementation can proceed without re-deciding the information architecture.
Revision note: 2026-02-28. Marked the document decomposition complete after creating the `docs/design-docs/` set, rewriting `ARCHITECTURE.md`, and performing a manual coverage review.
