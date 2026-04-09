# API and launcher process split

This change splits the current all-in-one server shape into an unprivileged API process and a local launcher process that owns sandbox execution. The first implementation keeps the launcher protocol intentionally narrow and synchronous for background-worker dispatch so the HTTP contract stays unchanged while the process boundary lands safely.

## Progress

- [x] Create a launcher Unix-socket service that can answer health checks and execute a job by `job_id`.
- [x] Move the API worker from direct local `run_job()` calls to launcher RPC dispatch.
- [x] Add CLI entrypoints for `api`, `launcher`, `dev`, and `stack`.
- [x] Keep `serve` as a compatibility alias for the API role.
- [ ] Follow up by moving the legacy hidden `internal firecracker-net` helper behind the launcher boundary completely, so no API-facing code retains host-network privilege assumptions.

## Notes

The production path is `systemd`-managed two-process startup via `openoman stack ...`. The local development path is `openoman dev`, which supervises both children from one command. This patch does not implement `jailer`; it creates the process split needed for that future work.
