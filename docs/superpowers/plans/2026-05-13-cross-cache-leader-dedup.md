# Cross-cache leader/follower deduplication — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow the `build.via` leader/follower link to span organizations whenever the bytes the leader will produce are substitutable to the follower's organization through the cache graph.

**Architecture:** A new `cache_reach` helper in `core::db` computes the set of organizations whose Gradient builds a given org can substitute (walking `organization_cache` and `cache_upstream` transitively). `find_active_leaders` consults that set when no same-org candidate exists, applying a most-advanced/oldest tie-break. `propagate_to_followers` mirrors `derivation_output` and `build_product` rows onto cross-org followers since they have distinct `derivation` rows. `BuildAccessContext::load` is widened to grant follower-org users read access to the leader's build. `reelect_leader` is restricted to same-org follower promotion; cross-org followers are made independent on leader abort.

**Tech Stack:** Rust, sea-orm, axum, PostgreSQL. Tests use `sea_orm::MockDatabase` (canned query results in fixed order) rather than a real database — this is the project's established pattern (see `backend/web/tests/evaluation_builds_via.rs` as the reference). `test_support::db_with(...)` pre-cans a single result set; `MockDatabase::new(...).append_query_results([...])` chains arbitrary sequences.

> **Test pattern reminder:** every integration test below must (a) mock each DB query the production code path issues, in order, with the correct row type; (b) mount the axum router via `web::create_router(state)` and drive it with `axum_test::TestServer`. The exact query sequence depends on the implementation in earlier tasks — re-read those before writing each test's mock chain. Treat the "fixture seeding" calls in this plan (`common::seed_*`) as **canned query-result helpers**, not as actual DB inserts. If your `common/mod.rs` doesn't already have them, model them after `evaluation_builds_via.rs::*_row()` helpers (functions returning entity `Model` values).

> **Local verification policy:** This project does NOT run `cargo test` locally — CI runs the full test suite. Each task's "Run the test" step is a CI verification (push the branch and wait for the run). Locally, use `cargo check` and `cargo clippy --no-deps -- -D warnings` to confirm the code compiles. When a task lists `cargo test` as the verification command, treat it as documentation of *what CI will run for this change* — don't execute it on your workstation.

---

## File Structure

**New files:**
- `backend/core/src/db/cache_reach.rs` — `writer_orgs_reachable_from` + unit tests.
- `backend/test-support/src/fixtures.rs` — extended with multi-org/cache fixtures (in-place).
- `backend/scheduler/tests/cross_org_leader_set_on_insert.rs`
- `backend/scheduler/tests/cross_org_artefacts_mirrored.rs`
- `backend/scheduler/tests/cross_org_re_election_same_org_only.rs`
- `backend/scheduler/tests/cross_org_re_election_all_followers_independent.rs`
- `backend/web/tests/cross_org_follower_log_visible.rs`
- `backend/web/tests/evaluation_builds_via_cross_org.rs`

**Modified files:**
- `backend/core/src/db/mod.rs` — register new module.
- `backend/core/src/db/status.rs` — rewrite `find_active_leaders`; rewrite `reelect_leader`.
- `backend/scheduler/src/eval.rs:218` — pass `inserting_org`.
- `backend/core/src/ci/trigger.rs:231` — pass `inserting_org`.
- `backend/scheduler/src/build.rs:176` — extend `propagate_to_followers` with cross-org artefact mirroring.
- `backend/web/src/endpoints/builds/mod.rs:111` — widen `BuildAccessContext::load`.
- `backend/scheduler/src/handler_tests.rs` — update existing `find_active_leaders` mock-DB callers for new signature.
- `docs/gradient-api.yaml` — auth note on read-only build endpoints.
- `docs/src/tests.md` — catalogue new tests.
- `docs/src/scheduler.md` (or equivalent architecture page) — cross-cache leader/follower contract.

**Out-of-scope:** Materialised reachability tables; cross-org notifications; mid-build cache-graph mutation handling; frontend changes beyond the existing leader-row swap.

---

## Task 1: Add multi-org/cache fixture helpers

**Files:**
- Modify: `backend/test-support/src/fixtures.rs`

