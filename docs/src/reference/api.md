# API

The REST API behind the web UI and the CLI, at `https://gradient.example.com/api/v1`. The OpenAPI spec is listing every endpoint with its parameters and responses.

[Open in Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml){ .md-button .md-button--primary }

## Authentication

Every endpoint except `/auth/*`, `/health` and `/config` is requiring a bearer token.

```http
Authorization: Bearer <token>
```

| Token | From | Form |
|---|---|---|
| Session | `POST /auth/basic/login`, or `gradient login` | JWT |
| API key | **Settings -> API Keys** or `POST /user/keys` | `GRAD...` |

Every response is the envelope `{ "error": false, "message": <payload> }`. A failure is setting `error` to `true`, with the reason in `message`.

## API Keys

- A key is acting with its own permissions, intersected with the owner's role in each project.
- A key pinned to a project is answering `404` for every other project.
- A key pinned to a cache is working only on that cache.
- `allowed_ips` is limiting a key to CIDR ranges. Other sources are getting `403 forbidden_source_ip`.
- The server is reading `X-Forwarded-For` only from `http.trustedProxies`.
- API keys cannot create, edit or delete API keys. Only a session can.

```sh
curl -X POST https://gradient.example.com/api/v1/user/keys \
  -H "Authorization: Bearer $SESSION" -H "Content-Type: application/json" \
  -d '{ "name": "ci", "permissions": ["viewProject", "triggerEvaluation"], "project": "acme", "expires_in_days": 90 }'
```

## Examples

Start an evaluation. The response message is the evaluation ID.

```sh
curl -X POST https://gradient.example.com/api/v1/tasks/acme/web-app/evaluate \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{ "commit": "9c1a2b3c...", "attr": "packages.x86_64-linux.web-app" }'
```

Find the store path of one attribute at one commit, e.g. for a deployment tool.

```sh
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/evaluations \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "commit=9c1a2b3c..." --data-urlencode "attr=packages.x86_64-linux.web-app"
curl -G https://gradient.example.com/api/v1/tasks/acme/web-app/entry-points \
  -H "Authorization: Bearer $TOKEN" --data-urlencode "evaluation_id=$EVAL_ID"
```

The entry point is carrying `outputs.out` as soon as the evaluation has resolved the attribute. `build_status` is telling whether the path is built and in the cache.

The closure endpoints are `/builds/{build}/closure`, `/runtime-closure` and the same paths under `/evals`. Their response is holding `roots`, `total_size_bytes` (always exact), `truncated`, `nodes` (`id`, `name`, `path`, `nar_size`) and `edges` (`source`, `target`). Each edge's `target` is depending on its `source`. The [closure view](../ui/closure-view.md) is drawing this data.

## Live Updates

Each path is upgrading to a WebSocket and pushing one [event envelope](events.md#envelope) per frame on every change of the resource.

| Path | Events |
|---|---|
| `/tasks/{project}/{task}/live` | `evaluation.<phase>`, `evaluation.progress`, `evaluation.activity` and `build.status_changed` of the task's evaluations |
| `/evals/{evaluation}/live` | The same, for one evaluation |
| `/builds/{build}/live` | The same for the build's evaluation, plus `build.progress` with downloaded and total bytes |
| `/board/live` | `worker.queue_depth`, `worker.job_dispatched`, `worker.connected`, and `worker.disconnected` for superusers |
| `/board/cache/live` | `cache.changed`, a ping to refetch `/board/cache` |

## Binary Cache

The cache paths are at the root, without `/api/v1`. Private caches are accepting HTTP Basic auth with any user name and a session or API key as password, e.g. through [netrc](../guides/share-a-cache.md#1-use-the-cache-on-a-machine).

| Method | Path | Description |
|---|---|---|
| `GET` | `/cache/{cache}/nix-cache-info` | Cache metadata for Nix |
| `GET` | `/cache/{cache}/{hash}.narinfo` | Path info, with `X-Cache: HIT` from the cache and `MISS` from an upstream cache |
| `GET` | `/cache/{cache}/nar/{hash}.nar.zst` | NAR archive |
| `GET` | `/cache/{cache}/log/{drv}` | Build log, for `nix log` |
| `GET` | `/cache/{cache}/debuginfo/{build_id}` | Debug info for `nixseparatedebuginfod`, `dwarffs` and `gdb` |
| `GET` | `/cache/{cache}/ls/{hash}` | File listing of a NAR |
| `GET` | `/cache/{cache}/serve/{hash}/{path}` | One file, or a directory as `tar.zst`, from a NAR |

- Unknown keys are always answering `404`, and Nix is moving on to the next substituter.
- `log` and `debuginfo` are falling back to the upstream caches for substituted paths.
- The rate limit of `ls`, `serve` and `log` is refilling one request every 333 ms (about 180 per minute).
- The burst is 180 for `ls` and `serve` and 900 for `log`.
