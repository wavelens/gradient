# Tests

How testing is structured and which patterns a new test follows. Individual tests are not catalogued here: the source tree is the catalogue, and a per-test list goes stale.

## Layers

| Layer | Location | Exercises |
|---|---|---|
| Backend unit | `#[cfg(test)] mod tests` beside the code | Pure logic, no I/O |
| Backend integration | `backend/gradient-*/tests/*.rs` | The real router and handlers over a mocked database |
| Shared harness | `backend/gradient-test-support/` | Fakes, fixtures and the test server every suite reuses |
| CLI | `cli/tests/*.rs` | The `gradient` binary against a stub HTTP server |
| Frontend | `frontend/src/**/*.spec.ts` | Components and services under vitest |
| Report inspector | `nix/tools/report-inspector/tests/` | Inspector commands over a report fixture built in the test |
| NixOS VM | `nix/tests/gradient/<name>/`, `nix/tests/harness/` | A booted machine running the packaged server, or a module against a scripted API |
| SQL plan gate | `backend/src/sql_gate/`, run by the e2e VM test | Every registered statement's plan at production scale |
| Mock daemon | `backend/gradient-daemon/`, `nix/tests/store-spec/`, `nix/tests/gradient/scheduler/` | Scheduler and workers against a scripted Nix store |

- A crate's `tests/` directory is for public entry points (an HTTP route, a CLI call); everything else goes into a `#[cfg(test)]` module next to the code.
- The inspector fixture is built in the test, never committed as a `.db`: a checked-in binary drifts from its schema.

## Running

| Command | Runs |
|---|---|
| `nix flake check` | Every check below |
| `nix build .#checks.x86_64-linux.unittest` | Backend tests (nextest, then doc tests) |
| `nix build .#checks.x86_64-linux.cli-unittest` | CLI tests |
| `nix build .#checks.x86_64-linux.gradient-<name>` | One VM test |
| `pnpm -C frontend exec ng test --watch=false` | Frontend tests |

!!! warning
    A local `cargo test` or `cargo nextest` over the whole backend workspace needs more memory than a typical workstation has. Use the Nix checks.

- The cargo suites are checks, not the packages' check phase: `nix build .#gradient` builds the binary only.
- Tests build under `[profile.test]`: unoptimised, without the full DWARF of the shipped binary; `[profile.dev.package."*"]` keeps dependencies optimised.
- Any folder under `nix/tests/gradient/` becomes the check `gradient-<folder>` without wiring.
- Every VM test stores NARs on local disk except `s3`, which runs against MinIO to cover presigned uploads and their commit.

## Shared Harness

`gradient-test-support` exists so no suite builds its own scaffolding; extend it rather than re-deriving a helper. `use gradient_test_support::prelude::*` brings in the common items.

| Module | Provides |
|---|---|
| `fakes/` | In-memory doubles: `FakeNixStoreProvider`, `FakeDerivationResolver`, `FakeWorkerStore`, `FakeDrvReader`, `RecordingJobReporter`, `RecordingCiReporter`, `InMemoryEmailSender` |
| `fakes/store_fixture.rs` | `StoreFixture`: a real 951-derivation `hello` closure from `backend/test-store/`, with helpers to mark subtrees built or unbuild a fraction |
| `fixtures.rs` | Canonical entity models and stable IDs (`project()`, `user()`, `eval_at()`, ...) |
| `db.rs` | `db_with(rows)`, the `MockDatabase` setup |
| `state.rs` | `test_state()` and variants: a wired `ServerState` |
| `web.rs` | `make_test_server()`, `make_token()`, `live_session()` for authenticated requests |
| `cli.rs` | `test_cli()` |
| `log_storage.rs` | `NoopLogStorage`, `RecordingLogStorage` |
| `cache_fixture.rs` | Populated cache and NAR state |

The prelude also re-exports the protocol doubles: `MockProtoServer` (`gradient-wire`) scripts an authority (handshake, offers, scores, claims, NAR relay); `ProtoPeer` (`gradient-worker-client`) scripts a worker on the real client connection.