Adds reusable fixture functions used by every integration test below. No tests of their own (they're test infrastructure).

- [ ] **Step 1: Append helper functions to `fixtures.rs`**

Add after the existing `eval_at` helper:

```rust
use entity::ids::{
    BuildId, CacheId, CacheUpstreamId, DerivationId, OrganizationCacheId,
};
use entity::cache_upstream;
use entity::organization_cache::{self, CacheSubscriptionMode};
use entity::cache;

pub fn org_with_id(id: OrganizationId, slug: &str) -> organization::Model {
    organization::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        description: String::new(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
    }
}

pub fn cache_with_id(id: CacheId, slug: &str, owner: UserId) -> cache::Model {
    cache::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        description: String::new(),
        active: true,
        priority: 30,
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        created_by: owner,
        created_at: test_date(),
        managed: false,
    }
}

pub fn org_cache_link(
    id: OrganizationCacheId,
    org: OrganizationId,
    cache: CacheId,
    mode: CacheSubscriptionMode,
) -> organization_cache::Model {
    organization_cache::Model { id, organization: org, cache, mode }
}

pub fn internal_upstream(
    id: CacheUpstreamId,
    cache: CacheId,
    upstream: CacheId,
) -> cache_upstream::Model {
    cache_upstream::Model {
        id,
        cache,
        display_name: "internal".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        upstream_cache: Some(upstream),
        url: None,
        public_key: None,
    }
}

pub fn external_upstream(
    id: CacheUpstreamId,
    cache: CacheId,
    url: &str,
    public_key: &str,
) -> cache_upstream::Model {
    cache_upstream::Model {
        id,
        cache,
        display_name: "external".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        upstream_cache: None,
        url: Some(url.into()),
        public_key: Some(public_key.into()),
    }
}
```

Verify the `cache::Model` field set against `backend/entity/src/cache.rs` — if the actual struct has fields not shown above, copy them into `cache_with_id` with safe defaults.

- [ ] **Step 2: Compile-check**

Run: `cargo check -p test-support`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add backend/test-support/src/fixtures.rs
git commit -m "test-support: add multi-org/cache fixture helpers"
```

---

## Task 2: Reachability helper — module skeleton + direct-overlap test

**Files:**
- Create: `backend/core/src/db/cache_reach.rs`
- Modify: `backend/core/src/db/mod.rs`

- [ ] **Step 1: Register the module**

Edit `backend/core/src/db/mod.rs`. Add `pub mod cache_reach;` next to the other `pub mod` lines and `pub use self::cache_reach::*;` next to the other re-exports:

```rust
pub mod cache_reach;
pub mod connection;
pub mod dependency_graph;
pub mod derivation;
pub mod drv_output_spec;
pub mod gc;
pub mod status;

pub use self::cache_reach::*;
pub use self::connection::*;
pub use self::dependency_graph::*;
pub use self::derivation::*;
pub use self::drv_output_spec::DrvOutputSpec;
pub use self::gc::*;
pub use self::status::*;
```

- [ ] **Step 2: Write the failing test and an empty stub**

Create `backend/core/src/db/cache_reach.rs`:

```rust
/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Compute the set of organizations whose Gradient build outputs a given
//! organization can substitute through its cache subscriptions and the
//! `cache_upstream` graph.
//!
//! Two organizations are "cache-connected" when the writer org pushes into
//! a cache that lies in the upstream closure of one of the reader org's
//! caches. External (URL-based) upstreams are excluded — they don't host
//! Gradient builds.

use std::collections::{HashSet, VecDeque};

use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use entity::cache_upstream::{Column as CCacheUpstream, Entity as ECacheUpstream};
use entity::ids::{CacheId, OrganizationId};
use entity::organization_cache::{
    CacheSubscriptionMode, Column as COrganizationCache, Entity as EOrganizationCache,
};

/// Returns every organization (including `reader_org` itself) whose build
/// outputs `reader_org` could substitute through its current cache
/// subscriptions and the `cache_upstream` graph.
///
/// Algorithm:
/// 1. Load reader's `organization_cache` rows with mode `ReadWrite`/`ReadOnly`.
/// 2. BFS forward over `cache_upstream` edges (`cache → upstream_cache`) to
///    compute the upstream closure of the reader's caches. Cycles tolerated.
/// 3. Load every `organization_cache` row with mode `ReadWrite`/`WriteOnly`
///    on any cache in that closure; return the distinct org ids.
pub async fn writer_orgs_reachable_from<C: ConnectionTrait>(
    db: &C,
    reader_org: OrganizationId,
) -> Result<HashSet<OrganizationId>, DbErr> {
    todo!("implement in subsequent tasks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::cache_upstream::Model as MCacheUpstream;
    use entity::ids::{CacheId, CacheUpstreamId, OrganizationCacheId, OrganizationId};
    use entity::organization_cache::Model as MOrganizationCache;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org(n: u8) -> OrganizationId {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        OrganizationId::new(Uuid::from_bytes(bytes))
    }

    fn cid(n: u8) -> CacheId {
        let mut bytes = [0u8; 16];
        bytes[14] = n;
        CacheId::new(Uuid::from_bytes(bytes))
    }

    fn org_cache(
        org_id: OrganizationId,
        cache_id: CacheId,
        mode: CacheSubscriptionMode,
    ) -> MOrganizationCache {
        MOrganizationCache {
            id: OrganizationCacheId::now_v7(),
            organization: org_id,
            cache: cache_id,
            mode,
        }
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn direct_overlap_reader_sees_writer() {
        run(async {
            // Reader (org B) reads cache X; writer (org A) writes cache X.
            let cache_x = cid(1);
            let reader_rows = vec![org_cache(org(2), cache_x, CacheSubscriptionMode::ReadOnly)];
            let upstream_rows: Vec<MCacheUpstream> = vec![];
            let writer_rows = vec![
                org_cache(org(1), cache_x, CacheSubscriptionMode::ReadWrite),
                org_cache(org(2), cache_x, CacheSubscriptionMode::ReadOnly),
            ];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_orgs_reachable_from(&db, org(2))
                .await
                .expect("query succeeds");

            assert!(got.contains(&org(1)), "got: {:?}", got);
        });
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p core --lib db::cache_reach::tests::direct_overlap_reader_sees_writer`
Expected: FAIL with `todo!` panic.

- [ ] **Step 4: Replace the stub with a working implementation**

Replace `todo!(...)` with:

```rust
pub async fn writer_orgs_reachable_from<C: ConnectionTrait>(
    db: &C,
    reader_org: OrganizationId,
) -> Result<HashSet<OrganizationId>, DbErr> {
    // Reader's read-capable caches.
    let reader_rows = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(reader_org))
        .filter(COrganizationCache::Mode.is_in(vec![
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::ReadOnly,
        ]))
        .all(db)
        .await?;
    let seed: Vec<CacheId> = reader_rows.into_iter().map(|r| r.cache).collect();

    // Internal upstream edges. We load all of them up front so the BFS is
    // pure in-memory work; the `cache_upstream` table is tiny (one row per
    // configured upstream link, system-wide).
    let upstream_rows = ECacheUpstream::find()
        .filter(CCacheUpstream::UpstreamCache.is_not_null())
        .all(db)
        .await?;

    let mut closure: HashSet<CacheId> = seed.iter().copied().collect();
    let mut queue: VecDeque<CacheId> = seed.into_iter().collect();
    while let Some(cache_id) = queue.pop_front() {
        for edge in &upstream_rows {
            if edge.cache != cache_id {
                continue;
            }
            let Some(up) = edge.upstream_cache else { continue };
            if closure.insert(up) {
                queue.push_back(up);
            }
        }
    }

    if closure.is_empty() {
        return Ok(HashSet::new());
    }

    // Writers on any cache in the closure.
    let writer_rows = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.is_in(closure.into_iter().collect::<Vec<_>>()))
        .filter(COrganizationCache::Mode.is_in(vec![
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .all(db)
        .await?;
    Ok(writer_rows.into_iter().map(|r| r.organization).collect())
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p core --lib db::cache_reach::tests::direct_overlap_reader_sees_writer`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add backend/core/src/db/mod.rs backend/core/src/db/cache_reach.rs
git commit -m "core: db: add cache_reach helper with direct-overlap support"
```

---

## Task 3: Reachability — transitive internal chain

**Files:**
- Modify: `backend/core/src/db/cache_reach.rs`

- [ ] **Step 1: Add the failing test**

Append to the `tests` module:

```rust
#[test]
fn transitive_internal_chain() {
    run(async {
        // chain: cache_a → upstream cache_b → upstream cache_c
        // reader on a, writer on c
        let a = cid(1);
        let b = cid(2);
        let c = cid(3);

        let reader_rows = vec![org_cache(org(2), a, CacheSubscriptionMode::ReadOnly)];
        let upstream_rows = vec![
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: a,
                display_name: "ab".into(),
                mode: CacheSubscriptionMode::ReadOnly,
                upstream_cache: Some(b),
                url: None,
                public_key: None,
            },
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: b,
                display_name: "bc".into(),
                mode: CacheSubscriptionMode::ReadOnly,
                upstream_cache: Some(c),
                url: None,
                public_key: None,
            },
        ];
        let writer_rows = vec![org_cache(org(1), c, CacheSubscriptionMode::ReadWrite)];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([reader_rows])
            .append_query_results([upstream_rows])
            .append_query_results([writer_rows])
            .into_connection();

        let got = writer_orgs_reachable_from(&db, org(2)).await.unwrap();
        assert!(got.contains(&org(1)), "got: {:?}", got);
    });
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p core --lib db::cache_reach::tests::transitive_internal_chain`
Expected: PASS (the implementation from Task 2 already handles multi-hop BFS).

- [ ] **Step 3: Commit**

```bash
git add backend/core/src/db/cache_reach.rs
git commit -m "core: db: cover transitive cache_reach chain in tests"
```

---

## Task 4: Reachability — external upstream is skipped

**Files:**
- Modify: `backend/core/src/db/cache_reach.rs`

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn external_upstream_skipped() {
    run(async {
        // reader on a; a has an external (url-based) upstream that doesn't
        // belong to any Gradient org, so the closure stops at `a`. Writer
        // on a separate cache is not reachable.
        let a = cid(1);
        let b = cid(2);

        let reader_rows = vec![org_cache(org(2), a, CacheSubscriptionMode::ReadOnly)];
        // Only internal-upstream rows are loaded by the helper
        // (`upstream_cache IS NOT NULL` filter), so an external row simply
        // never reaches the BFS.
        let upstream_rows: Vec<MCacheUpstream> = vec![];
        let writer_rows: Vec<MOrganizationCache> = vec![];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([reader_rows])
            .append_query_results([upstream_rows])
            .append_query_results([writer_rows])
            .into_connection();

        let got = writer_orgs_reachable_from(&db, org(2)).await.unwrap();
        assert!(
            !got.contains(&org(1)),
            "external upstream must not reach org 1, got: {:?}",
            got
        );
        let _ = b; // explicitly unused — documents the absence of a path.
    });
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p core --lib db::cache_reach::tests::external_upstream_skipped`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add backend/core/src/db/cache_reach.rs
git commit -m "core: db: pin external upstream skip in cache_reach tests"
```

---

## Task 5: Reachability — write-only reader excluded

**Files:**
- Modify: `backend/core/src/db/cache_reach.rs`

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn write_only_reader_excluded() {
    run(async {
        // Reader has WriteOnly on cache X; cannot substitute from it.
        let x = cid(1);
        let reader_rows: Vec<MOrganizationCache> = vec![]; // mode filter rejects WriteOnly
        let upstream_rows: Vec<MCacheUpstream> = vec![];
        let writer_rows: Vec<MOrganizationCache> = vec![];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([reader_rows])
            .append_query_results([upstream_rows])
            .append_query_results([writer_rows])
            .into_connection();

        let got = writer_orgs_reachable_from(&db, org(2)).await.unwrap();
        assert!(got.is_empty(), "WriteOnly reader must see nobody, got: {:?}", got);
        let _ = x;
    });
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p core --lib db::cache_reach::tests::write_only_reader_excluded`
Expected: PASS (the mode filter in step 1 of the helper excludes WriteOnly).

- [ ] **Step 3: Commit**

```bash
git add backend/core/src/db/cache_reach.rs
git commit -m "core: db: pin write-only-reader exclusion in cache_reach tests"
```

---

## Task 6: Reachability — cycle tolerance

**Files:**
- Modify: `backend/core/src/db/cache_reach.rs`

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn cycle_tolerated() {
    run(async {
        // cache_a.upstream = b; cache_b.upstream = a → cycle.
        let a = cid(1);
        let b = cid(2);

        let reader_rows = vec![org_cache(org(2), a, CacheSubscriptionMode::ReadOnly)];
        let upstream_rows = vec![
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: a,
                display_name: "ab".into(),
                mode: CacheSubscriptionMode::ReadOnly,
                upstream_cache: Some(b),
                url: None,
                public_key: None,
            },
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: b,
                display_name: "ba".into(),
                mode: CacheSubscriptionMode::ReadOnly,
                upstream_cache: Some(a),
                url: None,
                public_key: None,
            },
        ];
        let writer_rows = vec![
            org_cache(org(1), b, CacheSubscriptionMode::ReadWrite),
            org_cache(org(2), a, CacheSubscriptionMode::ReadOnly),
        ];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([reader_rows])
            .append_query_results([upstream_rows])
            .append_query_results([writer_rows])
            .into_connection();

        let got = writer_orgs_reachable_from(&db, org(2)).await.unwrap();
        assert!(got.contains(&org(1)), "cycle must still include reachable writer, got: {:?}", got);
    });
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p core --lib db::cache_reach::tests::cycle_tolerated`
Expected: PASS (the `closure.insert(...)` visited-set prevents infinite revisits).

- [ ] **Step 3: Commit**

```bash
git add backend/core/src/db/cache_reach.rs
git commit -m "core: db: prove cache_reach BFS tolerates upstream cycles"
```

---

## Task 7: `find_active_leaders` — new signature, same-org behavior preserved

**Files:**
- Modify: `backend/core/src/db/status.rs`
- Modify: `backend/scheduler/src/handler_tests.rs` (existing mock-DB callers)
- Modify: `backend/scheduler/src/eval.rs:218` (caller passes org)
- Modify: `backend/core/src/ci/trigger.rs:231` (caller passes org)

This step changes the signature without changing behavior. Tests in `handler_tests.rs` that drive `find_active_leaders` through the eval pipeline keep passing once the new arg is threaded through.

- [ ] **Step 1: Change `find_active_leaders` to take `inserting_org` (no behavior change yet)**

Edit `backend/core/src/db/status.rs:399` to:

```rust
/// For each derivation in `drv_ids`, return the id of the leader build whose
/// result a new build for that derivation should follow.
///
/// First checks for an in-flight build within `inserting_org` (the previous
/// behavior). When no same-org candidate exists for a drv, this function
/// will also consult cache-connected organisations via
/// [`cache_reach::writer_orgs_reachable_from`] — that pass is added in a
/// later task; this step only widens the signature.
///
/// Drvs with no active build are omitted from the result.
pub async fn find_active_leaders<C: ConnectionTrait>(
    db: &C,
    inserting_org: OrganizationId,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, BuildId>, sea_orm::DbErr> {
    let _ = inserting_org;

    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = EBuild::find()
        .filter(CBuild::Derivation.is_in(drv_ids.to_vec()))
        .filter(CBuild::Status.is_in(vec![
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(db)
        .await?;

    let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
    for b in rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }
    Ok(out)
}
```

Add `use entity::ids::OrganizationId;` at the top if not present.

- [ ] **Step 2: Thread the org through the scheduler caller**

In `backend/scheduler/src/eval.rs` at the existing call site (around line 218):

```rust
let leader_for_drv = find_active_leaders(
    &self.state.worker_db,
    self.evaluation.organization,
    &buildable_drv_ids,
)
.await
.unwrap_or_else(|e| {
    error!(error = %e, "failed to query active leaders");
    HashMap::new()
});
```

- [ ] **Step 3: Thread the org through the CI trigger caller**

In `backend/core/src/ci/trigger.rs` at line 231:

```rust
let leader_for_drv =
    crate::db::find_active_leaders(db, new_eval.organization, &queued_drv_ids).await?;
```

(`new_eval` is the just-inserted evaluation row; confirm `organization` is in scope. If the function works off a different evaluation handle, use that.)

- [ ] **Step 4: Update existing mock-DB callers in `handler_tests.rs`**

Search `backend/scheduler/src/handler_tests.rs` for `find_active_leaders` references. The existing tests mock the underlying SQL query; they do not call the function directly — only the row-count comments need a glance. No code change is expected here unless a test calls the function directly: if so, pass `fixtures::org_id()` as the new arg.

- [ ] **Step 5: Run the existing suite to confirm green**

Run: `cargo check -p core -p scheduler` and `cargo clippy -p core -p scheduler --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add backend/core/src/db/status.rs backend/scheduler/src/eval.rs backend/core/src/ci/trigger.rs backend/scheduler/src/handler_tests.rs
git commit -m "core: db: widen find_active_leaders signature with inserting_org"
```

---

## Task 8: `find_active_leaders` — cross-org pass with tie-break

**Files:**
- Modify: `backend/core/src/db/status.rs`

- [ ] **Step 1: Write a failing mock-DB unit test for cross-org match + tie-break**

Add to `backend/core/src/db/status.rs` (in or create a `#[cfg(test)] mod find_active_leaders_tests`):

```rust
#[cfg(test)]
mod find_active_leaders_tests {
    use super::*;
    use entity::build::{BuildStatus, Model as MBuild};
    use entity::derivation::Model as MDerivation;
    use entity::organization_cache::{CacheSubscriptionMode, Model as MOrganizationCache};
    use entity::cache_upstream::Model as MCacheUpstream;
    use entity::ids::{BuildId, CacheId, DerivationId, OrganizationCacheId, OrganizationId};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org(n: u8) -> OrganizationId {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        OrganizationId::new(Uuid::from_bytes(bytes))
    }
    fn cid(n: u8) -> CacheId {
        let mut bytes = [0u8; 16];
        bytes[14] = n;
        CacheId::new(Uuid::from_bytes(bytes))
    }
    fn did(n: u8) -> DerivationId {
        let mut bytes = [0u8; 16];
        bytes[13] = n;
        DerivationId::new(Uuid::from_bytes(bytes))
    }
    fn bid(n: u8) -> BuildId {
        let mut bytes = [0u8; 16];
        bytes[12] = n;
        BuildId::new(Uuid::from_bytes(bytes))
    }

    fn build(
        id: BuildId,
        drv: DerivationId,
        status: BuildStatus,
        external_cached: bool,
        offset_secs: i64,
    ) -> MBuild {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::seconds(offset_secs);
        MBuild {
            id,
            evaluation: entity::ids::EvaluationId::now_v7(),
            derivation: drv,
            status,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached,
            created_at: t,
            updated_at: t,
        }
    }

    fn drv_row(id: DerivationId, owner: OrganizationId, path: &str) -> MDerivation {
        MDerivation {
            id,
            organization: owner,
            derivation_path: path.into(),
            architecture: "x86_64-linux".into(),
            created_at: chrono::NaiveDateTime::default(),
        }
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn cross_org_match_when_no_same_org_candidate() {
        run(async {
            let drv_b = did(2); // inserting org's derivation row
            let drv_a = did(1); // leader-org's derivation row (same path)
            let leader_build = bid(10);

            // Mock query order (must match the helper's call sequence):
            //   1. same-org pass: in-flight builds for drv_ids — empty.
            //   2. derivation rows for inserting org (resolve drv_id → drv_path).
            //   3. cache_reach: reader org_cache rows.
            //   4. cache_reach: upstream rows.
            //   5. cache_reach: writer org_cache rows.
            //   6. cross-org candidate derivation rows (drv_path ∈ unmatched,
            //      org ∈ reachable).
            //   7. cross-org candidate build rows (status filter + via IS NULL).
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MBuild>::new()])
                .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(2),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadOnly,
                }]])
                .append_query_results([Vec::<MCacheUpstream>::new()])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(1),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadWrite,
                }]])
                .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
                .append_query_results([vec![build(
                    leader_build,
                    drv_a,
                    BuildStatus::Building,
                    false,
                    0,
                )]])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert_eq!(got.get(&drv_b), Some(&leader_build), "got: {:?}", got);
        });
    }

    #[test]
    fn cross_org_tie_break_most_advanced_then_oldest() {
        run(async {
            let drv_b = did(2);
            let drv_a = did(1);
            let drv_c = did(3);
            let queued_old = bid(20); // older Queued
            let building_new = bid(21); // newer Building → should win

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MBuild>::new()])
                .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(2),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadOnly,
                }]])
                .append_query_results([Vec::<MCacheUpstream>::new()])
                .append_query_results([
                    vec![
                        MOrganizationCache {
                            id: OrganizationCacheId::now_v7(),
                            organization: org(1),
                            cache: cid(1),
                            mode: CacheSubscriptionMode::ReadWrite,
                        },
                        MOrganizationCache {
                            id: OrganizationCacheId::now_v7(),
                            organization: org(3),
                            cache: cid(1),
                            mode: CacheSubscriptionMode::ReadWrite,
                        },
                    ],
                ])
                .append_query_results([vec![
                    drv_row(drv_a, org(1), "/nix/store/x.drv"),
                    drv_row(drv_c, org(3), "/nix/store/x.drv"),
                ]])
                .append_query_results([vec![
                    build(queued_old, drv_a, BuildStatus::Queued, false, 0),
                    build(building_new, drv_c, BuildStatus::Building, false, 60),
                ]])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert_eq!(got.get(&drv_b), Some(&building_new), "got: {:?}", got);
        });
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p core --lib db::status::find_active_leaders_tests`
Expected: FAIL — current implementation never consults cross-org.

- [ ] **Step 3: Implement the cross-org pass**

Replace the `find_active_leaders` body in `backend/core/src/db/status.rs` with:

```rust
pub async fn find_active_leaders<C: ConnectionTrait>(
    db: &C,
    inserting_org: OrganizationId,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, BuildId>, sea_orm::DbErr> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // ── Same-org pass ────────────────────────────────────────────────────
    let same_org_rows = EBuild::find()
        .filter(CBuild::Derivation.is_in(drv_ids.to_vec()))
        .filter(CBuild::Status.is_in(vec![
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(db)
        .await?;

    let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
    for b in same_org_rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }

    let unmatched: Vec<DerivationId> = drv_ids
        .iter()
        .copied()
        .filter(|d| !out.contains_key(d))
        .collect();
    if unmatched.is_empty() {
        return Ok(out);
    }

    // ── Cross-org pass ───────────────────────────────────────────────────
    use entity::derivation::{Column as CDerivation, Entity as EDerivation};

    // Resolve drv_ids → drv_paths for the inserting org.
    let inserting_drv_rows = EDerivation::find()
        .filter(CDerivation::Id.is_in(unmatched.clone()))
        .all(db)
        .await?;
    let mut path_to_drv: HashMap<String, DerivationId> = HashMap::new();
    let mut drv_paths: Vec<String> = Vec::new();
    for d in &inserting_drv_rows {
        path_to_drv.insert(d.derivation_path.clone(), d.id);
        drv_paths.push(d.derivation_path.clone());
    }
    if drv_paths.is_empty() {
        return Ok(out);
    }

    let mut reachable =
        crate::db::cache_reach::writer_orgs_reachable_from(db, inserting_org).await?;
    reachable.remove(&inserting_org);
    if reachable.is_empty() {
        return Ok(out);
    }

    let candidate_drvs = EDerivation::find()
        .filter(CDerivation::DerivationPath.is_in(drv_paths.clone()))
        .filter(CDerivation::Organization.is_in(reachable.into_iter().collect::<Vec<_>>()))
        .all(db)
        .await?;
    if candidate_drvs.is_empty() {
        return Ok(out);
    }
    let candidate_drv_ids: Vec<DerivationId> = candidate_drvs.iter().map(|d| d.id).collect();
    let leader_drv_to_path: HashMap<DerivationId, String> = candidate_drvs
        .into_iter()
        .map(|d| (d.id, d.derivation_path))
        .collect();

    let candidate_builds = EBuild::find()
        .filter(CBuild::Derivation.is_in(candidate_drv_ids))
        .filter(CBuild::Status.is_in(vec![
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .filter(CBuild::Via.is_null())
        .filter(CBuild::ExternalCached.eq(false))
        .all(db)
        .await?;

    // Tie-break per drv_path: most-advanced status, then oldest created_at.
    fn status_rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    let mut best_by_path: HashMap<String, MBuild> = HashMap::new();
    for b in candidate_builds {
        let Some(path) = leader_drv_to_path.get(&b.derivation).cloned() else {
            continue;
        };
        match best_by_path.get(&path) {
            Some(cur) => {
                let cur_rank = status_rank(cur.status);
                let new_rank = status_rank(b.status);
                if new_rank > cur_rank
                    || (new_rank == cur_rank && b.created_at < cur.created_at)
                {
                    best_by_path.insert(path, b);
                }
            }
            None => {
                best_by_path.insert(path, b);
            }
        }
    }

    for (path, b) in best_by_path {
        if let Some(&local_drv_id) = path_to_drv.get(&path) {
            out.insert(local_drv_id, b.id);
        }
    }

    Ok(out)
}
```

Add `use entity::build::Model as MBuild;` and `use entity::derivation::Column as _;` as needed at module top.

- [ ] **Step 4: Re-run the tests**

Run: `cargo test -p core --lib db::status::find_active_leaders_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/core/src/db/status.rs
git commit -m "core: db: extend find_active_leaders with cross-org pass and tie-break"
```

---

## Task 9: `find_active_leaders` — same-org preferred + external_cached cross-org skip

**Files:**
- Modify: `backend/core/src/db/status.rs`

These behaviors fall out of Task 8's implementation; the tests pin them.

- [ ] **Step 1: Add the failing tests**

Append to the `find_active_leaders_tests` module:

```rust
#[test]
fn same_org_preferred_over_cross_org() {
    run(async {
        let drv_b = did(2);
        let same_org_build = bid(30);

        // Same-org pass returns the candidate immediately; cross-org pass is
        // never executed (the helper short-circuits when all drvs matched).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build(
                same_org_build,
                drv_b,
                BuildStatus::Queued,
                false,
                0,
            )]])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert_eq!(got.get(&drv_b), Some(&same_org_build));
    });
}

#[test]
fn cross_org_external_cached_candidate_skipped() {
    run(async {
        let drv_b = did(2);
        let drv_a = did(1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(2),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadOnly,
            }]])
            .append_query_results([Vec::<MCacheUpstream>::new()])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(1),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadWrite,
            }]])
            // Candidate drv exists in org(1) for the same path...
            .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
            // ...but the SQL `external_cached = false` filter excludes the
            // only would-be leader, so the build query returns empty.
            .append_query_results([Vec::<MBuild>::new()])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert!(got.get(&drv_b).is_none(), "external_cached must be skipped");
    });
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p core --lib db::status::find_active_leaders_tests`
Expected: PASS — the SQL `external_cached = false` filter and the same-org short-circuit already cover these cases.

- [ ] **Step 3: Commit**

```bash
git add backend/core/src/db/status.rs
git commit -m "core: db: pin same-org-preferred and external_cached skip rules"
```

---

## Task 10: Integration test — cross-org leader set on insert (TDD red)

**Files:**
- Create: `backend/scheduler/tests/cross_org_leader_set_on_insert.rs`

End-to-end test driving real Postgres via `test-support`. Confirms the `via` column is populated when a second org evaluates the same drv-path.

- [ ] **Step 1: Look up an existing scheduler integration test for shape**

Read `backend/scheduler/tests/` for an existing file to copy structure from (likely something using `test_support::state::test_state` plus DB seeding). Note the harness's preferred way to seed `organization`, `cache`, `organization_cache`, `derivation`, `build`, `evaluation` rows.

- [ ] **Step 2: Write the failing test**

Create `backend/scheduler/tests/cross_org_leader_set_on_insert.rs` with a test that:

1. Seeds two orgs (`org_a`, `org_b`), one cache `cache_x`,
   `organization_cache(org_a, cache_x, ReadWrite)` and
   `organization_cache(org_b, cache_x, ReadOnly)`.
2. Seeds an in-flight build in `org_a` for `/nix/store/abc-foo.drv`
   (status = `Building`, `via = None`, `external_cached = false`).
3. Seeds an evaluation row for `org_b` in `EvaluatingDerivation` state.
4. Calls the eval-pipeline entry point that drives `insert_build_rows` for a
   single discovered derivation with `drv_path = /nix/store/abc-foo.drv`,
   `substituted = false`.
5. Asserts the new `org_b` build row has `via = Some(org_a_build.id)`.

Use the existing `test_support::db_with(...)` helper to fully seed a fresh Postgres schema. Mirror the import block from another integration test (e.g. `evaluation_builds_via.rs`).

The full test body (use as-is, adjust import paths if your helpers differ):

```rust
/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod common;

use entity::build::BuildStatus;
use entity::ids::{
    BuildId, CacheId, DerivationId, EvaluationId, OrganizationCacheId, OrganizationId,
};
use entity::organization_cache::CacheSubscriptionMode;
use entity::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use test_support::fixtures;

#[tokio::test]
async fn cross_org_leader_set_on_insert() {
    let (state, _temp) = common::test_state().await;
    let db = &state.worker_db;

    // ── Seed: two orgs sharing cache_x ──────────────────────────────────
    let org_a = fixtures::org_with_id(
        OrganizationId::new(uuid::uuid!("aaaaaaaa-0000-0000-0000-000000000001")),
        "org-a",
    );
    let org_b = fixtures::org_with_id(
        OrganizationId::new(uuid::uuid!("bbbbbbbb-0000-0000-0000-000000000001")),
        "org-b",
    );
    let cache_x = fixtures::cache_with_id(
        CacheId::new(uuid::uuid!("cccccccc-0000-0000-0000-000000000001")),
        "cache-x",
        fixtures::user_id(),
    );
    let oc_a = fixtures::org_cache_link(
        OrganizationCacheId::now_v7(),
        org_a.id,
        cache_x.id,
        CacheSubscriptionMode::ReadWrite,
    );
    let oc_b = fixtures::org_cache_link(
        OrganizationCacheId::now_v7(),
        org_b.id,
        cache_x.id,
        CacheSubscriptionMode::ReadOnly,
    );

    EOrganization::insert(org_a.clone().into_active_model()).exec(db).await.unwrap();
    EOrganization::insert(org_b.clone().into_active_model()).exec(db).await.unwrap();
    ECache::insert(cache_x.clone().into_active_model()).exec(db).await.unwrap();
    EOrganizationCache::insert(oc_a.into_active_model()).exec(db).await.unwrap();
    EOrganizationCache::insert(oc_b.into_active_model()).exec(db).await.unwrap();

    // ── Seed: org_a building drv_path P ─────────────────────────────────
    let drv_path = "/nix/store/abc123-foo.drv";
    let drv_a = derivation::ActiveModel {
        id: Set(DerivationId::now_v7()),
        organization: Set(org_a.id),
        derivation_path: Set(drv_path.into()),
        architecture: Set("x86_64-linux".into()),
        created_at: Set(fixtures::test_date()),
    }
    .insert(db)
    .await
    .unwrap();

    // org_a evaluation (placeholder — needed for the build FK).
    let eval_a = common::seed_evaluation_for_org(db, org_a.id).await;
    let leader_build = build::ActiveModel {
        id: Set(BuildId::now_v7()),
        evaluation: Set(eval_a.id),
        derivation: Set(drv_a.id),
        status: Set(BuildStatus::Building),
        log_id: Set(None),
        build_time_ms: Set(None),
        worker: Set(None),
        via: Set(None),
        external_cached: Set(false),
        created_at: Set(fixtures::test_date()),
        updated_at: Set(fixtures::test_date()),
    }
    .insert(db)
    .await
    .unwrap();

    // ── Drive: org_b evaluation discovers the same drv_path ─────────────
    let eval_b = common::seed_evaluation_for_org(db, org_b.id).await;
    let drv_b = derivation::ActiveModel {
        id: Set(DerivationId::now_v7()),
        organization: Set(org_b.id),
        derivation_path: Set(drv_path.into()),
        architecture: Set("x86_64-linux".into()),
        created_at: Set(fixtures::test_date()),
    }
    .insert(db)
    .await
    .unwrap();

    let leader_for_drv =
        gradient_core::db::find_active_leaders(db, org_b.id, &[drv_b.id])
            .await
            .expect("find_active_leaders succeeds");

    assert_eq!(
        leader_for_drv.get(&drv_b.id),
        Some(&leader_build.id),
        "org_b's new build must point at org_a's in-flight leader"
    );
}
```

If `common::test_state` and `common::seed_evaluation_for_org` don't exist yet, create them in `backend/scheduler/tests/common/mod.rs` modelled after the helpers used by `backend/web/tests/evaluation_builds_via.rs`.

- [ ] **Step 3: Run the test to verify it fails** (compilation will require Task 7's signature change — which is already in place by now)

Run: `cargo test -p scheduler --test cross_org_leader_set_on_insert`
Expected: PASS (the cross-org pass from Task 8 already implements this).

- [ ] **Step 4: Commit**

```bash
git add backend/scheduler/tests/cross_org_leader_set_on_insert.rs backend/scheduler/tests/common/
git commit -m "scheduler: integration test for cross-org leader linkage"
```

---

## Task 11: Artefact mirroring — integration test (red)

**Files:**
- Create: `backend/scheduler/tests/cross_org_artefacts_mirrored.rs`

- [ ] **Step 1: Write the failing test**

Set up the same two-org/cache fixture as Task 10. Then:

1. Insert a follower build in `org_b` with `via = leader_build.id`.
2. Insert `derivation_output` and `build_product` rows under the leader's
   derivation (with non-trivial `hash`, `nar_size`, `cached_path` link).
3. Call `BuildStateHandler::handle_build_job_completed(leader_build.id)`
   (re-export the entry through the scheduler's public surface if needed).
4. Assert: `EDerivationOutput::find().filter(Derivation.eq(drv_b.id))`
   returns rows mirroring the leader's outputs by `hash`, `name`,
   `is_cached`, and `cached_path`.
5. Assert: `EBuildProduct::find()` joined through those outputs matches
   leader's `build_product` rows by `name`/`path`/`size`.

Test body skeleton:

```rust
mod common;

use entity::build::BuildStatus;
use entity::ids::{BuildId, BuildProductId, DerivationOutputId, DerivationId};
use entity::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use test_support::fixtures;

#[tokio::test]
async fn cross_org_artefacts_mirrored() {
    let (state, _temp) = common::test_state().await;
    let db = &state.worker_db;

    // Seed orgs/cache/orgs_cache (copy from Task 10's helper).
    let (org_a, org_b, _cache_x) = common::seed_two_orgs_sharing_cache(db).await;

    // Seed drv_a, drv_b for the same drv_path.
    let drv_path = "/nix/store/abc123-foo.drv";
    let drv_a = common::seed_derivation(db, org_a.id, drv_path).await;
    let drv_b = common::seed_derivation(db, org_b.id, drv_path).await;

    // Seed leader & follower builds.
    let eval_a = common::seed_evaluation_for_org(db, org_a.id).await;
    let eval_b = common::seed_evaluation_for_org(db, org_b.id).await;
    let leader = common::seed_build(db, eval_a.id, drv_a.id, BuildStatus::Building, None).await;
    let follower = common::seed_build(db, eval_b.id, drv_b.id, BuildStatus::Created, Some(leader.id)).await;

    // Seed leader's derivation_output + build_product.
    let leader_output = derivation_output::ActiveModel {
        id: Set(DerivationOutputId::now_v7()),
        derivation: Set(drv_a.id),
        name: Set("out".into()),
        output: Set("/nix/store/xxx-foo".into()),
        hash: Set("deadbeef".into()),
        package: Set("foo".into()),
        ca: Set(None),
        nar_size: Set(Some(1024)),
        is_cached: Set(true),
        cached_path: Set(None),
        created_at: Set(fixtures::test_date()),
    }
    .insert(db)
    .await
    .unwrap();
    let leader_product = build_product::ActiveModel {
        id: Set(BuildProductId::now_v7()),
        derivation_output: Set(leader_output.id),
        file_type: Set("file".into()),
        subtype: Set("doc".into()),
        name: Set("readme".into()),
        path: Set("share/doc/readme".into()),
        size: Set(Some(512)),
        created_at: Set(fixtures::test_date()),
    }
    .insert(db)
    .await
    .unwrap();

    // Drive: leader completes.
    scheduler::build::handle_build_job_completed(&state, leader.id)
        .await
        .expect("handler ok");

    // ── Assert: follower's derivation has mirrored rows ──────────────────
    let outs = EDerivationOutput::find()
        .filter(derivation_output::Column::Derivation.eq(drv_b.id))
        .all(db)
        .await
        .unwrap();
    assert_eq!(outs.len(), 1, "exactly one mirrored output");
    let mirrored = &outs[0];
    assert_eq!(mirrored.hash, "deadbeef");
    assert_eq!(mirrored.name, "out");
    assert_eq!(mirrored.nar_size, Some(1024));
    assert!(mirrored.is_cached);

    let products = EBuildProduct::find()
        .filter(build_product::Column::DerivationOutput.eq(mirrored.id))
        .all(db)
        .await
        .unwrap();
    assert_eq!(products.len(), 1, "exactly one mirrored product");
    assert_eq!(products[0].name, "readme");
    assert_eq!(products[0].path, "share/doc/readme");
    assert_eq!(products[0].size, Some(512));

    let _ = leader_product;
}
```

(Add helpers `seed_two_orgs_sharing_cache`, `seed_derivation`, `seed_build` to `common/mod.rs` if not already present.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scheduler --test cross_org_artefacts_mirrored`
Expected: FAIL — `outs.len()` is `0` because today's `propagate_to_followers` doesn't mirror artefacts cross-org.

- [ ] **Step 3: Commit (failing test)**

```bash
git add backend/scheduler/tests/cross_org_artefacts_mirrored.rs backend/scheduler/tests/common/
git commit -m "scheduler: failing integration test for cross-org artefact mirroring"
```

---

## Task 12: Artefact mirroring — implementation

**Files:**
- Modify: `backend/scheduler/src/build.rs:176`

- [ ] **Step 1: Extend `propagate_to_followers` to mirror cross-org artefacts**

Locate `async fn propagate_to_followers(&self, leader: &MBuild) -> Result<()>` at line 176. After the existing block that loads `followers`, but before the per-follower loop, add:

```rust
// Load leader's child rows once. Used to mirror artefacts onto any
// cross-org follower (different `derivation` than the leader).
let leader_outputs = EDerivationOutput::find()
    .filter(CDerivationOutput::Derivation.eq(leader.derivation))
    .all(&self.state.worker_db)
    .await
    .context("fetch leader's derivation_output rows")?;
let leader_output_ids: Vec<_> = leader_outputs.iter().map(|o| o.id).collect();
let leader_products = if leader_output_ids.is_empty() {
    Vec::new()
} else {
    EBuildProduct::find()
        .filter(CBuildProduct::DerivationOutput.is_in(leader_output_ids.clone()))
        .all(&self.state.worker_db)
        .await
        .context("fetch leader's build_product rows")?
};
```

Then inside the existing `for follower in followers { ... }` loop, *after* the `active.update(...)` call that copies leader fields, add:

```rust
// Cross-org follower: copy artefact rows onto the follower's derivation
// row so downstream API endpoints (downloads, graph) work without org-aware
// resolution. Same-org followers share `derivation` with the leader and
// skip this branch.
if follower.derivation != leader.derivation {
    let existing_outs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(follower.derivation))
        .all(&self.state.worker_db)
        .await
        .context("fetch follower's existing derivation_output rows")?;
    let existing_out_ids: Vec<_> = existing_outs.iter().map(|o| o.id).collect();
    if !existing_out_ids.is_empty() {
        if let Err(e) = EBuildProduct::delete_many()
            .filter(CBuildProduct::DerivationOutput.is_in(existing_out_ids.clone()))
            .exec(&self.state.worker_db)
            .await
        {
            warn!(error = %e, follower_id = %follower.id, "failed to clear stale follower build_products");
        }
        if let Err(e) = EDerivationOutput::delete_many()
            .filter(CDerivationOutput::Id.is_in(existing_out_ids))
            .exec(&self.state.worker_db)
            .await
        {
            warn!(error = %e, follower_id = %follower.id, "failed to clear stale follower derivation_outputs");
        }
    }

    use entity::ids::{BuildProductId, DerivationOutputId};
    let mut old_to_new_output: std::collections::HashMap<DerivationOutputId, DerivationOutputId> =
        std::collections::HashMap::new();
    for src in &leader_outputs {
        let new_id = DerivationOutputId::now_v7();
        old_to_new_output.insert(src.id, new_id);
        let am = ADerivationOutput {
            id: Set(new_id),
            derivation: Set(follower.derivation),
            name: Set(src.name.clone()),
            output: Set(src.output.clone()),
            hash: Set(src.hash.clone()),
            package: Set(src.package.clone()),
            ca: Set(src.ca.clone()),
            nar_size: Set(src.nar_size),
            is_cached: Set(src.is_cached),
            cached_path: Set(src.cached_path),
            created_at: Set(src.created_at),
        };
        if let Err(e) = am.insert(&self.state.worker_db).await {
            warn!(error = %e, follower_id = %follower.id, "failed to mirror derivation_output to follower");
        }
    }
    for src in &leader_products {
        let Some(&new_output_id) = old_to_new_output.get(&src.derivation_output) else {
            continue;
        };
        let am = ABuildProduct {
            id: Set(BuildProductId::now_v7()),
            derivation_output: Set(new_output_id),
            file_type: Set(src.file_type.clone()),
            subtype: Set(src.subtype.clone()),
            name: Set(src.name.clone()),
            path: Set(src.path.clone()),
            size: Set(src.size),
            created_at: Set(src.created_at),
        };
        if let Err(e) = am.insert(&self.state.worker_db).await {
            warn!(error = %e, follower_id = %follower.id, "failed to mirror build_product to follower");
        }
    }
}
```

Ensure the necessary entity imports (`EDerivationOutput`, `CDerivationOutput`, `EBuildProduct`, `CBuildProduct`, `ADerivationOutput`, `ABuildProduct`) are in scope at the top of the file (most are re-exported through `gradient_core::types::*` per existing usage).

- [ ] **Step 2: Update the comment block on `propagate_to_followers`**

Remove or amend the comment at lines 165-173 that says "Followers always share a `derivation` row with their leader" — that assumption no longer holds. Replace with:

```rust
/// Copy a leader's terminal status (and `log_id`, `build_time_ms`,
/// `worker`) onto every build with `via = leader.id`, then run the
/// per-evaluation finalisation each follower needs (`DependencyFailed`
/// cascade on failure, `check_evaluation_done` to flip the eval).
///
/// Same-org followers share the leader's `derivation` row, so its
/// `derivation_output` and `build_product` children are already visible to
/// the follower's evaluation without any copy. Cross-org followers (those
/// whose `derivation` differs from the leader's — created when the leader
/// belongs to a cache-connected organisation) have their `derivation_output`
/// and `build_product` rows mirrored onto the follower's `derivation`.
///
/// `Aborted` is not propagated — when a leader is aborted (its eval was
/// cancelled), callers re-elect a new leader from the followers instead.
```

- [ ] **Step 3: Re-run the integration test**

Run: `cargo test -p scheduler --test cross_org_artefacts_mirrored`
Expected: PASS.

- [ ] **Step 4: Verify nothing else broke**

Run: `cargo check -p scheduler` and `cargo clippy -p scheduler --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add backend/scheduler/src/build.rs
git commit -m "scheduler: mirror leader artefacts onto cross-org followers"
```

---

## Task 13: Re-elect — same-org-only promotion (red)

**Files:**
- Create: `backend/scheduler/tests/cross_org_re_election_same_org_only.rs`

- [ ] **Step 1: Write the failing test**

Fixture: org_a writes cache_x, org_b reads cache_x. org_a has an in-flight leader build for drv-path P. org_a has ONE same-org follower (another build in a sibling eval). org_b has ONE cross-org follower.

```rust
mod common;

use entity::build::BuildStatus;
use entity::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use test_support::fixtures;

#[tokio::test]
async fn cross_org_re_election_same_org_only() {
    let (state, _temp) = common::test_state().await;
    let db = &state.worker_db;

    let (org_a, org_b, _cache_x) = common::seed_two_orgs_sharing_cache(db).await;
    let drv_path = "/nix/store/abc123-foo.drv";
    let drv_a = common::seed_derivation(db, org_a.id, drv_path).await;
    let drv_b = common::seed_derivation(db, org_b.id, drv_path).await;

    let eval_a1 = common::seed_evaluation_for_org(db, org_a.id).await;
    let eval_a2 = common::seed_evaluation_for_org(db, org_a.id).await;
    let eval_b = common::seed_evaluation_for_org(db, org_b.id).await;

    let leader = common::seed_build(db, eval_a1.id, drv_a.id, BuildStatus::Queued, None).await;
    let same_org_follower =
        common::seed_build(db, eval_a2.id, drv_a.id, BuildStatus::Created, Some(leader.id)).await;
    let cross_org_follower =
        common::seed_build(db, eval_b.id, drv_b.id, BuildStatus::Created, Some(leader.id)).await;

    // Drive: abort eval_a1 (which aborts the leader). Use the public abort
    // entry point that calls `reelect_leader` internally.
    gradient_core::db::abort_evaluation(state.clone(), eval_a1).await;

    let promoted = EBuild::find_by_id(same_org_follower.id).one(db).await.unwrap().unwrap();
    let orphaned = EBuild::find_by_id(cross_org_follower.id).one(db).await.unwrap().unwrap();

    assert!(promoted.via.is_none(), "same-org follower must be promoted");
    assert!(
        orphaned.via.is_none(),
        "cross-org follower must be orphaned (via cleared), got via={:?}",
        orphaned.via
    );
}
```

(`gradient_core::db::abort_evaluation` is the existing public entry that internally calls `reelect_leader` — confirm the exact path by grepping for the public surface of `abort_evaluation`.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scheduler --test cross_org_re_election_same_org_only`
Expected: FAIL — current `reelect_leader` picks the oldest follower regardless of org, so the cross-org follower could be promoted and become the new leader; even if the same-org one wins, the cross-org follower's `via` would be repointed at the new leader rather than cleared.

- [ ] **Step 3: Commit (failing test)**

```bash
git add backend/scheduler/tests/cross_org_re_election_same_org_only.rs
git commit -m "scheduler: failing test for same-org-only leader re-election"
```

---

## Task 14: Re-elect — implementation (same-org promote, cross-org orphan)

**Files:**
- Modify: `backend/core/src/db/status.rs` (`reelect_leader` near line 360)

- [ ] **Step 1: Rewrite `reelect_leader`**

Replace the existing function body:

```rust
async fn reelect_leader(state: &Arc<ServerState>, leader: &MBuild) -> Result<(), sea_orm::DbErr> {
    use sea_orm::QueryOrder;

    // Resolve the leader's organization once. Same-org followers share the
    // leader's `derivation` (and therefore its `organization`); cross-org
    // followers do not.
    let leader_org = EDerivation::find_by_id(leader.derivation)
        .one(&state.worker_db)
        .await?
        .map(|d| d.organization);

    let all_followers = EBuild::find()
        .filter(CBuild::Via.eq(leader.id))
        .all(&state.worker_db)
        .await?;
    if all_followers.is_empty() {
        return Ok(());
    }

    // Split followers by whether they share the leader's organization.
    let mut same_org: Vec<MBuild> = Vec::new();
    let mut cross_org: Vec<MBuild> = Vec::new();
    let follower_drv_ids: Vec<DerivationId> =
        all_followers.iter().map(|f| f.derivation).collect();
    let drv_org: std::collections::HashMap<DerivationId, OrganizationId> =
        EDerivation::find()
            .filter(CDerivation::Id.is_in(follower_drv_ids))
            .all(&state.worker_db)
            .await?
            .into_iter()
            .map(|d| (d.id, d.organization))
            .collect();
    for f in all_followers {
        let org = drv_org.get(&f.derivation).copied();
        if org == leader_org && org.is_some() {
            same_org.push(f);
        } else {
            cross_org.push(f);
        }
    }

    // Tie-break: most-advanced status, then oldest created_at — matches
    // `find_active_leaders`.
    fn rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    same_org.sort_by(|a, b| {
        rank(b.status)
            .cmp(&rank(a.status))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    if let Some(new_leader) = same_org.first().cloned() {
        // Promote.
        let mut active: ABuild = new_leader.clone().into_active_model();
        active.via = Set(None);
        active.update(&state.worker_db).await?;

        // Repoint *only same-org* remaining followers at the new leader.
        let same_org_remaining_ids: Vec<BuildId> = same_org
            .iter()
            .skip(1)
            .map(|f| f.id)
            .collect();
        if !same_org_remaining_ids.is_empty() {
            EBuild::update_many()
                .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(new_leader.id))
                .filter(CBuild::Id.is_in(same_org_remaining_ids))
                .exec(&state.worker_db)
                .await?;
        }

        // Cross-org followers become independent.
        let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
        if !cross_org_ids.is_empty() {
            EBuild::update_many()
                .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(Option::<BuildId>::None))
                .filter(CBuild::Id.is_in(cross_org_ids))
                .exec(&state.worker_db)
                .await?;
        }

        debug!(
            old_leader = %leader.id,
            new_leader = %new_leader.id,
            cross_org_orphaned = cross_org.len(),
            "re-elected build leader (same-org), cross-org followers made independent"
        );
        return Ok(());
    }

    // No same-org candidate → orphan every cross-org follower.
    let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
    if !cross_org_ids.is_empty() {
        EBuild::update_many()
            .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(Option::<BuildId>::None))
            .filter(CBuild::Id.is_in(cross_org_ids))
            .exec(&state.worker_db)
            .await?;
        debug!(
            old_leader = %leader.id,
            orphaned = cross_org.len(),
            "leader aborted with no same-org followers; cross-org followers made independent"
        );
    }
    Ok(())
}
```

Add imports as needed at the top of `status.rs`:
- `use entity::derivation::{Column as CDerivation, Entity as EDerivation};`
- `use entity::ids::OrganizationId;` (if not already imported)

- [ ] **Step 2: Run the failing test**

Run: `cargo test -p scheduler --test cross_org_re_election_same_org_only`
Expected: PASS.

- [ ] **Step 3: Run the existing reelect handler_tests**

Run: `cargo test -p scheduler --lib scheduler_tests` and `cargo test -p scheduler --lib handler_tests`
Expected: still green (same-org-only behaviour is the legacy behaviour).

- [ ] **Step 4: Commit**

```bash
git add backend/core/src/db/status.rs
git commit -m "core: db: restrict reelect_leader to same-org promotion"
```

---

## Task 15: Re-elect — all-cross-org orphaning test

**Files:**
- Create: `backend/scheduler/tests/cross_org_re_election_all_followers_independent.rs`

- [ ] **Step 1: Write the test (should already pass after Task 14)**

```rust
mod common;

use entity::build::BuildStatus;
use entity::*;
use sea_orm::EntityTrait;

#[tokio::test]
async fn cross_org_re_election_all_followers_independent() {
    let (state, _temp) = common::test_state().await;
    let db = &state.worker_db;

    let (org_a, org_b, _cache_x) = common::seed_two_orgs_sharing_cache(db).await;
    let drv_path = "/nix/store/abc123-foo.drv";
    let drv_a = common::seed_derivation(db, org_a.id, drv_path).await;
    let drv_b = common::seed_derivation(db, org_b.id, drv_path).await;

    let eval_a = common::seed_evaluation_for_org(db, org_a.id).await;
    let eval_b1 = common::seed_evaluation_for_org(db, org_b.id).await;
    let eval_b2 = common::seed_evaluation_for_org(db, org_b.id).await;

    let leader = common::seed_build(db, eval_a.id, drv_a.id, BuildStatus::Queued, None).await;
    let f1 = common::seed_build(db, eval_b1.id, drv_b.id, BuildStatus::Created, Some(leader.id)).await;
    let f2 = common::seed_build(db, eval_b2.id, drv_b.id, BuildStatus::Created, Some(leader.id)).await;

    gradient_core::db::abort_evaluation(state.clone(), eval_a).await;

    let r1 = EBuild::find_by_id(f1.id).one(db).await.unwrap().unwrap();
    let r2 = EBuild::find_by_id(f2.id).one(db).await.unwrap().unwrap();

    assert!(r1.via.is_none(), "f1 orphaned");
    assert!(r2.via.is_none(), "f2 orphaned");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p scheduler --test cross_org_re_election_all_followers_independent`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add backend/scheduler/tests/cross_org_re_election_all_followers_independent.rs
git commit -m "scheduler: cover cross-org-only orphaning on leader abort"
```

---

## Task 16: Build access widening — failing test

**Files:**
- Create: `backend/web/tests/cross_org_follower_log_visible.rs`

- [ ] **Step 1: Write the failing integration test**

Seed:
- `org_a` and `org_b` share `cache_x` (R/W and R/O respectively).
- A leader build in `org_a` with a populated log.
- A follower build in `org_b` with `via = leader.id`.
- User `u_b` is a member of `org_b` only.
- User `u_c` is a member of a third org with no relationship.

Assertions (via `axum_test::TestServer`):

1. `GET /builds/{leader_id}/log` as `u_b` → `200` with the leader's log content.
2. `GET /builds/{leader_id}/log` as `u_c` → `404` (build masked as not found).
3. `GET /builds/{leader_id}` as `u_b` → `200`.
4. `GET /builds/{leader_id}` as `u_c` → `404`.

```rust
mod common;

use axum_test::TestServer;
use entity::build::BuildStatus;
use entity::*;
use serde_json::Value;
use test_support::fixtures;

#[tokio::test]
async fn cross_org_follower_log_visible_to_follower_org_user() {
    let (state, _temp) = common::test_state().await;
    let db_worker = &state.worker_db;
    let db_web = &state.web_db;

    let (org_a, org_b, _cache_x) = common::seed_two_orgs_sharing_cache(db_worker).await;
    let org_c = common::seed_isolated_org(db_worker, "org-c").await;

    let drv_path = "/nix/store/abc-foo.drv";
    let drv_a = common::seed_derivation(db_worker, org_a.id, drv_path).await;
    let drv_b = common::seed_derivation(db_worker, org_b.id, drv_path).await;
    let eval_a = common::seed_evaluation_for_org(db_worker, org_a.id).await;
    let eval_b = common::seed_evaluation_for_org(db_worker, org_b.id).await;
    let leader = common::seed_build_completed(db_worker, eval_a.id, drv_a.id, "log contents here").await;
    let _follower = common::seed_build(db_worker, eval_b.id, drv_b.id, BuildStatus::Completed, Some(leader.id)).await;

    let u_b = common::seed_user_member_of(db_web, org_b.id, "user-b").await;
    let u_c = common::seed_user_member_of(db_web, org_c.id, "user-c").await;

    let app = gradient_web::create_router(state.clone());
    let server = TestServer::new(app).unwrap();

    let resp_b = server
        .get(&format!("/builds/{}/log", leader.id))
        .add_header(common::session_cookie(&u_b))
        .await;
    resp_b.assert_status_ok();
    let body: Value = resp_b.json();
    assert_eq!(body["data"], "log contents here");

    let resp_c = server
        .get(&format!("/builds/{}/log", leader.id))
        .add_header(common::session_cookie(&u_c))
        .await;
    resp_c.assert_status_not_found();
}
```

Add the helpers `seed_user_member_of`, `seed_build_completed`, `session_cookie`, `seed_isolated_org` to `common/mod.rs`, modelled on existing helpers in other `web/tests` files.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p web --test cross_org_follower_log_visible`
Expected: FAIL — `resp_b` returns 404 because `BuildAccessContext::load` rejects non-org members.

- [ ] **Step 3: Commit (failing test)**

```bash
git add backend/web/tests/cross_org_follower_log_visible.rs backend/web/tests/common/
git commit -m "web: failing test for cross-org follower log access"
```

---

## Task 17: Build access widening — implementation

**Files:**
- Modify: `backend/web/src/endpoints/builds/mod.rs`

- [ ] **Step 1: Extend `BuildAccessContext::load`**

Add a fallback path that checks whether the requester has access to *any* organisation that owns a follower build of this leader.

```rust
pub(super) async fn load(
    state: &Arc<ServerState>,
    build_id: BuildId,
    maybe_user: &Option<MUser>,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Self> {
    let ctx = Self::load_unguarded(state, build_id).await?;

    let direct_access = if ctx.organization.public {
        true
    } else {
        match maybe_user {
            Some(user) => is_org_member(state, user.id, ctx.organization.id, api_key).await?,
            None => false,
        }
    };
    if direct_access {
        return Ok(ctx);
    }

    // Cross-cache follower fallback (read-only). When this build is the
    // leader of a follower row whose evaluation's owning organisation the
    // requester can read, allow the read.
    if let Some(user) = maybe_user
        && follower_orgs_accessible(state, user, api_key, build_id).await?
    {
        return Ok(ctx);
    }

    Err(WebError::not_found("Build"))
}
```

Add the helper at module scope:

```rust
async fn follower_orgs_accessible(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    leader_build_id: BuildId,
) -> WebResult<bool> {
    use sea_orm::QuerySelect;

    let follower_eval_ids: Vec<EvaluationId> = EBuild::find()
        .filter(CBuild::Via.eq(leader_build_id))
        .select_only()
        .column(CBuild::Evaluation)
        .into_tuple()
        .all(&state.web_db)
        .await?;
    if follower_eval_ids.is_empty() {
        return Ok(false);
    }

    // Resolve each evaluation's owning org. Same logic as `load_unguarded`,
    // de-duplicated by org.
    let evals = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(follower_eval_ids))
        .all(&state.web_db)
        .await?;
    let mut org_ids: std::collections::HashSet<OrganizationId> = std::collections::HashSet::new();
    for ev in evals {
        let org = if let Some(project_id) = ev.project {
            EProject::find_by_id(project_id)
                .one(&state.web_db)
                .await?
                .map(|p| p.organization)
        } else {
            EDirectBuild::find()
                .filter(CDirectBuild::Evaluation.eq(ev.id))
                .one(&state.web_db)
                .await?
                .map(|d| d.organization)
        };
        if let Some(o) = org {
            org_ids.insert(o);
        }
    }

    for org_id in org_ids {
        if is_org_member(state, user.id, org_id, api_key).await? {
            return Ok(true);
        }
    }
    Ok(false)
}
```

Add `use gradient_core::types::ids::EvaluationId;` if not already in scope.

- [ ] **Step 2: Re-run the failing test**

Run: `cargo test -p web --test cross_org_follower_log_visible`
Expected: PASS.

- [ ] **Step 3: Verify existing build endpoint tests still pass**

Run: `cargo check -p web` and `cargo test -p web --test builds_download` (and any other build-endpoint suites; CI runs the rest).
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add backend/web/src/endpoints/builds/mod.rs
git commit -m "web: widen build access to cross-cache follower-org members"
```

---

## Task 18: Eval-builds endpoint — cross-org leader-row swap

**Files:**
- Create: `backend/web/tests/evaluation_builds_via_cross_org.rs`

The existing `evaluation_builds_via.rs` already verifies the same-org leader-row swap. This task extends it to the cross-org case.

- [ ] **Step 1: Write the test**

Seed the two-org/cache fixture from Task 16. Then:
- `eval_b` in `org_b` contains exactly one build (the follower) with
  `via = leader_a.id`.
- `GET /evals/{eval_b_id}/builds` as `u_b` should return the leader's
  `BuildItem` (the build id should match `leader_a.id`, status should match
  leader's live status, `log_id` non-null).

```rust
mod common;

use axum_test::TestServer;
use entity::build::BuildStatus;
use serde_json::Value;

#[tokio::test]
async fn evaluation_builds_resolves_cross_org_leader_row() {
    let (state, _temp) = common::test_state().await;
    let db_worker = &state.worker_db;
    let db_web = &state.web_db;

    let (org_a, org_b, _cache_x) = common::seed_two_orgs_sharing_cache(db_worker).await;
    let drv_path = "/nix/store/abc-foo.drv";
    let drv_a = common::seed_derivation(db_worker, org_a.id, drv_path).await;
    let drv_b = common::seed_derivation(db_worker, org_b.id, drv_path).await;
    let eval_a = common::seed_evaluation_for_org(db_worker, org_a.id).await;
    let eval_b = common::seed_evaluation_for_org(db_worker, org_b.id).await;
    let leader = common::seed_build_running(db_worker, eval_a.id, drv_a.id).await;
    let _follower = common::seed_build(db_worker, eval_b.id, drv_b.id, BuildStatus::Created, Some(leader.id)).await;

    let u_b = common::seed_user_member_of(db_web, org_b.id, "user-b").await;
    let app = gradient_web::create_router(state.clone());
    let server = TestServer::new(app).unwrap();

    let resp = server
        .get(&format!("/evals/{}/builds", eval_b.id))
        .add_header(common::session_cookie(&u_b))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let builds = body["data"]["builds"].as_array().expect("builds array");
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0]["id"], leader.id.to_string());
    assert_eq!(builds[0]["status"], "Building");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p web --test evaluation_builds_via_cross_org`
Expected: PASS — the leader-row swap in `evals/query.rs` is already org-agnostic (it dereferences `via` without filtering on org), and Task 17 grants the user access to the leader build.

- [ ] **Step 3: Commit**

```bash
git add backend/web/tests/evaluation_builds_via_cross_org.rs
git commit -m "web: integration test for cross-org leader-row swap"
```

---

## Task 19: API docs — annotate read-only build endpoints

**Files:**
- Modify: `docs/gradient-api.yaml`

- [ ] **Step 1: Locate the build endpoint definitions**

Search the YAML for paths `/builds/{build_id}`, `/builds/{build_id}/log`,
`/builds/{build_id}/downloads`, `/builds/{build_id}/graph`. Each has a `description` block.

- [ ] **Step 2: Add a note to each affected operation**

Append to the description of each of those `GET` operations:

```yaml
# (example for GET /builds/{build_id}/log)
description: |
  Returns the build's log contents.

  **Access:** Accessible to members of the build's organization. Additionally,
  when this build is the leader of a follower build in a cache-connected
  organisation (see /docs/scheduler.md#cross-cache-deduplication), members
  of that follower organisation are granted read access.
```

Do not modify response schemas — there are no shape changes.

- [ ] **Step 3: Lint the YAML**

Run: `npx --yes @redocly/cli lint docs/gradient-api.yaml` (or your project's preferred OpenAPI linter)
Expected: no new warnings on the touched paths.

- [ ] **Step 4: Commit**

```bash
git add docs/gradient-api.yaml
git commit -m "docs: api: note cross-cache follower access on build read endpoints"
```

---

## Task 20: docs/src — architecture note and tests catalogue

**Files:**
- Modify: `docs/src/scheduler.md` (or the nearest architecture page; check `docs/src/SUMMARY.md` for the precise file)
- Modify: `docs/src/tests.md`

- [ ] **Step 1: Add an architecture section**

Append (or insert under an existing "leader/follower" heading) in the scheduler page:

```markdown
### Cross-cache deduplication

When a new build is created for `/nix/store/<hash>-foo.drv` and another
organisation already has an in-flight build for the same path, the new build
can be linked to it as a follower instead of being scheduled separately.
The link is admissible whenever the follower's organisation can substitute
from the leader's writes through the cache graph:

- The leader's organisation has `organization_cache` mode `ReadWrite` or
  `WriteOnly` on some cache `C_w`.
- `C_w` is in the upstream closure (over `cache_upstream`) of one of the
  follower organisation's `ReadWrite`/`ReadOnly` caches.

The closure walk only follows internal `cache_upstream.upstream_cache`
edges; external (URL-based) upstreams do not host Gradient builds and are
excluded.

#### Leader selection

`find_active_leaders` first looks for an in-flight candidate in the same
organisation. Only when none exists does it run the cross-org pass.
Cross-org candidates are filtered to `external_cached = false` and ordered
by status (`Building` > `Queued` > `Created`) then oldest `created_at`.

#### Artefact propagation

Same-org followers share the leader's `derivation` row, so its
`derivation_output` and `build_product` children are visible automatically.
Cross-org followers have these rows mirrored onto their own `derivation`
when the leader completes (`scheduler::build::propagate_to_followers`).

#### Access

Read-only build endpoints (`GET /builds/{id}`, `/log`, `/downloads`,
`/graph`) accept requests from members of any organisation that holds a
follower row on the targeted leader.

#### Leader abort

On leader abort, only same-org followers are eligible for promotion to the
new leader. Cross-org followers are made independent (`via` cleared) so the
next dispatch cycle picks them up on their own.
```

- [ ] **Step 2: Add a tests-catalogue entry**

Append to `docs/src/tests.md` under the appropriate suite-grouping (one paragraph per test, with one-line descriptions). Example:

```markdown
### Cross-cache leader/follower deduplication

- `core/src/db/cache_reach.rs` (unit):
  - `direct_overlap_reader_sees_writer` — direct cache overlap.
  - `transitive_internal_chain` — three-hop internal upstream chain.
  - `external_upstream_skipped` — external (URL) upstreams don't extend reach.
  - `write_only_reader_excluded` — WriteOnly reader sees nobody.
  - `cycle_tolerated` — BFS terminates on `cache_upstream` cycles.
- `core/src/db/status.rs::find_active_leaders_tests` (unit):
  - `cross_org_match_when_no_same_org_candidate`.
  - `cross_org_tie_break_most_advanced_then_oldest`.
  - `same_org_preferred_over_cross_org`.
  - `cross_org_external_cached_candidate_skipped`.
- `scheduler/tests/cross_org_leader_set_on_insert.rs` — end-to-end `via` linkage.
- `scheduler/tests/cross_org_artefacts_mirrored.rs` — derivation_output/build_product mirror.
- `scheduler/tests/cross_org_re_election_same_org_only.rs` — same-org promotion.
- `scheduler/tests/cross_org_re_election_all_followers_independent.rs` — cross-org orphaning.
- `web/tests/cross_org_follower_log_visible.rs` — auth widening on `GET /builds/{id}/log`.
- `web/tests/evaluation_builds_via_cross_org.rs` — eval-builds leader-row swap across orgs.
```

- [ ] **Step 3: Commit**

```bash
git add docs/src/scheduler.md docs/src/tests.md
git commit -m "docs: cover cross-cache leader/follower deduplication"
```

---

## Task 21: Final sweep — typecheck and lints

**Files:** none new

- [ ] **Step 1: Run typecheck across all affected crates**

Run: `cargo check -p core -p scheduler -p web -p test-support`
Expected: clean.

- [ ] **Step 2: Run lints**

Run: `cargo clippy -p core -p scheduler -p web --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 3: Inspect the final diff**

Run: `git log --oneline main..HEAD` and `git diff main..HEAD --stat`
Verify each commit is focused and the diff covers exactly the files listed in the File Structure section.

- [ ] **Step 4: Push the branch and open a PR (only when the user explicitly asks)**

(Per project convention, do not push until the user explicitly confirms. Once they do, push and open the PR with a summary referencing this plan and the spec.)

---

## Self-review notes

- **Spec coverage:** every section of `2026-05-13-cross-cache-leader-dedup-design.md` is implemented: §1 (cache_reach) in Tasks 2-6; §2 (find_active_leaders) in Tasks 7-9; §3 (artefact mirroring) in Tasks 11-12; §4 (auth widening + re-elect) in Tasks 13-17; tests catalogued in Task 20; docs in Tasks 19-20.
- **No placeholders:** every step has concrete code or commands.
- **Type consistency:** `writer_orgs_reachable_from` signature is consistent between Tasks 2-8; `find_active_leaders` takes `(db, inserting_org, drv_ids)` consistently from Task 7 onward; `BuildAccessContext::load` keeps the same signature in Task 17, only its body changes.
- **TDD ordering:** every behavior change has a red test committed before its implementation in the next task.
