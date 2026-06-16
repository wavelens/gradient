# Managing cached NARs

Gradient caches hold many individual NARs. This page covers listing,
inspecting, deleting, and uploading them through the CLI or the web UI.

## CLI

```sh
gradient cache nar list <cache> [--hash <prefix>] [--package <substring>] \
    [--sort created_at|nar_size|last_fetched_at] [--order asc|desc] \
    [--page N] [--per-page N]
gradient cache nar show <cache> <hash>
gradient cache nar delete <cache> <hash> [-y]
gradient cache nar stats <cache>
gradient cache upload --nar-file <file.nar> --narinfo <file.narinfo> <cache>
gradient cache upload [--full-closure] <store-path>... <cache>   # nix feature only
```

`gradient cache nar list` is paginated. Default page size is 50, max 200.

`gradient cache nar delete` prompts for confirmation unless `-y/--yes` is
passed. In `--json` mode, `--yes` is mandatory (no interactive prompt is
possible).

## Uploading NARs

`gradient cache upload` pushes a NAR into a cache. Requires the `writeStore`
permission on the target cache.

### No-nix mode (always available)

Upload a pre-dumped NAR file together with its narinfo metadata. No local
Nix installation is required.

```sh
gradient cache upload \
    --nar-file result.nar \
    --narinfo result.narinfo \
    my-cache
```

`--narinfo` points to a standard `.narinfo` file. The CLI parses it for
store path, NAR hash, NAR size, and references before submitting.

### Nix mode (requires the `nix` Cargo feature)

When the CLI is built with the `nix` feature, store paths can be uploaded
directly from the local Nix daemon. Each path is resolved via harmonia,
NAR-dumped, and uploaded in one step.

```sh
# Upload a single store path
gradient cache upload /nix/store/abc123-hello-2.12.1 my-cache

# Upload the full runtime reference closure of every listed path
gradient cache upload --full-closure /nix/store/abc123-hello-2.12.1 my-cache
```

`--full-closure` walks the runtime reference closure of each given path and
uploads every reachable path in dependency order.

### Size cap

The server enforces a maximum upload size per NAR. The default is 512 MiB
and is controlled by the `GRADIENT_MAX_NAR_UPLOAD_SIZE` environment variable
on the server.

The bundled reverse proxy (nginx/Caddy) caps each HTTP request body at 100 MiB.
NARs larger than that still upload because the CLI splits them across multiple
chunked requests (see below); no single request exceeds the cap.

### Backend endpoints

- `POST /api/v1/caches/{cache}/nars` - single-shot multipart form with a
  `narinfo` JSON part and a `nar` binary part. Suitable for NARs under the
  reverse proxy's 100 MiB request limit.
- `PUT /api/v1/caches/{cache}/nars/{hash}/chunk?offset=N` - append one NAR
  slice to a server-side staging file. The CLI uses this to upload larger NARs
  in 32 MiB chunks so no single request exceeds the proxy limit.
- `POST /api/v1/caches/{cache}/nars/{hash}/finalize` - validate the fully
  staged NAR against its narinfo and ingest it.

The CLI automatically chunks; you do not call these endpoints by hand.

## Web UI

Open the cache page (`/caches/<name>`) and click **NARs** in the header. The
page supports filtering by hash prefix and package substring, sorting by
created, size, or last fetched, and per-row delete (requires edit access to
the cache).

## Ref-counted deletion

When a NAR is shared across multiple caches, deleting it from one cache only
removes that cache's signature row. The underlying NAR blob stays alive and
the other caches keep serving it. When the last cache holding a NAR deletes
it:

1. The signature row is removed (sync, in the request's DB transaction).
2. The `cached_path` row is deleted in the same transaction.
3. Any matching `derivation_output.is_cached` rows are set to `false`.
4. The NAR blob is removed from object storage asynchronously, after the
   HTTP response.

This mirrors how nix's own garbage collector handles paths with multiple
references.

## Permissions

- **List / show / stats / available:** anyone who can view the cache. Public
  caches are open; private caches require an API key or session belonging to
  the cache owner.
- **Delete:** the cache owner only. Matches existing `PATCH /caches/{cache}`
  semantics.
- **Upload (`writeStore`):** callers must hold the `writeStore` cache
  permission. Returns `403` otherwise.
