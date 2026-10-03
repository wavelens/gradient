# Tests

How testing is structured and which patterns a new test is following. Individual tests are not catalogued here. The source tree is the catalogue, and a per-test list would go stale.

## Layers

| Layer | Location | Coverage |
|---|---|---|
| Backend unit | `#[cfg(test)] mod tests` beside the code | Pure logic, no I/O |
| Backend integration | `backend/gradient-*/tests/*.rs` | The real router and handlers over a mocked database |
| Shared harness | `backend/gradient-test-support/` | Fakes, fixtures and the test server reused by every suite |
| CLI | `cli/tests/*.rs` | The `gradient` binary against a stub HTTP server |
| Frontend | `frontend/src/**/*.spec.ts` | Components and services under vitest |
| Report inspector | `nix/tools/report-inspector/tests/` | Inspector commands over a report fixture built in the test |
| NixOS VM | `nix/tests/gradient/<name>/`, `nix/tests/harness/` | A booted machine running the packaged server, or a module against a scripted API |
| SQL plan gate | `backend/src/sql_gate/`, started by the e2e VM test | Every registered statement's plan at production scale |
| Mock daemon | `backend/gradient-daemon/`, `nix/tests/store-spec/`, `nix/tests/gradient/scheduler/`, `nix/tests/gradient/cluster/` | Scheduler and workers against a scripted Nix store |
| Eval benchmark | `nix/tests/bench/evalbench/`, on request | Evaluation speed per stage, with a capture bundle per round ([Eval Benchmark](eval-benchmark.md)) |

- A crate's `tests/` directory is for public entry points (an HTTP route, a CLI call). Everything else is going into a `#[cfg(test)]` module next to the code.
- The inspector fixture is built in the test, never committed as a `.db`. A checked-in binary would drift from its schema.

## Running

| Command | Checks |
|---|---|
| `nix flake check` | Every check below |
| `nix build .#checks.x86_64-linux.unittest` | Backend tests (nextest, then doc tests) |
| `nix build .#checks.x86_64-linux.cli-unittest` | CLI tests |
| `nix build .#checks.x86_64-linux.gradient-<name>` | One VM test |
| `nix build .#gradient-evalbench` | The eval benchmark, outside `nix flake check` |
| `pnpm -C frontend exec ng test --watch=false` | Frontend tests |

!!! warning
    A local `cargo test` or `cargo nextest` over the whole backend workspace is needing more memory than a typical workstation has. The Nix checks are the supported way.

- The cargo suites are checks, not the packages' check phase. `nix build .#gradient` is building the binary only.
- Tests build under `[profile.test]`, unoptimised and without the full DWARF of the released binary. `[profile.dev.package."*"]` is keeping dependencies optimised.
- Any folder under `nix/tests/gradient/` is becoming the check `gradient-<folder>` without wiring.
- Every VM test is storing NARs on local disk. S3 storage has no VM test.

## Shared Harness

`gradient-test-support` is sparing every suite its own scaffolding. A missing helper is added to the crate rather than re-derived locally. `use gradient_test_support::prelude::*` is bringing in the common items.

| Module | Contents |
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

The prelude is also re-exporting the protocol doubles.

- `MockProtoServer` (`gradient-wire`) is scripting an authority: handshake, offers, scores, claims, NAR passthrough.
- `ProtoPeer` (`gradient-worker-client`) is scripting a worker on the real client connection.

## Patterns

### Scope

