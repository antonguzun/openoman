use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{error, info, warn, Level};

use crate::service::{JobView, OperatorService, SubmitJobInput};

const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct ServerState {
    service: OperatorService,
}

pub(crate) async fn serve(service: OperatorService) -> Result<(), String> {
    let bind_addr = service.config().server.bind_addr;
    let launcher_service = service.clone();
    tokio::task::spawn_blocking(move || launcher_service.ensure_launcher_available())
        .await
        .map_err(|e| format!("launcher availability check join failed: {e}"))??;
    let worker = spawn_job_worker(service.clone());
    let app = build_router(service);
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| format!("failed to bind {}: {e}", bind_addr))?;

    info!(%bind_addr, "server listening");
    let result = axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"));
    worker.abort();
    let _ = worker.await;
    result
}

pub(crate) fn build_router(service: OperatorService) -> Router {
    let auth_token = service.config().server.auth_token.clone();
    let state = ServerState { service };
    let protected = Router::new()
        .route("/jobs", post(create_job).get(list_jobs))
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/run", post(run_job))
        .route("/jobs/{job_id}/retry", post(retry_job))
        .route("/jobs/{job_id}/logs", get(get_logs))
        .route("/jobs/{job_id}/artifacts", get(list_artifacts))
        .route("/jobs/{job_id}/result", get(get_result))
        .layer(middleware::from_fn(move |req, next| {
            let auth_token = auth_token.clone();
            auth_middleware(auth_token, req, next)
        }));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn create_job(
    State(state): State<ServerState>,
    Json(payload): Json<CreateJobRequest>,
) -> Response {
    respond(
        StatusCode::CREATED,
        state.service.submit_job(SubmitJobInput {
            repo: payload.repo,
            revision: payload.revision,
            instruction: payload.instruction,
            check_profile: payload.check_profile.unwrap_or_else(|| "unit".to_string()),
            publish_policy: payload
                .publish_policy
                .unwrap_or_else(|| "on_validation_success".to_string()),
        }),
    )
}

async fn list_jobs(State(state): State<ServerState>) -> Response {
    respond(StatusCode::OK, state.service.list_jobs())
}

async fn get_job(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    respond(StatusCode::OK, state.service.get_job(&job_id))
}

async fn retry_job(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    respond(StatusCode::CREATED, state.service.retry_job(&job_id))
}

async fn run_job(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    match state.service.get_job(&job_id) {
        Ok(job) => {
            let status = match job.state.as_str() {
                "succeeded" | "failed" | "canceled" => StatusCode::CONFLICT,
                _ => StatusCode::ACCEPTED,
            };
            (status, Json(json!(accepted_run_payload(&job)))).into_response()
        }
        Err(err) => respond::<serde_json::Value>(StatusCode::ACCEPTED, Err(err)),
    }
}

async fn get_logs(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    respond(StatusCode::OK, state.service.get_logs(&job_id))
}

async fn list_artifacts(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    respond(StatusCode::OK, state.service.list_artifacts(&job_id))
}

async fn get_result(State(state): State<ServerState>, Path(job_id): Path<String>) -> Response {
    respond(StatusCode::OK, state.service.get_result(&job_id))
}

fn respond<T>(success_status: StatusCode, result: Result<T, String>) -> Response
where
    T: serde::Serialize,
{
    match result {
        Ok(payload) => (success_status, Json(payload)).into_response(),
        Err(err) => {
            let status = status_for_error(&err);
            (status, Json(json!({ "error": err }))).into_response()
        }
    }
}

fn status_for_error(error: &str) -> StatusCode {
    if error.starts_with("job not found:") {
        return StatusCode::NOT_FOUND;
    }
    if error.contains("must not be empty")
        || error.contains("unsupported")
        || error.contains("invalid")
        || error.contains("required")
    {
        return StatusCode::BAD_REQUEST;
    }
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn auth_middleware(
    auth_token: Option<String>,
    headers: axum::extract::Request,
    next: Next,
) -> Response {
    if let Some(expected_token) = auth_token {
        let provided = bearer_token(headers.headers());
        if provided.as_deref() != Some(expected_token.as_str()) {
            warn!(
                method = %headers.method(),
                uri = %headers.uri(),
                "rejected request with missing or invalid bearer token"
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "missing or invalid bearer token" })),
            )
                .into_response();
        }
    }

    next.run(headers).await
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let header_value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = header_value.strip_prefix("Bearer ")?;
    if token.trim().is_empty() {
        None
    } else {
        Some(token.trim().to_string())
    }
}

