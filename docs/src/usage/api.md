# API

All API endpoints are prefixed with `/api/v1`. The Nix binary cache endpoints live at the root (outside `/api/v1`).

## Reference

The full OpenAPI 3.1 specification is in the repository at `docs/gradient-api.yaml`. View it interactively:

[Open in Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml)

## Authentication

Endpoints under `/api/v1` (except `/auth/*`, `/health`, and `/config`) require a bearer token:

```
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
| `GET` | `/user/settings` | Get profile settings |
| `PATCH` | `/user/settings` | Update profile settings |

### Organizations

| Method | Path | Description |
|---|---|---|
| `GET` | `/orgs` | List organizations |
| `PUT` | `/orgs` | Create organization |
| `GET` | `/orgs/{org}` | Get organization |
| `PATCH` | `/orgs/{org}` | Update organization |
| `DELETE` | `/orgs/{org}` | Delete organization |
| `GET/POST/PATCH/DELETE` | `/orgs/{org}/users` | Manage members |
| `GET/POST` | `/orgs/{org}/ssh` | Get / regenerate SSH key |
| `GET` | `/orgs/{org}/subscribe` | List subscribed caches |
| `POST/DELETE` | `/orgs/{org}/subscribe/{cache}` | Subscribe / unsubscribe |

### Workers

Workers are `gradient-worker` processes that connect to the server over WebSocket to execute fetch, eval, build, and sign jobs.

| Method | Path | Description |
|---|---|---|
| `POST` | `/orgs/{org}/workers` | Register a worker — returns `peer_id` + one-time `token` |
| `GET` | `/orgs/{org}/workers` | List registered workers (merges live state) |
| `DELETE` | `/orgs/{org}/workers/{worker_id}` | Unregister a worker |
| `GET` | `/workers` | List all currently connected workers (superuser or `GRADIENT_GLOBAL_STATS_PUBLIC`) |

**Register a worker:**

```sh
curl -X POST https://gradient.example.com/api/v1/orgs/myorg/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "build-01"}'
```

Response:

```json
{
  "error": false,
  "message": {
    "peer_id": "550e8400-e29b-41d4-a716-446655440000",
    "token": "a1b2c3..."
  }
}
```

The `token` is shown **once only** — store it immediately. On the worker, write it to the peers file:

```sh
echo "550e8400-e29b-41d4-a716-446655440000:a1b2c3..." > /run/secrets/gradient-worker-peers
```

Set `GRADIENT_WORKER_PEERS_FILE` (or the NixOS `peersFile` option) to this path.

**List workers:**

`GET /orgs/{org}/workers` returns registered workers merged with live connection info:

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

`DELETE /orgs/{org}/workers/{worker_id}` removes the registration. The worker stays connected until it disconnects, then cannot reconnect.

### Projects

| Method | Path | Description |
|---|---|---|
| `GET` | `/projects/{org}` | List projects |
| `PUT` | `/projects/{org}` | Create project |
| `GET/PATCH/DELETE` | `/projects/{org}/{project}` | Get / update / delete |
| `GET` | `/projects/{org}/{project}/details` | Aggregated project data |
| `GET` | `/projects/{org}/{project}/entry-points` | Root builds |
| `POST` | `/projects/{org}/{project}/check-repository` | Test repo access |
| `POST` | `/projects/{org}/{project}/evaluate` | Trigger evaluation |
| `POST/DELETE` | `/projects/{org}/{project}/active` | Enable / disable |

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
| `GET/POST` | `/builds/{id}/log` | Get log / stream live log |
| `GET` | `/builds/{id}/graph` | Full dependency graph |
| `GET` | `/builds/{id}/dependencies` | Direct dependencies |
| `GET` | `/builds/{id}/downloads` | List artefacts |
| `GET` | `/builds/{id}/download/{filename}` | Download artefact |

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

### Nix Binary Cache (root, no `/api/v1` prefix)

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/nix-cache-info` | Cache metadata |
| `GET` | `/cache/{cache}/{hash}.narinfo` | Path info |
| `GET` | `/cache/{cache}/nar/{hash}.nar.zst` | NAR archive |

## Example: Trigger an Evaluation

```sh
TOKEN=$(curl -s -X POST https://gradient.example.com/api/v1/auth/basic/login \
  -H 'Content-Type: application/json' \
  -d '{"loginname":"alice","password":"secret"}' | jq -r .message)

curl -X POST "https://gradient.example.com/api/v1/projects/my-org/my-project/evaluate" \
  -H "Authorization: Bearer $TOKEN"
```

Response:

```json
{ "error": false, "message": "3fa85f64-5717-4562-b3fc-2c963f66afa6" }
```
