# AUDIT-FILESTORAGE.md - File & Object Storage

The storage abstraction layer: how NARs, build logs, source archives, eval-cache
blobs, and build-request blobs are persisted to object storage (S3) or local
disk, including presigned URLs, resumable/partial staging, the on-disk key
layout, and the object-vs-`cached_path` consistency boundary. Produced by a
first-hand read of `backend/gradient-storage` plus the server ingest boundary.
File:line references are against `main` at audit time.

Headline finding: the storage crate is a thin, mostly-clean wrapper over
`object_store`, but it has three structural problems. (1) It carries three
near-identical object families (NAR / eval-cache / blob) whose bodies are copied,
with four almost-identical presign methods. (2) Integrity is entirely
trust-the-worker: `NarStore` has no verify method, no server-side rehash exists
on either transport, and the S3 presigned commit records a `cached_path` row via
`ingest_metadata_only` without ever HEADing the object it claims is cached - the
zombie-cached-path manufacturing gap already flagged in AUDIT-PROTOCOL.md.
(3) `PartialStore` is fully synchronous `std::fs` invoked from async on both the
server receive path and the worker, and magic constants (zstd levels, chunk
sizes, presigned/partial TTLs) are re-declared per crate.

## Scope

Files in scope:
- `backend/gradient-storage/src/`: `nar.rs` (627), `log.rs` (527),
  `partial.rs` (291), `nar_extract.rs` (288), `sgr.rs` (192),
  `source_nar.rs` (180), `log_chunk.rs` (164), `email.rs` (311),
  `context.rs` (19), `lib.rs` (21).
- Storage-facing config in `backend/gradient-types/src/cli/storage.rs`.
- The server-side ingest boundary in `backend/gradient-proto/src/ingest.rs`
  (shared with AUDIT-PROTOCOL.md).
- Local-vs-S3 dispatch and log-backend selection in
  `backend/gradient-core/src/lib.rs:127-180`.
- Consistency machinery this layer feeds: `cleanup_orphaned_cache_files` /
  `purge_zombie_cached_paths` (`backend/gradient-cache/src/cacher/cleanup.rs`)
  and `demote_cached_output` (`backend/gradient-db/src/cache_storage.rs`).

## Storage - current flow

### Backend selection (one place: gradient-core)

```
init_state (gradient-core/src/lib.rs:127)
  |
  +- cli.s3_config() is Some ?
  |     yes -> NarStore::s3(bucket,region,endpoint,key,secret,prefix,vhost)   (nar.rs:64)
  |              store.ping() HEAD probe object                               (nar.rs:125)
  |     no  -> NarStore::local(base_path)                                     (nar.rs:46)
  |
  +- log_storage (lib.rs:171)
        s3_config Some -> S3LogStorage::new(FileLogStorage, nar.inner(), nar.prefix())  (log.rs:281)
        else           -> FileLogStorage                                                (log.rs:81)

  StorageCtx { nar_storage, log_storage, email }   (context.rs:15)
```

`NarStore` (nar.rs:20) is the unified abstraction over `Arc<dyn ObjectStore>`
(`object_store::local::LocalFileSystem` or `object_store::aws::AmazonS3`). It
holds four fields: `inner` (the erased store), `prefix` (non-empty S3 only),
`local_base` (Some for local, used by the orphan scan), and `s3_signer`
(`Option<Arc<AmazonS3>>`, present only for S3 so presigning has a concrete
signer). `inner()` is re-exported so `S3LogStorage` shares the same connection.

### Object key layout

```
NAR:            {prefix}nars/{hash[..2]}/{hash[2..]}.nar.zst        object_path      (nar.rs:110)
eval-cache:     {prefix}eval-cache/{fingerprint}                    eval_cache_path  (nar.rs:314)
build-request:  {prefix}build-request-blobs/{org-uuid}/{hex[..2]}/{hex}  blob_path   (nar.rs:409)
log (S3):       {prefix}logs/{attempt_id}.log                       object_path      (log.rs:294)
log chunk (S3): {prefix}logs/{attempt_id}/chunk_{index:08}.zst      chunk_object_path(log.rs:298)
log (local):    {base}/logs/{shard}/{attempt_id}.log               log_path         (log.rs:104)
log chunk(loc): {base}/logs/{shard}/{attempt_id}/chunk_{index:08}.zst chunk_path     (log.rs:112)
```

