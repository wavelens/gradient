# API

All API endpoints are prefixed with `/api/v1`. The Nix binary cache endpoints live at the root (outside `/api/v1`).

## Reference

The full OpenAPI 3.1 specification is in the repository at `docs/gradient-api.yaml`. View it interactively:

[Open in Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml)

## Authentication

Endpoints under `/api/v1` (except `/auth/*`, `/health`, and `/config`) require a bearer token:

```http
Authorization: Bearer <token>
```

Two token types are accepted:

| Type | How to obtain | Prefix |
|---|---|---|
| JWT | `POST /api/v1/auth/basic/login` | none |
| API key | `POST /api/v1/user/keys` | `GRAD` |

## Response Envelope

Every JSON response is wrapped in:

```json
{ "error": false, "message": <payload> }
```

On errors, `error` is `true` and `message` is a string describing the problem.

## Quick Reference

### Auth (no authentication required)

| Method | Path | Description |
|---|---|---|
| `POST` | `/auth/basic/register` | Register a new user |
| `POST` | `/auth/basic/login` | Log in, returns JWT |
| `POST` | `/auth/check-username` | Check username availability |
| `GET` | `/auth/verify-email?token=…` | Verify email address |
| `POST` | `/auth/resend-verification` | Resend verification email |
| `POST` | `/auth/oauth/authorize` | Get OIDC authorization URL |
| `GET` | `/auth/oidc/login` | Redirect to OIDC provider |
| `GET` | `/auth/oidc/callback` | OIDC callback handler |
| `POST` | `/auth/logout` | Logout |
| `GET` | `/health` | Health check |
| `GET` | `/config` | Server feature flags |

### User

| Method | Path | Description |
|---|---|---|
| `GET` | `/user` | Current user info |
| `DELETE` | `/user` | Delete account |
| `GET` | `/user/keys` | List API keys |
| `POST` | `/user/keys` | Create API key |
| `DELETE` | `/user/keys` | Delete API key |
| `GET` | `/user/keys/permissions` | List the permission catalogue |
| `PATCH` | `/user/keys/{api_id}` | Update an API key's name / permissions / project pin |
| `GET` | `/user/settings` | Get profile settings |
| `PATCH` | `/user/settings` | Update profile settings |

### Configuring API-key options

Each API key carries its own permission set and an optional project pin:

```bash
curl -X POST $API/user/keys \
  -H "Authorization: Bearer $SESSION" \
  -H "Content-Type: application/json" \
  -d '{
        "name": "ci-runner",
        "permissions": ["triggerEvaluation", "viewProject"],
        "project": "acme",
        "expires_in_days": 90
      }'
```

The key's effective authority on every request is `user_role_mask & key_mask`,
intersected with the project's role assignment for the caller. A key pinned to an
project 404s for every other project. The full permission catalogue is at
`GET /user/keys/permissions`.

To tighten an existing key without rotating the secret:

```bash
curl -X PATCH $API/user/keys/$KEY_ID \
  -H "Authorization: Bearer $SESSION" \
  -H "Content-Type: application/json" \
  -d '{ "permissions": ["viewProject"] }'
```

API-key-authenticated requests **cannot** create, edit, revoke, or delete API
keys - only session-authenticated calls can. This prevents a leaked key from
minting more powerful siblings.

### Source IP restrictions

Each API key can carry a CIDR allowlist. Requests from outside the list are
rejected with `403 forbidden_source_ip`; an empty / omitted list allows any
source. Bare IPs are auto-normalized to `/32` (v4) or `/128` (v6).

```bash
curl -X POST $API/user/keys \
  -H "Authorization: Bearer $SESSION" \
  -H "Content-Type: application/json" \
  -d '{
        "name": "office-ci",
        "permissions": ["triggerEvaluation"],
        "allowed_ips": ["10.0.0.0/8", "203.0.113.5"]
      }'
```

To tighten or clear the allowlist on an existing key, `PATCH` with
`"allowed_ips": [...]` (use `[]` to wipe).

The source IP is resolved from the connection peer with `X-Forwarded-For`
honored only when the peer is in `GRADIENT_NETWORK_TRUSTED_PROXIES`.

### Cache pinning

A key may be pinned to a single cache as an alternative to project
pinning (the two are mutually exclusive). Cache-pinned keys carry a
`CachePermission` bitmask and can be used only on routes targeting the pinned
cache. Creating a cache-pinned key requires the `manageCacheMembers` permission
on the target cache. Use the `availableCache` field on
`GET /user/keys/permissions` to enumerate valid capability names.

### Projects

