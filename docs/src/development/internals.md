# Internals

Key functions inside each crate.

## Evaluation Pipeline

`builder::scheduler::schedule_evaluation_loop` polls for queued evaluations every 60 seconds, up to `max_concurrent_evaluations` concurrent tasks.

For each evaluation, `schedule_evaluation` runs:

**1. Resolve context**

Looks up the project (or `DirectBuild`) and organization. Opens a connection to the local Nix daemon (`get_local_store`), which returns a `LocalNixStore` — either a Unix socket to `/nix/var/nix/daemon-socket/socket` or a fallback command-duplex subprocess.

**2. Fetch the flake** (`evaluator::evaluate`)

```
prefetch_flake(state, repo_url_at_commit, organization)
```

Converts the repository URL and commit hash into a Nix flake reference:
`git+https://host/repo?rev=<sha1>`. Then runs `nix flake prefetch` to populate the local store. SSH credentials for private repos are decrypted from the database using the organization's crypt key.

**3. Enumerate derivations**

```
get_flake_derivations(state, repo_ref, wildcards, org)
```

Expands the evaluation wildcard into a list of fully-qualified attribute paths using an embedded Nix expression, then resolves each to a store derivation path via the Nix C API.

Wildcard segments:

- `*` — **recursive**: matches any attribute name and, when at the trailing position, descends one additional level to recover derivations hidden by consecutive-wildcard collapsing (`packages.*.*` and `packages.*` are equivalent).
- `#` — **non-recursive**: matches any attribute name at exactly that depth and checks `type == "derivation"` without descending further. Use this to target a specific nesting level precisely.
- `!prefix` — **exclusion**: removes exact paths from the collected set (wildcards in exclusions are not allowed).

**4. Build dependency graph** (`query_all_dependencies`)

The BFS walks **derivations**, not builds. The build table is only the
per-evaluation attempt log; the dep graph lives on `derivation_dependency` and is
shared across every evaluation that ever touches a derivation path.

```
queue = [(root_drv_path, parent_derivation_id=None) for each root]

while queue not empty:
    (drv_path, parent) = queue.pop_front()

    if seen this drv_path in this eval → reuse derivation_id
    elif EDerivation::find(org, drv_path) is Some → reuse, was_new = false
    else → query nix store for outputs + features, push new MDerivation,
            push ADerivationOutput rows, was_new = true

    if parent is Some → push MDerivationDependency(parent → derivation_id)

    in_store = nix_store.query_missing_paths([drv_path]).is_empty()
    push MBuild(eval, derivation_id,
                status = if in_store { Substituted } else { Created })

    if was_new:
        # Walk references via QueryPathInfo and recurse on each.
        for ref in nix_store.query_pathinfo(drv_path).references:
            queue.push_back((ref, derivation_id))
    elif derivation already has derivation_dependency edges in DB:
        # The closure is authoritative — walk it locally and materialise a
        # fresh MBuild for every member, but do NOT re-fetch references.
        for d in load_closure(derivation_id):
            queue.materialise(d)  # build row only
    else:
        # Existing derivation that was previously stored as a leaf. Treat as
        # a leaf again; the next from-scratch eval will pick up its deps.
```

Deduplication is per derivation path (per organisation): a second evaluation
that hits the same path reuses the existing `derivation` row and inserts only
new `build` rows. Substituted builds skip the scheduler entirely and never
acquire a server.

**5. Batch insert**

Derivations, outputs, dependency edges, and builds are bulk-inserted in chunks
of 1 000 rows in FK order: `derivation` → `derivation_output` →
`derivation_dependency` → `build`. Entry points (top-level build UUIDs) are
recorded in the `entry_point` table after builds are persisted.

**6. Status transitions**

```
build:       Created → Queued → Building → Completed | Failed
                                         ↘ Substituted (already in store)
                                         ↘ Aborted | DependencyFailed
evaluation:  Queued → EvaluatingFlake → EvaluatingDerivation → Building
                                                            → Completed | Failed | Aborted
```

`Substituted` is distinct from `Completed`: it means the derivation was already
in the local Nix store at evaluation time and never ran on a builder
(`build_time_ms` and `server` stay `None`, `log_id` stays `None`). It is treated
as a successful terminal state by `check_evaluation_status` and the scheduler.

`update_evaluation_status` and `update_evaluation_status_with_error` write the new status and optional error string atomically.

---

## Build Dispatcher

`schedule_build_loop` polls for queued builds every 60 seconds, up to `max_concurrent_builds` concurrent tasks.

**Server selection** (`reserve_available_server`)

