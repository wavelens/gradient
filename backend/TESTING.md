# Testing

## Quick Start

```bash
cargo test -p core       # pure-function tests, no DB needed
cargo test -p web        # HTTP handler tests (mocked DB)
cargo test -p builder    # scheduler logic (mocked DB)
cargo test --workspace   # everything
```

---

## Philosophy

The deleted tests shared three anti-patterns that made them worthless:

| Anti-pattern | Why it's useless |
|---|---|
| `serde_json::to_string(x).contains("field")` | Tests serde's derive macro, not our code |
| SeaORM MockDB returning what you inserted | Tests SeaORM, not our code |
| Constructing a `Cli` and asserting `port == 3000` | That's a compile error, not a test |

**Tests should cover logic we wrote.** If deleting the test would not change the confidence we have in the code, the test should not exist.

---

## What Needs a Mock

### Database — `sea_orm::MockDatabase`

Any function that calls into `state.db`. SeaORM's `MockDatabase` replays pre-loaded query results in order; each `.append_query_results([...])` call feeds the next `SELECT`, each `.append_exec_results([...])` feeds the next `INSERT`/`UPDATE`/`DELETE`.

Use the `db_with` helper in `tests/common` (one per crate) instead of repeating the 4-line setup:

```rust
pub fn db_with<T: sea_orm::IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}
```

Chain multiple `.append_query_results` for handlers that run several queries.

### External Processes — trait injection

`execute_build` (builder) and `check_project_updates` (core) shell out to `nix` and `git`
via `tokio::process::Command`. These cannot be tested without real binaries **unless** the
call site is abstracted behind a trait.

Planned refactor — extract a `ProcessRunner` trait and inject it via `ServerState`:

```rust
// core/src/runner.rs
#[cfg_attr(test, mockall::automock)]
pub trait ProcessRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<std::process::Output>;
}
```

Until this refactor is done, do not write tests for code that shells out. Add the trait first,
then the tests.

### JWT / Secrets — temp files

Handlers that mint or verify JWTs read a secret from a file path in `Cli`. In tests, write a
known secret to a `tempfile::NamedTempFile` and pass its path. The `test_cli()` helper in
`tests/common` already does this.

---

## What to Test

### Priority 1 — Pure functions (no mocks, fast)

Live in `#[cfg(test)]` blocks inside the source file or in `tests/` integration files.

| Module | What to cover |
|---|---|
| `core::input` | Every validator/parser: valid input, each error variant, boundary values |
| `core::sources` | Path/hash utilities, `get_cache_nar_location`, SSH key format |
| `core::types` | `NixCacheInfo::to_nix_string()`, `NixPathInfo::to_nix_string()` |
| `entity::*` | Enum `FromStr`/Display roundtrips (`Architecture`, `BuildStatus`, `EvaluationStatus`) |
| `nix-daemon::nix::wire` | Wire protocol encode/decode (already covered inline) |

These run offline with no setup. They should be the majority of tests.

### Priority 2 — HTTP handlers (mock DB + axum-test)

Requires extracting `create_router(state: Arc<ServerState>) -> Router` from `serve_web` (one-line refactor).

For each endpoint, write tests for:
1. **Auth enforcement** — 401 without JWT, 403 when caller lacks permission
2. **Not-found** — 404 when the DB returns no rows
3. **Validation** — 400 for invalid body fields
4. **Happy path** — correct status code and key response values

Do NOT assert every JSON field — that re-tests serde. Only assert on values the handler itself
computes (aggregations, transformations, IDs it creates).

```rust
// web/tests/projects.rs
#[tokio::test]
async fn get_project_metrics_empty_returns_empty_points() {
    // DB: project found, then no completed evaluations
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![common::project(common::org().id)]])
        .append_query_results([Vec::<evaluation::Model>::new()])
        .into_connection();

    let resp = common::server(db).await
        .get("/api/v1/projects/test-org/test-project/metrics")
        .add_header("Authorization", common::bearer_token())
        .await;

    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["message"]["points"].as_array().unwrap().len(), 0);
}
```

### Priority 3 — Business logic (mock DB, async)

| Function | What to cover |
|---|---|
| `builder::scheduler::evaluation::gc_old_evaluations` | Keeps exactly `keep` evals; deletes oldest; nulls cross-references before deleting to avoid cascade issues |
| `web::endpoints::projects` — metrics aggregation | `build_time_total_ms` sums only Completed builds; falls back to timestamp diff when `build_time_ms` is NULL |
| `web::endpoints::auth` — login | Wrong password → 401; correct → response contains token |

---

## Avoiding Boilerplate

### The `Cli` problem

`Cli` has 30+ fields. Writing it inline in every test is the main source of boilerplate and
the main reason tests go stale (they stop compiling when a field is added).

