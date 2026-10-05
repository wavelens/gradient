# NAR Storage

NAR object locations, the write path into storage and the sync between storage and database. Delivery of NARs to clients is on [Cache Serving](cache-serving.md) instead.

```mermaid
flowchart LR
    w[Worker] -->|presigned PUT| s3[(S3)]
    w -->|passthrough /proto| srv[Server]
    cli[nix copy] -->|REST upload| srv
    srv -->|adopt / put| store[(NAR store)]
    store --- db[(cached_path)]
    gc[Deep GC] -.->|check| store
    gc -.-> db
```

## Layout

```text
${baseDir}/nars/<2 chars>/<rest>.nar.zst
```

- Named by the **store-path hash**.
- The server will presign upload URLs before the worker has the content hash.
- The narinfo will advertise `nar/<file_hash>.nar.zst` as the URL.
- The `resolve_effective_hash_db` function will map the file hash back to the key.
- `last_fetched_at` and `fetch_count` on `cached_path` feed eviction.

## Writes

| Path | Write |
|---|---|
| Presigned upload (S3) | The worker is writing to S3 directly. The server is completing multipart on `UploadFinished` |
| Passthrough (local storage) | Every upload is streaming through the server. The commit is checking length and SHA-256, then moving the file in with `adopt_file` |
| REST upload (`nix copy`) | `import_nar_reader` -> `put_nar_idempotent_reader`: skipped for a stored path, see [First Content](#first-content) |

### First Content

Stored paths keep the first content they received, like valid paths in Nix. Non-reproducible builds can produce other bytes for one input-addressed path.

- **Stored:** A confirmed `cached_path` row with a `file_hash` (`MCachedPath::is_stored`).
- **Grant:** Workers asking to upload a stored path get a `Skip` grant. They drop the NAR and count the output as uploaded.
- **Writes:** Passthrough, REST and SSH leave the object of a stored path untouched. Only identical bytes can replace a missing object.
- **Commit:** The NAR commit must keep the content fields of a stored row. Only `deriver` can change.
- **Signatures:** Workers with other bytes get the stored content signed into their project caches. REST and SSH clients with other bytes get no signature.
- **SSH:** An `AddMultipleToStore` with other bytes must succeed. The server can drain and drop those bytes.
- **Concurrent Uploads:** Two first uploads at the same moment can still race. The verify-on-read self-heal can demote a path with an object not matching its `file_hash`.

!!! warning "Bucket Requirements"
    - No object versioning, object lock or replication. Object lock and replication force versioning.
    - Versioned buckets keep a copy of every deleted NAR. No S3 GC can reclaim those copies.
    - An `AbortIncompleteMultipartUpload` lifecycle rule (e.g. 7 days). NARs over 1 GiB go up as multipart. A worker dying mid-upload will leave the parts behind.

## Build Logs

Build logs share the NAR backend (`gradient-storage/src/log.rs`). The shard is the last byte of the `BuildAttemptId` value. That byte is the random tail of a UUIDv7.

```text
logs/<last 2 chars>/<attempt>.log                  # live log
logs/<last 2 chars>/<attempt>/chunk_<n>.zst        # finalized
```

- **Live:** appended to `${baseDir}/logs/...` while the attempt is active, also with S3 (S3 has no append).
- **Finalized:** The `finalize_build_log` function will append the live file as zstd chunks after any earlier chunks. The function will index the chunks in `build_log_chunk` and drop the live file. The chunks go only to `<prefix>logs/...` with S3.
- **Finalize Triggers:** The shared build turning terminal. A new attempt replacing the latest one (abort, lost worker, retry). An upstream log arriving after the build (log substitution). Every trigger will queue a `LogFinalize` pending delivery (`pending_delivery` row) for the attempt.
- **Missing Chunks:** A chunk with a missing object will render as one `[log chunk unavailable]` line per indexed line. A re-finalize will keep one such line in its place.
- **Reclaimed:** The orphan-derivation GC will reclaim logs together with their derivation. The [deep GC](#deep-gc) log pass will reclaim logs without a `build_attempt` row.

## Storage Migrations

Layout changes of NAR, log and blob storage come as storage migrations, the storage counterpart of the database migrations (`gradient-cache/src/cacher/storage_migrations/`).

- **Units:** A migration will list its units in ascending order. Each unit is idempotent and will run again in full after a restart interrupted the unit.
- **Ledger:** The `storage_migration` table will hold one row per migration. The `checkpoint` column will name the last finished unit. The `applied_at` column will mark the migration as done.
- **Order:** Pending migrations are running in registry order, one unit per `gc.deepPaceMs`. Pending migrations are running before any [deep GC](#deep-gc) unit. The deep GC will read only the current layout.
- **Readers:** A migration moving live objects will keep the old location readable until the migration is complete.

| Migration | Units | Change |
|---|---|---|
| `m20261001_000000_shard_build_logs` | `local`, `s3` | Flat pre-shard `logs/<attempt>.log` and `logs/<attempt>/` entries move into their shard on local disk. The migration is deleting these entries with their `build_log_chunk` rows on S3 |

## Deep GC

The deep GC will check every storage backend against the database, one unit at a time (`gradient-cache/src/cacher/deep_gc/`). A round will walk every unit once, in ascending key order.

| Unit | Removed |
|---|---|
| `blobs` | `build-request-blobs/...` objects and `build_request_blob` rows without a partner |
| `logs/<xx>` | Logs of one shard, named by `BuildAttemptId`, without a `build_attempt` row. An attempt without a log is legitimate |
| `nars/<xx>` | One of the 1024 two-character NAR shards. Objects past `gc.narUploadGraceHours` without a keeping row. Confirmed `cached_path` rows of the shard (an index range on `hash`) with a missing object. Maintenance is evicting stale live paths |
| `partials` | Unfinished uploads under `nar-partial`, `nar-upload-partial` and `source-upload-partial` older than `nar.partialTtlSecs`, and directories left empty by the older nested layout. No other sweep is walking these roots. A walk on a session or request path would stall that path behind the filesystem |

- **Rounds:** an `admin_task` row, `kind = deep_gc`, `pending` -> `running` -> `completed`. The partial unique index `admin_task_one_active_per_kind` will allow one active round.
- **Background:** A round will start `gc.deepIntervalSecs` after the last round finished. A value of `0` will disable background rounds. A background round will run one unit per `gc.deepPaceMs`.
- **Requested:** A `POST /api/v1/admin/maintenance/deep-gc` call (superuser, `202`) will start a round. A `POST` during an active round will send that round back to its first unit. A requested round will run its units back to back.
- **Checkpoint:** The `admin_task.checkpoint` column will name the last finished unit. The `progress` column will hold the running report.
    - The `GET /api/v1/admin/tasks[/{task_id}]` endpoint will read both.
    - A server restart will resume the round after its checkpoint.
- **Restart Race:** The server will save a checkpoint only while `started_at` is still equal to the start of the round that took the unit. A `POST` in between will reset `started_at` and win.
- **Failure:** A failed unit will leave the checkpoint in place. The next tick will retry the same unit.
