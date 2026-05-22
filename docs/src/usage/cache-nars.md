# Managing cached NARs

Gradient caches hold many individual NARs. This page covers listing,
inspecting, and deleting them through the CLI or the web UI. NAR upload from
a local store is tracked separately under
[issue #261](https://github.com/wavelens/gradient/issues/261).

## CLI

```sh
gradient cache nar list <cache> [--hash <prefix>] [--package <substring>] \
    [--sort created_at|nar_size|last_fetched_at] [--order asc|desc] \
    [--page N] [--per-page N]
gradient cache nar show <cache> <hash>
gradient cache nar delete <cache> <hash> [-y]
gradient cache nar stats <cache>
```

`gradient cache nar list` is paginated. Default page size is 50, max 200.

`gradient cache nar delete` prompts for confirmation unless `-y/--yes` is
passed. In `--json` mode, `--yes` is mandatory (no interactive prompt is
possible).

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
