use std::fmt::{Display, Formatter};

use super::{
    events::JobEvent,
    request::{CheckProfile, PublishPolicy},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    pub fn new(value: impl Into<String>) -> Result<Self, JobValueError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JobValueError::EmptyJobId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef(String);

impl RepoRef {
    pub fn new(value: impl Into<String>) -> Result<Self, JobValueError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JobValueError::EmptyRepoRef);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision(String);

impl Revision {
    pub fn new(value: impl Into<String>) -> Result<Self, JobValueError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JobValueError::EmptyRevision);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef(String);

impl ArtifactRef {
    pub fn new(value: impl Into<String>) -> Result<Self, JobValueError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JobValueError::EmptyArtifactRef);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    CollectingArtifacts,
    Validating,
    Publishing,
    Notifying,
    Succeeded,
    Failed,
    Canceled,
}

impl JobState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::CollectingArtifacts => "collecting_artifacts",
            Self::Validating => "validating",
            Self::Publishing => "publishing",
            Self::Notifying => "notifying",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(value: &str) -> Result<Self, JobValueError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "collecting_artifacts" => Ok(Self::CollectingArtifacts),
            "validating" => Ok(Self::Validating),
            "publishing" => Ok(Self::Publishing),
            "notifying" => Ok(Self::Notifying),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            _ => Err(JobValueError::InvalidJobState(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub branch_name: String,
    pub pull_request_url: String,
    pub pull_request_number: u64,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub repo_ref: RepoRef,
    pub repo_alias: Option<String>,
    pub branch_name: Option<String>,
    pub commit_message: Option<String>,
    pub revision: Revision,
    pub instruction: String,
    pub check_profile: CheckProfile,
    pub publish_policy: PublishPolicy,
    pub state: JobState,
    pub attempts: Vec<Attempt>,
    pub artifacts: Vec<ArtifactRef>,
    pub publish_result: Option<PublishResult>,
    pub publish_warning: Option<String>,
    active_attempt_id: Option<u32>,
    validation_succeeded: bool,
}

#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: JobId,
    pub repo_ref: RepoRef,
    pub repo_alias: Option<String>,
    pub branch_name: Option<String>,
    pub commit_message: Option<String>,
    pub revision: Revision,
    pub instruction: String,
    pub check_profile: CheckProfile,
    pub publish_policy: PublishPolicy,
    pub state: JobState,
    pub attempts: Vec<Attempt>,
    pub artifacts: Vec<ArtifactRef>,
    pub publish_result: Option<PublishResult>,
    pub publish_warning: Option<String>,
    pub active_attempt_id: Option<u32>,
    pub validation_succeeded: bool,
}

impl Job {
    pub fn submit(
        id: JobId,
        repo_ref: RepoRef,
        repo_alias: Option<String>,
        branch_name: Option<String>,
        commit_message: Option<String>,
        revision: Revision,
        instruction: String,
        check_profile: CheckProfile,
        publish_policy: PublishPolicy,
    ) -> Result<(Self, JobEvent), JobError> {
        let job = Self {
            id: id.clone(),
            repo_ref,
            repo_alias,
            branch_name: normalize_optional_job_text(branch_name, JobError::EmptyBranchName)?,
            commit_message: normalize_optional_job_text(
                commit_message,
                JobError::EmptyCommitMessage,
            )?,
            revision,
            instruction,
            check_profile,
            publish_policy,
            state: JobState::Queued,
            attempts: vec![],
            artifacts: vec![],
            publish_result: None,
            publish_warning: None,
            active_attempt_id: None,
            validation_succeeded: false,
        };

        Ok((job, JobEvent::JobSubmitted { job_id: id }))
    }

    pub fn rehydrate(snapshot: JobSnapshot) -> Self {
        Self {
            id: snapshot.id,
            repo_ref: snapshot.repo_ref,
            repo_alias: snapshot.repo_alias,
            branch_name: snapshot.branch_name,
            commit_message: snapshot.commit_message,
            revision: snapshot.revision,
            instruction: snapshot.instruction,
            check_profile: snapshot.check_profile,
            publish_policy: snapshot.publish_policy,
            state: snapshot.state,
            attempts: snapshot.attempts,
            artifacts: snapshot.artifacts,
            publish_result: snapshot.publish_result,
            publish_warning: snapshot.publish_warning,
            active_attempt_id: snapshot.active_attempt_id,
            validation_succeeded: snapshot.validation_succeeded,
        }
    }

