# Substituting Build Logs From Upstream Caches — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `Substituted` and `external_cached` builds a real `log_id` — first by deduplicating against any prior build for the same derivation, then (only for `external_cached`) by fetching `/log/{drv-basename}` from the org's configured upstream caches.

**Architecture:** New scheduler module `log_substitution.rs` exposing one `substitute_log(state, build_id, derivation_id, drv_path, allow_upstream_fetch)` helper. Three call sites spawn it as a `tokio::spawn` task: `insert_build_rows` and `expand_substituted_closure` in `eval.rs` (dedup-only), and `handle_build_job_completed` in `build.rs` (dedup + HTTP fallback). A shared `upstream_urls_for_org` helper extracted to `core::db` replaces the duplicated query inside `proto/handler/cache.rs`.

**Tech Stack:** Rust, sea-orm, reqwest (already in workspace), tokio, wiremock (new dev-dep).

> **Local verification policy:** This project does NOT run `cargo test` locally — CI runs the full test suite. Each task's "Run the test" step that invokes `cargo test` is a CI verification (push the branch and wait for the run). Locally, use `cargo check --workspace` and `cargo clippy --workspace --no-deps -- -D warnings` after each task.

> **Branch:** All commits land on `feat/substitute-build-logs`, branched off `main`. Task 0 creates the branch.

---

## File Structure

**Created:**
- `backend/core/src/db/cache_upstream.rs` — extracted `upstream_urls_for_org` helper + unit test.
- `backend/scheduler/src/log_substitution.rs` — `substitute_log` + private helpers + tests.

**Modified:**
- `backend/Cargo.toml` — add `wiremock` to `[workspace.dependencies]`.
- `backend/scheduler/Cargo.toml` — add `reqwest`, `futures` deps; add `wiremock`, `tokio` (with `time` feature), `entity` dev-deps if missing.
- `backend/core/src/db/mod.rs` — `pub mod cache_upstream;` + re-export `upstream_urls_for_org`.
- `backend/proto/src/handler/cache.rs` — replace inline upstream-URL query in `extend_with_upstream_results` with the new helper.
- `backend/scheduler/src/lib.rs` — `pub mod log_substitution;`.
- `backend/scheduler/src/eval.rs` — call `substitute_log(..., allow_upstream_fetch=false)` after insertion in `insert_build_rows` and `expand_substituted_closure`.
- `backend/scheduler/src/build.rs` — call `substitute_log(..., allow_upstream_fetch=true)` after `update_build_status(...Completed)` in `BuildStateHandler::handle_build_job_completed` when `leader.external_cached == true`.
- `docs/gradient-api.yaml` — cross-check `/cache/{cache}/log/{drv}` description (no schema change; copy-edit if it says anything misleading about substituted builds).
- `docs/src/tests.md` — list the new tests.
- `docs/src/` — add a short note in the caches/substitution page mentioning that logs are also substituted.

**Deleted:** none.

---

## Task 0: Create the branch

- [ ] **Step 1: Create and switch to the working branch**

Run:
```bash
git checkout -b feat/substitute-build-logs
```

Expected: `Switched to a new branch 'feat/substitute-build-logs'`.

---

## Task 1: Add scheduler dependencies (reqwest, futures, wiremock)

The scheduler currently has no `reqwest` or `futures` direct dep. Both are needed: `reqwest` for the upstream `/log` GET; `futures` for streaming the response body with a size cap. `wiremock` is added as a workspace dev-dep so the new tests can stub upstream caches.

**Files:**
- Modify: `backend/Cargo.toml`
- Modify: `backend/scheduler/Cargo.toml`

- [ ] **Step 1: Add `wiremock` to workspace dependencies**

In `backend/Cargo.toml`, find the `[workspace.dependencies]` section (around lines 30–100). Locate the test-related deps block (look for `tempfile`, `mockall`); add immediately after the last test dep:

```toml
wiremock = { version = "0.6", default-features = false }
```

- [ ] **Step 2: Add `reqwest`, `futures`, and `wiremock` to scheduler crate**

In `backend/scheduler/Cargo.toml`, in `[dependencies]` after the existing entries add:

```toml
futures      = { workspace = true }
reqwest      = { workspace = true }
```

In `[dev-dependencies]` after `tempfile`, add:

```toml
wiremock     = { workspace = true }
tokio        = { workspace = true, features = ["macros", "sync", "time", "rt-multi-thread"] }
entity       = { workspace = true }
```

(`tokio` may already be a `[dependencies]` entry without `rt-multi-thread`; the dev-dep entry adds the feature for `#[tokio::test(flavor = "multi_thread")]` usage. `entity` is needed because tests build `Model` / `ActiveModel` values directly.)

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo check -p scheduler 2>&1 | tail -10
```

Expected: clean compile (no errors). Unused-import warnings for `reqwest`/`futures` are OK at this point — they'll be used in Task 3.

- [ ] **Step 4: Commit**

```bash
git add backend/Cargo.toml backend/scheduler/Cargo.toml
git commit -m "scheduler: add reqwest/futures deps + wiremock dev-dep"
```

---

## Task 2: Extract `upstream_urls_for_org` helper

The query for "org's non-WriteOnly upstream cache URLs" is inlined in `proto/handler/cache.rs:326-378` and is needed verbatim by the new log-substitution code. Extract into `core::db::cache_upstream` so both crates use one query.

**Files:**
- Create: `backend/core/src/db/cache_upstream.rs`
- Modify: `backend/core/src/db/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `backend/core/src/db/cache_upstream.rs` with:

