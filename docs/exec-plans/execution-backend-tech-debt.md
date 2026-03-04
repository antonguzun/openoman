# Retire execution-model technical debt and decouple OpenOMAN from microVM-only assumptions

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `/docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this work, `openoman` can run the same job through more than one execution environment. A Linux host with Firecracker can keep using a microVM. A host that is already isolated by some outer virtualization layer, or a host that cannot provide nested virtualization, can opt into a process-based backend that runs the agent directly on that host with explicit risk signaling. The same refactor also makes agent providers easier to extend, because adding a new agent will no longer require editing the CLI, the guest init script, the environment renderer, and test fakes all at once.

The user-visible proof is simple. A contributor can run one job with a Firecracker-backed configuration and another with a process-backed configuration and receive the same artifact classes: `workspace.sandbox_result`, `sandbox.patch`, `sandbox.report`, and `sandbox.logs`. A contributor can also add a new agent provider by implementing one adapter and tests around that adapter instead of touching execution orchestration end to end.

## Progress

- [x] (2026-03-03 17:20Z) Reviewed the current architecture documents, CLI orchestration, sandbox backend code, guest contract, and end-to-end tests.
- [x] (2026-03-03 17:28Z) Identified the main technical debt clusters: orchestration lives in the CLI, backend-neutral types are Firecracker-shaped, and agent providers are implemented as cross-cutting conditionals.
- [x] (2026-03-03 17:34Z) Wrote this technical debt retirement plan as a self-contained ExecPlan under `docs/exec-plans/`.
- [ ] Extract job execution orchestration from `crates/cli/src/main.rs` into a core application layer that persists stage transitions incrementally.
- [ ] Replace the current sandbox-centric configuration and factory with a backend-neutral execution abstraction that can host both Firecracker and process execution.
- [ ] Split the current Firecracker implementation into smaller adapters for runtime staging, VM launch, artifact extraction, and network control.
- [ ] Introduce a process backend for already-isolated hosts and for environments that cannot or should not use nested virtualization.
- [ ] Replace the current `AgentProvider` branching with an adapter registry that produces an execution plan for the selected agent.
- [ ] Add a real trusted validation port so `JobState::Validating` is not an immediate success transition.
- [ ] Add cross-backend tests that prove the same artifact pipeline and publishing gate work for both Firecracker and process execution.

## Surprises & Discoveries

- Observation: the design documents already describe an `application` layer and `adapters`, but the implementation still places the full run pipeline inside the CLI command handler.
  Evidence: `docs/design-docs/system-overview.md` suggests `crates/core/application` and `crates/core/adapters`, while `crates/cli/src/main.rs` performs workspace preparation, runner lifecycle, artifact handling, validation state changes, publishing, and persistence in the `Run` command path.
- Observation: `SandboxRuntimeConfig` is backend-neutral in name only. Its shared type already embeds Firecracker-specific configuration.
  Evidence: `crates/core/src/sandbox/backend.rs` defines `SandboxRuntimeConfig` with `firecracker: Option<FirecrackerBackendConfig>`.
- Observation: adding Cursor support already required changes across the CLI, core sandbox env rendering, the guest shell script, and fake Firecracker tests.
  Evidence: provider-specific logic appears in `crates/cli/src/main.rs`, `crates/core/src/sandbox/mod.rs`, `crates/core/src/sandbox/firecracker.rs`, `guest/openoman-init.sh`, and `crates/cli/tests/cli_e2e.rs`.
- Observation: the existing artifact pipeline is already mostly backend-independent because canonical patch generation happens on the trusted host after collecting a modified workspace tree.
  Evidence: `crates/core/src/git.rs` writes the canonical patch from `trusted_clone_dir` plus `modified_workspace_dir`, independent of how that modified workspace was produced.
- Observation: the current `Validating` state is structural rather than real, because the implementation moves from `start_validation()` straight to `mark_validation_succeeded()` without a validation adapter.
  Evidence: `crates/cli/src/main.rs` enters `JobState::Validating` and immediately marks validation successful in the same run path.

## Decision Log

- Decision: treat this work as technical debt retirement rather than as one feature.
  Rationale: the problem is structural. The current system can ship one backend, but it becomes progressively harder to add new execution substrates, new agents, and a non-virtualized mode without repeated cross-cutting edits.
  Date/Author: 2026-03-03 / Codex
- Decision: raise the abstraction from `sandbox backend` to `execution backend`.
  Rationale: a process backend that runs on an already-isolated host is a valid deployment mode, but it is not a sandbox in the same sense as a microVM. The broader term keeps the model honest and avoids encoding security assumptions into type names.
  Date/Author: 2026-03-03 / Codex
- Decision: keep Firecracker as one backend rather than the architectural center.
  Rationale: Firecracker remains the strongest built-in isolation mode on Linux hosts, but the system must also support environments with no nested virtualization or with pre-existing outer virtualization.
  Date/Author: 2026-03-03 / Codex
- Decision: make process execution an explicitly gated mode with a named risk posture.
  Rationale: running the agent directly on the host is acceptable only when the operator intentionally states that the host is already disposable or otherwise isolated. The configuration must not silently downgrade from microVM isolation to a plain process.
  Date/Author: 2026-03-03 / Codex
- Decision: replace the current agent enum branching with provider adapters keyed by stable string identifiers.
  Rationale: an enum in shared orchestration code forces every new provider to modify multiple unrelated modules. A registry keeps provider-specific knowledge in one place.
  Date/Author: 2026-03-03 / Codex
- Decision: preserve the current trusted artifact and publishing pipeline while changing execution internals.
  Rationale: the canonical patch generation, trusted Git operations, and trusted GitHub publishing model are solid foundations and should remain stable while execution internals are refactored.
  Date/Author: 2026-03-03 / Codex

## Outcomes & Retrospective

At plan creation time, no code has been refactored yet. The outcome of this document is a concrete, phased retirement plan for the current execution-model debt. The immediate value is that future implementation work can proceed without rediscovering the same architectural constraints: the CLI currently owns orchestration, Firecracker assumptions leak into shared types, and agent providers are not isolated behind one adapter seam.

The central lesson from the current repository state is that the system already contains the correct trusted-host pieces, but it has not yet completed the separation that the design documents describe. The refactor should therefore preserve the current good parts instead of rewriting them: trusted Git preparation, canonical patch generation, artifact storage, and trusted publishing should stay in place while execution becomes backend-neutral.

## Context and Orientation

This repository is a Rust workspace with `crates/core` for trusted host logic and `crates/cli` for the current operator interface. The most relevant files for this debt are:

`crates/cli/src/main.rs` currently parses config, loads jobs, prepares workspaces, starts execution, collects artifacts, marks validation state transitions, publishes to GitHub, and writes persistence updates. In plain language, this file is doing the job of an application service. That means any new backend or agent provider must pass through the CLI entrypoint.

`crates/core/src/sandbox/mod.rs` and `crates/core/src/sandbox/backend.rs` define the current execution interfaces. They use backend-neutral names such as `SandboxRuntimeConfig` and `SandboxBackend`, but the shared configuration already includes Firecracker-specific fields and only one backend kind.

`crates/core/src/sandbox/firecracker.rs` is the current concrete backend. It stages a runtime tree, builds an ext4 image, launches Firecracker, manages host-proxy networking, waits for completion, and extracts artifacts. In plain language, this one file contains several different responsibilities that would all have to be reimplemented again for another backend.

`guest/openoman-init.sh` is the guest-side launcher for the Firecracker path. It mounts the runtime disk, installs provider-specific auth files, configures networking, and branches on `AGENT_PROVIDER` to run either Codex or Cursor. That means agent provider behavior is partly encoded in shell, not only in Rust.

`crates/core/src/git.rs` prepares the trusted clone and sandbox workspace and later computes a canonical patch from the trusted clone plus a modified workspace directory. This is already a good abstraction because it does not care whether the modified workspace came from Firecracker or a plain host process.

`crates/core/src/github.rs` publishes the validated patch from the trusted clone. This should remain a trusted-host adapter and should not be coupled to any particular execution backend.

`crates/core/src/domain/job.rs` models the job state machine. It contains useful states such as `Validating`, `Publishing`, and `Notifying`, but the current runtime does not yet persist all of those transitions as durable stages during a run.

`crates/core/src/persistence/mod.rs` stores jobs, attempts, artifacts, and outbox events in SQLite. It is sufficient for the current MVP, but it will need more frequent writes during execution once orchestration moves into the core and stage transitions become durable.

For this plan, an "execution backend" means the mechanism that runs the untrusted agent workload and produces a modified workspace plus report and logs. A "process backend" means a backend that runs the agent as a direct host process instead of inside a guest virtual machine. An "already-isolated host" means a machine that is itself a disposable VM, a CI worker, or some other environment where the operator accepts host-level exposure because a stronger outer isolation boundary already exists.

## Plan of Work

Start by moving orchestration out of the CLI and into a new application layer in `crates/core/src/application/`. Create a `RunJobUseCase` that owns the full pipeline: loading the job, preparing the workspace, choosing the execution backend, persisting attempt start, running the attempt, collecting artifacts, entering validation, calling a validator, publishing when allowed, and persisting each state transition. The CLI should become a thin adapter that parses command-line arguments, loads config, and prints human-readable results. This is the first debt payment because it reduces the number of places that need to know how a job runs.

Once orchestration lives in the core, replace the current sandbox-oriented shared types with execution-oriented ones. Create a new module such as `crates/core/src/execution/`. Define shared types there for runtime limits, backend kind, backend capabilities, risk posture, attempt spec, and collected output. During migration, keep compatibility by letting the CLI continue to parse the old `[sandbox]` config shape and translate it into the new core types. Only after the new flow is stable should the public config surface switch to `[execution]` as the preferred name.

Next, split the current Firecracker implementation into smaller adapters under `crates/core/src/execution/firecracker/`. One adapter should stage the runtime tree and runtime image. One should launch the VM and wait for it. One should collect artifacts. One should manage optional host-proxy networking. This does not change behavior yet. It lowers the cost of keeping Firecracker while adding another backend, and it narrows the blast radius when Linux-specific code changes.

After that, add a process backend under `crates/core/src/execution/process.rs`. This backend should refuse to start unless the configuration explicitly marks the host risk posture as something like `already_isolated`. The process backend should create an attempt working directory, set a temporary `HOME`, inject only the approved agent inputs, run the selected agent in the prepared workspace, and write the same stable output files as the Firecracker path. It should not require `debugfs`, ext4 images, or `/dev/kvm`. This backend is the practical answer for nested-virtualization-free deployments and for hosts that are already running inside an outer VM.

Then, refactor agents. Create a new module such as `crates/core/src/agents/` with an adapter trait and one adapter per provider. Each adapter should validate provider-specific config, resolve secrets from the host, and produce an `AgentLaunchSpec` or `ExecutionPlan` that contains the command arguments, environment variables, auth file mounts or copies, and expected report capture mode. Firecracker and process backends should both consume the same provider-neutral launch spec. The guest init script should stop branching on provider-specific behavior and instead interpret a serialized launch plan for VM-style backends.

After the execution and agent seams are in place, add a real validator port in `crates/core/src/validation/`. The first implementation can be a shell-command validator driven by `check_profile`, but it must do actual trusted work before `mark_validation_succeeded()` is called. This step removes the current false sense of a validation stage.

Finally, add end-to-end coverage. Keep existing Firecracker fake tests, but add mirrored process-backend tests. Add a compatibility test that proves the current artifact pipeline is unchanged across backends. Add a safety test showing that the process backend is rejected when the host risk posture is too strict. Add a provider-extensibility test that demonstrates adding a minimal fake agent adapter without editing shared orchestration code.

## Milestones

### Milestone 1: Core-owned orchestration and durable stage transitions

At the end of this milestone, `crates/cli/src/main.rs` no longer owns the job pipeline. A new application-layer entrypoint in `crates/core` runs the job, and SQLite is updated after each meaningful stage instead of only at the end. A contributor can kill the process after attempt start or after artifact collection, inspect the database, and see a truthful stage rather than only `queued` or a final state.

Implement this by creating `crates/core/src/application/mod.rs` and `crates/core/src/application/run_job.rs`, moving the current `Run` command logic out of the CLI, and adding persistence calls around attempt start, artifact collection, validation start, validation result, publish result, and terminal state. Keep the external CLI behavior stable while changing the implementation boundary.

Validate this milestone by running:

    cargo test -p openoman-core
    cargo test -p openoman-cli

and by manually submitting a job, starting `openoman run <job_id>`, interrupting it after the attempt begins, and then checking `openoman status <job_id>` to confirm that the job is no longer reported as `queued`.

### Milestone 2: Backend-neutral execution layer with Firecracker migrated intact

At the end of this milestone, the shared execution model is no longer named or shaped around Firecracker. Firecracker still works, but it implements the new `ExecutionBackend` contract behind narrower sub-adapters.

Implement this by creating `crates/core/src/execution/mod.rs`, moving shared runner types there, adding backend capabilities and host risk posture, and changing the factory and config translation to build execution backends rather than sandbox backends. Move Firecracker-specific staging, VM launch, collection, and network code into smaller files beneath `crates/core/src/execution/firecracker/`.

Validate this milestone by running the existing Firecracker unit and CLI end-to-end tests and confirming that a Firecracker-backed run still produces the same artifact names and job result behavior as before.

### Milestone 3: Process backend for already-isolated hosts

At the end of this milestone, `openoman` can execute a job without nested virtualization when the operator intentionally opts into that risk posture. The process backend writes the same output files as Firecracker and feeds the same trusted patch and publishing pipeline.

Implement this by adding `crates/core/src/execution/process.rs`, a process runner that creates a per-attempt runtime directory, uses a temporary home directory, runs the selected agent in the prepared workspace, captures stdout and stderr into stable logs, and copies or moves the resulting workspace into the standard collected-output directory. Add explicit config validation so `backend = "process"` requires `host_risk_posture = "already_isolated"` or equivalent.

Validate this milestone by running a process-backed end-to-end test and by manually using a config file that selects the process backend. A successful run should still create `sandbox.patch`, `sandbox.report`, and `sandbox.logs` artifacts. A config that selects the process backend without the required risk posture should fail fast with a clear error before the job starts.

### Milestone 4: Agent adapter registry and provider-neutral launch plans

At the end of this milestone, adding a new agent provider no longer requires editing shared orchestration, shared config parsing, the guest shell script, and fake Firecracker logic together. Instead, one provider adapter produces a launch plan that each backend executes.

Implement this by adding `crates/core/src/agents/mod.rs`, moving provider-specific validation and secret resolution there, and replacing the current `AgentProvider` enum branches with a registry keyed by provider name strings. Define a serialized launch-plan format for VM-style backends and a native in-memory launch spec for process execution. Update `guest/openoman-init.sh` to interpret the launch-plan data instead of hardcoding `codex` and `cursor` branches.

Validate this milestone by keeping existing Codex and Cursor tests green and by adding a minimal fake provider in tests that proves a new adapter can be registered without editing the core run pipeline.

### Milestone 5: Real trusted validation and cross-backend acceptance

At the end of this milestone, `JobState::Validating` corresponds to real host-side validation work, and both Firecracker and process backends pass through the same validation gate before publishing.

Implement this by adding `crates/core/src/validation/mod.rs` and a first validator implementation that maps `check_profile` to a trusted command or command set executed in the trusted clone after applying the canonical patch. Update the run use case so `mark_validation_succeeded()` occurs only after the validator returns success. Record failures as `ValidationFailed`, not only as a generic job failure.

Validate this milestone by writing one end-to-end test where validation passes and publishing proceeds, one where the validator fails and publishing is skipped, and one matrix-style test that executes the same fixture repository through both Firecracker and process backends and compares the produced patch and report behavior.

## Concrete Steps

Work from the repository root unless a step says otherwise.

Begin each implementation milestone with formatting and targeted tests:

    cargo fmt --all
    cargo test -p openoman-core
    cargo test -p openoman-cli

Once the new execution layer exists, add focused validation runs for each backend:

    cargo test -p openoman-core execution
    cargo test -p openoman-cli cli_e2e

When the process backend lands, add one manual smoke run with a disposable local repository and a config that selects the process backend:

    cargo run -p openoman-cli -- --config ./config.process.toml submit --repo /abs/path/to/repo --revision main --instruction "append a blank line to README.md"
    cargo run -p openoman-cli -- --config ./config.process.toml run <job_id>
    cargo run -p openoman-cli -- --config ./config.process.toml artifacts <job_id>
    cargo run -p openoman-cli -- --config ./config.process.toml result <job_id>

Expected observable behavior after the full refactor:

    `artifacts <job_id>` lists the same artifact classes for both a Firecracker-backed run and a process-backed run.

    `result <job_id>` still prints a plain success or failure summary, plus branch and pull request metadata when publishing is enabled and validation succeeds.

    Selecting the process backend without the required host risk posture fails during startup validation with a message that explains why direct host execution is unsafe by default.

## Validation and Acceptance

Acceptance is met when a novice can verify all of the following behaviors without reading any source code:

1. `openoman run <job_id>` no longer depends on CLI-owned orchestration. Interrupting and restarting around stage boundaries leaves truthful persisted job state in SQLite.
2. Firecracker remains usable on Linux hosts and continues to produce the current artifact pipeline.
3. A process backend exists and can run on a host that does not offer nested virtualization, provided the operator explicitly opts into the corresponding risk posture.
4. Both backends feed the same trusted patch-generation, validation, and publishing path.
5. Validation is real host-side work and can fail independently of execution, producing a validation failure without publishing.
6. Adding a new agent provider requires implementing one adapter and tests around that adapter, not editing shared orchestration or backend internals.

The preferred automated validation suite at completion is:

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features

There should also be one manual matrix check:

    Firecracker config -> run one fixture job -> inspect artifacts and result
    Process config -> run the same fixture job -> inspect artifacts and result

The expected difference is only in execution substrate and safety posture, not in artifact classes or trusted publish behavior.

## Idempotence and Recovery

This refactor should be implemented in additive stages. Keep compatibility shims while moving from `sandbox` names to `execution` names so that current tests and local configs continue to work during the migration. Each backend should continue using per-attempt runtime directories so rerunning tests or rerunning the same job in a disposable local database does not require manual cleanup beyond deleting the temp database and workspace directories.

If a migration step fails halfway, the safe recovery path is to revert only the in-progress code changes in the current working tree, keep the database and fixture repositories disposable, and rerun the targeted test set for the last completed milestone before continuing. Do not attempt a one-shot rename of every `sandbox` symbol at the start; keep compatibility aliases or translation layers until both backends are proven under test.

## Artifacts and Notes

The current stable artifact contract should remain unchanged during the refactor:

    workspace.trusted_clone
    workspace.sandbox_input
    workspace.sandbox_result
    sandbox.patch
    sandbox.report
    sandbox.logs

During migration, prefer a config translation layer rather than an immediate public config break. The target configuration shape after the refactor should look like this:

    [execution]
    backend = "process"
    runtime_dir = "./workspaces/execution"
    host_risk_posture = "already_isolated"

    [execution.process]
    shell = "/bin/sh"

    [agent]
    provider = "cursor"
    bin = "cursor-agent"
    api_key_env = "OPENOMAN_CURSOR_API_KEY"

The Firecracker-backed equivalent should remain available:

    [execution]
    backend = "firecracker"
    runtime_dir = "./workspaces/execution"
    host_risk_posture = "protect_host"

    [execution.firecracker]
    mode = "direct"
    firecracker_bin = "firecracker"
    kernel_image_path = "./guest/out/vmlinux"
    rootfs_image_path = "./guest/out/rootfs.ext4"

## Interfaces and Dependencies

In `crates/core/src/execution/mod.rs`, define a backend-neutral surface similar to:

    pub enum ExecutionBackendKind {
        Firecracker,
        Process,
    }

    pub enum IsolationLevel {
        MicroVm,
        HostProcess,
    }

    pub enum HostRiskPosture {
        ProtectHost,
        AlreadyIsolated,
    }

    pub struct BackendCapabilities {
        pub isolation_level: IsolationLevel,
        pub supports_nested_virtualization: bool,
        pub supports_network_policy: bool,
        pub supports_guest_contract: bool,
    }

    pub struct ExecutionRuntimeConfig {
        pub backend: ExecutionBackendKind,
        pub runtime_dir: PathBuf,
        pub limits: ResourceLimits,
        pub host_risk_posture: HostRiskPosture,
        pub firecracker: Option<FirecrackerBackendConfig>,
        pub process: Option<ProcessBackendConfig>,
    }

    pub trait ExecutionBackend {
        fn kind(&self) -> ExecutionBackendKind;
        fn capabilities(&self) -> BackendCapabilities;
        fn check_runtime_dependencies(&self) -> Result<(), ExecutionError>;
        fn create_runner(&self) -> Result<Box<dyn ExecutionRunner>, ExecutionError>;
    }

    pub trait ExecutionRunner {
        fn start(&mut self, spec: AttemptSpec) -> Result<ExecutionHandle, ExecutionError>;
        fn wait(&mut self, handle: &ExecutionHandle) -> Result<ExecutionExitStatus, ExecutionError>;
        fn collect_output(
            &self,
            handle: &ExecutionHandle,
            job_id: &str,
            attempt_id: u32,
        ) -> Result<CollectedExecutionOutput, ExecutionError>;
        fn stop(&mut self, handle: &ExecutionHandle) -> Result<(), ExecutionError>;
    }

In `crates/core/src/agents/mod.rs`, define a provider adapter surface similar to:

    pub trait AgentAdapter {
        fn provider_name(&self) -> &'static str;
        fn resolve_host_inputs(
            &self,
            config: &AgentRuntimeConfig,
        ) -> Result<ResolvedAgentInputs, AgentConfigError>;
        fn build_launch_spec(
            &self,
            request: &AgentLaunchRequest,
        ) -> Result<AgentLaunchSpec, AgentConfigError>;
    }

The `ResolvedAgentInputs` type should hold already-resolved host secrets and file references. The `AgentLaunchSpec` type should be backend-neutral and contain the command, arguments, environment, working directory expectations, report capture mode, and any staged auth file data needed by either Firecracker or process execution.

In `crates/core/src/application/run_job.rs`, expose one orchestration entrypoint such as:

    pub struct RunJobUseCase { ... }

    impl RunJobUseCase {
        pub fn run(&self, job_id: &JobId) -> Result<RunJobResult, RunJobError>;
    }

In `crates/core/src/validation/mod.rs`, define:

    pub trait Validator {
        fn validate(
            &self,
            trusted_clone_dir: &Path,
            patch_path: &Path,
            check_profile: &CheckProfile,
        ) -> Result<ValidationOutcome, ValidationError>;
    }

Do not add new external services as part of this refactor. Reuse the current local SQLite store, trusted Git adapter, artifact storage, and GitHub publishing adapter. The main new dependency is architectural, not infrastructural: a new execution module, a new agents module, and a new validation module inside `crates/core`.

Revision note (2026-03-03): Created this ExecPlan to record the technical debt retirement path required to support both microVM and bare-metal-style execution modes, plus cleaner agent extensibility, without discarding the current trusted artifact and publishing pipeline.