    pub fn ensure_git_naming(
        &mut self,
        branch_name: impl Into<String>,
        commit_message: impl Into<String>,
    ) -> Result<(), JobError> {
        let branch_name =
            normalize_required_job_text(branch_name.into(), JobError::EmptyBranchName)?;
        let commit_message =
            normalize_required_job_text(commit_message.into(), JobError::EmptyCommitMessage)?;
        self.branch_name = Some(branch_name);
        self.commit_message = Some(commit_message);
        Ok(())
    }

    pub fn active_attempt_id(&self) -> Option<u32> {
        self.active_attempt_id
    }

    pub fn validation_succeeded(&self) -> bool {
        self.validation_succeeded
    }

    pub fn start_attempt(&mut self, attempt_id: u32) -> Result<JobEvent, JobError> {
        if self.state.is_terminal() {
            return Err(JobError::TerminalJob);
        }
        if self.active_attempt_id.is_some() {
            return Err(JobError::ActiveAttemptExists);
        }
        self.active_attempt_id = Some(attempt_id);
        self.attempts.push(Attempt { id: attempt_id });
        self.state = JobState::Running;

        Ok(JobEvent::JobAttemptStarted {
            job_id: self.id.clone(),
            attempt_id,
        })
    }

    pub fn collect_artifacts(&mut self, artifacts: Vec<ArtifactRef>) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state != JobState::Running {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::CollectingArtifacts,
            });
        }

        self.state = JobState::CollectingArtifacts;
        self.artifacts = artifacts.clone();

        Ok(JobEvent::ArtifactsCollected {
            job_id: self.id.clone(),
            attempt_id,
            artifacts,
        })
    }

    pub fn start_validation(&mut self) -> Result<(), JobError> {
        if self.state != JobState::CollectingArtifacts {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Validating,
            });
        }

        self.state = JobState::Validating;
        Ok(())
    }

    pub fn mark_validation_succeeded(&mut self) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state != JobState::Validating {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Publishing,
            });
        }

        self.validation_succeeded = true;
        self.state = JobState::Publishing;

        Ok(JobEvent::ValidationSucceeded {
            job_id: self.id.clone(),
            attempt_id,
        })
    }

    pub fn mark_validation_failed(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state != JobState::Validating {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Failed,
            });
        }

        let reason = reason.into();
        self.state = JobState::Failed;
        self.active_attempt_id = None;

        Ok(JobEvent::ValidationFailed {
            job_id: self.id.clone(),
            attempt_id,
            reason,
        })
    }

    pub fn mark_pull_request_created(
        &mut self,
        branch_name: impl Into<String>,
        url: impl Into<String>,
        pull_request_number: u64,
    ) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state != JobState::Publishing {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Notifying,
            });
        }
        if !self.validation_succeeded {
            return Err(JobError::PublishRequiresValidationSuccess);
        }

        self.publish_result = Some(PublishResult {
            branch_name: branch_name.into(),
            pull_request_url: url.into(),
            pull_request_number,
        });
        self.publish_warning = None;
        self.state = JobState::Notifying;

        Ok(JobEvent::PullRequestCreated {
            job_id: self.id.clone(),
            attempt_id,
            url: self
                .publish_result
                .as_ref()
                .expect("publish result stored before event creation")
                .pull_request_url
                .clone(),
        })
    }

    pub fn mark_publish_skipped(&mut self) -> Result<(), JobError> {
        if self.state != JobState::Publishing {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Notifying,
            });
        }
        if !self.validation_succeeded {
            return Err(JobError::PublishRequiresValidationSuccess);
        }
        if self.publish_policy != PublishPolicy::Never {
            return Err(JobError::PublishSkipRequiresNeverPolicy);
        }

        self.publish_warning = None;
        self.state = JobState::Notifying;
        Ok(())
    }

    pub fn mark_publish_skipped_with_warning(
        &mut self,
        warning: impl Into<String>,
    ) -> Result<(), JobError> {
        if self.state != JobState::Publishing {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Notifying,
            });
        }
        if !self.validation_succeeded {
            return Err(JobError::PublishRequiresValidationSuccess);
        }

        let warning = warning.into();
        let warning = warning.trim();
        if warning.is_empty() {
            return Err(JobError::EmptyPublishWarning);
        }

        self.publish_warning = Some(warning.to_string());
        self.state = JobState::Notifying;
        Ok(())
    }

    pub fn mark_succeeded(&mut self) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state != JobState::Notifying {
            return Err(JobError::InvalidTransition {
                from: self.state.clone(),
                to: JobState::Succeeded,
            });
        }

        self.state = JobState::Succeeded;
        self.active_attempt_id = None;

        Ok(JobEvent::JobSucceeded {
            job_id: self.id.clone(),
            attempt_id,
        })
    }

    pub fn mark_failed(&mut self, reason: impl Into<String>) -> Result<JobEvent, JobError> {
        let attempt_id = self.active_attempt_id_or_error()?;
        if self.state.is_terminal() {
            return Err(JobError::TerminalJob);
        }

        self.state = JobState::Failed;
        self.active_attempt_id = None;

        Ok(JobEvent::JobFailed {
            job_id: self.id.clone(),
            attempt_id,
            reason: reason.into(),
        })
    }

    fn active_attempt_id_or_error(&self) -> Result<u32, JobError> {
        self.active_attempt_id.ok_or(JobError::NoActiveAttempt)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobValueError {
    EmptyJobId,
    EmptyRepoRef,
    EmptyRevision,
    EmptyArtifactRef,
    InvalidJobState(String),
}

impl Display for JobValueError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyJobId => write!(f, "job id must not be empty"),
            Self::EmptyRepoRef => write!(f, "repo ref must not be empty"),
            Self::EmptyRevision => write!(f, "revision must not be empty"),
            Self::EmptyArtifactRef => write!(f, "artifact ref must not be empty"),
            Self::InvalidJobState(value) => write!(f, "invalid job state: {value}"),
        }
    }
}