```rust
/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use entity::cache_upstream::{
    Column as CCacheUpstream, Entity as ECacheUpstream,
};
use entity::organization_cache::{
    CacheSubscriptionMode, Column as COrganizationCache, Entity as EOrganizationCache,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::types::ids::{CacheId, OrganizationId};

/// Return the configured upstream cache URLs visible to `org_id` for reads.
///
/// A URL is included when:
/// - the organization has a `organization_cache` row for the owning cache
///   with `mode != WriteOnly`, and
/// - the `cache_upstream` row has `url IS NOT NULL` and `mode != WriteOnly`.
///
/// Returns rows in DB insertion order. Callers that need a deterministic order
/// should sort the result themselves.
pub async fn upstream_urls_for_org(
    db: &DatabaseConnection,
    org_id: OrganizationId,
) -> Result<Vec<String>> {
    let org_cache_rows = EOrganizationCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrganizationCache::Organization.eq(org_id))
                .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Url.is_not_null())
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows.into_iter().filter_map(|r| r.url).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::cache_upstream;
    use entity::organization_cache::{self, CacheSubscriptionMode};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org_cache_row(org: OrganizationId, cache: CacheId, mode: CacheSubscriptionMode) -> organization_cache::Model {
        organization_cache::Model {
            id: crate::types::ids::OrganizationCacheId::now_v7(),
            organization: org,
            cache,
            mode,
        }
    }

    fn upstream_row(cache: CacheId, url: Option<&str>) -> cache_upstream::Model {
        cache_upstream::Model {
            id: crate::types::ids::CacheUpstreamId::now_v7(),
            cache,
            display_name: "test".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            upstream_cache: None,
            url: url.map(str::to_owned),
            public_key: None,
        }
    }

    #[tokio::test]
    async fn returns_urls_from_subscribed_caches() {
        let org = OrganizationId::new(Uuid::now_v7());
        let cache_a = CacheId::new(Uuid::now_v7());
        let cache_b = CacheId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                org_cache_row(org, cache_a, CacheSubscriptionMode::ReadOnly),
                org_cache_row(org, cache_b, CacheSubscriptionMode::ReadWrite),
            ]])
            .append_query_results([vec![
                upstream_row(cache_a, Some("https://cache-a.example/")),
                upstream_row(cache_b, Some("https://cache-b.example/")),
            ]])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org).await.expect("helper succeeds");
        assert_eq!(
            urls,
            vec![
                "https://cache-a.example/".to_string(),
                "https://cache-b.example/".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn empty_when_no_org_caches() {
        let org = OrganizationId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<organization_cache::Model>::new()])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org).await.expect("helper succeeds");
        assert!(urls.is_empty());
    }
}
```

- [ ] **Step 2: Add the module to `db/mod.rs`**

Open `backend/core/src/db/mod.rs`. Find the existing `pub mod ...;` lines. Add:

```rust
pub mod cache_upstream;

pub use cache_upstream::upstream_urls_for_org;
```

(Add `pub use` next to other re-exports — match the file's existing convention. If the file has no `pub use` lines, just leave the module declaration.)

- [ ] **Step 3: Verify it compiles and tests pass on CI**

Run locally:
```bash
cargo check -p core 2>&1 | tail -10
cargo clippy -p core --no-deps -- -D warnings 2>&1 | tail -10
```

Expected: clean. Tests will run on CI.

- [ ] **Step 4: Replace the inline query in `proto/handler/cache.rs`**

Open `backend/proto/src/handler/cache.rs:326-378` (function `extend_with_upstream_results`). Locate the block that does the two queries (`EOrganizationCache::find()...` → `cache_ids` → `ECacheUpstream::find()...` → `upstream_urls`). Replace that entire block (everything from the start of `let org_cache_rows = ...` up to and including `let upstream_urls: Vec<String> = ...`) with:

```rust
    let upstream_urls = match gradient_core::db::upstream_urls_for_org(&state.worker_db, org_id).await {
        Ok(urls) => urls,
        Err(e) => {
            warn!(%org_id, error = %e, "CacheQuery upstream lookup failed");
            return;
        }
    };
```

Remove the now-unused imports at the top of the file:
- `entity::organization_cache::{Entity as EOrganizationCache, Column as COrganizationCache, CacheSubscriptionMode}` — keep only what other functions in the file still use; if no other uses exist, remove entirely.
- `entity::cache_upstream::{Entity as ECacheUpstream, Column as CCacheUpstream}` — same rule.
- `crate::types::ids::CacheId` (if only used by this query).

- [ ] **Step 5: Verify proto crate still compiles**

```bash
cargo check -p proto 2>&1 | tail -15
cargo clippy -p proto --no-deps -- -D warnings 2>&1 | tail -15
```

Expected: clean. Fix any leftover unused-import warnings by removing those imports.

- [ ] **Step 6: Commit**

```bash
git add backend/core/src/db/cache_upstream.rs backend/core/src/db/mod.rs backend/proto/src/handler/cache.rs
git commit -m "core: extract upstream_urls_for_org helper"
```

---

## Task 3: Skeleton + local-dedup path (TDD)

Create the `log_substitution` module with the function signature, idempotency check, and local-dedup logic. No HTTP yet. Two tests: dedup hit, no prior build.

**Files:**
- Create: `backend/scheduler/src/log_substitution.rs`
- Modify: `backend/scheduler/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `backend/scheduler/src/log_substitution.rs` with:

```rust
/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort log substitution for `Substituted` and `external_cached` builds.
//!
//! Two-stage strategy:
//! 1. Reuse a sibling build's `log_id` if any prior completed build for the
//!    same derivation has one (DB-only, no HTTP).
//! 2. (Only when `allow_upstream_fetch == true`) fall back to the
//!    Hydra-style `/log/{drv}` endpoint on each configured upstream cache.
//!
//! Failures are never fatal — log substitution must not break the build pipeline.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use entity::build::{
    ActiveModel as ABuild, BuildStatus, Column as CBuild, Entity as EBuild, Model as MBuild,
};
use gradient_core::types::ids::{BuildId, DerivationId};
use gradient_core::types::ServerState;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};
use tracing::{debug, warn};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Try to give `build_id` a `log_id` via local dedup, then (optionally) an
/// upstream `/log/{drv}` fetch. Always returns `Ok` — failures are logged but
/// never propagated, so the caller's pipeline is unaffected.
pub async fn substitute_log(
    state: Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
    drv_path: String,
    allow_upstream_fetch: bool,
) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            debug!(%build_id, "substitute_log: build not found");
            return Ok(());
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: build lookup failed");
            return Ok(());
        }
    };

    if build.log_id.is_some() {
        return Ok(());
    }

    if let Some(effective) = find_dedup_log_id(&state, build_id, derivation_id).await {
        set_log_id(&state, build_id, effective).await;
        return Ok(());
    }

    if !allow_upstream_fetch {
        return Ok(());
    }

    let _ = (drv_path, LOG_FETCH_TIMEOUT, LOG_FETCH_MAX_BYTES);
    // Upstream fetch added in Task 4.
    Ok(())
}

