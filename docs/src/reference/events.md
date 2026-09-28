# Events and Webhooks

Every state change Gradient makes is a typed event with a dotted name (`build.completed`, `task.star`, `proto.client.nar_push`).

- **Durable** events are written to the outbox with the change that caused them and delivered to webhooks and task actions, with retries.
- **Firehose-only** events are high-rate telemetry. They are only visible on the debug stream.

`GET /api/v1/events/catalog` lists every name with its `durable` flag.

## Envelope

The same JSON everywhere: websocket frames, webhook bodies, task action bodies.

```json
{
  "event": "build.completed",
  "at": "2026-09-26T12:00:00.123Z",
  "content": { "build_id": "...", "derivation_build": "...", "evaluation_id": "...", "status": 3, "task": "...", "project": "..." }
}
```

## Event Families

| Family | Durable | Examples |
|---|---|---|
| `build.<status>` | yes (entry points) | `build.queued`, `build.started`, `build.completed`, `build.failed`, `build.substituted` |
| `evaluation.<phase>` | yes | `evaluation.queued`, `evaluation.building`, `evaluation.failed`, `evaluation.approval_granted` |
| `project.*`, `task.*`, `cache.*` | yes | `project.create`, `task.star`, `task.action.update`, `cache.nar.upload`, `cache.member.create` |
| `gc.*` | yes | `gc.swept`, `gc.deep_finished` |
| account activity | yes, instance webhooks only | `login.success`, `api_key.create`, `session.revoke` |
| `build.status_changed`, `build.progress`, `evaluation.progress` | no | every build job transition, download progress |
| `graph.*` | no | `graph.ingested`, `graph.demoted`, `graph.collected` |
| `worker.*` | no | `worker.connected`, `worker.job_dispatched`, `worker.queue_depth` |
| `proto.client.*`, `proto.server.*` | no | message type, worker, job id and size; never the payload |
| `cache.changed`, `cache.nar.fetched`, `cache.narinfo.served`, `cache.nar.signed` | no | cache traffic; `cache.nar.signed` names the cache a fresh upload was signed into |

Every `build.*` event carrying a per-evaluation `build_id` also carries `derivation_build`, the shared build anchor that `worker.job_dispatched` names as its `build_id`.

## Webhooks

Webhooks are managed under **Webhooks** in the project settings, on the cache page, and for superusers under **Job Board -> System Health -> Instance Webhooks**. [Task actions](../guides/actions.md) receive the same envelope for single tasks.

| Scope | API | Who may manage | Receives |
|---|---|---|---|
| Project | `/api/v1/projects/{project}/webhooks` | `manageWebhooks` | events of the project, its tasks and evaluations |
| Cache | `/api/v1/caches/{cache}/webhooks` | `manageCacheWebhooks` | events of the cache |
| Instance | `/api/v1/admin/webhooks` | superuser | every durable event |

- `events` is a list of globs (`build.*`, `task.star`). Empty receives everything in scope.
- The signing secret is returned once, on create and on `rotate-secret`.
- A failed delivery (transport error or non-2xx) retries 6 times, backing off from 30 s to 15 min, then dead-letters.
- `POST .../{id}/test` sends a `webhook.ping` immediately and returns the recorded delivery.

Request headers:

```http
Content-Type: application/json
X-Gradient-Event: build.completed
X-Gradient-Delivery: <uuid>                    # stable across retries, for deduplication
X-Gradient-Signature: sha256=<hex HMAC-SHA256(secret, raw body)>
```

Verifying a signature (Python):

```python
import hashlib
import hmac

def verify(secret: str, body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header)
```

## Firehose

!!! warning
    For debugging only: best-effort, per server process, and dropped on lag. Integrations use webhooks.

`GET /api/v1/metrics/events` (superuser) upgrades to a websocket that streams every event, durable or not, as one JSON envelope per frame. `?events=` takes comma-separated globs.

```sh
websocat -H "Authorization: Bearer $TOKEN" "wss://gradient.example.com/api/v1/metrics/events?events=build.*,proto.client.*"
```

A subscriber that falls behind receives `{"event":"stream.lagged","content":{"skipped":N}}` and continues from the newest event.
