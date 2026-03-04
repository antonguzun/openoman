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
- [x] (2026-03-04 12:55Z) Completed Milestone 1 by moving the `run` pipeline into `crates/core/src/application/mod.rs`, shrinking `crates/cli/src/main.rs` to configuration plus output handling, and persisting job state after attempt start, artifact collection, validation transitions, publish transitions, and terminal completion.
- [x] (2026-03-04 13:16Z) Introduced `crates/core/src/execution/` as the real backend-neutral execution layer, moved the Firecracker backend under it, switched the CLI and application layer to `execution::*`, and reduced `crates/core/src/sandbox/` to a compatibility shim.
- [x] (2026-03-04 14:02Z) Split the Firecracker backend into `runtime`, `launch`, `artifacts`, and `network` adapters under `crates/core/src/execution/firecracker/`, leaving `firecracker.rs` as orchestration plus validation helpers.
- [x] (2026-03-04 14:34Z) Added `crates/core/src/execution/process.rs`, enabled `sandbox.backend = "process"` with explicit `sandbox.host_risk_posture = "already_isolated"`, and proved the same artifact pipeline through new core and CLI tests.
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
- Observation: SQLite state persistence can now be asserted during a live run without special hooks in the CLI.
  Evidence: `crates/core/src/application/mod.rs` now contains a test that pauses a fake runner inside `wait()`, reloads the same job from SQLite, and observes `JobState::Running` with one persisted attempt before the run is allowed to continue.
- Observation: keeping publishing config resolution as a closure passed from the CLI avoided pulling TOML parsing concerns into the new application layer.
  Evidence: `RunJobUseCase::run()` accepts a publisher-config resolver, so `crates/core` now owns orchestration while `crates/cli/src/main.rs` still owns config-file interpretation.
- Observation: the compatibility shim from `crate::sandbox` to `crate::execution` let the repository switch internal callers first without breaking the existing CLI config shape or every test at once.
  Evidence: `crates/core/src/sandbox/mod.rs` now re-exports `crate::execution::*` under legacy sandbox names while `crates/cli/src/main.rs` and `crates/core/src/application/mod.rs` call the new execution API directly.
- Observation: once Firecracker was split into submodules, sibling modules needed to import shared execution types from the parent `execution` module rather than from `firecracker`, or the refactor failed at compile time.
  Evidence: `crates/core/src/execution/firecracker/runtime.rs`, `network.rs`, `launch.rs`, and `artifacts.rs` now import shared types through `super::super::*`, while Firecracker-local helpers stay under `super::*`.
- Observation: the process backend could reuse the existing trusted artifact pipeline unchanged as long as it staged its own per-attempt workspace under the runtime directory and copied that workspace into the standard collected-output layout.
  Evidence: `crates/core/src/execution/process.rs` now runs the agent against `run_dir/workspace`, then returns the same `workspace-result`, `report.txt`, and `logs.txt` structure that `RunJobUseCase` already fingerprints and publishes.

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
- Decision: keep the first extracted run use case in `crates/core/src/application/mod.rs` instead of splitting `run_job.rs` immediately.
  Rationale: Milestone 1 is about moving orchestration and persistence boundaries first. Keeping the use case and its artifact helpers in one file made it easier to prove behavioral parity and remove the duplicate CLI implementation before deeper module reshaping in later milestones.
  Date/Author: 2026-03-04 / Codex
- Decision: inject publish configuration resolution from the CLI into the core run use case instead of moving config parsing into `crates/core`.
  Rationale: this preserves the existing CLI-owned TOML parsing boundary while letting `crates/core` own orchestration, stage persistence, artifact handling, and publishing decisions.
  Date/Author: 2026-03-04 / Codex
- Decision: represent runtime backend selection as `ExecutionBackendConfig` plus `HostRiskPosture` instead of keeping a Firecracker-specific optional field on the shared runtime config.
  Rationale: this is the minimum structural change that makes the shared execution model honest about future non-Firecracker backends while still preserving the current `[sandbox]` config file surface through translation in the CLI.
  Date/Author: 2026-03-04 / Codex

## Outcomes & Retrospective

Milestone 1 is now complete. The run pipeline lives in `crates/core/src/application/mod.rs`, and the CLI no longer owns workspace preparation, runner lifecycle, artifact collection, validation-state transitions, or publish-state transitions. The job store is updated at each durable boundary, so a live run now leaves truthful intermediate SQLite state instead of deferring nearly all persistence to the end.