| Method | Path | Description |
|---|---|---|
| `GET` | `/projects` | List projects |
| `PUT` | `/projects` | Create project |
| `GET` | `/projects/{project}` | Get project |
| `PATCH` | `/projects/{project}` | Update project |
| `DELETE` | `/projects/{project}` | Delete project |
| `GET/POST/PATCH/DELETE` | `/projects/{project}/users` | Manage members |
| `GET/POST` | `/projects/{project}/ssh` | Get / regenerate SSH key |
| `GET` | `/projects/{project}/subscribe` | List subscribed caches |
| `POST/DELETE` | `/projects/{project}/subscribe/{cache}` | Subscribe / unsubscribe |

### Workers

Workers are `gradient-worker` processes that connect to the server over WebSocket to execute fetch, eval, build, and sign jobs.

| Method | Path | Description |
|---|---|---|
| `POST` | `/projects/{project}/workers` | Register a worker - returns `peer_id` and optionally a one-time `token` |
| `GET` | `/projects/{project}/workers` | List registered workers (merges live state) |
| `DELETE` | `/projects/{project}/workers/{worker_id}` | Unregister a worker |
| `GET` | `/admin/workers` | List all currently connected workers (superuser or `GRADIENT_GLOBAL_STATS_PUBLIC`) |

All endpoints under `/admin/*` require the calling user to have the
`superuser` flag set on their account.

**Register a worker:**

`worker_id` must be a **UUID v4**. On a NixOS host running the `gradient-worker` service the worker auto-generates one on first start and persists it to `/var/lib/gradient-worker/worker-id` - read it with:

```sh
cat /var/lib/gradient-worker/worker-id
```

```sh
# Server generates the token (returned once, store it immediately)
curl -X POST https://gradient.example.com/api/v1/projects/myproject/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "550e8400-e29b-41d4-a716-446655440001"}'
```

Response (server-generated token):

```json
{
  "error": false,
  "message": {
    "peer_id": "550e8400-e29b-41d4-a716-446655440000",
    "token": "a1b2c3..."
  }
}
```

Alternatively, supply a pre-generated token (`openssl rand -base64 48` - exactly 64 standard base64 characters). The server stores its hash and **does not** return it in the response:

```sh
curl -X POST https://gradient.example.com/api/v1/projects/myproject/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"worker_id\": \"550e8400-e29b-41d4-a716-446655440001\", \"token\": \"$(openssl rand -base64 48)\"}"
```

Response (pre-supplied token - no `token` field):

```json
{
  "error": false,
  "message": {
    "peer_id": "550e8400-e29b-41d4-a716-446655440000"
  }
}
```

Write the `peer_id` and token to the peers file on the worker host:

```sh
echo "<peer_id>:<token>" > /run/secrets/gradient-worker-peers
```

Set `GRADIENT_WORKER_PEERS_FILE` (or the NixOS `peersFile` option) to this path.

**List workers:**

`GET /projects/{project}/workers` returns registered workers merged with live connection info:

```json
{
  "error": false,
  "message": [
    {
      "worker_id": "build-01",
      "registered_at": "2026-04-12T10:00:00Z",
      "live": {
        "architectures": ["x86_64-linux"],
        "system_features": ["kvm", "big-parallel"],
        "max_concurrent_builds": 8,
        "assigned_job_count": 2,
        "draining": false
      }
    }
  ]
}
```

`live` is `null` if the worker is not currently connected.

**Unregister a worker:**

`DELETE /projects/{project}/workers/{worker_id}` removes the registration. The worker stays connected until it disconnects, then cannot reconnect.

### Tasks

| Method | Path | Description |
|---|---|---|
| `GET` | `/tasks/{project}` | List tasks |
| `PUT` | `/tasks/{project}` | Create task |
| `GET/PATCH/DELETE` | `/tasks/{project}/{task}` | Get / update / delete |
| `GET` | `/tasks/{project}/{task}/details` | Aggregated task data |
| `GET` | `/tasks/{project}/{task}/evaluations` | List / search evaluations (`?limit=`, `?commit=`, `?status=`, `?attr=`) |
| `GET` | `/tasks/{project}/{task}/entry-points` | Root builds |
| `POST` | `/tasks/{project}/{task}/check-repository` | Test repo access |
| `POST` | `/tasks/{project}/{task}/evaluate` | Trigger evaluation |
| `POST/DELETE` | `/tasks/{project}/{task}/active` | Enable / disable |

### Build requests

| Method | Path | Description |
|---|---|---|
| `POST` | `/build-requests/url` | Build a remote repository at a ref or commit (no upload) |
| `POST` | `/build-requests/manifest` | Start a source upload session |
| `POST` | `/build-requests/{session}/blobs` | Upload missing blobs |
| `POST` | `/build-requests/{session}/dispatch` | Finalise and queue |
| `POST` | `/build-requests/source` | Upload a packed source NAR and queue |

All of these run on the project's reserved `build-request` task, created lazily
on first use, so no task is created per job.

### Evaluations

