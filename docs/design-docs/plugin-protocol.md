# Plugin Protocol

Plugins are reserved for post-MVP work, but their interface is intentionally frozen early so the trusted core can emit stable integration events and future plugin authors know what contract to target.

## Plugin model

Plugins are out-of-process executables discovered by a manifest file such as `plugin.yaml`. A manifest declares:

- plugin name and version
- entrypoint command array
- declared capabilities, with `notify` as the MVP-era focus
- configuration schema or a list of required configuration keys

## Transport

The transport is standard input and standard output. The protocol is JSON-RPC 2.0.

Required core-to-plugin methods are:

- `handshake(protocol_version)`
- `get_manifest()`
- `configure(config | config_path)`
- `notify_event(event_envelope)`
- `health()` as an optional health probe

## Example messages

Handshake request:

```json
{"jsonrpc":"2.0","id":1,"method":"handshake","params":{"protocol_version":"1.0"}}
```

Handshake response:

```json
{"jsonrpc":"2.0","id":1,"result":{"ok":true,"plugin":"telegram","version":"0.1.0"}}
```

Notification delivery:

```json
{"jsonrpc":"2.0","id":2,"method":"notify_event","params":{"event":{"event_id":"...","event_type":"job.succeeded","occurred_at":"...","job_id":"...","severity":"info","payload":{}}}}
```

Successful plugin response:

```json
{"jsonrpc":"2.0","id":2,"result":{"status":"sent","retry_after_seconds":null}}
```

## Error conventions

The protocol reserves these error categories:

- `InvalidConfig`
- `TemporarilyUnavailable`
- `RateLimited`
- `PermanentFailure`

## Integration event envelope

The core writes integration events into an outbox with a stable envelope:

- `event_id`: unique and stable across retries
- `event_type`: for example `job.submitted`, `job.started`, `job.validation_failed`, `job.pr_created`, `job.failed`, or `job.succeeded`
- `occurred_at`: timestamp in RFC 3339 format
- `job_id`: the related job
- `severity`: `info`, `warning`, or `error`
- `payload`: event-specific data with no secrets; large outputs should be referenced as artifacts instead of embedded inline

Example envelope:

```json
{
  "event_id": "01HRA0ZK8J2YJ9P4D0T2QK6N1Z",
  "event_type": "job.succeeded",
  "occurred_at": "2026-03-01T10:12:34Z",
  "job_id": "job_01HRA0YV9B1D7QJ5F8M3ZQ2X0A",
  "severity": "info",
  "payload": {
    "repo": "owner/name",
    "revision": "a1b2c3d4e5",
    "pr_url": "https://github.com/owner/name/pull/123",
    "summary": "Validation passed; PR created.",
    "artifacts": {
      "report_ref": "artifact://job_.../report.txt",
      "logs_ref": "artifact://job_.../sandbox.log"
    }
  }
}
```

## Delivery semantics and language support

Delivery is at least once. Plugins must deduplicate using `event_id`, and a plugin may ask the core to retry later by returning `retry_after_seconds`. Because the protocol is process-based and language-neutral, plugins can be written in Python, Node.js, TypeScript, or any other language that can read and write JSON over standard input and output.