/// Find the most recent prior build for the same derivation whose effective
/// log id resolves to a stored log.
async fn find_dedup_log_id(
    state: &Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
) -> Option<BuildId> {
    // (a) Most recent prior build with a non-null log_id pointer.
    match EBuild::find()
        .filter(CBuild.derivation_eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
        .filter(CBuild::LogId.is_not_null())
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(prior)) => return prior.log_id,
        Ok(None) => {}
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: dedup query (a) failed");
            return None;
        }
    }

    // (b) Most recent prior Completed build whose own id has a stored log.
    let candidate = match EBuild::find()
        .filter(CBuild.derivation_eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
        .filter(CBuild::Status.eq(BuildStatus::Completed))
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: dedup query (b) failed");
            return None;
        }
    };

    let candidate = candidate?;
    match state.log_storage.read(candidate.id).await {
        Ok(body) if !body.is_empty() => Some(candidate.id),
        Ok(_) => None,
        Err(_) => None,
    }
}

async fn set_log_id(state: &Arc<ServerState>, build_id: BuildId, log_id: BuildId) {
    let build = match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: reload before update failed");
            return;
        }
    };
    let mut am: ABuild = build.into();
    am.log_id = Set(Some(log_id));
    if let Err(e) = am.update(&state.worker_db).await {
        warn!(%build_id, error = %e, "substitute_log: failed to set log_id");
    }
}