All NARs are stored pre-compressed (`.nar.zst`); the server never packs or
compresses a NAR (compression happens on the worker, or in `source_nar.rs` for
server-materialised sources). The two-char shard is the first two hex chars of
the store hash for NARs, and the final UUID byte for logs (`FileLogStorage::shard`,
log.rs:96). `object_path` defends the formatter against a too-short hash with a
`"__"` shard (nar.rs:114) rather than validating - `NarStore` trusts the key
string it is handed (validation lives at callers, e.g. `parse_store_path`,
ingest.rs:44).

### NarStore operations

```
put(hash, Vec<u8>)                    single PUT                            nar.rs:135
exists(hash) -> bool                  single HEAD (idempotent-write guard)  nar.rs:147
put_streaming(hash, chunk_size)       put_multipart -> WriteMultipart       nar.rs:159
get(hash) -> Option<Vec<u8>>          buffered GET                          nar.rs:168
get_stream(hash) -> (size, stream)    streaming GET, no buffering           nar.rs:185
get_stream_from(hash, offset)         range GET (HEAD for full size)        nar.rs:208
delete(hash)                          DELETE (NotFound = Ok)                nar.rs:244
presigned_get_url(hash, expires)      Signer::signed_url GET, None local    nar.rs:262
presigned_put_url(hash, expires)      Signer::signed_url PUT, None local    nar.rs:290
list_hashes[_with_modified]()         list nars/, rebuild hash from path    nar.rs:449
```

Plus a full parallel set for eval-cache blobs (`put_eval_cache`/`get_eval_cache`/
`get_eval_cache_stream`/`presigned_eval_cache_{get,put}_url`/`delete_eval_cache`,
nar.rs:318-405) and a partial set for build-request blobs (`put_blob`/`get_blob`/
`delete_blob`/`list_blobs`, nar.rs:418-520). These three families have
structurally identical bodies differing only in the path helper.

### Presigned URL flow (S3 only)

```
worker CacheQuery{Pull}  --> handler/cache.rs:248  presigned_get_url(hash, expire) --> S3 signed GET
worker CacheQuery{Push}  --> handler/cache.rs:288  presigned_put_url(hash, expire) --> S3 signed PUT
                              (build_uncached_push_entry; url:None on local forces WS relay)
expire = Duration::from_secs(3600)   hardcoded at cache.rs:522 and cache.rs:697
```

Presigning uses `object_store::signer::Signer::signed_url`; local-disk stores
return `None`, which forces the WebSocket relay transport. The expiry is passed
in by the caller and is a hardcoded one-hour literal at both call sites; two more
independent one-hour constants exist elsewhere (`PRESIGN_TTL`
eval_cache.rs:33, `UPLOAD_SESSION_TTL` constants.rs:10).

### Resumable staging (PartialStore)

```
PartialStore (partial.rs:27)  root + ttl,  ALL sync std::fs
  files:  {root}/{key}.partial   +   {root}/{key}.token   (stream_token sidecar)

  received_len(key, token)  read .token; token mismatch -> discard, return 0   partial.rs:68
  append(key, token, off, data)  off==0 truncates; else off must == len (bail) partial.rs:92
  staged_len / token / read_all / discard                                      partial.rs:122-155
  total_bytes()  sum over walk()                                               partial.rs:158
  gc()  delete .partial older than ttl (ttl==0 disables)                       partial.rs:164
  walk_dir()  recursive, one level of {peer}/ nesting                          partial.rs:201

  three independent callers, three roots:
    worker main.rs:80            nar_partial_dir()          (worker pull staging)
    web upload.rs:159            {base}/nar-upload-partial  (chunked cache PUT)
    proto NarReceiveStore        dispatch.rs                (server push-resume)
  key namespacing: raw hash (pull) | {peer_id}/{hash} (server push) | {job_id}/{hash} (worker pull)
```

Resume is contiguity-enforced: `append` requires `offset == current len` or
`bail!`, and `offset == 0` always truncates so a restarted transfer never trips
the check. A `stream_token` mismatch (e.g. a worker zstd upgrade changed the
byte stream) truncates to 0 via `received_len`. This half of the resume protocol
is solid; the smell is that it is entirely blocking I/O called from async
(see below).

### Log storage (trait + two impls)

```
LogStorage trait (log.rs:22): append read finalize delete list_logs
  write_chunk read_chunk delete_chunks delete_inline_log reassemble_chunks

FileLogStorage (log.rs:81)  local only
  append -> tokio::fs append + flush (log.rs:149)
  read   -> local file, else reassemble_chunks fallback (log.rs:167)
  finalize -> no-op (default)
  shard_existing_logs -> one-time startup relocation of pre-sharding flat files (log.rs:120)

S3LogStorage (log.rs:266)  wraps FileLogStorage local + Arc<dyn ObjectStore> + prefix
  append   -> delegate to local (S3 has no efficient append)          log.rs:307
  read     -> local file, else S3 object, else S3 chunks (3-tier)     log.rs:311
  finalize -> upload local live file to S3, keep local as read cache  log.rs:330
  write_chunk -> S3 only, never mirrored to local disk                log.rs:387
  read_chunk  -> local chunk cache, else S3                           log.rs:407
```

