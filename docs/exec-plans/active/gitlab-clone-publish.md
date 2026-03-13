# GitLab Clone And Publish Support

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, a repository alias configured with `platform = "gitlab"` or `platform = "gitlab_self_hosted"` can be used end-to-end in OpenOMAN. The trusted host will be able to clone private GitLab repositories with the bound account token, and a successful `run` with `publish_policy = "on_validation_success"` will publish the validated patch to a branch and open a GitLab merge request. The existing CLI output remains compatible by storing and printing GitLab merge request metadata through the already existing `pull_request_*` fields.

The result is observable through tests and the CLI. A successful GitLab publish run should leave a pushed branch on the target remote, `openoman result <job_id>` should print a branch plus merge-request URL and number, and jobs missing GitLab credentials should still succeed with an explicit `publish_warning`.

## Progress

- [x] (2026-03-13 10:58Z) Reviewed current GitHub-only publish flow, repo alias config model, clone token resolution, and existing GitLab alias coverage.
- [x] (2026-03-13 11:08Z) Implemented provider-neutral trusted publish orchestration and a new GitLab publisher adapter in `crates/core`.
- [x] (2026-03-13 11:15Z) Extended CLI config resolution so GitLab aliases can clone with account tokens and resolve GitLab publish plans for both gitlab.com and self-hosted hosts.
- [x] (2026-03-13 11:19Z) Added unit and end-to-end tests for GitLab clone token use, merge-request creation, subgroup path inference, and publish warnings.
- [x] (2026-03-13 11:23Z) Updated `README.md` and `config.example.toml` with GitLab publish examples and compatibility notes.
- [x] (2026-03-13 11:27Z) Ran `cargo fmt --all`, `cargo test -p openoman-core`, and `cargo test -p openoman-cli --test cli_e2e`.

## Surprises & Discoveries

- Observation: repository aliases already accept `platform = "gitlab"` and `platform = "gitlab_self_hosted"`, but the current implementation uses those values only to skip publishing.
  Evidence: `crates/cli/src/config.rs` parses both values in `RepoPlatform::parse`, then `resolve_publish_plan_for_job` returns a warning for any non-GitHub platform.
- Observation: clone token injection is implemented in the shared git adapter through HTTPS `http.extraheader`, but the CLI currently withholds alias tokens unless the repo platform is GitHub.
  Evidence: `crates/core/src/git.rs` accepts `clone_token` for any HTTPS repo, while `AppConfig::resolve_clone_token_for_job` immediately returns `None` for non-GitHub aliases.
- Observation: repo-scoped alias publishing had no configurable `curl` binary, which made local fake API testing impossible for GitLab alias coverage.
  Evidence: `GitRepoConfig` originally exposed `api_base_url` and `push_url`, but only legacy `[publishing]` had `curl_bin`, while alias publish planning hardcoded `"curl"`.

## Decision Log

- Decision: keep `pull_request_url` and `pull_request_number` as the persisted and printed compatibility fields for GitLab merge requests.
  Rationale: this avoids a schema and CLI surface rename while still exposing the provider result to users and tests.
  Date/Author: 2026-03-13 / Codex
- Decision: support both `gitlab.com` and self-hosted GitLab in the same iteration.
  Rationale: the existing config already models both platforms, and the main additional requirement is provider-aware API base inference rather than a separate architecture path.
  Date/Author: 2026-03-13 / Codex
- Decision: keep legacy raw `[publishing]` resolution GitHub-only and add GitLab only to alias-based repo config.
  Rationale: raw publish flows already have a separate stable compatibility surface, while alias-based provider resolution contains the necessary account binding and repo identity data for GitLab.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

GitLab alias support is now end-to-end. Trusted clone token resolution works for GitLab aliases, successful validated jobs can publish a branch and create a merge request against either `gitlab.com` or a self-hosted GitLab instance, and the existing CLI result surface remains compatible by storing merge-request metadata in the existing `pull_request_*` fields. The implementation stayed additive: raw `[publishing]` remains GitHub-only, while alias-based publishing now chooses the correct provider from `[[git.repos]].platform`.

## Context and Orientation

Trusted run orchestration lives in `crates/core/src/application/mod.rs`. The `RunJobUseCase` prepares the trusted clone, runs the sandbox, writes a canonical patch, validates the job, and then branches into a publish step controlled by `PublishExecutionPlan`. Today that enum supports only `GitHub` and `SkipWithWarning`.

The existing GitHub publisher lives in `crates/core/src/github.rs`. It applies the canonical patch to the trusted clone, creates a branch, commits with the configured identity, pushes to the configured remote, and creates a pull request through the GitHub REST API by shelling out to `curl`. The same trust boundary should be preserved for GitLab.

CLI config loading and provider resolution live in `crates/cli/src/config.rs`. `AppConfig::resolve_clone_token_for_job` chooses the token used for authenticated clone, while `AppConfig::resolve_publish_plan_for_job` converts repo alias config into a runtime publish plan. Today the repo catalog stores GitHub-specific publish metadata and skips all non-GitHub providers.

End-to-end CLI coverage lives in `crates/cli/tests/cli_e2e.rs`. There is already coverage for repo aliases, env overlays, GitHub publishing, and a GitLab alias path that succeeds only because publishing is skipped with a warning.

