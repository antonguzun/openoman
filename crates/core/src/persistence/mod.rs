use std::{cell::RefCell, path::Path, rc::Rc};

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::domain::{
    job::{
        ArtifactRef, Attempt, Job, JobId, JobSnapshot, JobState, JobValueError, PublishResult,
        RepoRef, Revision,
    },
    plugin::{CheckProfile, PluginValueError, PublishPolicy},
};

#[derive(Debug)]
pub enum PersistenceError {
    Sql(rusqlite::Error),
    JobValue(JobValueError),
    PluginValue(PluginValueError),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sql(err) => write!(f, "sqlite error: {err}"),
            Self::JobValue(err) => write!(f, "job value error: {err}"),
            Self::PluginValue(err) => write!(f, "plugin value error: {err}"),
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<rusqlite::Error> for PersistenceError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<JobValueError> for PersistenceError {
    fn from(value: JobValueError) -> Self {
        Self::JobValue(value)
    }
}

impl From<PluginValueError> for PersistenceError {
    fn from(value: PluginValueError) -> Self {
        Self::PluginValue(value)
    }
}

#[derive(Clone)]
pub struct SqliteStore {
    conn: Rc<RefCell<Connection>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<(), PersistenceError> {
        let conn = self.conn.borrow();
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                repo_ref TEXT NOT NULL,
                repo_alias TEXT,
                revision TEXT NOT NULL,
                instruction TEXT NOT NULL DEFAULT '',
                check_profile TEXT NOT NULL,
                publish_policy TEXT NOT NULL,
                state TEXT NOT NULL,
                active_attempt_id INTEGER,
                validation_succeeded INTEGER NOT NULL,
                publish_branch_name TEXT,
                pull_request_url TEXT,
                pull_request_number INTEGER,
                publish_warning TEXT
            );