## Patterns

### Scope

- **Test what we wrote.** A test that asserts serde serialised a field, or that SeaORM returned the handed row, covers a dependency. A test whose deletion changes nobody's confidence is not added.
- **Fakes, not a mock framework.** Everything touching nix, git, the filesystem or the network sits behind a trait; the fake implements the trait and records calls. Recording fakes let a test assert the sequence of effects.
- **A fake for one algorithm stays local.** A trait that only lifts one algorithm out of its I/O (`UpstreamIo` in the worker's substitute executor) keeps its fake in that module's tests.
- **Actors with side effects sit behind small traits.** `gradient-effects` takes `OutboxStore` and `Dispatch`; the actor's one promise (at most the worker count in flight, one pass per burst of wakes) is asserted against in-memory fakes with no database and no clock.
- **Closure and scheduling logic uses `StoreFixture`**, a genuine derivation graph instead of a hand-built three-node tree.

### MockDatabase

- **Results replay in FIFO order.** Each `append_query_results` feeds the next `SELECT`: the script is the handler's query sequence (auth, loads, the endpoint's work). State the sequence in the module doc comment.
- **`INSERT ... RETURNING`** needs both an `append_query_results` for the row and an `append_exec_results` with `rows_affected: 1`; otherwise SeaORM treats the insert as a no-op.
- **Assert on one statement, never on a formatted transaction.** A transaction is one log entry bracketed by a synthetic `BEGIN`/`COMMIT`. `gradient_db::pool::statements(log)` flattens the log to one string per statement without transaction control.
- **Quoted identifiers or bound values** need `raw_statements(log)` and a match on `s.sql` / `s.values`: `statements` returns `Debug` strings with escaped quotes.
- **Spawned work races the result buffer.** Assert on the response, not on how many queries ran.

### VM Tests

- **A module under test gets a scripted API.** A module that only consumes the HTTP API (`gradient-deploy.nix`) runs against a stdlib-only stub driven through `/control`, which swaps the scripted state and pushes the matching WebSocket event. The VM needs no Postgres and no builder.
- **Assert on the database's own accounting.** The e2e test checks plan shapes (`EXPLAIN`: nested loop, no merge join) and bills the run through `pg_stat_statements`. Thresholds stay loose (pathology detectors on a slow VM), and the top statements are printed.
- **Two database sessions prove a lock.** Two FIFO-fed `psql` sessions from one script, a third connection polling `pg_stat_activity` for `wait_event_type = 'Lock'`, and the same interleaving twice: with and without the lock. Assert the contrast.
    - Bound every wait and dump `pg_stat_activity` into the failure.
    - Tear both backends down on every exit path; lock only synthetic rows.
    - Rows no fixture offers go into a phase-owned schema: `CREATE TABLE <schema>.<t> (LIKE public.<t> INCLUDING DEFAULTS INCLUDING INDEXES)` plus `SET search_path = <schema>, public`.
    - Statements come from `gradient-sql-gate --print <NAME>`, never pasted: a pasted copy passes after the code changed.

### SQL Plan Gate

`gradient_db::sql!` declares every hand-written statement with its parameter kinds and tier; `backend/clippy.toml` forbids building a `Statement` any other way. The e2e test's last phase amplifies the database to production scale and runs `gradient-sql-gate`, which draws real parameters and explains each statement in a rolled-back transaction.

| Tier | Covers | Extra |
|---|---|---|
| `Hot` | Default | |
| `Bulk` | Cost follows a working set: dashboards, metric scrapes, array-keyed batches, the dispatcher's ranking | |
| `Walk` | Recursive closure walks | Asserts the `OFFSET 0` fence in the recursive term |
| `Sweep` | Timer-driven work allowed to scan | |

- **Failures:** a sequential scan of a large relation that throws most of its read away, a buffer or amplification budget overrun, a per-row rescan, a disk spill.
- **Exceptions:** aggregates, statements returning nothing and batches are measured by their own rule.
- **No wall-clock budgets:** the runner is shared and slow.
- **Unmeasured statements** (empty relations) are reported, not passed; more than 40 fail the phase.
- **Tables only a user fills** (stars) are filled through the real API before the gate; values that differ per run (commit hash prefixes) are drawn parameter kinds.

