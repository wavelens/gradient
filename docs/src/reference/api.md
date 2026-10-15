# API

The REST API at `https://gradient.example.com/api/v1` powers both the web UI and the CLI. Every endpoint, with parameters and responses, appears in the OpenAPI spec.

[Open in Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml){ .md-button .md-button--primary }

## Authentication

All endpoints require a bearer token except `/auth/*`, `/health` and `/config`.

```http
Authorization: Bearer <token>
```

Two ways to obtain a token:

| Token | Source | Form |
|---|---|---|
| Session | `POST /auth/basic/login` or `gradient login` | JWT |
| API key | **Settings -> API Keys** or `POST /user/keys` | `GRAD...` |

Responses use the envelope `{ "error": false, "message": <payload> }`. Set `error` to `true` on failure and place the reason in `message`.

## API Keys

- Each key acts with its own permission set. These permissions intersect with the owner's role in each project.
- A key pinned to a project returns `404` for every other project.
- A key pinned to a cache can operate on that cache only.
- The `allowed_ips` field can limit a key to a list of CIDR blocks. Requests from other sources receive `403 forbidden_source_ip`.
- The server reads `X-Forwarded-For` only from proxies listed in `http.trustedProxies`.
- API keys cannot create, edit or delete other API keys. Only a session can do this.

```sh
curl -X POST https://gradient.example.com/api/v1/user/keys \
  -H "Authorization: Bearer $SESSION" -H "Content-Type: application/json" \
  -d '{ "name": "ci", "permissions": ["viewProject", "triggerEvaluation"], "project": "acme", "expires_in_days": 90 }'
```

## Examples

An evaluation start request return the evaluation ID in the response message.

```sh
curl -X POST https://gradient.example.com/api/v1/tasks/acme/web-app/evaluate \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{ "commit": "9c1a2b3c...", "attr": "packages.x86_64-linux.web-app" }'
```

To find the store path of a single attribute at a single commit, for example when preparing a deployment tool, run two requests. The first request locate the evaluation, the second fetch its entry point.

```sh
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/evaluations \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "commit=9c1a2b3c..." --data-urlencode "attr=packages.x86_64-linux.web-app"
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/entry-points \
  -H "Authorization: Bearer $TOKEN" --data-urlencode "evaluation_id=$EVAL_ID"
```

Once the evaluation has resolved the attribute, the entry point carry `outputs.out`. The `build_status` field indicate whether the path has been built and cached.

Closure data live at `/builds/{build}/closure` and `/runtime-closure`, and at the same paths under `/evals`. A response contain the fields `roots`, `total_size_bytes` (always exact), `truncated`, `nodes` (`id`, `name`, `path`, `nar_size`) and `edges` (`source`, `target`). In each edge, the `target` depend on the `source`. The [closure view](../ui/closure-view.md) draw this data as a graph.

## Live Updates

Each path below upgrades to a WebSocket. On every resource change, the server pushes one [event envelope](events.md#envelope) per frame.

| Path | Events |
|---|---|
| `/tasks/{project}/{task}/live` | `evaluation.<phase>`, `evaluation.progress`, `evaluation.activity` and `build.status_changed` for the task's evaluations |
| `/evals/{evaluation}/live` | The same set, scoped to one evaluation |
| `/builds/{build}/live` | `evaluation.<phase>`, `evaluation.progress` and `build.status_changed` for the build's evaluation, plus `build.progress` with the phase, bytes and paths of the build's transfers |
| `/board/live` | `worker.queue_depth`, `worker.job_dispatched`, `worker.connected` and `worker.disconnected`, restricted to superusers |
| `/board/cache/live` | `cache.changed`, a ping that triggers a refetch of `/board/cache` |

## Binary Cache

Cache endpoints live at the root of the host, with no `/api/v1` prefix. Private caches require HTTP Basic auth. Any user name works; the password has to be a session or API key. See [netrc](../guides/share-a-cache.md#1-use-the-cache-on-a-machine) for one way to pass credentials.

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/nix-cache-info` | Cache metadata for Nix |
| `GET` | `/cache/{cache}/{hash}.narinfo` | Path info. `X-Cache: HIT` means the response came from this cache, `MISS` from an upstream cache |
| `GET` | `/cache/{cache}/nar/{hash}.nar.zst` | NAR archive |
| `GET` | `/cache/{cache}/log/{drv}` | Build log, used by `nix log` |
| `GET` | `/cache/{cache}/debuginfo/{build_id}` | Debug info for `nixseparatedebuginfod`, `dwarffs` and `gdb` |
| `GET` | `/cache/{cache}/ls/{hash}` | File listing of a NAR |
| `GET` | `/cache/{cache}/serve/{hash}/{path}` | A single file from a NAR, or a whole directory as `tar.zst` |

- A request for an unknown key answers with `404`, and Nix then tries the next substituter.
- `log` and `debuginfo` fall back to upstream caches for substituted paths.
- `ls`, `serve` and `log` have a rate limit that refills at one request per 333 ms, about 180 per minute.
- The burst allowance is 180 for `ls` and `serve`, and 900 for `log`.