Live logs append to a local file (fast path); on terminal state `finalize`
ships the file to S3 and `compress_and_store_chunks` (log_chunk.rs:84) writes
line-bounded zstd chunks. `chunk_log` (log_chunk.rs:28) splits on line
boundaries at ~`log_chunk_bytes` (config default 262144), recording the active
SGR color state per chunk (sgr.rs) so each chunk renders standalone.

### Source-NAR materialisation (server-authoritative)

`materialise_source_nar` / `source_nar_from_bytes` (source_nar.rs:52,59) pack a
staging directory into a canonical NAR via `harmonia_file_nar::NarByteStream`,
SHA-256 it to `nar_hash`, compute the `/nix/store/<hash>-source` path with
`make_store_path_from_ca`, then zstd-6 compress and record `file_hash`/`file_size`
of the compressed bytes - the same `.nar.zst` shape `NarStore` persists. This is
the one place in this crate that computes hashes.

### NAR extraction

`nar_extract.rs` extracts a single path from a zstd NAR: a file returns its
bytes, a directory returns a `tar.zst` of the subtree (`TAR_ZSTD_LEVEL = 1`). It
buffers the entire compressed NAR in memory (`MAX_PREALLOC = 16 MiB` per file
cap) and is intended for small build products, not large outputs.

## Integrity & consistency

### The object <-> cached_path invariant

The intended invariant is: `cached_path.file_hash IS NOT NULL` iff the object
`nars/xx/rest.nar.zst` physically exists. It is never enforced transactionally;
it is written best-effort and repaired asynchronously.

```
ingest_nar (ingest.rs:53)
  1. put_nar_idempotent  -> object          (nar written FIRST)   ingest.rs:62
  2. upsert_and_sign     -> cached_path row + refs + signatures    ingest.rs:63
  "NAR written first; DB failure leaves an unreferenced blob - GC reclaims it" (ingest.rs:61)

ingest_metadata_only (ingest.rs:97)   <-- S3 presigned commit path
  upsert_and_sign only; NO object write, NO HEAD, NO verify
```

`put_nar_idempotent` (ingest.rs:73) is the one integrity-aware primitive: it
skips the write only when the recorded `file_hash` matches AND `nar_storage.exists`
returns true; a matching-hash-but-object-gone case (a zombie row) re-writes to
restore the invariant. Its four cases are unit-tested (ingest.rs:331-392).

### Hash / size verification is absent server-side

`NarStore` exposes no verify/rehash method at all. On ingest, `file_hash` and
`nar_hash` are stored verbatim (ingest.rs:150-152, normalized only for format).
The relayed transport checks staged length vs the worker-reported `file_size`
and nothing else; the S3 presigned transport checks nothing. There is no code
path where the server recomputes a NAR digest, even though it holds the full
bytes on the relayed path and could cheaply HEAD on the presigned path.

Cross-reference: AUDIT-PROTOCOL.md Part 2 ("Hash / size verification (the weak
spot)") finds the S3 presigned commit in `on_nar_uploaded` skips the whole
`is_active` verify block and jumps to `mark_nar_stored`, so a `NarUploaded`
whose PUT truncated or failed still creates an `is_cached=true` row over a
missing/corrupt object. That gap is realised here as the `ingest_metadata_only`
call: the storage layer offers `exists()` (a single HEAD) but the presigned
commit does not call it. This is the single highest-severity finding touching
this layer, echoed in AUDIT.md's cross-cutting list.

### The zombie-cached-path class and its repairs

A zombie is a `cached_path` row (`file_hash` set) whose object is gone. Sources:
GC deleting the object (`gc_orphan_derivations`), external S3 lifecycle
expiration, an unverified/truncated S3 PUT, or orphan-file reclamation of a
freshly-uploaded NAR before its rows commit. Three mechanisms interact:

```
Prevention (grace):  cleanup_orphaned_cache_files (cleanup.rs:260)
   list_hashes_with_modified (nar.rs:464) pairs each object with last_modified;
   spare NARs younger than keep_orphan_derivations_hours (cleanup.rs:277) so an
   upload landing before its rows commit is not reclaimed into a zombie.

Repair (sweep):      purge_zombie_cached_paths (cleanup.rs:318)
   delete cached_path rows whose file_hash is set but hash not in the on-disk set
   (batched 8000; cached_path_signature cascades).

Repair (reactive):   demote_cached_output (cache_storage.rs:251)
   delete the row, DELETE the object, reset the producer build to Created so it
   rebuilds; driven by a worker InputsUnavailable / corrupt-NAR report.
```