- **Test what we wrote.** A test asserting that serde serialised a field, or that SeaORM returned the handed row, is covering a dependency. A test whose deletion would change nobody's confidence is not added.
- **Fakes, not a mock framework.** Everything touching nix, git, the filesystem or the network is sitting behind a trait. The fake is implementing the trait and recording calls. Recording fakes let a test assert the sequence of effects.
- **A fake for one algorithm is staying local.** Some traits only lift one algorithm out of its I/O (`UpstreamIo` in the worker's substitute executor). Such a trait is keeping its fake in that module's tests.
- **Actors with side effects sit behind small traits.** `gradient-effects` is taking `PendingDeliveryStore` and `Handoff`. The actor's one promise is at most the worker count in flight, with one pass per burst of wakes. In-memory fakes are asserting this promise with no database and no clock.
- **Closure and scheduling logic is using `StoreFixture`**, a genuine derivation graph instead of a hand-built three-node tree.

### MockDatabase

- **Results replay in FIFO order.** Each `append_query_results` is feeding the next `SELECT`. The script is the handler's query sequence (auth, loads, the endpoint's work). The module doc comment is stating that sequence.
- **`INSERT ... RETURNING`** is needing both an `append_query_results` for the row and an `append_exec_results` with `rows_affected: 1`. SeaORM is otherwise treating the insert as a no-op.
- **Assert on one statement, never on a formatted transaction.** A transaction is one log entry bracketed by a synthetic `BEGIN`/`COMMIT`. `gradient_db::pool::statements(log)` is flattening the log to one string per statement without transaction control.
- **Quoted identifiers or bound values** need `raw_statements(log)` and a match on `s.sql` / `s.values`. `statements` is returning `Debug` strings with escaped quotes.
- **Spawned work is racing the result buffer.** Assertions target the response, not the count of executed queries.

### VM Tests

- **A module under test is getting a scripted API.** Some modules only consume the HTTP API (`gradient-client/deploy.nix`). Such a module is tested against a stdlib-only stub driven through `/control`.
- The stub's `/control` is swapping the scripted state and pushing the matching WebSocket event. The VM is needing no Postgres and no builder.
- **Assert on the database's own accounting.** The e2e test is checking plan shapes (`EXPLAIN`: nested loop, no merge join). The test is billing the round through `pg_stat_statements`. Thresholds stay loose (pathology detectors on a slow VM), and the top statements are printed.
- **Two database sessions prove a lock.** The setup is using two FIFO-fed `psql` sessions from one script and a third connection polling `pg_stat_activity` for `wait_event_type = 'Lock'`. The same interleaving is played twice, with and without the lock. The test is asserting the contrast.
    - Every wait is bounded, and the failure is carrying a `pg_stat_activity` dump.
    - Both backends are torn down on every exit path. Only synthetic rows are locked.
    - Rows offered by no fixture go into a phase-owned schema: `CREATE TABLE <schema>.<t> (LIKE public.<t> INCLUDING DEFAULTS INCLUDING INDEXES)` plus `SET search_path = <schema>, public`.
    - Statements come from `gradient-sql-gate --print <NAME>`, never pasted. A pasted copy would still pass after the code changed.

### SQL Plan Gate

`gradient_db::sql!` is declaring every hand-written statement with its parameter kinds and tier. `backend/clippy.toml` is forbidding any other way of building a `Statement`. The e2e test's last phase is amplifying the database to production scale before starting `gradient-sql-gate`. The gate is drawing real parameters and explaining each statement in a rolled-back transaction.

| Tier | Scope | Extra |
|---|---|---|
| `Hot` | Default | |
| `Bulk` | Cost following a working set: dashboards, metric scrapes, batches over id arrays, the build assigner's ranking | |
| `Walk` | Recursive closure walks | Asserting the `OFFSET 0` fence in the recursive term |
| `Sweep` | Timer-driven work allowed to scan | |

- **Failures:** a sequential scan of a large relation throwing most of its read away, a buffer or amplification budget overrun, a per-row rescan, a disk spill.
- **Exceptions:** aggregates, statements returning nothing and batches are measured by their own rule.
- **No wall-clock budgets:** the runner is shared and slow.
- **Unmeasured statements** (empty relations) are reported, not passed. More than 40 are failing the phase.
- **Tables filled only by users** (stars) are filled through the real API before the gate. Values differing per round (commit hash prefixes) are drawn parameter kinds.
- **Draws are ordered**, never heap order. The single evaluation is the one naming the most shared builds, the worst case its statements must fit. Every other kind is taking the oldest rows by id.

### CLI and Lints

- **CLI tests drive the real binary.** `assert_cmd` is running `gradient` with `HOME` and `XDG_CONFIG_HOME` in a `TempDir` with a seeded `config.toml`. `wiremock` is standing in for the server.
- **`unwrap` only with a reason.** Both workspaces deny `clippy::unwrap_used`. Test scaffolding is opting out per file with the attribute below.