`get_next_build` joins `build → derivation` so the picker sees the architecture
and required features without re-resolving them. A build is eligible only when
**every** dependent derivation already has a `build` row in the same evaluation
with status `Completed` (3) or `Substituted` (7) — enforced via a `NOT EXISTS`
subquery against `derivation_dependency`.

Once a build is picked, it is matched to an active server whose:
- `architectures` set includes the **derivation's** `architecture`
- `features` set satisfies the **derivation's** required features
  (`derivation_feature` table)

The first matching server is reserved by atomically setting `build.server =
server.id` and `build.status = Building`.

**Build execution** (`schedule_build`)

1. Decrypt SSH private key for the organization.
2. Open SSH connection via `core::executer::connect` (wraps `russh`). Retries
   up to 3 times with 5-second waits.
3. Resolve sorted dependency order: `get_build_dependencies_sorted` walks
   `derivation_dependency` from `build.derivation`, resolves each edge to a
   `(build, derivation)` pair in the same evaluation, and topologically sorts
   them. Dependencies are copied to the remote server first, in order.
4. Copy inputs: send `AddToStoreNar` commands over the Nix daemon wire protocol
   through the SSH tunnel.
5. Build: send `BuildDerivation` with a `BasicDerivation` constructed from the
   `.drv` file. Env vars, builder path, args, and output paths are parsed from
   `nix derivation show --json`. Structured attributes (`structuredAttrs`) are
   serialized as `__json` in the env, matching Nix C++ behaviour.
6. Copy outputs back: receive `AddToStoreNar` responses, write NARs to disk,
   then **update** the existing `derivation_output` rows (matched by
   `(derivation, name)`) with the resolved hashes / sizes / `has_artefacts`.
   Output metadata is therefore populated **once per derivation**, never
   re-inserted on subsequent evaluations.
7. On failure: `update_build_status_recursivly` walks reverse
   `derivation_dependency` edges, restricted to the current evaluation, and
   marks every dependent build as `DependencyFailed`. The originally failing
   build is set to `Failed`.

**Log streaming**

Build logs are appended to `build.log` in the database as they arrive over the SSH channel. `POST /builds/{id}/log` polls the database every 500 ms and streams new log chunks as NDJSON.

---

## Nix Daemon Wire Protocol (`nix-daemon` crate)

The crate implements the [Nix daemon protocol](https://nixos.org/manual/nix/stable/protocols/nix-archive.html) at the binary level.

Key operations used:

| Operation | When used |
|---|---|
| `QueryPathInfo` | During evaluation to get a derivation's `references` and NAR hash |
| `QueryMissing` | Check which paths need to be built (not already in store) |
| `AddToStoreNar` | Copy a NAR to the remote daemon store |
| `BuildDerivation` | Execute a derivation on the remote builder |

The `DaemonStore<C>` type is generic over an async read+write stream `C`, so the same code works over a local Unix socket and over an SSH channel.

---

## Binary Cache

**Serving a NAR**
Nars are currently only served with ZSTD compression. Currently nars are stored in ${base_dir}/nars/[first 2 chars of hash]/[rest of the hash].nar.zst

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

Returned `DependencyEdge { source, target }` are still build IDs — `source` is the dependency's build and `target` is the dependent's build, so `source` must be built before `target`. Because edges are stored once per derivation pair (not per evaluation), the same lookup serves every evaluation that touches those derivations, and the resolved build IDs reflect the current evaluation's attempt rows.

Batching the BFS (one DB round-trip per level) keeps the query count proportional to graph depth rather than node count.

---

## Authentication

**JWT** — `HS256` signed with `GRADIENT_JWT_SECRET`. Payload contains `sub: user_uuid`. Regular tokens expire after 24 hours; `remember_me` tokens after 30 days. Generated in `web::authorization::encode_jwt`.

**API keys** — 32 random bytes encoded as hex, stored hashed in `api.key`, prefixed with `GRAD` when returned to the user. The `authorization::authorize` middleware accepts both token types in the `Authorization: Bearer` header.

**OIDC** — `oidc_login_create` starts the PKCE flow and stores the verifier in the database. `oidc_login_verify` exchanges the code, fetches user info, upserts the user row, and returns a JWT. Endpoint discovery is automatic from `GRADIENT_OIDC_DISCOVERY_URL/.well-known/openid-configuration`.

---

## State-Managed Resources

Users, organizations, servers, and caches created by the NixOS module configuration carry `managed = true`. The API rejects mutations and deletions of these records with `403 Forbidden`. This allows declarative configuration to be the source of truth without Gradient's UI overwriting it.