fn spawn_job_worker(service: OperatorService) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("background job worker started");
        loop {
            match service.next_queued_job_id() {
                Ok(Some(job_id)) => {
                    info!(%job_id, "dispatching queued job");
                    let run_service = service.clone();
                    let job_id_for_task = job_id.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        run_service.dispatch_job_to_launcher(&job_id_for_task)
                    })
                    .await;
                    match result {
                        Ok(Ok(_)) => info!(%job_id, "queued job finished"),
                        Ok(Err(err)) => error!(%job_id, %err, "background job run failed"),
                        Err(err) => error!(%job_id, %err, "background worker join failed"),
                    }
                }
                Ok(None) => tokio::time::sleep(WORKER_POLL_INTERVAL).await,
                Err(err) => {
                    error!(%err, "background worker scan failed");
                    tokio::time::sleep(WORKER_POLL_INTERVAL).await;
                }
            }
        }
    })
}

fn accepted_run_payload(job: &JobView) -> AcceptedRunResponse {
    AcceptedRunResponse {
        job_id: job.job_id.clone(),
        state: job.state.clone(),
        status_url: format!("/jobs/{}", job.job_id),
        logs_url: format!("/jobs/{}/logs", job.job_id),
        result_url: format!("/jobs/{}/result", job.job_id),
    }
}

#[derive(Debug, Clone, Serialize)]
struct AcceptedRunResponse {
    job_id: String,
    state: String,
    status_url: String,
    logs_url: String,
    result_url: String,
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    repo: String,
    revision: String,
    instruction: String,
    check_profile: Option<String>,
    publish_policy: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr, fs, os::unix::fs::PermissionsExt, path::Path, process::Command as StdCommand,
        time::Duration,
    };

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::{config::AppConfig, launcher, service::OperatorService};

    fn write_agent(root: &Path) -> std::path::PathBuf {
        let script = root.join("agent.sh");
        fs::write(
            &script,
            r#"#!/usr/bin/env sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "codex-cli 0.0-test"
  exit 0
fi
if [ "${1:-}" != "exec" ]; then
  echo "unexpected invocation" >&2
  exit 2
fi
shift
workdir="."
report=""
instruction=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -C)
      workdir="$2"
      shift 2
      ;;
    -o)
      report="$2"
      shift 2
      ;;
    --dangerously-bypass-approvals-and-sandbox)
      shift 1
      ;;
    --color)
      shift 2
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
if [ -z "$report" ]; then
  echo "missing report path" >&2
  exit 3
