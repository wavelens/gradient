# Database Migrations

Gradient uses [SeaORM migrations](https://www.sea-ql.org/SeaORM/docs/migration/setting-up-migration/). Files live in `backend/gradient-migration/src/`, registered in its `lib.rs`; every server start applies the ones not yet in `seaql_migrations`.

```mermaid
flowchart LR
    start[Server start] --> prune[prune_removed_migrations]
    prune --> up["Migrator::up"]
    up --> baseline{Fresh database?}
    baseline -->|yes| schema[Baseline emits the schema]
    baseline -->|no| noop[Baseline no-ops]
    schema --> later[Later migrations replay]
    noop --> later
```

## Adding a Migration

1. A new file `mYYYYMMDD_NNNNNN_<name>.rs` in `backend/gradient-migration/src/`, registered in `lib.rs`.
2. The matching entity in `backend/gradient-entity/src/`.
3. A `down()` that is a real inverse, or an explicit `Err(DbErr::Migration("... is irreversible"))` for a lossy change. A silent no-op `down()` is not allowed: such a `down()` claims a reversibility the migration lacks.

## Baseline

`m20241101_000000_baseline` replaces the 151 migrations before globalization (`m20241107_135027_create_table_user` to `m20260619_000001_drop_cached_path_store_path`, #478).

| Database | Baseline does |
|---|---|
| Fresh | Emits the schema that chain left: a cleaned `pg_dump` of the real chain, verified by dump diff, plus the constant `cache_role` seed rows |
| Already provisioned | Detects the schema and no-ops; `prune_removed_migrations` drops the deleted files' `seaql_migrations` rows |

**Upgrade floor:** a database must be at or past `m20260619_010000_globalize_derivation` before upgrading to a release with the baseline. A database stuck earlier upgrades through an older release first.

**Regenerating after a future squash:**

1. Run the full chain into a scratch PostgreSQL: `initdb`, then `DATABASE_URL=... cargo run -p gradient-migration -- up -n <boundary>`.
2. `pg_dump --schema-only --no-owner --no-privileges --exclude-table=seaql_migrations`.
3. Strip the psql `\restrict` and `SET` prelude and re-append constant seed rows.
4. Verify with a schema and data dump diff between a full-chain database and a baseline database.

## Cancelling Pairs

A column added in one release and dropped in a later one leaves an `add_X` / `drop_X` pair every new install runs for nothing. Such pairs are removed under these rules.

**Removable when both hold:**

- The release with `drop_X` has shipped, and at least one later minor release on top of it: live installs get a window to run the drop.
- No migration between the two touches `X` in a way the removal changes. Check with `rg -n "<ColumnName>|<column_name>" backend/gradient-migration/`.

**Removal must not** edit the original `create_table_*` migration of the table: that changes the schema of every install path. Intermediate migrations may change only mechanically, e.g. dropping a column declaration the `drop_X` would remove anyway.

**Existing installs** keep `seaql_migrations` rows for deleted files, which SeaORM rejects ("Applied migrations not found in migration list"). `prune_removed_migrations` (`backend/gradient-db/src/connection.rs`) deletes every row not in `Migrator::migrations()` before `Migrator::up` and logs the pruned versions at `info`. Deleting the file and its `lib.rs` entry is the whole change.

## Retired Pairs

| Pair | Issue | Notes |
|---|---|---|
| `add_has_artefacts_to_build_output` / `drop_has_artefacts_from_derivation_output` | #71 | Also removed the column re-declaration in `m20260408_000000_split_build_into_derivation` |
| `add_github_app_enabled_to_project` / `drop_github_app_enabled_from_project` | #71 | Only the two pair files referenced the column |
