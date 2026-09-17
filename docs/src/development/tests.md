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
| SQL plan gate | `backend/src/sql_gate/`, run by the e2e VM test | every registered statement's plan at production scale |

A crate's own `tests/` directory is for anything that has to go through a public
entry point (an HTTP route, a CLI invocation). Everything else belongs in a
`#[cfg(test)]` module next to the code it covers.

## Running them

```sh
cargo test --workspace --tests          # backend, from backend/
cargo test --manifest-path cli/Cargo.toml --tests
pnpm -C frontend exec ng test --watch=false
nix flake check                         # every check below
```

VM tests are discovered by directory: any folder added under
`nix/tests/gradient/` becomes the check `gradient-<folder>` with no wiring.

The cargo suites are checks (`unittest`, `cli-unittest`) rather than the check
phase of the packages: `nix build .#gradient` produces the binary only. Doc tests
run in `unittest` after nextest, where the workspace is already compiled.
They build under `[profile.test]`, so a test target compiles unoptimised and
without the full DWARF that `separateDebugInfo` puts on the shipped binary,
while `[profile.dev.package."*"]` keeps their dependencies optimised.

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

**Assert on one statement, never on a formatted transaction.** A `MockDatabase`
records everything a transaction ran as ONE log entry, and sea-orm brackets that
entry's statement list with a synthetic `BEGIN`/`COMMIT` (and `SAVEPOINT` for a
nested one). Formatting entries lets a `contains` straddle two statements, and
counting them shifts every index by one. `gradient_db::pool::statements(log)`
flattens the log to one string per statement with the transaction control
removed; use it instead of mapping `into_transaction_log()` by hand.

**A spawned task races the result buffer.** The buffer is ordered and shared, so
a handler that spawns database work pops results out from under the request path.
Assert on what the response says, not on how many queries were consumed.

**Reach for a fake, not a mock framework.** Anything touching nix, git, the
filesystem or the network is behind a trait; implement the trait in
`test-support/src/fakes/` and record the calls. Recording fakes
(`RecordingJobReporter`, `RecordingWebhookClient`) let a test assert on the
sequence of effects rather than on internal state. When the trait exists only to
lift one algorithm out of its I/O (`RelayIo` under the worker's closure relay),
the fake stays in that module's own `tests`: it is a fixture for one walk, not a
double anything else will reuse.

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
moved, so the e2e test asserts the plan shape (`EXPLAIN`: a nested loop, no merge
join) and bills the run through `pg_stat_statements` (`shared_preload_libraries` on
the test's Postgres, statements filtered to the server's role). Keep the thresholds
loose enough to be pathology detectors on a slow shared VM, and print the top
statements so a human reads the numbers the assertion cannot.

**Every hand-written statement is registered and explained.** `gradient_db::sql!`
declares a statement, its parameter kinds and its tier, and registers it;
`backend/clippy.toml` forbids building a `Statement` any other way, so the list
cannot fall behind. The e2e test's last phase amplifies its database to
production scale and runs `gradient-sql-gate`, which draws real parameter values
out of that data, explains each statement in a rolled-back transaction (an
`EXPLAIN ANALYZE` of an `INSERT` really does insert) and fails on a sequential
scan of a large relation, a buffer or amplification budget overrun, a per-row
rescan or a disk spill. Tier `Hot` is the default, `Walk` covers the recursive
closure walks and also asserts the `OFFSET 0` fence survived, `Sweep` covers
timer-driven work that is allowed to scan. Nothing is asserted on wall clock: the
runner is shared and slow, so a millisecond budget would measure the runner. A
statement whose relations are empty is reported unmeasured rather than passed,
and the phase fails once more than 40 of them are, so the count is a ratchet
rather than a demand that the VM's fixture exercise every table.

**Two database sessions, held against each other, prove a lock is load-bearing.**
Each `psql` helper is a fresh process, so a phase that replays statements in order
shows only that nothing bad happened - it still passes once the discipline is
deleted from the code. The e2e test drives two FIFO-fed `psql` sessions from one
shell script, with a third connection polling `pg_stat_activity` for
`wait_event_type = 'Lock'` as the handshake, and runs the same interleaving twice:
once taking the ordered lock in its own statement, once without. Assert the
contrast - the unlocked arm stores the stale value, the locked arm writes nothing -
so the phase fails if the two ever agree. Bound every wait and dump
`pg_stat_activity` into the failure message, tear both backends down on every exit
path (a leaked one wedges the phases after it), and lock only synthetic rows the
rest of the test never reads.

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
