use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream as StdUnixStream,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

use crate::service::{OperatorService, RunJobView};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum LauncherRequest {
    Health,
    RunJob { job_id: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum LauncherResponse {
    Health {
        ok: bool,
        error: Option<String>,
    },
    RunJob {
        ok: bool,
        run: Option<RunJobView>,
        error: Option<String>,
    },
}

pub(crate) async fn serve(service: OperatorService) -> Result<(), String> {
    let socket_path = service.config().launcher.socket_path.clone();
    prepare_socket_path(&socket_path)?;
    let listener = UnixListener::bind(&socket_path).map_err(|e| {
        format!(
            "failed to bind launcher socket {}: {e}",
            socket_path.display()
        )
    })?;
    println!("launcher listening on {}", socket_path.display());

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("launcher accept failed: {e}"))?;
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(service, stream).await {
                eprintln!("launcher connection failed: {err}");
            }
        });
    }
}

pub(crate) fn health(socket_path: &Path) -> Result<(), String> {
    match send_request(socket_path, &LauncherRequest::Health)? {
        LauncherResponse::Health {
            ok: true,
            error: None,
        } => Ok(()),
        LauncherResponse::Health { error, .. } => {
            Err(error
                .unwrap_or_else(|| "launcher health check failed without an error".to_string()))
        }
        other => Err(format!("unexpected launcher response: {other:?}")),
    }
}

pub(crate) fn run_job(socket_path: &Path, job_id: &str) -> Result<RunJobView, String> {
    match send_request(
        socket_path,
        &LauncherRequest::RunJob {
            job_id: job_id.to_string(),
        },
    )? {
        LauncherResponse::RunJob {
            ok: true,
            run: Some(run),
            error: None,
        } => Ok(run),
        LauncherResponse::RunJob { error, .. } => {
            Err(error.unwrap_or_else(|| "launcher run failed without an error".to_string()))
        }
        other => Err(format!("unexpected launcher response: {other:?}")),
    }
}

pub(crate) fn default_socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("openoman-launcher.sock")
}

fn prepare_socket_path(socket_path: &Path) -> Result<(), String> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "failed to create launcher socket directory {}: {e}",
                parent.display()
            )
        })?;
    }
    if socket_path.exists() {
        fs::remove_file(socket_path).map_err(|e| {
            format!(
                "failed to remove stale launcher socket {}: {e}",
                socket_path.display()
            )
        })?;
    }
    Ok(())
}

fn send_request(socket_path: &Path, request: &LauncherRequest) -> Result<LauncherResponse, String> {
    let mut stream = StdUnixStream::connect(socket_path).map_err(|e| {
        format!(
            "failed to connect to launcher socket {}: {e}",
            socket_path.display()
        )
    })?;
    let payload = serde_json::to_vec(request)
        .map_err(|e| format!("failed to encode launcher request: {e}"))?;
    stream
        .write_all(&payload)
        .map_err(|e| format!("failed to write launcher request: {e}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("failed to finish launcher request: {e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("failed to read launcher response: {e}"))?;
    serde_json::from_slice(&response)
        .map_err(|e| format!("failed to decode launcher response: {e}"))
}

async fn handle_connection(service: OperatorService, mut stream: UnixStream) -> Result<(), String> {
    let mut payload = Vec::new();
    stream
        .read_to_end(&mut payload)
        .await
        .map_err(|e| format!("failed to read launcher request: {e}"))?;
    let request: LauncherRequest = serde_json::from_slice(&payload)
        .map_err(|e| format!("invalid launcher request payload: {e}"))?;
    let response = match request {
        LauncherRequest::Health => LauncherResponse::Health {
            ok: true,
            error: None,
        },
        LauncherRequest::RunJob { job_id } => {
            let result = tokio::task::spawn_blocking(move || service.run_job(&job_id))
                .await
                .map_err(|e| format!("launcher worker join failed: {e}"))?;
            match result {
                Ok(run) => LauncherResponse::RunJob {
                    ok: true,
                    run: Some(run),
                    error: None,
                },
                Err(err) => LauncherResponse::RunJob {
                    ok: false,
                    run: None,
                    error: Some(err),
                },
            }
        }
    };
    let encoded = serde_json::to_vec(&response)
        .map_err(|e| format!("failed to encode launcher response: {e}"))?;
    stream
        .write_all(&encoded)
        .await
        .map_err(|e| format!("failed to write launcher response: {e}"))?;
    Ok(())
}