```rust
#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
```

- Bare `#[allow(unused)]`, `#[allow(dead_code)]` and `#[allow(unused_imports)]` fail CI everywhere, tests included.

## Mock Daemon

`gradient-scheduler` and `gradient-cluster` are running the real server and workers. `gradient-daemon serve --backend mock` is replacing each worker's `nix-daemon` on the stock socket. Both suites share the server node (`scheduler/server.nix`) and the script helpers (`scheduler/helpers.py`). The `mock` feature of `backend/gradient-daemon` is building the binary into the `daemon` output of the `gradient` package.

| Aspect | Behavior |
|---|---|
| Store spec | A plain attrset: `derivations.<id>` with `deps`, `outputs.<o>.references` (`"<node>.<output>"`), `build.outcome` (`success`, `fail`, `hang`), `present.workers`, `present.cache`, `sameAs` (a twin: another `.drv` with the same name, output paths and FOD content). Defaults and invariants in `nix/tests/store-spec/default.nix`. Presets `chain n`, `diamond`, `fanOut n`, `wide depth width` |
| Paths | `derivations.nix` is feeding both the published flake and the daemon config. The `store-spec` check is asserting that drv and output paths agree |
| Downloads | `download = true` is turning a node into a real `<nix/fetchurl.nix>` call with an SRI hash. The server node is serving its content under `/downloads/`, and a worker is fetching it without the mock daemon |
| Presence | `present.workers` is seeded at boot. `present.cache` is becoming a signed file cache used as upstream cache |
| Timing | Seeded lognormal delays (median 40 ms). `GRADIENT_DAEMON_SEED` is replaying a round |
| Violations | A build with a missing input, a rebuild of a valid output, an unknown derivation, an unmodelled op. Every phase is ending with `violations == []` |
| Control | `gradient-daemon ctl` with the query commands `journal`, `builds`, `latency`, `violations` and `running` and the steering commands `seed`, `forget`, `outcome` and `release` |
| Latency | Each phase is appending to `latency.jsonl` the spreads from can-start to assignment and of builds, the critical path and the overhead factor |

Every reference is inside the runtime closure of the requested inputs, as in Nix.

**Production replay:** `gradient-report <report.db> store-spec -o spec.nix` is turning a report into a store spec with edges, references, sizes and scaled durations. `--allow-failed` is replaying failures, and `--anonymize` is renaming packages to `n0`, `n1`, ...

## Topologies

The scheduler and e2e suites are `mk.nix { self, pkgs, topology }`. Their `default.nix` is passing `nix/tests/harness/topologies/direct.nix`. The flake is exporting both as `lib.tests.{scheduler,e2e} { system, topology }` for other repositories.

- A topology is `{ pkgs, lib, workers, token, ... }: { nodes, upstreamPeers, upstreamUrls?, workerNodes, provides, pythonPrelude }`.
- The optional `upstreamUrls` is mapping each upstream peer the server is dialing to its URL.
- The suite is owning the `server` node (e2e also `client`) and each worker's role. The topology is owning how workers reach the server and the IDs registered by the server.
- `nix/tests/harness/contract.nix` (`lib.tests.contract`) is asserted at evaluation. `check.nix` (the `test-topologies` check) is pinning the direct topology.
- Scripts use the prelude, never a hardcoded unit or ID: `WORKER_NODES`, `wait_workers_ready()`, `fleet_units()`, `requires(what, *tags)`.
- Some assertions need the server to see each worker directly. These go under `requires(..., "distinct-upstream-workers")`.
- The gate is placed on the assertion, not the phase, when later phases need the state.
- The proxy repository is running both suites over its `proxied` and `server-dials` topologies as `scheduler-proxied`, `e2e-proxied`, `scheduler-server-dials` and `e2e-server-dials`.

## Conventions

- One file per contract, named after the contract: `cache_roles.rs`, `auth_middleware.rs`, `body_size_limit.rs`.
- A module doc comment is stating the file's coverage and the harness setup, such as the query script replayed by the mocked database.
- Test names read as the asserted behaviour (`a_refused_session_backs_off`).
- Regression tests carry a one-line comment naming the defect and the issue number.