The grace window depends on object `last_modified`, reconstructed by
`list_hashes_with_modified` from `object_store` metadata (filesystem mtime for
local, upload time for S3) - a minor semantic difference between backends but
both approximate "recently uploaded".

### S3-vs-local parity gaps

- Verification is coupled to backend choice, not to a policy. Local returns
  `url: None`, forcing the relay (which at least length-checks); S3 returns a
  presigned PUT and commits unverified. Choosing S3 silently drops the only
  server-side check that exists.
- Idempotent-write guard (`exists` HEAD in `put_nar_idempotent`) runs for both
  but is S3-motivated: it prevents piling up retained versions on a
  versioning-enabled bucket (documented ingest.rs:71, nar.rs:143). Local has no
  versioning, so the guard is dead weight there.
- Log storage: S3 keeps a local read cache and a no-op-to-real `finalize`;
  local uses files directly with a no-op `finalize`. Two divergent read
  fallbacks (log.rs:167 vs log.rs:311).
- `put_streaming` multipart works on both (LocalFileSystem emulates), but no NAR
  ingest path currently uses it - all inbound bytes are buffered.

## Messiness & code smells

Ranked by impact, with file:line.

1. `nar.rs` (627) carries three copy-pasted object families and four
   near-identical presign methods. NAR (nar.rs:135-309), eval-cache
   (nar.rs:314-405), and blob (nar.rs:409-520) each re-implement put/get/delete
   with the same `NotFound => Ok(None/false)` + `.context(...)` shape; only the
   path helper differs. `presigned_get_url` / `presigned_put_url` /
   `presigned_eval_cache_get_url` / `presigned_eval_cache_put_url`
   (nar.rs:262,290,361,381) are byte-for-byte identical except path and method.

2. `log.rs` (527) duplicates behavior between `FileLogStorage` and
   `S3LogStorage`. The three-tier read fallback (local file -> object -> chunks)
   is written twice (log.rs:167, log.rs:311); `delete`/`delete_chunks`/
   `list_logs` each re-thread local-then-S3; the prefix-normalization block
   (`if prefix.is_empty() || prefix.ends_with('/')`) is copy-pasted from
   nar.rs:96 into log.rs:282.

3. `partial.rs` (291) is entirely synchronous `std::fs` called from async on
   both server and worker. `append` (partial.rs:92), `received_len`
   (partial.rs:68), `read_all` (partial.rs:135), `walk_dir` (partial.rs:201),
   and `gc` (partial.rs:164) all do blocking `File`/`metadata`/`read_dir`/
   `remove_file` with no `spawn_blocking`. Contrast `log.rs`, which uses
   `tokio::fs`. On a busy receive path this parks tokio worker threads.

4. Duplicated hashing/compression, no shared digest helper. `source_nar.rs`
   computes `nar_hash` + `file_hash` (source_nar.rs:61,81); ingest stores hashes
   verbatim (ingest.rs:150); the worker has its own `verify_hash` (per
   AUDIT-PROTOCOL smell 5). There is no single `NarDigest`/`verify` in the
   storage crate, so the layer that owns the objects cannot check them.

5. Magic constants re-declared per crate. zstd level appears as `6`
   (source_nar.rs:25), `1` (nar_extract.rs:29), and inline `0`
   (log_chunk.rs:93). Presigned expiry is `Duration::from_secs(3600)` at
   cache.rs:522 and cache.rs:697, plus `PRESIGN_TTL` (eval_cache.rs:33) and
   `UPLOAD_SESSION_TTL` (constants.rs:10) - three independent one-hour literals.
   `MAX_PREALLOC = 16 MiB` (nar_extract.rs:25) is a fourth ad-hoc size ceiling.
   AUDIT-PROTOCOL smell 10 flags the same sprawl on the transport side.

6. `list_hashes_with_modified` reconstructs the hash by string-splitting the
   object path (nar.rs:471-480) instead of sharing the inverse of `object_path`
   (nar.rs:110). The layout is now encoded in two places that can drift.

7. `email.rs` (311) does not belong in a storage crate. SMTP delivery via lettre
   plus two ~40-line inline HTML templates (verification email.rs:128, password
   reset email.rs:211) with duplicated boilerplate. It rides along only because
   `StorageCtx` bundles it (context.rs:17).

