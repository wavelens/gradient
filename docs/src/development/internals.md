# Internals

Key functions inside each crate.

## Evaluation Pipeline

`builder::scheduler::schedule_evaluation_loop` polls for queued evaluations every 5 seconds, up to `max_concurrent_evaluations` concurrent tasks.

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

Expands the evaluation wildcard (e.g. `packages.x86_64-linux`) into a list of fully-qualified attribute paths, then runs `nix eval --json` to resolve each to a store path.

**4. Build dependency graph** (`query_all_dependencies`)

For each top-level derivation an iterative stack-based traversal is performed (not recursive, to avoid stack overflow on deep trees):

```
stack = [(root_drv, parent_id=None, new_uuid)]

while stack is not empty:
    pop (path, parent_id, build_id)
    references = get_pathinfo(path, local_store).references

    for each ref:
        if already in all_builds or stack → record edge, skip
        if already in DB and in local store → mark Completed, record edge
        if already in DB but missing → re-queue, record edge
        else → push onto stack with (ref, build_id, new_uuid)

    create MBuild for path (status=Queued)
    if parent_id != None → create MBuildDependency(build=parent, dependency=build_id)
```

Deduplication is O(1) via a hash-set of seen derivation paths. References come directly from the Nix daemon's `QueryPathInfo` response.

**5. Batch insert**

Builds and dependencies are bulk-inserted in chunks of 1 000 rows to stay within PostgreSQL's parameter limit. Entry points (top-level build UUIDs) are recorded in the `entry_point` table.

**6. Status transitions**

```
Queued → Evaluating → Building → Completed | Failed | Aborted
```

`update_evaluation_status` and `update_evaluation_status_with_error` write the new status and optional error string atomically.

---

## Build Dispatcher

`schedule_build_loop` polls for queued builds every 5 seconds, up to `max_concurrent_builds` concurrent tasks.

**Server selection** (`reserve_available_server`)

Finds an active server whose:
- `architectures` set includes the build's `architecture`
- `features` set satisfies the derivation's required features

The first matching server is reserved by atomically setting `build.server = server.id` and `build.status = Building`.

**Build execution** (`schedule_build`)

1. Decrypt SSH private key for the organization.
2. Open SSH connection via `core::executer::connect` (wraps `russh`). Retries up to 3 times with 5-second waits.
3. Resolve sorted dependency order: `get_build_dependencies_sorted` queries `build_dependency` edges and does a topological sort. Dependencies are copied to the remote server first, in order.
4. Copy inputs: send `AddToStoreNar` commands over the Nix daemon wire protocol through the SSH tunnel.
5. Build: send `BuildDerivation` with a `BasicDerivation` constructed from the `.drv` file. Env vars, builder path, args, and output paths are parsed from `nix derivation show --json`. Structured attributes (`structuredAttrs`) are serialized as `__json` in the env, matching Nix C++ behaviour.
6. Copy outputs back: receive `AddToStoreNar` responses, write NARs to disk, update `build_output` rows with store paths.
7. On failure: `update_build_status_recursivly` aborts all dependent builds.

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

Constructs a `NixPathInfo` response by querying `build_output` + `build_output_signature`, calling `QueryPathInfo` on the local store for NAR size/hash/references, and converting the NAR hash from hex to Nix's base-32 encoding via `nix hash convert`.

---

## Dependency Graph API

`GET /builds/{build}/graph` — BFS from the requested build, capped at 500 nodes:

```
visited = {build_id}
queue = [[build_id]]

while queue not empty and nodes.len() < 500:
    batch = queue.pop_front()
    fetch builds in batch
    fetch build_dependency edges where build IN batch

    for each edge:
        if dep not in visited:
            visited.add(dep)
            next_batch.push(dep)

    if next_batch not empty: queue.push(next_batch)
```

Edges are `source → target` where `source` must be built before `target`, matching the `build_dependency` table's `(build, dependency)` semantics inverted: the returned `DependencyEdge { source: dep.dependency, target: dep.build }`.

Batching the BFS (one DB round-trip per level) keeps the query count proportional to graph depth rather than node count.

---

## Authentication

**JWT** — `HS256` signed with `GRADIENT_JWT_SECRET`. Payload contains `sub: user_uuid`. Regular tokens expire after 24 hours; `remember_me` tokens after 30 days. Generated in `web::authorization::encode_jwt`.

**API keys** — 32 random bytes encoded as hex, stored hashed in `api.key`, prefixed with `GRAD` when returned to the user. The `authorization::authorize` middleware accepts both token types in the `Authorization: Bearer` header.

**OIDC** — `oidc_login_create` starts the PKCE flow and stores the verifier in the database. `oidc_login_verify` exchanges the code, fetches user info, upserts the user row, and returns a JWT. Endpoint discovery is automatic from `GRADIENT_OIDC_DISCOVERY_URL/.well-known/openid-configuration`.

---

## State-Managed Resources

Users, organizations, servers, and caches created by the NixOS module configuration carry `managed = true`. The API rejects mutations and deletions of these records with `403 Forbidden`. This allows declarative configuration to be the source of truth without Gradient's UI overwriting it.