impl std::error::Error for JobValueError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    InvalidTransition { from: JobState, to: JobState },
    ActiveAttemptExists,
    NoActiveAttempt,
    PublishRequiresValidationSuccess,
    PublishSkipRequiresNeverPolicy,
    EmptyBranchName,
    EmptyCommitMessage,
    EmptyPublishWarning,
    TerminalJob,
}

impl Display for JobError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid transition from {from:?} to {to:?}")
            }
            Self::ActiveAttemptExists => write!(f, "job already has an active attempt"),
            Self::NoActiveAttempt => write!(f, "job does not have an active attempt"),
            Self::PublishRequiresValidationSuccess => {
                write!(f, "publishing requires successful validation")
            }
            Self::PublishSkipRequiresNeverPolicy => {
                write!(f, "publish skipping requires publish policy never")
            }
            Self::EmptyBranchName => write!(f, "branch name must not be empty"),
            Self::EmptyCommitMessage => write!(f, "commit message must not be empty"),
            Self::EmptyPublishWarning => write!(f, "publish warning must not be empty"),
            Self::TerminalJob => write!(f, "job is in a terminal state"),
        }
    }
}

impl std::error::Error for JobError {}

fn normalize_optional_job_text(
    value: Option<String>,
    empty_error: JobError,
) -> Result<Option<String>, JobError> {
    value
        .map(|raw| normalize_required_job_text(raw, empty_error.clone()))
        .transpose()
}