8. `NarStore` never validates the hash it keys on. `object_path`'s `"__"` shard
   fallback (nar.rs:114) papers over a too-short/empty hash rather than
   rejecting it; key safety depends entirely on callers and on
   `object_store::Path` sanitisation.

9. `nar_extract.rs` buffers the whole compressed NAR in memory and still exports
   a `#[deprecated]` `ExtractedFile` alias (nar_extract.rs:56) plus a
   compatibility shim `extract_file_from_nar_bytes` (nar_extract.rs:69) - dead
   surface kept for callers that should have migrated.

10. `S3LogStorage::delete_chunks` lists then deletes objects one at a time
    (log.rs:420-433); no bulk delete. Fine at low volume, a latency cliff on S3
    for logs with many chunks.

## Refactoring recommendations

Ranked by impact; aligned with AUDIT.md's "one legible flow" north star and the
NAR-transfer unification proposed in AUDIT-PROTOCOL.md.

1. Close the S3 verification gap at the storage boundary. Give `NarStore` a
   `verify(hash, expected_file_hash, expected_size)` that at minimum HEADs and
   compares size, and optionally GETs and rehashes. Have the presigned commit
   (the `ingest_metadata_only` call from `on_nar_uploaded`, AUDIT-PROTOCOL
   dispatch.rs:994) invoke it before `mark_nar_stored`, reusing the existing
   `exists` HEAD (nar.rs:147). This makes the object <-> row invariant hold on
   both transports instead of only the relayed one. Highest-value change; small.

2. Collapse the three object families into one generic keyed accessor.
   A private `object` helper (or `ObjectFamily { path_fn }`) parameterised by
   the path builder gives put/get/get_stream/delete/list/presign once, with NAR,
   eval-cache, and blob as thin path wrappers. Fold the four presign methods
   into one `presign(path, method, expires)`. Removes roughly half of nar.rs.

3. Centralise storage constants. Add a `storage::config` (or extend the
   `gradient-types` CLI/constants) owning zstd levels, chunk sizes, presigned
   TTL, and partial TTL; delete the three independent 3600 literals and the
   scattered zstd-level and size ceilings. This is the storage-side half of
   AUDIT-PROTOCOL rec 7 (centralize into `nar_transfer::config`); do it once,
   shared across both crates.

4. Make staging non-blocking and give it one home. Convert `PartialStore` to
   `tokio::fs` (or wrap every call in `spawn_blocking`), and fold it into the
   proposed `nar_transfer` module (AUDIT-PROTOCOL recs 2 and 6) so the server
   `NarReceiveStore`, the worker pull staging, and the web chunked-upload
   staging share one resumable-staging type instead of three call sites over the
   same primitive.

5. Extract shared layout + prefix helpers. One prefix-normalization function
   and one `hash <-> object key` codec shared by `NarStore` and `S3LogStorage`;
   `list_hashes` reuses the codec instead of re-splitting strings (kills smell 6
   and the log.rs:282 copy).

6. Consolidate hashing into a `NarDigest` helper used by `source_nar`, the ingest
   verify path (rec 1), and the worker (AUDIT-PROTOCOL rec 4). One definition of
   "compute file_hash/nar_hash/size", one definition of "verify against
   expected", shared across worker push, worker import, and server recompute.

7. Unify the object-store handle. `NarStore` and `S3LogStorage` already share
   `nar_storage.inner()`/`prefix()` (core lib.rs:175). A single `StorageBackend`
   owning the `ObjectStore` + prefix + optional signer would remove the
   duplicated prefix logic and the local/S3 log branching in `init_state`.

8. Move `email.rs` out of `gradient-storage`. Email delivery is not storage;
   relocate it (e.g. a `gradient-notify` module or into `gradient-web`) and
   extract the two inline HTML bodies into templates. Removes a cross-concern
   from the crate and shrinks `StorageCtx` to actual storage. Retire the
   `nar_extract` `#[deprecated]` alias and file shim while touching the crate.

## Related

- AUDIT-PROTOCOL.md Part 2 (NAR upload path) covers the two transports, the
  worker upload origins, and the server ingest in `on_nar_uploaded`; its
  headline finding that the S3 presigned commit skips server-side verification
  is the transport-side view of this audit's integrity gap (recommendation 1).
- AUDIT.md cross-cutting list ("the S3 presigned NAR upload path skips
  server-side verification entirely") and the "one legible flow" north star.
- AUDIT-SCHEDULER.md (GC and reconciliation) for the `demote_cached_output` /
  zombie-cached-path repair paths this layer feeds.