fi
cd "$workdir"
printf "\n" >> README.md
printf "agent ok: %s\n" "$instruction" > "$report"
echo "sandbox log"
"#,
        )
        .expect("write agent");
        let mut perms = fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("set perms");
        script
    }

    fn init_fixture_repo(path: &Path) {
        fs::create_dir_all(path).expect("create fixture repo");
        git(path, ["init", "-b", "main"]);
        git(path, ["config", "user.name", "Open OMan"]);
        git(path, ["config", "user.email", "openoman@example.com"]);
        fs::write(path.join("README.md"), "initial content\n").expect("write readme");
        git(path, ["add", "README.md"]);
        git(path, ["commit", "-m", "initial"]);
    }

    fn git(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .expect("git status");
        assert!(status.success(), "git command failed");
    }

    fn write_config(root: &Path, auth_token: Option<&str>) -> std::path::PathBuf {
        let config_path = root.join("config.toml");
        let db_path = root.join("openoman.sqlite");
        let workspace_path = root.join("workspaces");
        let runtime_path = root.join("runtime");
        let launcher_socket = root.join("launcher.sock");
        let agent = write_agent(root);
        let server_auth = auth_token
            .map(|token| format!("\n[server]\nauth_token = \"{token}\"\n"))
            .unwrap_or_else(|| "\n[server]\nport = 18080\n".to_string());
        fs::write(
            &config_path,
            format!(
                "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"process\"\nruntime_dir = \"{}\"\nhost_risk_posture = \"already_isolated\"\n\n[agent]\nprovider = \"codex\"\nbin = \"{}\"\n\n[launcher]\nsocket_path = \"{}\"\n{}",
                db_path.display(),
                workspace_path.display(),
                runtime_path.display(),
                agent.display(),
                launcher_socket.display(),
                server_auth,
            ),
        )
        .expect("write config");
        config_path
    }

    async fn app(root: &Path, auth_token: Option<&str>) -> Router {
        let config = AppConfig::load(&write_config(root, auth_token)).expect("config");
        let service = OperatorService::for_api(config).expect("service");
        build_router(service)
    }

    async fn app_with_worker(
        root: &Path,
        auth_token: Option<&str>,
    ) -> (
        Router,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let config = AppConfig::load(&write_config(root, auth_token)).expect("config");
        let api_service = OperatorService::for_api(config.clone()).expect("api service");
        let launcher_service = OperatorService::for_launcher(config).expect("launcher service");
        let launcher_task = tokio::spawn(async move {
            let _ = launcher::serve(launcher_service).await;
        });
        for _ in 0..50 {
            let health_service = api_service.clone();
            let ready =
                tokio::task::spawn_blocking(move || health_service.ensure_launcher_available())
                    .await
                    .expect("launcher health join");
            if ready.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::task::spawn_blocking({
            let health_service = api_service.clone();
            move || health_service.ensure_launcher_available()
        })
        .await
        .expect("launcher health join")
        .expect("launcher ready");
        let worker = spawn_job_worker(api_service.clone());
        (build_router(api_service), worker, launcher_task)
    }

    async fn json_body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn health_endpoint_is_public() {
        let temp = TempDir::new().expect("tempdir");
        let response = app(temp.path(), Some("secret"))
            .await
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["ok"], true);
    }

    #[tokio::test]
    async fn protected_endpoints_require_token() {
        let temp = TempDir::new().expect("tempdir");
        let response = app(temp.path(), Some("secret"))
            .await
            .oneshot(
                Request::builder()
                    .uri("/jobs")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_and_list_jobs_over_http() {
        let temp = TempDir::new().expect("tempdir");
        let app = app(temp.path(), None).await;
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/jobs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"repo":"https://example.com/repo.git","revision":"main","instruction":"update readme"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(create.status(), StatusCode::CREATED);

        let created = json_body(create).await;
        let job_id = created["job_id"].as_str().expect("job id");

        let list = app
            .oneshot(
                Request::builder()
                    .uri("/jobs")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(list.status(), StatusCode::OK);
        let jobs = json_body(list).await;
        assert_eq!(jobs.as_array().expect("array").len(), 1);
        assert_eq!(jobs[0]["job_id"], job_id);
    }

    #[tokio::test]
    async fn retry_job_over_http_inherits_branch_identity() {
        let temp = TempDir::new().expect("tempdir");
        let app = app(temp.path(), None).await;
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/jobs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"repo":"https://example.com/repo.git","revision":"main","instruction":"update readme"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(create.status(), StatusCode::CREATED);
        let created = json_body(create).await;
        let job_id = created["job_id"].as_str().expect("job id");
        let branch_name = created["branch_name"]
            .as_str()
            .expect("branch name should be present");
        let commit_message = created["commit_message"]
            .as_str()
            .expect("commit message should be present");

        let retry = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/jobs/{job_id}/retry"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(retry.status(), StatusCode::CREATED);
        let retried = json_body(retry).await;
        assert_ne!(retried["job_id"], created["job_id"]);
        assert_eq!(retried["branch_name"], branch_name);
        assert_eq!(retried["commit_message"], commit_message);
    }

    #[tokio::test]
    async fn run_endpoint_returns_accepted_and_job_finishes_in_background() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);
        let (app, worker, launcher_task) = app_with_worker(temp.path(), None).await;
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/jobs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"repo":"{}","revision":"main","instruction":"update readme","publish_policy":"never"}}"#,
                        fixture_repo.display()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(create.status(), StatusCode::CREATED);
        let created = json_body(create).await;
        let job_id = created["job_id"].as_str().expect("job id").to_string();

        let run = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/jobs/{job_id}/run"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(run.status(), StatusCode::ACCEPTED);
        let accepted = json_body(run).await;
        assert_eq!(accepted["job_id"], job_id);
        assert_eq!(accepted["status_url"], format!("/jobs/{job_id}"));
        assert_eq!(accepted["logs_url"], format!("/jobs/{job_id}/logs"));
        assert_eq!(accepted["result_url"], format!("/jobs/{job_id}/result"));

        let mut final_state = None;
        for _ in 0..50 {
            let status = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/jobs/{job_id}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(status.status(), StatusCode::OK);
            let payload = json_body(status).await;
            let state = payload["state"].as_str().expect("state");
            if matches!(state, "succeeded" | "failed" | "canceled") {
                final_state = Some(state.to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert_eq!(final_state.as_deref(), Some("succeeded"));

        let result = app
            .oneshot(
                Request::builder()
                    .uri(format!("/jobs/{job_id}/result"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(result.status(), StatusCode::OK);
        let result = json_body(result).await;
        assert_eq!(result["result"], "success");
        assert_eq!(result["status"], "succeeded");
        assert!(result["branch_url"].is_null());
        assert!(result["merge_request_url"].is_null());

        worker.abort();
        launcher_task.abort();
    }

    #[tokio::test]
    async fn run_endpoint_rejects_terminal_jobs() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);
        let (app, worker, launcher_task) = app_with_worker(temp.path(), None).await;
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/jobs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"repo":"{}","revision":"main","instruction":"update readme"}}"#,
                        fixture_repo.display()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");
        let created = json_body(create).await;
        let job_id = created["job_id"].as_str().expect("job id").to_string();

        for _ in 0..50 {
            let status = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/jobs/{job_id}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            let payload = json_body(status).await;
            let state = payload["state"].as_str().expect("state");
            if matches!(state, "succeeded" | "failed" | "canceled") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let run = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/jobs/{job_id}/run"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(run.status(), StatusCode::CONFLICT);

        worker.abort();
        launcher_task.abort();
    }
}