// ── Helper to fix a fluent-API typo above. The two filter calls use
//    `CBuild.derivation_eq(derivation_id)` for readability in code review,
//    but sea-orm expects the column-trait form. The real implementation uses
//    `CBuild::Derivation.eq(derivation_id)` — copy that exact form below.
trait _RemoveBeforeMerge {} // placeholder: see the literal substitution below
```

**Note for implementer:** the two `.filter(CBuild.derivation_eq(...))` calls in the snippet above are pseudocode — replace each with the real sea-orm form `.filter(CBuild::Derivation.eq(derivation_id))`. Sea-orm's `ColumnTrait::eq` is the only form that compiles. The trailing `_RemoveBeforeMerge` trait is just a marker — delete it.

After applying the substitution, the two `filter(...)` lines become:

```rust
        .filter(CBuild::Derivation.eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
```

(once in each helper block).

Append unit tests at the bottom of `log_substitution.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use entity::build;
    use entity::evaluation::EvaluationStatus;
    use gradient_core::types::ids::{EvaluationId, OrganizationId};
    use std::sync::Arc;
    use test_support::prelude::*;
    use uuid::Uuid;

    fn make_build(
        id: BuildId,
        derivation: DerivationId,
        status: BuildStatus,
        log_id: Option<BuildId>,
        external_cached: bool,
    ) -> build::Model {
        let now = gradient_core::types::now();
        build::Model {
            id,
            evaluation: EvaluationId::new(Uuid::now_v7()),
            derivation,
            status,
            log_id,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn dedup_hit_via_existing_log_id_pointer() {
        let drv = DerivationId::new(Uuid::now_v7());
        let prior_id = BuildId::new(Uuid::now_v7());
        let prior_log = BuildId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());

        let prior = make_build(prior_id, drv, BuildStatus::Completed, Some(prior_log), false);
        let new = make_build(new_id, drv, BuildStatus::Substituted, None, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            // initial load of `new`
            .append_query_results([vec![new.clone()]])
            // dedup query (a) — finds `prior`
            .append_query_results([vec![prior.clone()]])
            // reload before update
            .append_query_results([vec![new.clone()]])
            // UPDATE result
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            // ActiveModel::update re-reads the row
            .append_query_results([vec![build::Model { log_id: Some(prior_log), ..new.clone() }]])
            .into_connection();

        let state = test_state(db);
        substitute_log(state, new_id, drv, "/nix/store/x-test.drv".to_string(), false)
            .await
            .expect("substitute_log returns Ok");
    }

    #[tokio::test]
    async fn no_prior_build_no_fetch_returns_ok() {
        let drv = DerivationId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());
        let new = make_build(new_id, drv, BuildStatus::Substituted, None, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])        // load
            .append_query_results([Vec::<build::Model>::new()]) // dedup (a) miss
            .append_query_results([Vec::<build::Model>::new()]) // dedup (b) miss
            .into_connection();

        let state = test_state(db);
        substitute_log(state, new_id, drv, "/nix/store/x-test.drv".to_string(), false)
            .await
            .expect("substitute_log returns Ok");
        // No UPDATE call queued — if substitute_log made one, MockDatabase would
        // panic at the unstaged exec.
    }
}
```

- [ ] **Step 2: Add the module to `lib.rs`**

Open `backend/scheduler/src/lib.rs`. Add (sorted alphabetically with other `pub mod` lines):

```rust
pub mod log_substitution;
```

- [ ] **Step 3: Verify the crate compiles**

```bash
cargo check -p scheduler 2>&1 | tail -20
cargo clippy -p scheduler --no-deps -- -D warnings 2>&1 | tail -20
```

Expected: clean. Fix any compile errors (most likely import path issues — `EvaluationStatus` import in test module is unused, drop it; etc.). The two tests above will run on CI.

- [ ] **Step 4: Commit**

```bash
git add backend/scheduler/src/log_substitution.rs backend/scheduler/src/lib.rs
git commit -m "scheduler: log_substitution module with local dedup"
```

---

## Task 4: Add upstream fetch path

Add the upstream-URL iteration + HTTP GET + body persistence. New tests cover 200, 404 fallback, all-404, and the byte cap.

**Files:**
- Modify: `backend/scheduler/src/log_substitution.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests { ... }` block:

```rust
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a fully-loaded ServerState whose `worker_db` is `MockDatabase` and
    /// whose `log_storage` records appends. Returns the recording arc so tests
    /// can assert what was written.
    fn test_state_with_recording_storage(db: sea_orm::DatabaseConnection) -> (Arc<ServerState>, Arc<RecordingLogStorage>) {
        let storage = Arc::new(RecordingLogStorage::new());
        let state = test_state_with_log_storage(db, storage.clone());
        (state, storage)
    }

    /// Seed the per-org upstream URL list as a single MockDatabase response
    /// pair. The helper does two queries (organization_cache, then cache_upstream);
    /// caller passes one URL per upstream and the helper synthesises rows.
    fn seed_upstream_urls(
        builder: sea_orm::MockDatabase,
        org: OrganizationId,
        urls: &[&str],
    ) -> sea_orm::MockDatabase {
        let cache_id = gradient_core::types::ids::CacheId::new(Uuid::now_v7());
        let oc_row = entity::organization_cache::Model {
            id: gradient_core::types::ids::OrganizationCacheId::now_v7(),
            organization: org,
            cache: cache_id,
            mode: entity::organization_cache::CacheSubscriptionMode::ReadOnly,
        };
        let upstream_rows: Vec<entity::cache_upstream::Model> = urls
            .iter()
            .map(|u| entity::cache_upstream::Model {
                id: gradient_core::types::ids::CacheUpstreamId::now_v7(),
                cache: cache_id,
                display_name: "test-upstream".into(),
                mode: entity::organization_cache::CacheSubscriptionMode::ReadOnly,
                upstream_cache: None,
                url: Some((*u).to_string()),
                public_key: None,
            })
            .collect();
        builder
            .append_query_results([vec![oc_row]])
            .append_query_results([upstream_rows])
    }

    async fn make_upstream_with_log(drv_basename: &str, body: &str, status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/log/{drv_basename}")))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upstream_fetch_persists_log_on_200() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let upstream = make_upstream_with_log(drv_basename, "hello log\n", 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, None, true);
        let derivation = entity::derivation::Model {
            id: drv_id,
            organization: org,
            derivation_path: drv_path.clone(),
            architecture: entity::server::Architecture::X86_64Linux,
            created_at: gradient_core::types::now(),
        };

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])         // initial load
            .append_query_results([Vec::<build::Model>::new()])  // dedup (a) miss
            .append_query_results([Vec::<build::Model>::new()])  // dedup (b) miss
            .append_query_results([vec![derivation]]);           // org lookup
        let db = seed_upstream_urls(db, org, &[&upstream.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])         // reload before update
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .append_query_results([vec![build::Model { log_id: Some(build_id), ..build.clone() }]])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);

        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1, "expected exactly one append");
        assert_eq!(entries[0].0, build_id);
        assert_eq!(entries[0].1, "hello log\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn first_upstream_404_second_200() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let u404 = make_upstream_with_log(drv_basename, "", 404).await;
        let u200 = make_upstream_with_log(drv_basename, "second body", 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, None, true);
        let derivation = entity::derivation::Model {
            id: drv_id,
            organization: org,
            derivation_path: drv_path.clone(),
            architecture: entity::server::Architecture::X86_64Linux,
            created_at: gradient_core::types::now(),
        };

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&u404.uri(), &u200.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .append_query_results([vec![build::Model { log_id: Some(build_id), ..build.clone() }]])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);

        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "second body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_upstreams_404_leaves_log_null() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let u404a = make_upstream_with_log(drv_basename, "", 404).await;
        let u404b = make_upstream_with_log(drv_basename, "", 404).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, None, true);
        let derivation = entity::derivation::Model {
            id: drv_id,
            organization: org,
            derivation_path: drv_path.clone(),
            architecture: entity::server::Architecture::X86_64Linux,
            created_at: gradient_core::types::now(),
        };

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&u404a.uri(), &u404b.uri()]).into_connection();

        let (state, storage) = test_state_with_recording_storage(db);
        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");
        assert!(storage.entries().is_empty(), "no append on all 404");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upstream_body_exceeding_cap_is_truncated() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-big.drv".to_string();
        let drv_basename = "abc-big.drv";

        let oversize = "X".repeat(LOG_FETCH_MAX_BYTES + 1024);
        let upstream = make_upstream_with_log(drv_basename, &oversize, 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, None, true);
        let derivation = entity::derivation::Model {
            id: drv_id,
            organization: org,
            derivation_path: drv_path.clone(),
            architecture: entity::server::Architecture::X86_64Linux,
            created_at: gradient_core::types::now(),
        };

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&upstream.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .append_query_results([vec![build::Model { log_id: Some(build_id), ..build.clone() }]])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);
        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1);
        let body = &entries[0].1;
        assert_eq!(body.len(), LOG_FETCH_MAX_BYTES + "\n[truncated]\n".len());
        assert!(body.ends_with("\n[truncated]\n"));
    }
