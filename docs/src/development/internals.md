# Internals

Key functions inside each crate.

---

## Forge Webhook Ingestion

`web::endpoints::forge_hooks` handles incoming push events from external forges. All endpoints are unauthenticated and self-verify via HMAC.

**GitHub App** (`POST /api/v1/hooks/github`):
- Verifies `X-Hub-Signature-256` against `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`.
- `push` → calls `core::evaluation_trigger::trigger_evaluation` for each matching project.
- `installation` / `installation_repositories` → stores or clears `organization.github_installation_id`.

**Generic forges** (`POST /api/v1/hooks/{forge}/{org}`):
- Looks up the organization by name.
- Decrypts `organization.forge_webhook_secret` (same `crypt_secret_file` infrastructure as SSH keys).
- Verifies the forge-specific signature header.
- `push` → calls `trigger_evaluation` for each matching project.

**Shared trigger** (`core::evaluation_trigger::trigger_evaluation`):
1. Checks no evaluation is already in progress (returns `TriggerError::AlreadyInProgress` if so).
2. Inserts a `Commit` row with the push SHA.
3. Inserts an `Evaluation` row with status `Queued`.
4. Sets `project.force_evaluation = true` and resets `last_check_at` to the epoch.
5. The scheduler picks it up on its next tick (≤ 60 s) via the existing pre-created `Queued` evaluation path.

Repository matching normalises URLs by stripping trailing `.git` and compares against all active projects.

---

## Evaluation Pipeline

`builder::scheduler::schedule_evaluation_loop` polls for queued evaluations every 60 seconds, up to `max_concurrent_evaluations` concurrent tasks.

For each evaluation, a `PendingEvalJob` is enqueued into the proto scheduler's `JobTracker`. The scheduler dispatches it to an eligible connected worker that has the `eval` (and optionally `fetch`) capability negotiated.

The worker executes:

**1. FetchFlake** (if `fetch` capability)

Converts the repository URL and commit hash into a Nix flake reference: `git+https://host/repo?rev=<sha1>`, then runs `nix flake prefetch` to populate the local store. SSH credentials for private repos are delivered via the `Credential::SshKey` proto message.

**2. EvaluateFlake + EvaluateDerivations** (if `eval` capability)

Expands the evaluation wildcard into attribute paths, resolves each to a `.drv` path via the Nix C API, and walks the dependency closure via BFS. During the walk the worker sends **incremental `EvalResult` batches** as derivations are discovered — the server inserts rows immediately without waiting for the full walk.

Wildcard segments:

- `*` — **recursive**: matches any attribute name and, when at the trailing position, descends one additional level to recover derivations hidden by consecutive-wildcard collapsing (`packages.*.*` and `packages.*` are equivalent).
- `#` — **non-recursive**: matches any attribute name at exactly that depth and checks `type == "derivation"` without descending further.
- `!prefix` — **exclusion**: removes exact paths from the collected set.

**3. Server-side batch insert** (on each `EvalResult` batch)

