# Events and Webhooks

Gradient records each state change as a typed event. Event names use dotted notation, such as `build.completed`, `task.star`, or `proto.client.nar_push`.

Two delivery classes exist:

- **Durable** events land in pending deliveries alongside the change that triggered them. Gradient then delivers them to webhooks and task actions and retries on failure.
- **Firehose-only** events carry high-rate telemetry. The debug stream is the only place where they appear.

`GET /api/v1/events/catalog` lists all event names together with their `durable` flag.

## Envelope

All three payloads use one JSON structure: websocket messages, webhook bodies, and task action bodies.

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
| `runner_job.<status>` | yes | `runner_job.queued`, `runner_job.started`, `runner_job.phase`, `runner_job.completed`, `runner_job.failed`, `runner_job.aborted` |
| `project.*`, `task.*`, `cache.*` | yes | `project.create`, `task.star`, `task.action.update`, `cache.nar.upload`, `cache.member.create` |
| `gc.*` | yes | `gc.swept`, `gc.deep_finished` |
| `worker.demand` | yes, team webhooks only | waiting evaluations that need a system, feature, network, secrets or eval worker missing from the connected workers. Delivered once per team granting its workers to the project |
| account activity | yes, instance webhooks only | `login.success`, `api_key.create`, `session.revoke` |
| `build.status_changed`, `build.progress`, `evaluation.progress`, `evaluation.activity` | no | each build job transition, build transfer progress, fetch and eval progress |
| `graph.*` | no | `graph.recorded`, `graph.demoted`, `graph.collected` |
| `worker.*` | no | `worker.connected`, `worker.job_dispatched`, `worker.queue_depth` |
| `proto.client.*`, `proto.server.*` | no | message type, worker, job id and size, never the payload |
| `cache.changed`, `cache.nar.fetched`, `cache.narinfo.served`, `cache.nar.signed` | no | cache traffic. `cache.nar.signed` marks a fresh upload as signed into a cache |

Every `build.*` event with a per-evaluation `build_id` also includes `derivation_build`. That field identifies the shared build, and `worker.job_dispatched` names this same build as its `build_id`.

## Webhooks

Manage webhooks under **Webhooks** in the project settings, on the cache page or in the team settings. Superusers manage instance webhooks under **Job Board -> System Health -> Instance Webhooks**. [Task actions](../guides/actions.md) receive the same envelope for single tasks.

| Scope | API | Who may manage | Receives |
|---|---|---|---|
| Project | `/api/v1/projects/{project}/webhooks` | `manageWebhooks` | events of the project, its tasks and evaluations |
| Cache | `/api/v1/caches/{cache}/webhooks` | `manageCacheWebhooks` | events of the cache |
| Team | `/api/v1/teams/{team}/webhooks` | team Admin | team events (`worker.demand`, `team.worker.*`, `team.webhook.*`), never events of granting projects |
| Instance | `/api/v1/admin/webhooks` | superuser | every durable event |

- `events` is a list of globs (`build.*`, `task.star`). With an empty list, the webhook receives everything in scope.
- The API exposes the signing secret exactly once, on create and on `rotate-secret`.
- Gradient retries a failed delivery (transport error or non-2xx) up to 6 times in total. Backoff starts at 30 s and caps at 8 min. After the last attempt, Gradient dead-letters the delivery.
- `POST .../{id}/test` dispatches a `webhook.ping` immediately and gives back the recorded delivery.

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

`GET /api/v1/metrics/events` (superuser) upgrades to a websocket. Every event, durable or not, flows through it as one JSON envelope per frame. The `?events=` parameter accepts comma-separated globs.

```sh
websocat -H "Authorization: Bearer $TOKEN" "wss://gradient.example.com/api/v1/metrics/events?events=build.*,proto.client.*"
```

A lagging subscriber will receive `{"event":"stream.lagged","content":{"skipped":N}}` and continue from the newest event.
