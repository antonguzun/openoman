use std::fmt::{Display, Formatter};

use super::{
    events::JobEvent,
    plugin::{CheckProfile, PublishPolicy},
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

#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub repo_ref: RepoRef,
    pub revision: Revision,
    pub check_profile: CheckProfile,
    pub publish_policy: PublishPolicy,
    pub state: JobState,
    pub attempts: Vec<Attempt>,
    pub artifacts: Vec<ArtifactRef>,
    active_attempt_id: Option<u32>,
    validation_succeeded: bool,
}

#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: JobId,
    pub repo_ref: RepoRef,
    pub revision: Revision,
    pub check_profile: CheckProfile,
    pub publish_policy: PublishPolicy,
    pub state: JobState,
    pub attempts: Vec<Attempt>,
    pub artifacts: Vec<ArtifactRef>,
    pub active_attempt_id: Option<u32>,
    pub validation_succeeded: bool,
}

impl Job {
    pub fn submit(
        id: JobId,
        repo_ref: RepoRef,
        revision: Revision,
        check_profile: CheckProfile,
        publish_policy: PublishPolicy,
    ) -> (Self, JobEvent) {
        let job = Self {
            id: id.clone(),
            repo_ref,
            revision,
            check_profile,
            publish_policy,
            state: JobState::Queued,
            attempts: vec![],
            artifacts: vec![],
            active_attempt_id: None,
            validation_succeeded: false,
        };

        (job, JobEvent::JobSubmitted { job_id: id })
    }

    pub fn rehydrate(snapshot: JobSnapshot) -> Self {
        Self {
            id: snapshot.id,
            repo_ref: snapshot.repo_ref,
            revision: snapshot.revision,
            check_profile: snapshot.check_profile,
            publish_policy: snapshot.publish_policy,
            state: snapshot.state,
            attempts: snapshot.attempts,
            artifacts: snapshot.artifacts,
            active_attempt_id: snapshot.active_attempt_id,
            validation_succeeded: snapshot.validation_succeeded,
        }
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
        url: impl Into<String>,
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

        self.state = JobState::Notifying;

        Ok(JobEvent::PullRequestCreated {
            job_id: self.id.clone(),
            attempt_id,
            url: url.into(),
        })
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
            Self::TerminalJob => write!(f, "job is in a terminal state"),
        }
    }
}

impl std::error::Error for JobError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_job() -> Job {
        Job::submit(
            JobId::new("job-1").expect("valid id"),
            RepoRef::new("github.com/acme/repo").expect("valid repo"),
            Revision::new("main").expect("valid revision"),
            CheckProfile::new("unit").expect("valid check profile"),
            PublishPolicy::OnValidationSuccess,
        )
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
            .mark_pull_request_created("https://example.test/pr/1")
            .expect("pr can be created");
        assert!(matches!(pr, JobEvent::PullRequestCreated { .. }));

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
            .mark_pull_request_created("https://example.test/pr/1")
            .expect_err("cannot publish before validation");

        assert!(matches!(
            err,
            JobError::InvalidTransition {
                from: JobState::Running,
                to: JobState::Notifying
            }
        ));
    }
}