            CREATE TABLE IF NOT EXISTS attempts (
                job_id TEXT NOT NULL,
                attempt_id INTEGER NOT NULL,
                PRIMARY KEY (job_id, attempt_id),
                FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS job_artifact_refs (
                job_id TEXT NOT NULL,
                ref TEXT NOT NULL,
                PRIMARY KEY (job_id, ref),
                FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS outbox_events (
                event_id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                status TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS artifacts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                artifact_ref TEXT NOT NULL,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                size_bytes INTEGER NOT NULL
            );
            ",
        )?;

        let has_instruction = {
            let mut stmt = conn.prepare("PRAGMA table_info(jobs)")?;
            let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for column in columns {
                if column?.eq("instruction") {
                    found = true;
                    break;
                }
            }
            found
        };

        if !has_instruction {
            conn.execute(
                "ALTER TABLE jobs ADD COLUMN instruction TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }

        ensure_jobs_column(
            &conn,
            "publish_branch_name",
            "ALTER TABLE jobs ADD COLUMN publish_branch_name TEXT",
        )?;
        ensure_jobs_column(
            &conn,
            "pull_request_url",
            "ALTER TABLE jobs ADD COLUMN pull_request_url TEXT",
        )?;
        ensure_jobs_column(
            &conn,
            "pull_request_number",
            "ALTER TABLE jobs ADD COLUMN pull_request_number INTEGER",
        )?;
        ensure_jobs_column(
            &conn,
            "repo_alias",
            "ALTER TABLE jobs ADD COLUMN repo_alias TEXT",
        )?;
        ensure_jobs_column(
            &conn,
            "publish_warning",
            "ALTER TABLE jobs ADD COLUMN publish_warning TEXT",
        )?;

        Ok(())
    }

    pub fn jobs(&self) -> JobRepository {
        JobRepository {
            conn: Rc::clone(&self.conn),
        }
    }

    pub fn outbox(&self) -> OutboxRepository {
        OutboxRepository {
            conn: Rc::clone(&self.conn),
        }
    }

    pub fn artifacts(&self) -> ArtifactsRepository {
        ArtifactsRepository {
            conn: Rc::clone(&self.conn),
        }
    }

    pub fn update_job_and_insert_outbox(
        &self,
        job: &Job,
        event: &NewOutboxEvent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        upsert_job(&tx, job)?;
        insert_outbox_tx(&tx, event)?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_job_and_insert_artifacts(
        &self,
        job: &Job,
        artifacts: &[NewArtifactRecord],
    ) -> Result<(), PersistenceError> {
        self.update_job_with_related_records(job, artifacts, &[])
    }

    pub fn update_job_with_related_records(
        &self,
        job: &Job,
        artifacts: &[NewArtifactRecord],
        outbox_events: &[NewOutboxEvent],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        upsert_job(&tx, job)?;
        for artifact in artifacts {
            insert_artifact_tx(&tx, artifact)?;
        }
        for event in outbox_events {
            insert_outbox_tx(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub struct JobRepository {
    conn: Rc<RefCell<Connection>>,
}

impl JobRepository {
    pub fn create(&self, job: &Job) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        upsert_job(&tx, job)?;
        tx.commit()?;
        Ok(())
    }

    pub fn update(&self, job: &Job) -> Result<(), PersistenceError> {
        self.create(job)
    }

    pub fn load(&self, id: &JobId) -> Result<Option<Job>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, repo_ref, repo_alias, revision, instruction, check_profile, publish_policy, state, active_attempt_id, validation_succeeded,
                    publish_branch_name, pull_request_url, pull_request_number, publish_warning
             FROM jobs WHERE id = ?1",
        )?;

        let row = stmt
            .query_row(params![id.as_str()], |row| {
                let id: String = row.get(0)?;
                let repo_ref: String = row.get(1)?;
                let repo_alias: Option<String> = row.get(2)?;
                let revision: String = row.get(3)?;
                let instruction: String = row.get(4)?;
                let check_profile: String = row.get(5)?;
                let publish_policy: String = row.get(6)?;
                let state: String = row.get(7)?;
                let active_attempt_id: Option<u32> = row.get(8)?;
                let validation_succeeded: bool = row.get(9)?;
                let publish_branch_name: Option<String> = row.get(10)?;
                let pull_request_url: Option<String> = row.get(11)?;
                let pull_request_number: Option<u64> = row.get(12)?;
                let publish_warning: Option<String> = row.get(13)?;

                Ok((
                    id,
                    repo_ref,
                    repo_alias,
                    revision,
                    instruction,
                    check_profile,
                    publish_policy,
                    state,
                    active_attempt_id,
                    validation_succeeded,
                    publish_branch_name,
                    pull_request_url,
                    pull_request_number,
                    publish_warning,
                ))
            })
            .optional()?;

        let Some((
            id,
            repo_ref,
            repo_alias,
            revision,
            instruction,
            check_profile,
            publish_policy,
            state,
            active_attempt_id,
            validation_succeeded,
            publish_branch_name,
            pull_request_url,
            pull_request_number,
            publish_warning,
        )) = row
        else {
            return Ok(None);
        };

        let attempts = load_attempts(&conn, &id)?;
        let artifacts = load_artifact_refs(&conn, &id)?;

        let job = Job::rehydrate(JobSnapshot {
            id: JobId::new(id)?,
            repo_ref: RepoRef::new(repo_ref)?,
            repo_alias,
            revision: Revision::new(revision)?,
            instruction,
            check_profile: CheckProfile::new(check_profile)?,
            publish_policy: PublishPolicy::parse(&publish_policy)?,
            state: JobState::parse(&state)?,
            attempts,
            artifacts,
            publish_result: build_publish_result(
                publish_branch_name,
                pull_request_url,
                pull_request_number,
            )?,
            publish_warning,
            active_attempt_id,
            validation_succeeded,
        });

        Ok(Some(job))
    }
}

fn upsert_job(tx: &Transaction<'_>, job: &Job) -> Result<(), PersistenceError> {
    let publish_branch_name = job
        .publish_result
        .as_ref()
        .map(|result| result.branch_name.as_str());
    let pull_request_url = job
        .publish_result
        .as_ref()
        .map(|result| result.pull_request_url.as_str());
    let pull_request_number = job
        .publish_result
        .as_ref()
        .map(|result| result.pull_request_number);
    let repo_alias = job.repo_alias.as_deref();
    let publish_warning = job.publish_warning.as_deref();

    tx.execute(
        "INSERT INTO jobs(
            id, repo_ref, repo_alias, revision, instruction, check_profile, publish_policy, state,
            active_attempt_id, validation_succeeded, publish_branch_name, pull_request_url,
            pull_request_number, publish_warning
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
            repo_ref = excluded.repo_ref,
            repo_alias = excluded.repo_alias,
            revision = excluded.revision,
            instruction = excluded.instruction,
            check_profile = excluded.check_profile,
            publish_policy = excluded.publish_policy,
            state = excluded.state,
            active_attempt_id = excluded.active_attempt_id,
            validation_succeeded = excluded.validation_succeeded,
            publish_branch_name = excluded.publish_branch_name,
            pull_request_url = excluded.pull_request_url,
            pull_request_number = excluded.pull_request_number,
            publish_warning = excluded.publish_warning",
        params![
            job.id.as_str(),
            job.repo_ref.as_str(),
            repo_alias,
            job.revision.as_str(),
            job.instruction.as_str(),
            job.check_profile.as_str(),
            job.publish_policy.as_str(),
            job.state.as_str(),
            job.active_attempt_id(),
            job.validation_succeeded(),
            publish_branch_name,
            pull_request_url,
            pull_request_number,
            publish_warning,
        ],
    )?;

    tx.execute(
        "DELETE FROM attempts WHERE job_id = ?1",
        params![job.id.as_str()],
    )?;
    for attempt in &job.attempts {
        tx.execute(
            "INSERT INTO attempts(job_id, attempt_id) VALUES (?1, ?2)",
            params![job.id.as_str(), attempt.id],
        )?;
    }

    tx.execute(
        "DELETE FROM job_artifact_refs WHERE job_id = ?1",
        params![job.id.as_str()],
    )?;
    for artifact in &job.artifacts {
        tx.execute(
            "INSERT INTO job_artifact_refs(job_id, ref) VALUES (?1, ?2)",
            params![job.id.as_str(), artifact.as_str()],
        )?;
    }

    Ok(())
}

fn load_attempts(conn: &Connection, job_id: &str) -> Result<Vec<Attempt>, PersistenceError> {
    let mut stmt =
        conn.prepare("SELECT attempt_id FROM attempts WHERE job_id = ?1 ORDER BY attempt_id")?;
    let rows = stmt.query_map(params![job_id], |row| {
        Ok(Attempt {
            id: row.get::<_, u32>(0)?,
        })
    })?;

    let mut attempts = Vec::new();
    for row in rows {
        attempts.push(row?);
    }
    Ok(attempts)
}

fn load_artifact_refs(
    conn: &Connection,
    job_id: &str,
) -> Result<Vec<ArtifactRef>, PersistenceError> {
    let mut stmt =
        conn.prepare("SELECT ref FROM job_artifact_refs WHERE job_id = ?1 ORDER BY ref")?;
    let rows = stmt.query_map(params![job_id], |row| row.get::<_, String>(0))?;

    let mut refs = Vec::new();
    for row in rows {
        refs.push(ArtifactRef::new(row?)?);
    }
    Ok(refs)
}

fn ensure_jobs_column(
    conn: &Connection,
    column_name: &str,
    alter_sql: &str,
) -> Result<(), PersistenceError> {
    if has_jobs_column(conn, column_name)? {
        return Ok(());
    }

    conn.execute(alter_sql, [])?;
    Ok(())
}

fn has_jobs_column(conn: &Connection, column_name: &str) -> Result<bool, PersistenceError> {
    let mut stmt = conn.prepare("PRAGMA table_info(jobs)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column?.eq(column_name) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn build_publish_result(
    branch_name: Option<String>,
    pull_request_url: Option<String>,
    pull_request_number: Option<u64>,
) -> Result<Option<PublishResult>, PersistenceError> {
    match (branch_name, pull_request_url, pull_request_number) {
        (None, None, None) => Ok(None),
        (Some(branch_name), Some(pull_request_url), Some(pull_request_number)) => {
            Ok(Some(PublishResult {
                branch_name,
                pull_request_url,
                pull_request_number,
            }))
        }
        _ => Err(PersistenceError::Sql(
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "incomplete publish result columns in jobs table",
                )),
            ),
        )),
    }
}

pub struct OutboxRepository {
    conn: Rc<RefCell<Connection>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    Sent,
    Failed,
}

impl OutboxStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewOutboxEvent {
    pub event_id: String,
    pub job_id: String,
    pub event_type: String,
    pub payload: String,
    pub status: OutboxStatus,
}

#[derive(Debug, Clone)]
pub struct OutboxEventRecord {
    pub event_id: String,
    pub job_id: String,
    pub event_type: String,
    pub payload: String,
    pub status: OutboxStatus,
}

impl OutboxRepository {
    pub fn insert(&self, event: &NewOutboxEvent) -> Result<(), PersistenceError> {
        let conn = self.conn.borrow();
        insert_outbox_conn(&conn, event)
    }

    pub fn list_by_status(
        &self,
        status: OutboxStatus,
    ) -> Result<Vec<OutboxEventRecord>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT event_id, job_id, event_type, payload, status
             FROM outbox_events WHERE status = ?1 ORDER BY event_id",
        )?;
        let rows = stmt.query_map(params![status.as_str()], |row| {
            Ok(OutboxEventRecord {
                event_id: row.get(0)?,
                job_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: row.get(3)?,
                status: OutboxStatus::parse(&row.get::<_, String>(4)?),
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn list_by_job(&self, job_id: &str) -> Result<Vec<OutboxEventRecord>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT event_id, job_id, event_type, payload, status
             FROM outbox_events WHERE job_id = ?1 ORDER BY event_id",
        )?;
        let rows = stmt.query_map(params![job_id], |row| {
            Ok(OutboxEventRecord {
                event_id: row.get(0)?,
                job_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: row.get(3)?,
                status: OutboxStatus::parse(&row.get::<_, String>(4)?),
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn update_status(
        &self,
        event_id: &str,
        status: OutboxStatus,
    ) -> Result<(), PersistenceError> {
        let conn = self.conn.borrow();
        conn.execute(
            "UPDATE outbox_events SET status = ?2 WHERE event_id = ?1",
            params![event_id, status.as_str()],
        )?;
        Ok(())
    }
}

fn insert_outbox_tx(tx: &Transaction<'_>, event: &NewOutboxEvent) -> Result<(), PersistenceError> {
    tx.execute(
        "INSERT INTO outbox_events(event_id, job_id, event_type, payload, status)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            &event.event_id,
            &event.job_id,
            &event.event_type,
            &event.payload,
            event.status.as_str()
        ],
    )?;
    Ok(())
}

fn insert_outbox_conn(conn: &Connection, event: &NewOutboxEvent) -> Result<(), PersistenceError> {
    conn.execute(
        "INSERT INTO outbox_events(event_id, job_id, event_type, payload, status)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            &event.event_id,
            &event.job_id,
            &event.event_type,
            &event.payload,
            event.status.as_str()
        ],
    )?;
    Ok(())
}

pub struct ArtifactsRepository {
    conn: Rc<RefCell<Connection>>,
}

#[derive(Debug, Clone)]
pub struct NewArtifactRecord {
    pub job_id: String,
    pub artifact_ref: String,
    pub kind: String,
    pub path: String,
    pub content_hash: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct ArtifactRecord {
    pub artifact_ref: String,
    pub kind: String,
    pub path: String,
    pub content_hash: String,
    pub size_bytes: i64,
}

impl ArtifactsRepository {
    pub fn insert(&self, artifact: &NewArtifactRecord) -> Result<(), PersistenceError> {
        let conn = self.conn.borrow();
        insert_artifact_conn(&conn, artifact)?;
        Ok(())
    }

    pub fn list_by_job(&self, job_id: &str) -> Result<Vec<ArtifactRecord>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT artifact_ref, kind, path, content_hash, size_bytes
             FROM artifacts WHERE job_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![job_id], |row| {
            Ok(ArtifactRecord {
                artifact_ref: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                content_hash: row.get(3)?,
                size_bytes: row.get(4)?,
            })
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row?);
        }

        Ok(artifacts)
    }
}

fn insert_artifact_tx(
    tx: &Transaction<'_>,
    artifact: &NewArtifactRecord,
) -> Result<(), PersistenceError> {
    tx.execute(
        "INSERT INTO artifacts(job_id, artifact_ref, kind, path, content_hash, size_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &artifact.job_id,
            &artifact.artifact_ref,
            &artifact.kind,
            &artifact.path,
            &artifact.content_hash,
            artifact.size_bytes,
        ],
    )?;
    Ok(())
}

fn insert_artifact_conn(
    conn: &Connection,
    artifact: &NewArtifactRecord,
) -> Result<(), PersistenceError> {
    conn.execute(
        "INSERT INTO artifacts(job_id, artifact_ref, kind, path, content_hash, size_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &artifact.job_id,
            &artifact.artifact_ref,
            &artifact.kind,
            &artifact.path,
            &artifact.content_hash,
            artifact.size_bytes,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;
    use crate::domain::{job::RepoRef, plugin::PublishPolicy};

    fn submitted_job() -> Job {
        Job::submit(
            JobId::new("job-epic-2").expect("job id"),
            RepoRef::new("github.com/acme/repo").expect("repo ref"),
            None,
            Revision::new("main").expect("revision"),
            "persisted instruction".to_string(),
            CheckProfile::new("unit").expect("profile"),
            PublishPolicy::OnValidationSuccess,
        )
        .0
    }

    #[test]
    fn migration_creates_schema_on_clean_database() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");

        let conn = store.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='jobs'")
            .expect("query sqlite_master");

        let table_name: String = stmt
            .query_row([], |row| row.get(0))
            .expect("jobs table should exist");

        assert_eq!(table_name, "jobs");
    }

    #[test]
    fn create_update_and_reload_job_round_trips() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");
        let jobs = store.jobs();

        let mut job = submitted_job();
        jobs.create(&job).expect("insert job");

        job.start_attempt(1).expect("start attempt");
        job.collect_artifacts(vec![ArtifactRef::new("artifacts/report.txt").expect("ref")])
            .expect("collect artifacts");

        jobs.update(&job).expect("update job");

        let reloaded = jobs
            .load(&job.id)
            .expect("load job")
            .expect("job should exist");

        assert_eq!(reloaded.state, JobState::CollectingArtifacts);
        assert_eq!(reloaded.attempts.len(), 1);
        assert_eq!(reloaded.artifacts.len(), 1);
    }

    #[test]
    fn published_job_round_trips_publish_result() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");
        let jobs = store.jobs();

        let mut job = submitted_job();
        jobs.create(&job).expect("insert job");

        job.start_attempt(1).expect("start attempt");
        job.collect_artifacts(vec![ArtifactRef::new("artifacts/report.txt").expect("ref")])
            .expect("collect artifacts");
        job.start_validation().expect("start validation");
        job.mark_validation_succeeded()
            .expect("validation should succeed");
        job.mark_pull_request_created("openoman/job-epic-2", "https://example.test/pr/7", 7)
            .expect("store publish result");
        job.mark_succeeded().expect("job should succeed");

        jobs.update(&job).expect("update job");

        let reloaded = jobs
            .load(&job.id)
            .expect("load job")
            .expect("job should exist");
        assert_eq!(
            reloaded.publish_result,
            Some(PublishResult {
                branch_name: "openoman/job-epic-2".to_string(),
                pull_request_url: "https://example.test/pr/7".to_string(),
                pull_request_number: 7,
            })
        );
    }

    #[test]
    fn job_round_trips_repo_alias_and_publish_warning() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");
        let jobs = store.jobs();

        let mut job = submitted_job();
        job.repo_alias = Some("demo-alias".to_string());
        jobs.create(&job).expect("insert job");

        job.start_attempt(1).expect("start attempt");
        job.collect_artifacts(vec![ArtifactRef::new("artifacts/report.txt").expect("ref")])
            .expect("collect artifacts");
        job.start_validation().expect("start validation");
        job.mark_validation_succeeded()
            .expect("validation should succeed");
        job.mark_publish_skipped_with_warning("publishing skipped: missing token")
            .expect("skip with warning");
        job.mark_succeeded().expect("job should succeed");
        jobs.update(&job).expect("update job");

        let reloaded = jobs
            .load(&job.id)
            .expect("load job")
            .expect("job should exist");
        assert_eq!(reloaded.repo_alias.as_deref(), Some("demo-alias"));
        assert_eq!(
            reloaded.publish_warning.as_deref(),
            Some("publishing skipped: missing token")
        );
    }

    #[test]
    fn job_update_and_outbox_insert_are_atomic() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");
        let jobs = store.jobs();

        let mut job = submitted_job();
        jobs.create(&job).expect("create job");

        job.start_attempt(1).expect("start attempt");

        let event = NewOutboxEvent {
            event_id: "evt-1".to_string(),
            job_id: job.id.as_str().to_string(),
            event_type: "job.attempt_started".to_string(),
            payload: "{}".to_string(),
            status: OutboxStatus::Pending,
        };

        store
            .update_job_and_insert_outbox(&job, &event)
            .expect("transaction should commit");

        let reloaded = jobs
            .load(&job.id)
            .expect("load job")
            .expect("job should exist");
        assert_eq!(reloaded.state, JobState::Running);

        let pending = store
            .outbox()
            .list_by_status(OutboxStatus::Pending)
            .expect("query outbox");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_id, "evt-1");
    }