fn normalize_required_job_text(value: String, empty_error: JobError) -> Result<String, JobError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(empty_error);
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_job() -> Job {
        Job::submit(
            JobId::new("job-1").expect("valid id"),
            RepoRef::new("github.com/acme/repo").expect("valid repo"),
            None,
            Some("openoman/job-1".to_string()),
            Some("OpenOMAN job job-1".to_string()),
            Revision::new("main").expect("valid revision"),
            "test instruction".to_string(),
            CheckProfile::new("unit").expect("valid check profile"),
            PublishPolicy::OnValidationSuccess,
        )
        .expect("submit should succeed")
        .0
    }

    #[test]
    fn follows_happy_path_and_emits_events() {
        let mut job = new_job();

        let start = job.start_attempt(1).expect("attempt should start");
        assert!(matches!(start, JobEvent::JobAttemptStarted { .. }));

        let artifacts_event = job
            .collect_artifacts(vec![
                ArtifactRef::new("artifacts/patch.diff").expect("valid ref")
            ])
            .expect("collects artifacts");
        assert!(matches!(
            artifacts_event,
            JobEvent::ArtifactsCollected { .. }
        ));

        job.start_validation().expect("validation can start");

        let validation = job
            .mark_validation_succeeded()
            .expect("validation succeeds");
        assert!(matches!(validation, JobEvent::ValidationSucceeded { .. }));

        let pr = job
            .mark_pull_request_created("openoman/job-1", "https://example.test/pr/1", 1)
            .expect("pr can be created");
        assert!(matches!(pr, JobEvent::PullRequestCreated { .. }));
        assert_eq!(
            job.publish_result,
            Some(PublishResult {
                branch_name: "openoman/job-1".to_string(),
                pull_request_url: "https://example.test/pr/1".to_string(),
                pull_request_number: 1,
            })
        );

        let done = job.mark_succeeded().expect("job can succeed");
        assert!(matches!(done, JobEvent::JobSucceeded { .. }));
        assert_eq!(job.state, JobState::Succeeded);
    }

    #[test]
    fn refuses_invalid_transition() {
        let mut job = new_job();

        let err = job
            .collect_artifacts(vec![ArtifactRef::new("a").expect("valid ref")])
            .expect_err("collecting before running should fail");

        assert!(matches!(err, JobError::NoActiveAttempt));
    }

    #[test]
    fn enforces_one_active_attempt() {
        let mut job = new_job();
        job.start_attempt(1).expect("first attempt starts");

        let err = job
            .start_attempt(2)
            .expect_err("second active attempt must fail");

        assert_eq!(err, JobError::ActiveAttemptExists);
    }

    #[test]
    fn publishing_requires_validation_success() {
        let mut job = new_job();
        job.start_attempt(1).expect("attempt should start");

        let err = job
            .mark_pull_request_created("openoman/job-1", "https://example.test/pr/1", 1)
            .expect_err("cannot publish before validation");

        assert!(matches!(
            err,
            JobError::InvalidTransition {
                from: JobState::Running,
                to: JobState::Notifying
            }
        ));
    }

    #[test]
    fn publish_policy_never_can_skip_pull_request_creation() {
        let mut job = Job::submit(
            JobId::new("job-never").expect("valid id"),
            RepoRef::new("github.com/acme/repo").expect("valid repo"),
            None,
            Some("openoman/job-never".to_string()),
            Some("OpenOMAN job job-never".to_string()),
            Revision::new("main").expect("valid revision"),
            "test instruction".to_string(),
            CheckProfile::new("unit").expect("valid check profile"),
            PublishPolicy::Never,
        )
        .expect("submit should succeed")
        .0;

        job.start_attempt(1).expect("attempt should start");
        job.collect_artifacts(vec![
            ArtifactRef::new("artifacts/patch.diff").expect("valid ref")
        ])
        .expect("collects artifacts");
        job.start_validation().expect("validation can start");
        job.mark_validation_succeeded()
            .expect("validation succeeds");

        job.mark_publish_skipped()
            .expect("publish can be skipped for never policy");
        job.mark_succeeded().expect("job can succeed");

        assert_eq!(job.state, JobState::Succeeded);
        assert!(job.publish_result.is_none());
    }

    #[test]
    fn publish_policy_on_validation_success_can_skip_with_warning() {
        let mut job = new_job();
        job.start_attempt(1).expect("attempt should start");
        job.collect_artifacts(vec![
            ArtifactRef::new("artifacts/patch.diff").expect("valid ref")
        ])
        .expect("collects artifacts");
        job.start_validation().expect("validation can start");
        job.mark_validation_succeeded()
            .expect("validation succeeds");

        job.mark_publish_skipped_with_warning("missing publish token")
            .expect("publish can be skipped with warning");
        job.mark_succeeded().expect("job can succeed");

        assert_eq!(job.state, JobState::Succeeded);
        assert_eq!(
            job.publish_warning.as_deref(),
            Some("missing publish token")
        );
        assert!(job.publish_result.is_none());
    }

    #[test]
    fn ensure_git_naming_populates_missing_fields() {
        let mut job = Job::submit(
            JobId::new("job-missing-naming").expect("valid id"),
            RepoRef::new("github.com/acme/repo").expect("valid repo"),
            None,
            None,
            None,
            Revision::new("main").expect("valid revision"),
            "test instruction".to_string(),
            CheckProfile::new("unit").expect("valid check profile"),
            PublishPolicy::OnValidationSuccess,
        )
        .expect("submit should succeed")
        .0;

        job.ensure_git_naming("topic/job-missing-naming", "Refresh docs")
            .expect("naming should be stored");

        assert_eq!(job.branch_name.as_deref(), Some("topic/job-missing-naming"));
        assert_eq!(job.commit_message.as_deref(), Some("Refresh docs"));
    }
}
