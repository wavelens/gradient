# Cache Serving

The `/cache/{cache}/` route will answer Nix requests. Topics here are signatures, narinfo from the database, pull-through from upstream caches, debug info and status codes for clients. Access and complete closures are on [Cache Closure](../scheduler/cache-closure.md) instead.

```mermaid
sequenceDiagram
    participant N as nix
    participant C as /cache/{cache}
    participant D as Database
    participant U as Upstream
    N->>C: GET <hash>.narinfo
    C->>D: cached_path + signature
    alt known locally
        D-->>C: row
        C-->>N: narinfo (X-Cache HIT)
    else upstream serving
        C->>U: narinfo
        U-->>C: signed narinfo
        C-->>N: re-signed, URL nar/upstream/{id}/... (X-Cache MISS)
    else unknown
        C-->>N: 404
    end
```

## Signing

- **Keys:** One Ed25519 key per cache, encrypted with the crypt secret.
    - The `format_cache_key` function will return the decrypted private key.
    - The `format_cache_public_key` function will return the `<host>-<name>:<base64>` public key.
- **Signer:** Only the server can sign, never the worker. The `sign_into_caches` function (`gradient-graph/src/nar.rs`) will sign inside the NAR commit, for worker and REST uploads alike.
- **Signatures:** A commit will write the `cached_path_signature` row of every subscribed cache. The same statement will sign each row with the cache's key.
    - The sign sweep (`sign_missing_signatures` in `gradient-cache/src/cacher/sign_sweep.rs`) will fill rows inserted with a later subscription.
    - The sweep will also fill rows a commit left unsigned.

## Narinfo

The server will build `GET /cache/{cache}/<hash>.narinfo` from the database only. The server is never re-packing or re-hashing a NAR.

| Source | Fields |
|---|---|
| `derivation_output` | Output of a known derivation |
| `cached_path` | Sizes, hashes, references, `CA:` for content-addressed paths. Fallback for `.drv` files and standalone paths |
| `cached_path_signature` | `Sig:` lines, and the access check |

### Pull-Through

- The server will rewrite the `URL:` of an upstream narinfo to `nar/upstream/{id}/...` paths.
- The server will re-sign an upstream narinfo only after verifying the upstream signature.
- The `X-Cache` header will answer `HIT` for a local narinfo and `MISS` for an upstream one.

## Debug Info

**Response:** The `GET /cache/{cache}/debuginfo/{build_id}` route will mirror Nix's `index-debug-info`.

```json
{"archive": "../nar/<file_hash>.nar.zst", "member": "lib/debug/.build-id/<xx>/<yy>.debug"}
```

- `archive` is relative to the requested key.
- The server will accept both spellings, `<build-id>` (Hydra) and `<build-id>.debug` (`nix copy`).
- The signature join is the access check.
- The `debug_info` index will come from the NARs of paths ending in `-debug` (`separateDebugInfo` outputs).
- An upload will walk its own NAR on a detached task.
- The `cached_path.debug_info_indexed` column will mark a scanned NAR. The `debug-index` sweep will read each `file_hash` at most once.
- A miss will fall through to the upstream caches, with `archive` rewritten through `nar/upstream/{id}/...` paths.
- The server will refuse an absolute `archive` or one outside the upstream root.

## Status Codes

| Case | Answer |
|---|---|
| Unknown key | `404`, never another `4xx`. Substituters and debuginfod clients treat other codes as hard errors instead of trying the next source |
| Disabled cache | `400` |
| Private cache without credentials | `401` |