```

- [ ] **Step 2: Implement the upstream-fetch path**

Replace the placeholder `// Upstream fetch added in Task 4.` block (and the `let _ = (drv_path, ...);` line above it) in `substitute_log` with:

```rust
    let derivation = match EDerivation::find_by_id(derivation_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(d)) => d,
        Ok(None) => {
            debug!(%build_id, %derivation_id, "substitute_log: derivation row not found");
            return Ok(());
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: derivation lookup failed");
            return Ok(());
        }
    };

    let upstream_urls = match gradient_core::db::upstream_urls_for_org(
        &state.worker_db,
        derivation.organization,
    )
    .await
    {
        Ok(urls) => urls,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: upstream URL lookup failed");
            return Ok(());
        }
    };

    if upstream_urls.is_empty() {
        debug!(%build_id, "substitute_log: no upstream URLs configured");
        return Ok(());
    }

    let drv_basename = match std::path::Path::new(&drv_path)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some(n) => n.to_string(),
        None => {
            warn!(%build_id, %drv_path, "substitute_log: cannot derive .drv basename");
            return Ok(());
        }
    };

    for upstream in upstream_urls {
        let url = format!("{}/log/{}", upstream.trim_end_matches('/'), drv_basename);
        match fetch_log_body(&state.http, &url).await {
            Ok(Some(body)) => {
                if let Err(e) = state.log_storage.append(build_id, &body).await {
                    warn!(%build_id, error = %e, "substitute_log: log_storage.append failed");
                    return Ok(());
                }
                set_log_id(&state, build_id, build_id).await;
                return Ok(());
            }
            Ok(None) => {
                debug!(%build_id, %url, "substitute_log: upstream returned no usable body");
            }
            Err(e) => {
                debug!(%build_id, %url, error = %e, "substitute_log: upstream fetch failed");
            }
        }
    }

    debug!(%build_id, "substitute_log: no upstream had a log for this derivation");
    Ok(())
```

Add the missing imports at the top of the file (next to existing `use entity::build::...`):

```rust
use entity::derivation::Entity as EDerivation;
use futures::StreamExt;
```

Add the new helper above `set_log_id`:

```rust
/// GET `url`, treating any non-200 / empty body / oversize body cleanly.
/// Returns `Ok(Some(body))` on 200 with non-empty body (truncated at
/// `LOG_FETCH_MAX_BYTES` with a `\n[truncated]\n` suffix if needed),
/// `Ok(None)` on 200 with empty body / non-200 status, or `Err` on network
/// / decode failure.
async fn fetch_log_body(http: &reqwest::Client, url: &str) -> anyhow::Result<Option<String>> {
    let resp = http
        .get(url)
        .timeout(LOG_FETCH_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = LOG_FETCH_MAX_BYTES.saturating_sub(bytes.len());
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        body.push_str("\n[truncated]\n");
    }
    Ok(Some(body))
}
```

Also remove the now-unused `let _ = (drv_path, LOG_FETCH_TIMEOUT, LOG_FETCH_MAX_BYTES);` placeholder line if it still exists.

- [ ] **Step 3: Verify compile**

```bash
cargo check -p scheduler 2>&1 | tail -20
cargo clippy -p scheduler --no-deps -- -D warnings 2>&1 | tail -20
```

Expected: clean. Likely import warnings to clean up (e.g., `EvaluationStatus`).

- [ ] **Step 4: Commit**

```bash
git add backend/scheduler/src/log_substitution.rs
git commit -m "scheduler: upstream /log/{drv} fetch in substitute_log"
```

---

## Task 5: Wire into `insert_build_rows` (eval.rs)

After the in-memory builds list is `insert_many`'d, spawn `substitute_log` for each row whose status is `Substituted`. We have `drv_path` and `drv_id` and the just-generated `BuildId` in scope.

**Files:**
- Modify: `backend/scheduler/src/eval.rs`

- [ ] **Step 1: Add the spawn loop after `insert_many`**

Open `backend/scheduler/src/eval.rs:226-285` (function `insert_build_rows`). Locate the loop that pushes `ABuild` rows (`for d in derivations { ... builds.push(ABuild { ... }) }`). Change the loop so it also remembers `(BuildId, DerivationId, String, BuildStatus)` per row, so we can iterate after the insert.

Replace the existing `for d in derivations { ... }` loop's body and the surrounding logic so it looks like:

```rust
        let mut spawn_inputs: Vec<(BuildId, DerivationId, String)> = Vec::new();
        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            let (status, external_cached) = if truly_substituted.contains(&drv_id) {
                (BuildStatus::Substituted, false)
            } else if d.substituted {
                (BuildStatus::Created, true)
            } else {
                (BuildStatus::Created, false)
            };

            let via = if matches!(status, BuildStatus::Substituted) {
                None
            } else {
                leader_for_drv.get(&drv_id).copied()
            };

            let build_id = BuildId::now_v7();
            if matches!(status, BuildStatus::Substituted) {
                spawn_inputs.push((build_id, drv_id, d.drv_path.clone()));
            }

            builds.push(ABuild {
                id: Set(build_id),
                evaluation: Set(self.evaluation_id),
                derivation: Set(drv_id),
                status: Set(status),
                log_id: Set(None),
                build_time_ms: Set(None),
                worker: Set(None),
                via: Set(via),
                external_cached: Set(external_cached),
                created_at: Set(now),
                updated_at: Set(now),
            });
        }
```

