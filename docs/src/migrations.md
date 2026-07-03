# Database migrations

Gradient uses [SeaORM migrations](https://www.sea-ql.org/SeaORM/docs/migration/setting-up-migration/). Migration files live in `backend/gradient-migration/src/` and are registered in `backend/gradient-migration/src/lib.rs`. Each NixOS service start runs `Migrator::up(&db, None)` (see `backend/gradient-db/src/connection.rs`), which applies every registered migration not yet recorded in the `seaql_migrations` table.

## Baseline

`m20241101_000000_baseline` replaces the 151 pre-globalization migrations (`m20241107_135027_create_table_user` through `m20260619_000001_drop_cached_path_store_path`, issue #478). On a fresh database it emits the schema exactly as that chain left it (a cleaned `pg_dump` of the real chain, verified by dump diff, plus the constant `cache_role` seed rows); the post-globalization migrations replay on top unchanged. On an already-provisioned database it detects the existing schema and no-ops, while `prune_removed_migrations` drops the deleted files' `seaql_migrations` rows.

Supported upgrade floor: a database must have completed the pre-globalization chain (be at or past `m20260619_010000_globalize_derivation`) before upgrading to a release that ships the baseline. Databases that stopped mid-way through the pre-globalization chain must first upgrade through an older release that still ships it.

Regenerating the baseline after a future squash: run the full historical chain into a scratch PostgreSQL (`initdb` + `DATABASE_URL=... cargo run -p gradient-migration -- up -n <boundary>`), `pg_dump --schema-only --no-owner --no-privileges --exclude-table=seaql_migrations`, strip the psql `\restrict` and `SET` prelude, re-append any constant seed rows, and verify with a schema+data dump diff between a full-chain database and a baseline-chain database.

## down() policy

Every migration's `down()` is either a real inverse (where cheap and lossless) or an explicit `Err(DbErr::Migration("... is irreversible"))` for lossy transforms (e.g. `globalize_derivation`, the baseline). Silent no-op `down()`s are not allowed: they claim reversibility the migration does not have.

## Squash policy for cancelling pairs

Over time, migrations of the form `add_X` + `drop_X` accumulate as a column is added in one release and dropped in a later one. New installs pay the cost of running every such pair even though the net effect is zero. To keep the migration list manageable, Gradient removes provably-cancelling pairs under the following rules.

### When a pair may be removed

A migration pair `add_X` / `drop_X` may be removed once **both** of these are true:

1. The release containing `drop_X` has shipped, **and** at least one subsequent minor version has shipped on top of it. (This gives live operators a deterministic window where their installs run the drop.)
2. No other migration between `add_X` and `drop_X` reads, writes, or references `X` in a way that the deletion would change its semantics. Verify with `rg -n "<ColumnName>|<column_name>" backend/migration/`.

### What removal must NOT do

- Removal must **not** edit the original `create_table_*` migration that introduced the table containing `X`. Editing original create migrations changes the schema seen by every install path and is out of scope for this policy.
- Removal **may** edit intermediate migrations only when the edit is mechanical (e.g. dropping a column declaration that the `drop_X` migration would have removed seconds later anyway). Example: the rename migration `m20260408_000000_split_build_into_derivation` re-declared `has_artefacts` on the new table; that re-declaration was removed when the `has_artefacts` pair was retired.

### Existing installs

Removing a pair leaves orphan rows in the `seaql_migrations` table on installs that already ran the deleted migrations. SeaORM 1.1's validator rejects this state with "Applied migrations not found in migration list", so Gradient prunes any row whose `version` is no longer in `Migrator::migrations()` at startup, before `Migrator::up`, via `prune_removed_migrations` in `backend/gradient-db/src/connection.rs`. The pruned versions are emitted at `info` so the cleanup is auditable. No per-retirement bookkeeping is required: deleting a migration file and dropping its entry from `migrator/src/lib.rs` is the complete change.

## Retired pairs

| Pair | Removed in | Notes |
| --- | --- | --- |
| `add_has_artefacts_to_build_output` / `drop_has_artefacts_from_derivation_output` | issue #71 | Also removed the matching column re-declaration in `m20260408_000000_split_build_into_derivation`. |
| `add_github_app_enabled_to_organization` / `drop_github_app_enabled_from_organization` | issue #71 | Pure pair; only the two pair-files referenced the column. |
