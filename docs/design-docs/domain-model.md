# Domain Model

The architecture uses a small domain model so the trusted core can reason about long-running work without depending on sandbox internals. The important concept is the job: one request to modify a repository and optionally publish the validated result.

## Aggregates

### Job

`Job` is the main aggregate root. It represents one end-to-end unit of work and owns the authoritative state machine.

Its responsibilities are:

- storing the original request, including repository reference, revision, instruction text, check profile, and publish policy
- recording each attempt and its outcome
- tracking artifact references for patch, report, and logs
- storing publish results such as branch name and pull request URL
- enforcing invariants around validation and publishing

Important invariants:

- a job can have only one active attempt at a time
- artifacts copied out of the sandbox are untrusted until validation succeeds
- publishing is allowed only after trusted validation succeeds
- terminal states are final unless the user creates a new job as a rerun

### Plugin

`Plugin` is a separate aggregate reserved for post-MVP integration support. It records plugin identity, version, entrypoint command, declared capabilities, whether the plugin is enabled, and a reference to its configuration schema and secret source.

## Entities and value objects

### Attempt

An `Attempt` belongs to a job and captures one execution of the sandbox pipeline. It records the attempt number, sandbox resource settings, sandbox exit reason, collected artifact references, and timestamps for each stage.

### ArtifactRef

An `ArtifactRef` identifies a collected artifact without making the artifact itself trusted. It includes the artifact type, a storage pointer such as a local path or content-addressed reference, and any useful metadata such as hash or size.

### Request value objects

`RepoRef`, `Revision`, `CheckProfile`, and `PublishPolicy` are value objects that keep the original job specification explicit and stable. They should be treated as part of the job contract rather than as loose fields spread across the system.

## Job state machine

The recommended MVP states are:

- `Queued`
- `Running`
- `CollectingArtifacts`
- `Validating`
- `Publishing`
- `Notifying`
- `Succeeded`
- `Failed`
- `Canceled`

The state machine exists so the core can resume inspection after restart, explain what happened to operators, and enforce the publish-after-validate rule. `Publishing` is reachable only after validation succeeds. `Succeeded`, `Failed`, and `Canceled` are terminal.
