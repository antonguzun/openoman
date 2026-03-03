use super::job::{ArtifactRef, JobId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    JobSubmitted {
        job_id: JobId,
    },
    JobAttemptStarted {
        job_id: JobId,
        attempt_id: u32,
    },
    ArtifactsCollected {
        job_id: JobId,
        attempt_id: u32,
        artifacts: Vec<ArtifactRef>,
    },
    ValidationSucceeded {
        job_id: JobId,
        attempt_id: u32,
    },
    ValidationFailed {
        job_id: JobId,
        attempt_id: u32,
        reason: String,
    },
    PullRequestCreated {
        job_id: JobId,
        attempt_id: u32,
        url: String,
    },
    JobSucceeded {
        job_id: JobId,
        attempt_id: u32,
    },
    JobFailed {
        job_id: JobId,
        attempt_id: u32,
        reason: String,
    },
}

impl JobEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::JobSubmitted { .. } => "job.submitted",
            Self::JobAttemptStarted { .. } => "job.attempt_started",
            Self::ArtifactsCollected { .. } => "job.artifacts_collected",
            Self::ValidationSucceeded { .. } => "job.validation_succeeded",
            Self::ValidationFailed { .. } => "job.validation_failed",
            Self::PullRequestCreated { .. } => "job.pr_created",
            Self::JobSucceeded { .. } => "job.succeeded",
            Self::JobFailed { .. } => "job.failed",
        }
    }
}