| Method | Path | Description |
|---|---|---|
| `GET` | `/evals/{id}` | Get evaluation |
| `POST` | `/evals/{id}` | Abort (`{"method":"abort"}`) |
| `GET` | `/evals/{id}/builds` | List builds |
| `POST` | `/evals/{id}/builds` | Stream all build logs (NDJSON) |

### Builds

| Method | Path | Description |
|---|---|---|
| `POST` | `/builds` | Submit direct build (multipart) |
| `GET` | `/builds/direct/recent` | Recent direct builds |
| `GET` | `/builds/{id}` | Build with outputs |
| `GET/POST` | `/builds/{id}/log` | Get full log / stream live log |
| `GET` | `/builds/{id}/log/chunks` | Chunk index of a finalized log |
| `GET` | `/builds/{id}/log/chunk/{index}` | One decompressed chunk (plaintext) |
| `GET` | `/builds/{id}/log/lines` | Line range, e.g. `?range=L120-L130` |
| `GET` | `/builds/{id}/log/search` | NDJSON stream of search hits (`?q=`) |
| `GET` | `/builds/{id}/graph` | Full dependency graph |
| `GET` | `/builds/{id}/dependencies` | Direct dependencies |
| `GET` | `/builds/{id}/downloads` | List artefacts |
| `GET` | `/builds/{id}/download/{filename}` | Download artefact |

The `/log*` endpoints fall back to the most recent prior build of the same
derivation for a `Substituted` build (which has no log of its own).

### Caches

| Method | Path | Description |
|---|---|---|
| `GET` | `/caches` | List caches |
| `PUT` | `/caches` | Create cache |
| `GET/PATCH/DELETE` | `/caches/{cache}` | Get / update / delete |
| `POST/DELETE` | `/caches/{cache}/active` | Enable / disable |
| `GET` | `/caches/{cache}/key` | Public signing key |

### Commits

| Method | Path | Description |
|---|---|---|
| `GET` | `/commits/{id}` | Get commit |

### Live updates (WebSocket)

These endpoints upgrade to a WebSocket and push JSON events when the relevant
resource changes, so the frontend refetches on change instead of polling. Each
channel only forwards events for its resource (authorized at connect).

| Path | Events |
|---|---|
| `/board/live` | `queue_depth`, `job_dispatched`, `worker_connected`, `worker_disconnected` (scope-masked) |
| `/tasks/{project}/{task}/live` | `evaluation_status_changed`, `build_status_changed`, `evaluation_progress` for the task |
| `/evals/{evaluation}/live` | `evaluation_status_changed`, `build_status_changed`, `evaluation_progress` for the evaluation |
| `/builds/{build}/live` | `build_status_changed` for the build's evaluation (its dependency graph) |
| `/board/cache/live` | `cache_changed` (content-free ping; refetch `/board/cache`) |

Frames are JSON with a `type` field, e.g.
`{"type":"build_status_changed","evaluation_id":"…","build_id":"…","status":2}`.
`evaluation_progress` (`{"type":"evaluation_progress","task":"…","evaluation_id":"…"}`)
is a content-free ping emitted as builds and entry-points are persisted during
the evaluation phase - before any build changes status - so the build and
dependency totals grow live instead of only appearing once evaluation finishes.

### Nix Binary Cache (root, no `/api/v1` prefix)

Private caches require HTTP Basic Auth (any username, JWT or API key as password - returns `401` without credentials).

**Substituter surface** (used by `nix`, `nixos-rebuild`, etc.):

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/nix-cache-info` | Cache metadata (add `?json` for JSON) |
| `GET` | `/cache/{cache}/gradient-cache-info` | Gradient cache metadata (add `?json` for JSON) |
| `GET` | `/cache/{cache}/{hash}.narinfo` | Path info (add `?json` for JSON). `References`/`Deriver` are store-path basenames; the empty `References` line is omitted. Responds with `X-Cache: HIT` when served from our store, `MISS` when proxied from an upstream. |
| `GET` | `/cache/{cache}/nar/{hash}.nar.zst` | NAR archive |
| `GET` | `/cache/{cache}/debuginfo/{build_id}` | DWARF debug info for an ELF build id (also accepts `{build_id}.debug`) |

Every key the cache does not serve answers `404`, never another `4xx`: a
substituter or debuginfod client treats anything else as a hard error and gives
up instead of moving on to the next substituter.

#### Debug info

`debuginfo/{build_id}` implements the same index nix writes when a binary cache
is created with `index-debug-info=true`, so `nixseparatedebuginfod`, `dwarffs`
and `gdb`'s debuginfod client resolve symbols straight from a Gradient cache:

```json
{ "archive": "../nar/<file-hash>.nar.zst",
  "member": "lib/debug/.build-id/7d/beaca53fbc9a489b633871093c37dae3857a37.debug" }