## Plan of Work

First, add a new `crates/core/src/gitlab.rs` module that mirrors the structure of `github.rs` but targets GitLab merge requests. It must accept a `GitLabPublisherConfig`, apply the canonical patch to the trusted clone, create and push a branch, and call `POST /projects/:id/merge_requests` against the configured API base URL. It must support subgroup paths by URL-encoding the full `group/subgroup/project` path used in the route. The publisher will return the branch name, merge-request web URL, and merge-request internal number mapped into the existing publish-result fields.

Next, make the publish orchestration in `crates/core/src/application/mod.rs` provider-neutral by adding a GitLab variant to `PublishExecutionPlan` and dispatching to either `GitHubPublisher` or `GitLabPublisher`. The outbox payload and job persistence should continue using the existing publish result structure so the rest of the system stays stable.

Then, refactor `crates/cli/src/config.rs` so repo alias runtime config is not GitHub-specific. The runtime plan should support GitHub, GitLab cloud, and self-hosted GitLab. Clone token resolution should return the bound account token for any alias platform that uses token-authenticated HTTPS clone. GitLab publish planning should infer the project path and, for self-hosted repos, the host-based default API base URL from `push_url` first and then `repo_ref`, while still allowing explicit overrides.

After the config and core changes are in place, extend tests. Add config-unit tests for GitLab clone token resolution, GitLab publish plan inference, subgroup handling, and deterministic warnings or errors. Add core tests for the GitLab publisher, including branch push and merge-request API creation through a fake `curl` script. Update the CLI end-to-end suite so a GitLab alias can complete a trusted publish flow and `result` prints the stored branch and merge-request metadata.

Finally, update `README.md` and `config.example.toml` so users can discover the new GitLab behavior, see a working configuration shape, and understand that `pull_request_*` output now also carries GitLab merge request data.

## Concrete Steps

From the repository root:

    cargo fmt --all
    cargo test -p openoman-core
    cargo test -p openoman-cli --test cli_e2e

Expected observable signals after implementation:

    test publish_patch_pushes_branch_and_creates_merge_request ... ok
    test app_config_publish_plan_resolves_gitlab_self_hosted_repo ... ok
    test gitlab_alias_publishes_and_result_prints_merge_request_metadata ... ok

## Validation and Acceptance

Acceptance is met when the following are true.

For clone support, a repo alias configured with GitLab plus a bound account token can prepare the trusted workspace successfully, including when the repository is private and accessed over HTTPS.

For publish support, a successful run against a GitLab alias with `publish_policy = "on_validation_success"` creates a branch on the configured remote, opens a GitLab merge request through the trusted host publisher, persists the branch and merge-request metadata on the job, and prints that metadata through `openoman result <job_id>`.

For degraded behavior, a GitLab alias without a token must still complete the job successfully when validation passes, but the job result must include a `publish_warning` and no publish-result fields.

Automated validation consists of the core and CLI test commands listed above, with new tests failing before the change and passing after it.

## Idempotence and Recovery

The publish adapters must not persist credentials into `.git/config`. They should continue using one-shot git command arguments and HTTP headers so reruns start from a clean state. Existing databases must remain readable because the persisted publish-result schema is unchanged. Test remotes and fake API scripts should live under temporary directories so they can be recreated safely on each run.

## Artifacts and Notes

Expected `result` output shape after a GitLab-published job:

    job_id=job-123 result=success
    branch=openoman/job-123
    pull_request_number=17
    pull_request_url=https://gitlab.example.test/group/project/-/merge_requests/17

Expected warning shape when publishing is skipped due to missing GitLab credentials:

    publish_warning=publishing skipped: git account 'gitlab-account' has no token configured

## Interfaces and Dependencies

In `crates/core/src/gitlab.rs`, define:

    pub struct GitLabPublisherConfig {
        pub api_base_url: String,
        pub project_path: String,
        pub base_branch: String,
        pub branch_prefix: String,
        pub push_url: String,
        pub token: String,
        pub curl_bin: String,
        pub git_user_name: String,
        pub git_user_email: String,
    }

    pub struct PublishedMergeRequest {
        pub branch_name: String,
        pub pull_request_url: String,
        pub pull_request_number: u64,
    }

    pub struct GitLabPublisher { ... }

    impl GitLabPublisher {
        pub fn new(config: GitLabPublisherConfig) -> Self;
        pub fn publish_patch(
            &self,
            job_id: &str,
            instruction: &str,
            trusted_clone_dir: &Path,
            patch_path: &Path,
        ) -> Result<PublishedMergeRequest, GitLabPublishError>;
    }

In `crates/cli/src/config.rs`, define a runtime publish plan enum that supports both GitHub and GitLab providers and expose provider-specific runtime config structs as needed by `crates/cli/src/app.rs`.

Revision note (2026-03-13): Created this plan before implementation because GitLab support spans trusted publishing, config resolution, clone auth, tests, and user-facing documentation.
Revision note (2026-03-13): Updated after implementation to record the shipped provider-neutral publish path, the repo-scoped `curl_bin` addition, and the validation commands that passed.