Derivations, outputs, dependency edges, and builds are bulk-inserted in chunks of 1 000 rows in FK order: `derivation` → `derivation_output` → `derivation_dependency` → `build`. Substituted builds (already in the worker's store) are inserted with status `Substituted` and immediately eligible for signing.

**4. Status transitions**

```
build:       Created → Queued → Building → Completed | Failed
                                         ↘ Substituted (already in store)
                                         ↘ Aborted | DependencyFailed
evaluation:  Queued → Fetching → EvaluatingFlake → EvaluatingDerivation → Building
                                                                       → Completed | Failed | Aborted
```

`Substituted` is distinct from `Completed`: it means the derivation was already in the local Nix store at evaluation time and never ran on a builder.

---

## Build Dispatcher

The proto scheduler's dispatch loop (`proto::scheduler::dispatch`) polls for eligible builds and pushes `JobOffer` messages to connected workers with the `build` capability.

**Eligibility:** a build is eligible only when every dependent derivation already has a `build` row in the same evaluation with status `Completed` (3) or `Substituted` (7) — enforced via a `NOT EXISTS` subquery against `derivation_dependency`.

**Worker matching:** `JobOffer` is sent to workers whose `WorkerCapabilities` include the build's target architecture and all required features from `derivation_feature`. Workers score each candidate against their local store (missing required paths) and stream scores back via `RequestJobChunk`. The server assigns to the worker with the lowest `missing` count; ties broken by fewest assigned jobs.

**Execution** (on the worker):
1. Receive `AssignJob` with the full dependency chain in topological order.
2. Send `NarRequest` for missing input paths (known from scoring).
3. Receive input NARs via `NarPush` or presigned S3 URLs.
4. Build each derivation in order via the local Nix daemon (`build_derivation`).
5. Stream `JobUpdate::BuildOutput` for each completed derivation.
6. Compress outputs and send `JobUpdate::Compressing`.
7. Sign outputs and send `JobUpdate::Signing` (if `sign` capability).
8. Send `JobCompleted`.

The server updates `build` and `derivation_output` rows as `JobUpdate` messages arrive, making results visible in the UI immediately.

**Failure cascade:** when a build fails, the server walks reverse `derivation_dependency` edges and marks all downstream builds `DependencyFailed`. The failing build is set to `Failed`.

---

## Binary Cache

**Serving a NAR**
NARs are served with ZSTD compression. They are stored in `${base_dir}/nars/[first 2 chars of hash]/[rest of the hash].nar.zst`.

**Signing**

Each cache has a dedicated Ed25519 signing key encrypted in the database (using the server's crypt secret). `format_cache_key` decrypts it and returns the public key in Nix's `<hostname>-<name>:<base64>` format for use in `trusted-public-keys`.

**Narinfo** (`GET /cache/{cache}/{hash}.narinfo`)

Constructs a `NixPathInfo` response by querying `derivation_output` + `derivation_output_signature`, calling `QueryPathInfo` on the local store for NAR size/hash/references, and converting the NAR hash from hex to Nix's base-32 encoding via `nix hash convert`. Sizes/hashes are read directly from the `derivation_output` row — they were populated once when the derivation was first built and are reused on every subsequent narinfo request.

**Closure presence** (`cache_derivation`)

The cacher maintains the invariant: a `cache_derivation(cache, derivation)` row exists iff every `derivation_output` of `derivation` has `is_cached = true` AND every transitive dependency of `derivation` has its own `cache_derivation` row for the same cache. After caching an output, `try_record_cache_derivation` checks both conditions and inserts the row when they hold; otherwise the next caching pass picks it up. Invalidation walks reverse `derivation_dependency` edges in `revoke_cache_derivation_closure` and deletes every dependent's `cache_derivation` row for the affected cache, since their closure assertion no longer holds.

This makes "is the full closure of build B available in cache C" a single DB lookup against `cache_derivation` instead of a per-output filesystem probe.

---

## Dependency Graph API

`GET /builds/{build}/graph` — BFS from the requested build, capped at 500 nodes. The graph is stored on derivations, so the BFS walks `derivation_dependency` and resolves each visited derivation back to a `build` row in the same evaluation for UI display:

```
root_drv = build.derivation
visited_drvs = {root_drv}
queue = [[root_drv]]

while queue not empty and nodes.len() < 500:
    batch = queue.pop_front()
    fetch derivation_dependency edges where derivation IN batch
    resolve dep drv ids → builds in the same evaluation as the requested build

    for each edge:
        if edge.dependency not in visited_drvs:
            visited_drvs.add(edge.dependency)
            next_batch.push(edge.dependency)

    if next_batch not empty: queue.push(next_batch)
```

Returned `DependencyEdge { source, target }` are build IDs — `source` is the dependency's build and `target` is the dependent's build, so `source` must be built before `target`.

Batching the BFS (one DB round-trip per level) keeps the query count proportional to graph depth rather than node count.

---

## Authentication

**JWT** — `HS256` signed with `GRADIENT_JWT_SECRET`. Payload contains `sub: user_uuid`. Regular tokens expire after 24 hours; `remember_me` tokens after 30 days. Generated in `web::authorization::encode_jwt`.

**API keys** — 32 random bytes encoded as hex, stored hashed in `api.key`, prefixed with `GRAD` when returned to the user. The `authorization::authorize` middleware accepts both token types in the `Authorization: Bearer` header.

**OIDC** — `oidc_login_create` starts the PKCE flow and stores the verifier in the database. `oidc_login_verify` exchanges the code, fetches user info, upserts the user row, and returns a JWT. Endpoint discovery is automatic from `GRADIENT_OIDC_DISCOVERY_URL/.well-known/openid-configuration`.

---

## Worker Registration & Auth

Workers authenticate to the server using a challenge-response flow:

1. A peer (org admin) calls `POST /api/v1/orgs/{org}/workers` with `{"worker_id": "<string>"}`.
2. The server generates a 32-byte random token, stores `sha256(token)` in `worker_registration` with `peer_id = org.id`, and returns `{peer_id, token}`.
3. The worker operator configures `GRADIENT_WORKER_PEERS_FILE` with `peer_id:token` pairs.
4. On connect, the server sends `AuthChallenge { peers }` listing all org IDs that registered this worker ID.
5. The worker responds with `AuthResponse { tokens: {peer_id: token} }`.
6. The server validates each token by comparing `sha256(token)` against the stored hash. The worker is authorized for all peers that pass.

A worker may be authorized for multiple orgs simultaneously — it sees job candidates from all its authorized peers.

---

## State-Managed Resources

Users, organizations, and caches created by the NixOS module configuration carry `managed = true`. The API rejects mutations and deletions of these records with `403 Forbidden`. This allows declarative configuration to be the source of truth without Gradient's UI overwriting it.