The result preserves the current trusted-host behavior rather than rewriting it. Trusted Git preparation, canonical patch generation, artifact storage, and GitHub publishing still behave the same from the user’s perspective, but they are now exercised through a core application service. The remaining work is still substantial: the shared execution types are Firecracker-shaped, Firecracker itself is monolithic, validation is still structural, and process execution plus provider adapters do not exist yet.

Milestone 2 is now complete. The shared execution model is no longer centered on `crates/core/src/sandbox/`: `crates/core/src/execution/` defines the runtime config, backend trait, backend capabilities, risk posture, and Firecracker implementation, while `crates/core/src/sandbox/` exists only as a compatibility shim. Firecracker itself now sits behind narrower `runtime`, `launch`, `artifacts`, and `network` adapters, so the root runner file is mostly orchestration instead of a single Linux-specific grab bag.

Milestone 3 is now complete. `openoman` can run a job through a direct host-process backend when the operator explicitly sets `sandbox.backend = "process"` and `sandbox.host_risk_posture = "already_isolated"`. The new backend stages a per-attempt workspace and home directory under the runtime root, executes Codex- or Cursor-style agent commands directly on the host, and still feeds the same trusted patch, artifact, and publishing pipeline as Firecracker.

## Context and Orientation

This repository is a Rust workspace with `crates/core` for trusted host logic and `crates/cli` for the current operator interface. The most relevant files for this debt are:

`crates/core/src/application/mod.rs` now owns the `run` use case. It loads the queued job from SQLite, prepares the trusted and sandbox workspaces, drives the sandbox runner, fingerprints and stores artifacts, advances the job state machine, and records publish outbox events. In plain language, this file is now the application service that the design documents described.

`crates/cli/src/main.rs` still parses config and implements operator-facing commands, but its `Run` path is now a thin adapter that validates environment prerequisites, resolves the selected agent execution inputs, and calls the core run use case.

`crates/core/src/execution/mod.rs` and `crates/core/src/execution/backend.rs` now define the real execution interfaces. They introduce `ExecutionRuntimeConfig`, `ExecutionBackendConfig`, `ExecutionBackend`, backend capabilities, and host risk posture. In plain language, this is now the shared contract for running untrusted agent work regardless of substrate.

`crates/core/src/execution/firecracker.rs` is the current concrete backend. It stages a runtime tree, builds an ext4 image, launches Firecracker, manages host-proxy networking, waits for completion, and extracts artifacts. In plain language, this one file still contains several different responsibilities that would all have to be reimplemented again for another backend, which is why the next debt payment is splitting it into narrower adapters.

`crates/core/src/sandbox/mod.rs` is now only a compatibility layer. It re-exports the new execution types under legacy sandbox names so the migration can remain additive while callers move over incrementally.

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

Implement this by creating `crates/core/src/application/mod.rs`, moving the current `Run` command logic out of the CLI, and adding persistence calls around attempt start, artifact collection, validation start, validation result, publish result, and terminal state. Keep the external CLI behavior stable while changing the implementation boundary.

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

Milestone 1 validation completed with:

    cargo fmt --all
    cargo test -p openoman-core
    cargo test -p openoman-cli

The observed result on 2026-03-04 was that `openoman-core` passed 36 tests and `openoman-cli` passed 33 tests with the refactored orchestration path.

Milestone 2 validation so far completed with:

    cargo fmt --all
    cargo test -p openoman-core
    cargo test -p openoman-cli

The observed result on 2026-03-04 after introducing `crates/core/src/execution/` and finishing the Firecracker split was still `openoman-core` with 36 passing tests and `openoman-cli` with 33 passing tests, which shows that the execution abstraction and Firecracker decomposition landed without changing the current Firecracker-backed behavior.

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

Milestone 3 validation completed with:

    cargo fmt --all
    cargo test -p openoman-core --quiet
    cargo test -p openoman-cli --quiet

The observed result on 2026-03-04 after adding `crates/core/src/execution/process.rs` was `openoman-core` with 39 passing tests and `openoman-cli` with 36 passing tests across unit and end-to-end coverage, including a process-backed CLI run that produced the standard patch, report, and logs artifacts.

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

Revision note (2026-03-04): Updated the plan after completing Milestone 1 to document the extracted `RunJobUseCase`, the new incremental persistence behavior, the targeted validation results, and the temporary decision to keep publisher-config parsing in the CLI.

Revision note (2026-03-04): Updated the plan again after starting Milestone 2 to record the new `crates/core/src/execution/` module, the legacy sandbox compatibility shim, the addition of backend capabilities and host risk posture, and the fact that Firecracker decomposition is still pending.