    #[test]
    fn job_artifact_and_outbox_updates_commit_atomically() {
        let db = NamedTempFile::new().expect("temp db");
        let store = SqliteStore::open(db.path()).expect("open store");
        let jobs = store.jobs();

        let mut job = submitted_job();
        jobs.create(&job).expect("create job");
        job.start_attempt(1).expect("start attempt");

        let artifact = NewArtifactRecord {
            job_id: job.id.as_str().to_string(),
            artifact_ref: "sandbox.patch".to_string(),
            kind: "sandbox.patch".to_string(),
            path: "/tmp/sandbox.patch".to_string(),
            content_hash: "abc".to_string(),
            size_bytes: 3,
        };
        let event = NewOutboxEvent {
            event_id: "evt-pr".to_string(),
            job_id: job.id.as_str().to_string(),
            event_type: "job.pr_created".to_string(),
            payload: "{}".to_string(),
            status: OutboxStatus::Pending,
        };

        store
            .update_job_with_related_records(&job, &[artifact], &[event])
            .expect("transaction should commit");

        let artifacts = store
            .artifacts()
            .list_by_job(job.id.as_str())
            .expect("artifacts should load");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_ref, "sandbox.patch");

        let events = store
            .outbox()
            .list_by_job(job.id.as_str())
            .expect("outbox should load");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "job.pr_created");
    }
}
