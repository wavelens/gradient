# API

The REST API behind the web UI and the CLI, at `https://gradient.example.com/api/v1`. Every endpoint with its parameters and responses is in the OpenAPI spec:

[Open in Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml){ .md-button .md-button--primary }

## Authentication

Every endpoint except `/auth/*`, `/health` and `/config` takes a bearer token:

```http
Authorization: Bearer <token>
```

| Token | From | Form |
|---|---|---|
| Session | `POST /auth/basic/login`, or `gradient login` | JWT |
| API key | **Settings -> API Keys** or `POST /user/keys` | `GRAD...` |

Every response is an envelope: `{ "error": false, "message": <payload> }`; on failure `error` is `true` and `message` holds the reason.

## API Keys

- A key acts with its own permissions intersected with the owner's role in each project.
- A key pinned to a project answers `404` for every other project; a key pinned to a cache works only on that cache.
- `allowed_ips` limits a key to CIDR ranges; other sources get `403 forbidden_source_ip`. `X-Forwarded-For` counts only from `http.trustedProxies`.
- API keys cannot create, edit or delete API keys; only a session can.

```sh
curl -X POST https://gradient.example.com/api/v1/user/keys \
  -H "Authorization: Bearer $SESSION" -H "Content-Type: application/json" \
  -d '{ "name": "ci", "permissions": ["viewProject", "triggerEvaluation"], "project": "acme", "expires_in_days": 90 }'
```

## Examples

Start an evaluation; the response message is the evaluation ID:

```sh
curl -X POST https://gradient.example.com/api/v1/tasks/acme/web-app/evaluate \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{ "commit": "9c1a2b3c...", "attr": "packages.x86_64-linux.web-app" }'
```

Find the store path of one attribute at one commit, e.g. for a deployment tool:

```sh
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/evaluations \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "commit=9c1a2b3c..." --data-urlencode "attr=packages.x86_64-linux.web-app"
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/entry-points \
  -H "Authorization: Bearer $TOKEN" --data-urlencode "evaluation_id=$EVAL_ID"
```

The entry point carries `outputs.out` as soon as the evaluation resolves the attribute; `build_status` tells whether the path is built and in the cache.

The closure endpoints (`/builds/{build}/closure`, `/runtime-closure` and the same under `/evals`) return `roots`, `total_size_bytes` (always exact), `truncated`, `nodes` (`id`, `name`, `path`, `nar_size`) and `edges` (`source`, `target`, where `target` depends on `source`), the data behind the [closure view](../ui/closure-view.md).

## Live Updates

These paths upgrade to a WebSocket and push JSON frames with a `type` field whenever the resource changes.

| Path | Frames |
|---|---|
| `/tasks/{project}/{task}/live` | `evaluation_status_changed`, `build_status_changed`, `evaluation_progress` |
| `/evals/{evaluation}/live` | The same, for one evaluation |
| `/builds/{build}/live` | `build_status_changed`, `build_progress` with downloaded and total bytes |
| `/board/live` | `queue_depth`, `job_dispatched`, `worker_connected`, `worker_disconnected` |
| `/board/cache/live` | `cache_changed`, a ping to refetch `/board/cache` |

## Binary Cache

At the root, without `/api/v1`. Private caches take HTTP Basic auth with any user name and a session or API key as password, e.g. through [netrc](../guides/share-a-cache.md#1-use-the-cache-on-a-machine).

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/nix-cache-info` | Cache metadata for Nix |
| `GET` | `/cache/{cache}/{hash}.narinfo` | Path info; `X-Cache: HIT` from the cache, `MISS` from an upstream |
| `GET` | `/cache/{cache}/nar/{hash}.nar.zst` | NAR archive |
| `GET` | `/cache/{cache}/log/{drv}` | Build log, for `nix log` |
| `GET` | `/cache/{cache}/debuginfo/{build_id}` | Debug info for `nixseparatedebuginfod`, `dwarffs` and `gdb` |
| `GET` | `/cache/{cache}/ls/{hash}` | File listing of a NAR |
| `GET` | `/cache/{cache}/serve/{hash}/{path}` | One file, or a directory as `tar.zst`, from a NAR |

- Unknown keys always answer `404`, and Nix moves on to the next substituter.
- `log` and `debuginfo` fall back to the upstreams for substituted paths.
- `ls`, `serve` and `log` refill one request every 333 ms (about 180 per minute), with a burst of 180 for `ls` and `serve` and 900 for `log`.
