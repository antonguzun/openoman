# Sandbox And Safety

The system is only useful if the agent can run realistic local workloads without being trusted. This document captures the isolation model and the control points that keep the host and publishing path safe.

## microVM requirement

Each job attempt runs inside its own microVM. The separate guest kernel is the primary isolation boundary. The microVM must be able to host the agent runtime and a local container runtime so the agent can run Docker Compose stacks entirely inside the guest rather than against the host.

## Workspace transfer model

The MVP can use copy-in and copy-out semantics:

- the core copies a prepared workspace snapshot into the guest
- the guest returns a patch, a report, and logs to the host

A shared filesystem can be considered later, but only for the dedicated job workspace. The guest must never receive arbitrary host mounts.

## Network control

Direct open internet access from the microVM should be disabled by default. The preferred architecture is to route the guest through one proxy or mirror endpoint that can enforce a small domain allowlist and provide caches for package managers or container registries. This keeps outbound policy simple enough to operate while still allowing the guest to fetch what it needs.

## Resource limits

Each attempt should have explicit limits for CPU, memory, disk, process count, and wall-clock timeout. These limits protect the host from denial-of-service behavior and make failed attempts easier to reason about.

## Trusted validation gate

Validation is mandatory before publishing. The core applies the sandbox-produced patch to a fresh trusted clone and reruns deterministic checks there. This keeps the publishing decision tied to reproducible evidence instead of whatever state the sandbox happened to leave behind.

## Trusted GitHub publishing

GitHub credentials belong only to the trusted core process or its secret store. Once validation succeeds, the core can create a branch, push the validated changes, open a pull request, and store the resulting branch name and PR URL on the job. The sandbox never receives the credentials or performs the publish step itself.

## Safety summary

The architecture depends on these guarantees:

- no host secrets in the sandbox
- no host container runtime socket in the sandbox
- no arbitrary host filesystem access from the guest
- no publishing without trusted validation
- no unconstrained resource usage by an attempt
