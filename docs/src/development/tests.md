# Tests

How testing is structured in Gradient and which patterns to follow when adding
one. Individual tests are deliberately not catalogued here: the source tree is
the catalogue, and a per-test list goes stale and collides on every merge.

## Where tests live

| Layer | Location | Exercises |
|---|---|---|
| Backend unit | `#[cfg(test)] mod tests` beside the code | pure logic, no I/O |
| Backend integration | `backend/gradient-{web,core,report}/tests/*.rs` | the real router and handlers over a mocked database |
| Shared harness | `backend/gradient-test-support/` | fakes, fixtures and the test server every suite reuses |
| CLI | `cli/tests/*.rs`, `cli/connector/tests/*.rs` | the installed `gradient` binary against a stub HTTP server |
| Frontend | `frontend/src/**/*.spec.ts` | components and services under vitest |
| NixOS VM | `nix/tests/gradient/<name>/` | a booted machine running the packaged server, or a NixOS module against a scripted API |

A crate's own `tests/` directory is for anything that has to go through a public
entry point (an HTTP route, a CLI invocation). Everything else belongs in a
`#[cfg(test)]` module next to the code it covers.

## Running them

```sh
cargo test --workspace --tests          # backend, from backend/
cargo test --manifest-path cli/Cargo.toml --tests
pnpm -C frontend exec ng test --watch=false
nix flake check                         # VM tests plus workspace clippy
```

VM tests are discovered by directory: any folder added under
`nix/tests/gradient/` becomes the check `gradient-<folder>` with no wiring.

## The shared harness

`gradient-test-support` exists so no suite builds its own scaffolding. Prefer
extending it over re-deriving a helper in a test file.

| Module | Provides |
|---|---|
| `fakes/` | In-memory doubles for the production traits: `NixStoreProvider`, `DerivationResolver`, `WorkerStore`, `DrvReader`, `JobReporter`, `BuildExecutor`, `CiReporter`, `EmailSender`, `WebhookClient`, `LogStorage`, plus `MockProtoServer` for the worker protocol |
| `fakes/store_fixture.rs` | `StoreFixture`, a real 951-derivation `hello` closure loaded from `test/store/`, with helpers to mark subtrees built or unbuild a deterministic fraction |
| `fixtures.rs` | Canonical entity models and stable IDs (`project()`, `user()`, `eval_at()`, …) |
| `db.rs` | `db_with(rows)`, the four-line `MockDatabase` setup |
| `state.rs` | `test_state()` and variants, a wired `ServerState` around a connection |
| `web.rs` | `make_test_server()`, `make_token()`, `live_session()` for authenticated requests |
| `cache_fixture.rs` | Populated cache and NAR state |

## Patterns

**Test what we wrote.** A test that asserts serde serialised a field, or that
SeaORM returned the row you handed it, covers a dependency rather than Gradient.
If deleting the test would not change your confidence in the code, do not add it.

**`MockDatabase` replays results in FIFO order.** Each `append_query_results`
feeds the next `SELECT` the handler issues, so a test script is the handler's
query sequence written out: auth, then the loads, then whatever the endpoint
does. Handlers that `INSERT … RETURNING` need both an `append_query_results` for
the returned row and an `append_exec_results` with `rows_affected: 1`, otherwise
SeaORM treats the insert as a no-op. State that sequence in the module doc
comment; it is what makes the test readable a year later.

**A spawned task races the result buffer.** The buffer is ordered and shared, so
a handler that spawns database work pops results out from under the request path.
Assert on what the response says, not on how many queries were consumed.

**Reach for a fake, not a mock framework.** Anything touching nix, git, the
filesystem or the network is behind a trait; implement the trait in
`test-support/src/fakes/` and record the calls. Recording fakes
(`RecordingJobReporter`, `RecordingWebhookClient`) let a test assert on the
sequence of effects rather than on internal state.

**Closure and scheduling work uses `StoreFixture`.** It carries a real
derivation graph, so dependency ordering, readiness and cache-presence logic get
tested against genuine `.drv` shapes instead of a hand-built three-node tree.

**A NixOS module under test gets a scripted API, not a server.** A module that
only consumes the HTTP API (`nix/modules/gradient-deploy.nix`) is exercised
against a stdlib-only stub in its test directory, driven through a `/control`
endpoint that swaps the scripted state and pushes the matching live-WebSocket
event. That keeps the VM free of Postgres and a builder, so the test asserts on
the module's own behaviour: what it waits for, what it never requests, and how
many times it asks.

**A VM test can assert on the database's own accounting.** Nothing in the type
system notices a lost `OFFSET 0` fence or a counter that is re-derived instead of
moved, so the cache test asserts the plan shape (`EXPLAIN`: a nested loop, no merge
join) and bills the run through `pg_stat_statements` (`shared_preload_libraries` on
the test's Postgres, statements filtered to the server's role). Keep the thresholds
loose enough to be pathology detectors on a slow shared VM, and print the top
statements so a human reads the numbers the assertion cannot.

**CLI tests drive the real binary.** `assert_cmd` runs `gradient` with `HOME`
and `XDG_CONFIG_HOME` pointed at a `TempDir` holding a seeded `config.toml`, and
`wiremock` stands in for the server. That covers argument parsing, config
resolution and exit codes in one pass, which is where CLI bugs actually live.

**`unwrap` needs a reason.** The workspace denies `clippy::unwrap_used`. Test
scaffolding opts out per file with an explicit reason:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
```

Bare `#[allow(unused)]`, `#[allow(dead_code)]` and `#[allow(unused_imports)]` are
rejected by CI everywhere, tests included.

## Conventions

- One file per contract, named after the contract, not the module:
  `cache_roles.rs`, `auth_middleware.rs`, `body_size_limit.rs`.
- A module doc comment at the top of each integration test file states what the
  file covers and any harness setup a reader needs, such as the query script the
  mocked database replays.
- Test names read as the behaviour being asserted
  (`a_refused_session_backs_off`), not as the function under test.
- Regression tests carry a one-line comment naming the defect and, where there
  is one, the issue number. That comment is the documentation; there is no
  separate page to update.
