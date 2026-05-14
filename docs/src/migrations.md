# Database migrations

Gradient uses [SeaORM migrations](https://www.sea-ql.org/SeaORM/docs/migration/setting-up-migration/). Migration files live in `backend/migration/src/` and are registered in `backend/migration/src/lib.rs`. Each NixOS service start runs `Migrator::up(&db, None)` (see `backend/core/src/db/connection.rs`), which applies every registered migration not yet recorded in the `seaql_migrations` table.

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

Removing a pair leaves orphan rows in the `seaql_migrations` table on installs that already ran the deleted migrations. This is **intentional and supported**: SeaORM's migrator ignores rows whose migration name is no longer registered — it does not attempt to down-migrate or re-run them. End state is unchanged.

### What is out of scope

A full baseline squash (collapsing all pre-cutoff migrations into a single baseline + stamp-on-detect bootstrap) is out of scope for this policy. That work, if it is ever undertaken, belongs in a post-2.0 design.

## Retired pairs

| Pair | Removed in | Notes |
| --- | --- | --- |
| `add_has_artefacts_to_build_output` / `drop_has_artefacts_from_derivation_output` | issue #71 | Also removed the matching column re-declaration in `m20260408_000000_split_build_into_derivation`. |
| `add_github_app_enabled_to_organization` / `drop_github_app_enabled_from_organization` | issue #71 | Pure pair; only the two pair-files referenced the column. |