**Rule: never spell out `Cli` fields outside of `tests/common`.** All test files use
`common::test_cli()`. When a field is added to `Cli`, update exactly one place.

### Pattern: `tests/common.rs` (one per crate)

```rust
// core/tests/common.rs

pub fn test_cli() -> Cli {
    Cli {
        log_level: "error".into(),   // suppress noise in test output
        ip: "127.0.0.1".into(),
        port: 3000,
        serve_url: "http://127.0.0.1:3000".into(),
        database_url: None,
        database_url_file: None,
        max_concurrent_evaluations: 2,
        max_concurrent_builds: 10,
        evaluation_timeout: 5,
        store_path: None,
        base_path: "/tmp/gradient-test".into(),
        disable_registration: false,
        oidc_enabled: false,
        oidc_required: false,
        oidc_client_id: None,
        oidc_client_secret_file: None,
        oidc_scopes: None,
        oidc_discovery_url: None,
        crypt_secret_file: "test-secret".into(),   // tests/fixtures/test-secret
        jwt_secret_file: "test-jwt".into(),        // tests/fixtures/test-jwt
        serve_cache: false,
        binpath_nix: "nix".into(),
        binpath_ssh: "ssh".into(),
        report_errors: false,
        email_enabled: false,
        email_require_verification: false,
        email_smtp_host: None,
        email_smtp_port: 587,
        email_smtp_username: None,
        email_smtp_password_file: None,
        email_from_address: None,
        email_from_name: "Gradient Test".into(),
        email_disable_tls: false,
        state_file: None,
        delete_state: true,
        keep_evaluations: 30,
        max_nixdaemon_connections: 2,
        nar_ttl_hours: 0,
    }
}

pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    Arc::new(ServerState { db, cli: test_cli() })
}

pub fn db_empty<T: sea_orm::IntoMockRow>() -> DatabaseConnection {
    db_with(Vec::<T>::new())
}

pub fn db_with<T: sea_orm::IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}

// Fixture builders — deterministic values, easy to read in assertions
pub fn org() -> organization::Model { ... }
pub fn project(org_id: Uuid) -> project::Model { ... }
pub fn user() -> user::Model { ... }
pub fn evaluation(project_id: Uuid) -> evaluation::Model { ... }
pub fn build(eval_id: Uuid) -> build::Model { ... }
```

### Pattern: web handler test

```rust
// web/tests/common/mod.rs
pub async fn server(db: DatabaseConnection) -> TestServer {
    // Requires: pub fn create_router(state: Arc<ServerState>) -> Router in web/src/lib.rs
    let app = web::create_router(test_state(db));
    TestServer::new(app).unwrap()
}

// web/tests/orgs.rs
#[tokio::test]
async fn get_org_unauthenticated_returns_401() {
    let s = common::server(common::db_with(vec![common::org()])).await;
    s.get("/api/v1/orgs/test-org").await.assert_status_unauthorized();
}

#[tokio::test]
async fn get_org_unknown_returns_404() {
    let s = common::server(common::db_empty::<organization::Model>()).await;
    s.get("/api/v1/orgs/nonexistent")
        .add_header("Authorization", common::bearer_token())
        .await
        .assert_status_not_found();
}
```

---

## File Layout

```
core/
  tests/
    mod.rs          ← pub mod common; pub mod input; pub mod sources;
    common.rs       ← test_cli(), test_state(), db_with(), fixture builders
    input.rs        ← pure function tests for core::input (validators, parsers)
    sources.rs      ← SSH key generation, path/hash utilities

web/
  tests/
    common/
      mod.rs        ← test_cli(), test_state(), server(), bearer_token(), fixtures
    auth.rs         ← register / login / logout flows
    orgs.rs         ← CRUD, member management, auth enforcement
    projects.rs     ← CRUD, metrics, keep_evaluations GC
    builds.rs       ← status queries, log endpoints
    evals.rs        ← evaluation actions, build listing

builder/
  tests/
    common/
      mod.rs        ← test_state(), db_with()
    gc.rs           ← gc_old_evaluations: keeps N, deletes oldest, safe cascade

entity/
  (no integration tests — entity crate only defines models and enums)
  (enum FromStr/Display roundtrips live in #[cfg(test)] blocks in each entity file)
```

---

## Adding a New Test

1. Add the test function to the appropriate file in `tests/`.
2. Use `common::test_state(db)` — never build `Cli` inline.
3. For a handler test: pre-load **only** the DB rows that handler will query, in order.
4. Assert on **computed values** only — not on IDs or timestamps you inserted.
5. Name the test `<subject>_<condition>_<expected_outcome>`, e.g.
   `get_project_metrics_no_evals_returns_empty_points`.