### CLI and Lints

- **CLI tests drive the real binary:** `assert_cmd` runs `gradient` with `HOME` and `XDG_CONFIG_HOME` in a `TempDir` with a seeded `config.toml`; `wiremock` stands in for the server.
- **`unwrap` needs a reason.** Both workspaces deny `clippy::unwrap_used`; test scaffolding opts out per file:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
```

- Bare `#[allow(unused)]`, `#[allow(dead_code)]` and `#[allow(unused_imports)]` fail CI everywhere, tests included.

## Mock Daemon

`gradient-scheduler` runs the real server and workers; each worker's `nix-daemon` is replaced by `gradient-daemon serve --backend mock` on the stock socket. The `mock` feature of `backend/gradient-daemon` builds the binary into the `daemon` output of the `gradient` package.

| Aspect | Behavior |
|---|---|
| Store spec | A plain attrset: `derivations.<id>` with `deps`, `outputs.<o>.references` (`"<node>.<output>"`), `build.outcome` (`success`, `fail`, `hang`), `present.workers`, `present.cache`. Defaults and invariants in `nix/tests/store-spec/default.nix`; presets `chain n`, `diamond`, `fanOut n`, `wide depth width` |
| Paths | `derivations.nix` feeds both the published flake and the daemon config; the `store-spec` check asserts drv and output paths agree |
| Presence | `present.workers` is seeded at boot; `present.cache` becomes a signed file cache used as upstream |
| Timing | Seeded lognormal delays (median 40 ms); `GRADIENT_DAEMON_SEED` replays a run |
| Violations | A build with a missing input, a rebuild of a valid output, an unknown derivation, an unmodelled op. Every phase ends with `violations == []` |
| Control | `gradient-daemon ctl`: `journal`, `builds`, `latency`, `violations`, `running`; `seed`, `forget`, `outcome`, `release` |
| Latency | Each phase appends ready-to-dispatch and build spreads, the critical path and the overhead factor to `latency.jsonl` |

Every reference lies in the runtime closure of the requested inputs, as in Nix. **Production replay:** `gradient-report <report.db> store-spec -o spec.nix` turns a report into a store spec with edges, references, sizes and scaled durations; `--allow-failed` replays failures, `--anonymize` renames packages to `n0`, `n1`, ...

## Topologies

The scheduler and e2e suites are `mk.nix { self, pkgs, topology }`. Their `default.nix` passes `nix/tests/harness/topologies/direct.nix`; the flake exports both as `lib.tests.{scheduler,e2e} { system, topology }` for other repositories.

- A topology is `{ pkgs, lib, workers, token, ... }: { nodes, upstreamPeers, workerNodes, provides, pythonPrelude }`.
- The suite owns the `server` node (e2e also `client`) and each worker's role; the topology owns how workers reach the server and which IDs the server registers.
- `nix/tests/harness/contract.nix` (`lib.tests.contract`) is asserted at evaluation; `check.nix` (the `test-topologies` check) pins the direct topology.
- Scripts use the prelude, never a hardcoded unit or ID: `WORKER_NODES`, `wait_workers_ready()`, `fleet_units()`, `requires(what, *tags)`.
- An assertion that needs the server to see each worker directly goes under `requires(..., "distinct-upstream-workers")`. Gate the assertion, not the phase, when later phases need the state.
- The proxy repository runs both suites over `topologies.proxied` as `scheduler-proxied` and `e2e-proxied`.

## Conventions

- One file per contract, named after the contract: `cache_roles.rs`, `auth_middleware.rs`, `body_size_limit.rs`.
- A module doc comment states what the file covers and the harness setup, such as the query script the mocked database replays.
- Test names read as the asserted behaviour (`a_refused_session_backs_off`).
- Regression tests carry a one-line comment naming the defect and the issue number.
