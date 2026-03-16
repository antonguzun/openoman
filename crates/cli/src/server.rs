use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::service::{OperatorService, RunJobView, SubmitJobInput};

#[derive(Clone)]
struct ServerState {
    service: OperatorService,
}

pub(crate) async fn serve(service: OperatorService) -> Result<(), String> {
    let bind_addr = service.config().server.bind_addr;
    let app = build_router(service);
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| format!("failed to bind {}: {e}", bind_addr))?;

    println!("server listening on http://{bind_addr}");
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"))
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
    let service = state.service.clone();
    let job_id_for_task = job_id.clone();
    let response = tokio::task::spawn_blocking(move || service.run_job(&job_id_for_task))
        .await
        .map_err(|e| format!("failed to join run task: {e}"))
        .and_then(|result| result);
    let status = match &response {
        Ok(RunJobView {
            failure_reason: Some(_),
            ..
        }) => StatusCode::CONFLICT,
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    respond(status, response)
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
    use std::{fs, path::Path};

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::{config::AppConfig, service::OperatorService};

    fn write_agent(root: &Path) -> std::path::PathBuf {
        let script = root.join("agent.sh");
        fs::write(
            &script,
            "#!/usr/bin/env sh\nprintf 'agent ok\\n' > \"$OPENOMAN_OUTPUT_DIR/report.txt\"\nprintf 'sandbox log\\n' > \"$OPENOMAN_OUTPUT_DIR/logs.txt\"\nexit 0\n",
        )
        .expect("write agent");
        let mut perms = fs::metadata(&script).expect("metadata").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("set perms");
        script
    }

    fn write_config(root: &Path, auth_token: Option<&str>) -> std::path::PathBuf {
        let config_path = root.join("config.toml");
        let db_path = root.join("openoman.sqlite");
        let workspace_path = root.join("workspaces");
        let runtime_path = root.join("runtime");
        let agent = write_agent(root);
        let server_auth = auth_token
            .map(|token| format!("\n[server]\nauth_token = \"{token}\"\n"))
            .unwrap_or_else(|| "\n[server]\nport = 18080\n".to_string());
        fs::write(
            &config_path,
            format!(
                "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"process\"\nruntime_dir = \"{}\"\nhost_risk_posture = \"already_isolated\"\n\n[agent]\nprovider = \"codex\"\nbin = \"{}\"\n{}",
                db_path.display(),
                workspace_path.display(),
                runtime_path.display(),
                agent.display(),
                server_auth,
            ),
        )
        .expect("write config");
        config_path
    }

    async fn app(root: &Path, auth_token: Option<&str>) -> Router {
        let config = AppConfig::load(&write_config(root, auth_token)).expect("config");
        let service = OperatorService::new(config).expect("service");
        build_router(service)
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
}
