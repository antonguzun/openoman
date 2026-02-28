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
