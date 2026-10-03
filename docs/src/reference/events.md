# Events and Webhooks

Every state change in Gradient is a typed event with a dotted name (`build.completed`, `task.star`, `proto.client.nar_push`).

- **Durable** events are landing in pending deliveries together with the change that caused them. Gradient is then delivering them to webhooks and task actions, with retries.
- **Firehose-only** events are high-rate telemetry. They are only visible on the debug stream.

`GET /api/v1/events/catalog` is listing every name with its `durable` flag.

## Envelope

One JSON shape for websocket frames, webhook bodies and task action bodies.

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
| `build.status_changed`, `build.progress`, `evaluation.progress`, `evaluation.activity` | no | every build job transition, build transfer progress, fetch and eval progress |
| `graph.*` | no | `graph.recorded`, `graph.demoted`, `graph.collected` |
| `worker.*` | no | `worker.connected`, `worker.job_dispatched`, `worker.queue_depth` |
| `proto.client.*`, `proto.server.*` | no | message type, worker, job id and size, never the payload |
| `cache.changed`, `cache.nar.fetched`, `cache.narinfo.served`, `cache.nar.signed` | no | cache traffic. `cache.nar.signed` is naming the cache a fresh upload was signed into |

Every `build.*` event with a per-evaluation `build_id` is also carrying `derivation_build`. `derivation_build` is the shared build that `worker.job_dispatched` is naming as its `build_id`.

## Webhooks

Webhook management is available under **Webhooks** in the project settings and on the cache page. Superusers are managing instance webhooks under **Job Board -> System Health -> Instance Webhooks**. [Task actions](../guides/actions.md) are receiving the same envelope for single tasks.

| Scope | API | Who may manage | Receives |
|---|---|---|---|
| Project | `/api/v1/projects/{project}/webhooks` | `manageWebhooks` | events of the project, its tasks and evaluations |
| Cache | `/api/v1/caches/{cache}/webhooks` | `manageCacheWebhooks` | events of the cache |
| Instance | `/api/v1/admin/webhooks` | superuser | every durable event |

- `events` is a list of globs (`build.*`, `task.star`). An empty list is receiving everything in scope.
- The API is returning the signing secret once, on create and on `rotate-secret`.
- Gradient is trying a failed delivery (transport error or non-2xx) 6 times in total. The backoff is growing from 30 s to 8 min. The delivery is then dead-lettered.
- `POST .../{id}/test` is sending a `webhook.ping` immediately and returning the recorded delivery.

**Request Headers**

```http
Content-Type: application/json
X-Gradient-Event: build.completed
X-Gradient-Delivery: <uuid>                    # stable across retries, for deduplication
X-Gradient-Signature: sha256=<hex HMAC-SHA256(secret, raw body)>
```

**Signature Verification (Python)**

```python
import hashlib
import hmac

def verify(secret: str, body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header)
```

## Firehose

!!! warning
    The firehose is for debugging only. Delivery is best-effort, per server process, and dropped on lag. Integrations are using webhooks.

`GET /api/v1/metrics/events` (superuser) is upgrading to a websocket. The websocket is streaming every event, durable or not, as one JSON envelope per frame. `?events=` is taking comma-separated globs.

```sh
websocat -H "Authorization: Bearer $TOKEN" "wss://gradient.example.com/api/v1/metrics/events?events=build.*,proto.client.*"
```

A lagging subscriber is receiving `{"event":"stream.lagged","content":{"skipped":N}}` and continuing from the newest event.