```

`archive` is relative to the requested key, so it resolves against the cache
root. The index is built by walking the NAR of every cached
`separateDebugInfo` output - store paths whose name ends in `-debug`, the only
ones nixpkgs puts a `lib/debug/.build-id` tree in. Uploads index themselves;
`GRADIENT_DEBUG_INDEX_INTERVAL_SECS` paces the backfill that covers paths cached
before the index existed.

For a path the cache substituted rather than built, the lookup falls through to
the cache's upstreams and rewrites their `archive` link through
`nar/upstream/{id}/...`, so a pull-through cache serves debug info for what it
mirrors. `X-Cache` reports `HIT` for our own index and `MISS` for an upstream's.

**Inspection surface** (NAR content inspection and build logs):

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/ls/{hash}` | JSON tree listing of the NAR (nix-serve `.ls` v1 schema) |
| `GET` | `/cache/{cache}/serve/{hash}/{path}` | Extract a single file (bytes) or directory (tar.zst) from a NAR |
| `GET` | `/cache/{cache}/log/{drv}` | Build log for `<drv>.drv` (substituter compat - `nix log`) |

The inspection endpoints (`/ls`, `/serve`) are rate-limited at 60 req/min. The `/log` endpoint is rate-limited at ~300 req/min on its own tier. All endpoints return `404` when the hash or derivation is unknown.

`/log` serves this cache's own log whenever a build produced one - a failed build's log included, since that is the one worth reading. When the cache has no log of its own, which is the normal case for a path it substituted rather than built, it asks its upstreams in configured order and proxies the first that answers. The response carries `X-Cache: HIT` for our own log and `MISS` for an upstream's, so `nix log` works against a pull-through cache.

## Example: Trigger an Evaluation

```sh
TOKEN=$(curl -s -X POST https://gradient.example.com/api/v1/auth/basic/login \
  -H 'Content-Type: application/json' \
  -d '{"loginname":"alice","password":"secret"}' | jq -r .message)

curl -X POST "https://gradient.example.com/api/v1/tasks/my-project/my-task/evaluate" \
  -H "Authorization: Bearer $TOKEN"
```

Response:

```json
{ "error": false, "message": "3fa85f64-5717-4562-b3fc-2c963f66afa6" }
```

The `message` is the new evaluation's UUID, so you can follow the run you just
started with `GET /evals/{id}` or the `GET /evals/{id}/live` stream.

## Example: Has this attribute at this commit been built?

For pull-based deployment tools that follow a branch and need the store path for
one flake output at one commit. Search the evaluations first:

```sh
curl -s -G "https://gradient.example.com/api/v1/tasks/my-project/my-task/evaluations" \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "commit=9c1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3" \
  --data-urlencode "attr=packages.x86_64-linux.my-package"
```

`attr` matches the evaluation's *wildcard*, which is set the moment the
evaluation row is created. That means an evaluation still `Queued` or
`EvaluatingFlake` is found too, so a caller can wait for a run already in flight
instead of triggering a duplicate. An empty list means no evaluation covers that
attribute at that commit; trigger one with `POST .../evaluate`.

A match means the attribute was *in scope*, not that it exists in the flake or
that it built. Confirm and read the store path from the entry points:

```sh
curl -s -G "https://gradient.example.com/api/v1/tasks/my-project/my-task/entry-points" \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "evaluation_id=$EVAL_ID"
```

```json
{
  "error": false,
  "message": [
    {
      "eval": "packages.x86_64-linux.my-package",
      "build_status": "Completed",
      "outputs": { "out": "/nix/store/7g0q1x3j...-my-package-1.0.0" }
    }
  ]
}
```

`outputs` comes from the resolved `.drv`, so it is populated before the build
finishes; `build_status` is what tells you whether that path is realised and
fetchable from the cache. To wait rather than poll, attach to
`GET /evals/{id}/live`.

If no evaluation covers the commit yet, ask for one. There are two ways:

```sh
# On the task, so the run appears in its evaluation history.
curl -X POST "https://gradient.example.com/api/v1/tasks/my-project/my-task/evaluate" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"commit":"9c1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3",
       "attr":"packages.x86_64-linux.my-package"}'
```

A pinned run is `concurrent`, so it neither queues behind nor aborts the task's
own CI run, and it does not bump tracked flake inputs.

```sh
# Without a task at all: point Gradient at any repository URL.
curl -X POST "https://gradient.example.com/api/v1/build-requests/url" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"project":"my-project",
       "url":"ssh://git@github.com/org/repo.git",
       "rev":"9c1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3",
       "target":"packages.x86_64-linux.my-package"}'
```

Send `ref` instead of `rev` to have the server resolve a branch or tag, or
neither to take the repository's default branch. Both forms answer with the new
evaluation's id, so the wait-and-read steps above are identical afterwards.