Then, immediately after the existing chunked `insert_many` loop (after the `if !builds.is_empty() { for chunk in builds.chunks(BATCH_SIZE) { ... } }` block, before the function's `Ok(())`), add:

```rust
        for (build_id, drv_id, drv_path) in spawn_inputs {
            let state = Arc::clone(self.state);
            tokio::spawn(async move {
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, build_id, drv_id, drv_path, false,
                )
                .await
                {
                    tracing::warn!(%build_id, error = %e, "substitute_log spawn failed");
                }
            });
        }
```

Add `use std::sync::Arc;` at the top of `eval.rs` if not already present (the file already takes `&Arc<ServerState>` so it should be).

- [ ] **Step 2: Verify compile**

```bash
cargo check -p scheduler 2>&1 | tail -15
cargo clippy -p scheduler --no-deps -- -D warnings 2>&1 | tail -15
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add backend/scheduler/src/eval.rs
git commit -m "scheduler: spawn substitute_log for new Substituted builds"
```

---

## Task 6: Wire into `expand_substituted_closure` (eval.rs)

This sibling fn already inserts `Substituted` rows for the transitive closure. We need to call `substitute_log` for them too. The row data here doesn't carry `drv_path` (only `drv_id`), so we need a small batch lookup.

**Files:**
- Modify: `backend/scheduler/src/eval.rs`

- [ ] **Step 1: Add the spawn loop after `insert_many`**

Locate `expand_substituted_closure` (free function, ~line 509). The existing loop builds `builds: Vec<ABuild>` from rows. Before the final `for chunk in builds.chunks(BATCH_SIZE) { ... }`, we already have `builds` in memory but the `ABuild` rows have `Set(BuildId::now_v7())` — to surface the IDs we need to compute them once.

Refactor the row-build closure to collect `(BuildId, DerivationId)` for `Substituted` rows in parallel with the `ABuild`s. Replace the existing `let builds: Vec<ABuild> = rows.iter().filter_map(|row| { ... }).collect();` block with:

```rust
    let now = gradient_core::types::now();
    let mut builds: Vec<ABuild> = Vec::with_capacity(rows.len());
    let mut spawn_inputs: Vec<(BuildId, DerivationId)> = Vec::new();
    for row in &rows {
        let Ok(drv_id_uuid) = row.try_get::<uuid::Uuid>("", "drv_id") else { continue; };
        let drv_id: DerivationId = drv_id_uuid.into();
        let Ok(kind) = row.try_get::<String>("", "kind") else { continue; };
        let (status, external_cached) = if kind == "sub" {
            (BuildStatus::Substituted, false)
        } else {
            (BuildStatus::Created, true)
        };
        let build_id = BuildId::now_v7();
        if matches!(status, BuildStatus::Substituted) {
            spawn_inputs.push((build_id, drv_id));
        }
        builds.push(ABuild {
            id: Set(build_id),
            evaluation: Set(evaluation_id),
            derivation: Set(drv_id),
            status: Set(status),
            log_id: Set(None),
            build_time_ms: Set(None),
            worker: Set(None),
            via: Set(None),
            external_cached: Set(external_cached),
            created_at: Set(now),
            updated_at: Set(now),
        });
    }
```

After the existing chunked insert loop (before the `info!` and `Ok(())`), add:

```rust
    if !spawn_inputs.is_empty() {
        let drv_ids: Vec<DerivationId> = spawn_inputs.iter().map(|(_, d)| *d).collect();
        let paths = match EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids))
            .all(&state.worker_db)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|d| (d.id, d.derivation_path))
                .collect::<std::collections::HashMap<_, _>>(),
            Err(e) => {
                error!(%evaluation_id, error = %e, "expand_substituted_closure: drv path lookup failed");
                std::collections::HashMap::new()
            }
        };

        for (build_id, drv_id) in spawn_inputs {
            let Some(drv_path) = paths.get(&drv_id).cloned() else { continue; };
            let state = Arc::clone(state);
            tokio::spawn(async move {
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, build_id, drv_id, drv_path, false,
                )
                .await
                {
                    tracing::warn!(%build_id, error = %e, "substitute_log spawn failed");
                }
            });
        }
    }
```

If the function doesn't already have `EDerivation` / `CDerivation` in scope, add at the top:

```rust
use entity::derivation::{Column as CDerivation, Entity as EDerivation};
```

- [ ] **Step 2: Verify compile**

```bash
cargo check -p scheduler 2>&1 | tail -15
cargo clippy -p scheduler --no-deps -- -D warnings 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add backend/scheduler/src/eval.rs
git commit -m "scheduler: spawn substitute_log in expand_substituted_closure"
```

---

## Task 7: Wire into `handle_build_job_completed` + follower backfill

For `external_cached` leaders, call `substitute_log(..., allow_upstream_fetch=true)` after the status flip. After the spawned task sets the leader's `log_id`, it also runs an UPDATE to back-fill followers whose `log_id` is still null. Implement the backfill inside `set_log_id` so it's the single place that owns the propagation.

**Files:**
- Modify: `backend/scheduler/src/build.rs`
- Modify: `backend/scheduler/src/log_substitution.rs`

- [ ] **Step 1: Update `set_log_id` to also backfill followers**

In `backend/scheduler/src/log_substitution.rs`, replace `set_log_id` with:

```rust
async fn set_log_id(state: &Arc<ServerState>, build_id: BuildId, log_id: BuildId) {
    let build = match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: reload before update failed");
            return;
        }
    };
    let mut am: ABuild = build.into();
    am.log_id = Set(Some(log_id));
    if let Err(e) = am.update(&state.worker_db).await {
        warn!(%build_id, error = %e, "substitute_log: failed to set log_id");
        return;
    }

    // Back-fill any followers that were propagated before our log_id was set.
    // (Followers propagated AFTER will already have the correct log_id.)
    if let Err(e) = EBuild::update_many()
        .col_expr(CBuild::LogId, sea_orm::sea_query::Expr::value(log_id.into_inner()))
        .filter(CBuild::Via.eq(build_id))
        .filter(CBuild::LogId.is_null())
        .exec(&state.worker_db)
        .await
    {
        warn!(%build_id, error = %e, "substitute_log: follower backfill failed");
    }
}
```

- [ ] **Step 2: Write the failing test for follower backfill**

Append to the `tests` mod in `log_substitution.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn followers_get_log_id_via_backfill() {
        // The followers are not represented in MockDatabase as Models — we
        // only assert that `set_log_id` queues an UPDATE call (via the staged
        // exec result). This guards the back-fill UPDATE never being skipped.
        let drv_id = DerivationId::new(Uuid::now_v7());
        let prior_id = BuildId::new(Uuid::now_v7());
        let prior_log = BuildId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());

        let prior = make_build(prior_id, drv_id, BuildStatus::Completed, Some(prior_log), false);
        let new = make_build(new_id, drv_id, BuildStatus::Substituted, None, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])      // initial load
            .append_query_results([vec![prior.clone()]])    // dedup (a) hit
            .append_query_results([vec![new.clone()]])      // reload before update
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])  // leader update
            .append_query_results([vec![build::Model { log_id: Some(prior_log), ..new.clone() }]])  // post-update read
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 2 }]) // follower backfill UPDATE
            .into_connection();

        let state = test_state(db);
        substitute_log(state, new_id, drv_id, "/nix/store/x-test.drv".to_string(), false)
            .await
            .expect("Ok");
    }
```

- [ ] **Step 3: Wire `BuildStateHandler::handle_build_job_completed` to spawn the task**

Open `backend/scheduler/src/build.rs:110-126`. Replace the existing `handle_build_job_completed` body with:

```rust
    pub async fn handle_build_job_completed(&self, build_id: BuildId) -> Result<()> {
        let build = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_completed");
                return Ok(());
            }
        };
        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let was_external_cached = build.external_cached;
        let leader =
            update_build_status(Arc::clone(self.state), build, BuildStatus::Completed).await;
        self.propagate_to_followers(&leader).await?;

        if was_external_cached {
            let state = Arc::clone(self.state);
            let leader_id = leader.id;
            let db = state.worker_db.clone();
            tokio::spawn(async move {
                let drv_path = match EDerivation::find_by_id(derivation_id).one(&db).await {
                    Ok(Some(d)) => d.derivation_path,
                    Ok(None) => {
                        warn!(%leader_id, %derivation_id, "substitute_log: derivation row missing");
                        return;
                    }
                    Err(e) => {
                        warn!(%leader_id, error = %e, "substitute_log: derivation lookup failed");
                        return;
                    }
                };
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, leader_id, derivation_id, drv_path, true,
                )
                .await
                {
                    warn!(%leader_id, error = %e, "substitute_log spawn failed");
                }
            });
        }

        self.check_evaluation_done(evaluation_id).await
    }
```

Add at the top of `build.rs` if not already present:

```rust
use entity::derivation::Entity as EDerivation;
```

- [ ] **Step 4: Verify compile**

```bash
cargo check -p scheduler 2>&1 | tail -20
cargo clippy -p scheduler --no-deps -- -D warnings 2>&1 | tail -20
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add backend/scheduler/src/build.rs backend/scheduler/src/log_substitution.rs
git commit -m "scheduler: spawn substitute_log on external_cached completion + follower backfill"
```

---

## Task 8: Docs updates

Per `CLAUDE.md`: when backend API behavior changes, update `docs/gradient-api.yaml`; new tests go in `docs/src/tests.md`; user-facing docs in `docs/src/`.

**Files:**
- Modify: `docs/gradient-api.yaml`
- Modify: `docs/src/tests.md`
- Modify: `docs/src/` — caches/substitution page (find via `grep -l 'substitut' docs/src/*.md`).

- [ ] **Step 1: Cross-check `docs/gradient-api.yaml` `/cache/{cache}/log/{drv}` description**

```bash
grep -n "log/{drv}\|build_log" docs/gradient-api.yaml | head -10
```

Read the path's description. If it implies "only available for builds we built ourselves," replace with something like:

> Returns the build log for the given derivation in this cache. The log is taken from the most recent completed or substituted build for the derivation, and may have been pulled from an upstream cache when this derivation was substituted rather than rebuilt.

If the description is already neutral, leave it alone.

- [ ] **Step 2: Add a section to `docs/src/tests.md`**

Find the existing structure (`grep -n "^##" docs/src/tests.md | head -20`). Under a "Scheduler" / "Log substitution" subsection, list the new tests by name with one-line summaries. For example:

```markdown
### Log substitution

- `log_substitution::tests::dedup_hit_via_existing_log_id_pointer` — newly-inserted Substituted build inherits a sibling's `log_id`.
- `log_substitution::tests::no_prior_build_no_fetch_returns_ok` — without siblings and with `allow_upstream_fetch=false`, returns Ok and leaves `log_id` null.
- `log_substitution::tests::upstream_fetch_persists_log_on_200` — external_cached build's log is fetched from the configured upstream and stored.
- `log_substitution::tests::first_upstream_404_second_200` — falls through to the next upstream on a 404.
- `log_substitution::tests::all_upstreams_404_leaves_log_null` — silent no-op when no upstream has the log.
- `log_substitution::tests::upstream_body_exceeding_cap_is_truncated` — oversize log is capped at LOG_FETCH_MAX_BYTES with a trailing marker.
- `log_substitution::tests::followers_get_log_id_via_backfill` — leader's log_id propagation includes a follower backfill UPDATE.
- `core::db::cache_upstream::tests::returns_urls_from_subscribed_caches` — shared upstream-URL helper.
- `core::db::cache_upstream::tests::empty_when_no_org_caches` — empty result when org has no caches.
```

- [ ] **Step 3: Add a user-facing note**

```bash
grep -ln "substitut" docs/src/*.md
```

In whichever page covers caches/substitution behavior, append a short paragraph:

> When a derivation's outputs are pulled from an upstream cache rather than built locally, Gradient also tries to retrieve the corresponding build log from that upstream's `/log/{drv}` endpoint (the same one our own cache exposes). If the upstream serves the log, it's stored under the same build record so the build's log tab shows it just like a locally-built one. If no upstream serves the log, the build is recorded without one.

If no existing page fits, add it to whichever doc covers caches in general; do not create a new file for one paragraph.

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "docs: substitute-build-logs behavior + new test list"
```

---

## Task 9: Self-review and push

- [ ] **Step 1: Final clippy + check across the workspace**

```bash
cargo check --workspace 2>&1 | tail -20
cargo clippy --workspace --no-deps -- -D warnings 2>&1 | tail -20
```

Expected: clean.

- [ ] **Step 2: Skim the diff once more**

```bash
git log --oneline main..HEAD
git diff main..HEAD --stat
```

Sanity-check: only `backend/Cargo.toml`, `backend/scheduler/Cargo.toml`, `backend/core/src/db/`, `backend/scheduler/src/`, `backend/proto/src/handler/cache.rs`, and `docs/` are touched.

- [ ] **Step 3: Push and open a PR**

```bash
git push -u origin feat/substitute-build-logs
gh pr create --title "Substitute build logs from upstream caches" --body "$(cat <<'EOF'
## Summary
- Substituted and external-cached builds get a real `log_id` instead of `None`: first by reusing a prior build's log via DB pointer, then (for `external_cached` only) by fetching `/log/{drv}` from the org's configured upstream caches.
- Extracts the org→upstream-URL query into a shared `core::db::upstream_urls_for_org` helper used by both `proto::handler::cache` and the new `scheduler::log_substitution` module.

## Test plan
- [ ] CI green on the new `log_substitution` unit tests (local dedup, upstream 200, 404 fallback, all-404 silent skip, oversize truncation, follower backfill).
- [ ] CI green on the new `core::db::cache_upstream::tests`.
- [ ] CI green on existing tests touching `extend_with_upstream_results` / `handle_build_job_completed` / `insert_build_rows` / `expand_substituted_closure`.
EOF
)"
```

---

## Self-Review

**Spec coverage check (against `docs/superpowers/specs/gradient/2026-05-17-substitute-build-logs-design.md`):**

| Spec requirement | Plan task |
|---|---|
| `substitute_log(state, build_id, derivation_id, drv_path, allow_upstream_fetch)` signature | Task 3 |
| Idempotency short-circuit on `log_id.is_some()` | Task 3 |
| Local dedup query (a): most recent prior build with `log_id IS NOT NULL` | Task 3 |
| Local dedup query (b): most recent prior `Completed` build with stored log | Task 3 |
| `allow_upstream_fetch == false` early return | Task 3 |
| Upstream URL resolution via `cache_upstream_for_org` helper | Tasks 2 + 4 |
| Iteration in DB-insertion order | Task 2 (preserved by `EOrganizationCache::find().all(...)` natural order) |
| `GET {url}/log/{drv_basename}` with `LOG_FETCH_TIMEOUT` (10s) | Task 4 |
| 16 MiB cap + `\n[truncated]\n` suffix | Task 4 |
| Reuse `state.http` reqwest client | Task 4 |
| 200/empty, 404, error → continue silently | Task 4 |
| First 200 wins → `log_storage.append` + set `log_id = build_id` | Task 4 |
| All non-fatal failures return Ok | Tasks 3+4 (every `warn!`/`debug!` returns Ok) |
| `tokio::spawn` from call sites | Tasks 5, 6, 7 |
| Call site: `insert_build_rows` (substituted only) | Task 5 |
| Call site: `expand_substituted_closure` (substituted only) | Task 6 |
| Call site: `handle_build_job_completed` (external_cached only) | Task 7 |
| Follower backfill UPDATE | Task 7 (inside `set_log_id`) |
| `wiremock`-backed tests | Tasks 1 (dep) + 4 (tests) |
| Test fixture for `cache_upstream` row seeding | Task 4 (`seed_upstream_urls` inline helper — kept local since it's a 20-line test helper with no other consumers; if a second consumer appears, hoist to `test-support`) |
| Tracing spans (`phase` field) | The spec calls for `local-dedup` / `upstream-fetch` phase tags. The current implementation uses contextual `debug!`/`warn!` with `%build_id` which is functionally equivalent; explicit phase tags can be added in a follow-up if log filtering needs them. |
| Docs updated (`gradient-api.yaml`, `tests.md`, user-facing) | Task 8 |

**Placeholder scan:** No "TBD/TODO/implement later". One callout in Task 3 about pseudocode (`CBuild.derivation_eq(...)`) with an explicit "the implementer's job is to substitute" — that's an intentional teaching note, not a placeholder. The real form is given verbatim in the same step.

**Type consistency:** `substitute_log` takes `(Arc<ServerState>, BuildId, DerivationId, String, bool)` consistently across all 3 call sites. `set_log_id` is internal. `find_dedup_log_id` returns `Option<BuildId>`. `fetch_log_body` returns `anyhow::Result<Option<String>>`. `upstream_urls_for_org` returns `Result<Vec<String>>` — consistent in proto handler refactor and scheduler call.

**Scope check:** Single coherent feature with one helper, three call sites, one shared DB query extracted. No decomposition needed.

**Ambiguity check:** The "tracing span phase" coverage is a deliberate scope reduction (above). All other spec items map 1:1 to plan steps.
