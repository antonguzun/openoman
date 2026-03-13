# Control Plane API

This document describes the current HTTP API exposed by `openoman serve`.

For a machine-readable draft, see [openapi.yaml](/home/antonguzun/Work/personal/openoman/docs/api/openapi.yaml).

## Base URL and auth

- Default base URL: `http://127.0.0.1:8080`
- If `[server].auth_token` or `[server].auth_token_env` is set, send `Authorization: Bearer <token>`
- `GET /health` is public; all other endpoints require the bearer token when auth is enabled

Example:

```bash
curl -H 'Authorization: Bearer secret-token' http://127.0.0.1:8080/jobs
```

## Error format

Errors are returned as JSON:

```json
{"error":"job not found: job-123"}
```

Common statuses:

- `400` for invalid input or unsupported values
- `401` for missing or invalid bearer token
- `404` for unknown job IDs
- `409` when `POST /jobs/:id/run` returns a run-level failure
- `500` for internal failures

## Endpoints

### `GET /health`

Response:

```json
{"ok":true}
```

### `POST /jobs`

Creates a queued job.

Request body:

```json
{
  "repo": "https://github.com/acme/repo.git",
  "revision": "main",
  "instruction": "update README",
  "check_profile": "unit",
  "publish_policy": "on_validation_success"
}
```

Defaults:

- `check_profile`: `unit`
- `publish_policy`: `on_validation_success`

Response shape:

```json
{
  "job_id": "job-1741860000000",
  "repo_ref": "https://github.com/acme/repo.git",
  "repo_alias": null,
  "revision": "main",
  "instruction": "update README",
  "check_profile": "unit",
  "publish_policy": "on_validation_success",
  "state": "queued",
  "attempts": 0,
  "artifact_refs": [],
  "publish_warning": null,
  "publish_result": null
}
```

### `GET /jobs`

Returns all jobs in reverse insertion order.

Response: array of the same job objects returned by `POST /jobs` and `GET /jobs/:id`.

### `GET /jobs/:id`

Returns one job object.

### `POST /jobs/:id/run`

Runs the job using the existing trusted execution path.

Response:

```json
{
  "job_id": "job-1741860000000",
  "state": "succeeded",
  "failure_reason": null,
  "publish_warning": null
}
```

If execution reaches a run-level failure, the endpoint returns `409` with the same shape and a non-null `failure_reason`.

### `POST /jobs/:id/retry`

Creates a new queued job by copying the original job request fields.

Response: same job object shape as `POST /jobs`.

### `GET /jobs/:id/logs`

If sandbox logs exist, response shape is:

```json
{
  "job_id": "job-1741860000000",
  "source": "artifact",
  "contents": "sandbox log\n",
  "events": []
}
```

If only outbox events exist:

```json
{
  "job_id": "job-1741860000000",
  "source": "outbox",
  "contents": null,
  "events": [
    {
      "event_id": "job-1741860000000-submitted",
      "event_type": "job.submitted",
      "status": "pending"
    }
  ]
}
```

If neither exist, `source` is `none`.

### `GET /jobs/:id/artifacts`

Response:

```json
[
  {
    "artifact_ref": "sandbox.logs",
    "kind": "text/plain",
    "path": "/abs/path/to/logs.txt",
    "content_hash": "sha256:...",
    "size_bytes": 128
  }
]
```

### `GET /jobs/:id/result`

Response:

```json
{
  "job_id": "job-1741860000000",
  "result": "success",
  "publish_warning": null,
  "publish_result": {
    "branch_name": "openoman/job-1741860000000",
    "pull_request_url": "https://github.com/acme/repo/pull/123",
    "pull_request_number": 123
  }
}
```

`result` can currently be:

- `success`
- `failed`
- `canceled`
- `in_progress`

## Stability note

This is the current API contract, not a versioned compatibility promise yet. If adapters or a first-party UI start depending on it, the next step should be formal API versioning plus validation against [openapi.yaml](/home/antonguzun/Work/personal/openoman/docs/api/openapi.yaml).
