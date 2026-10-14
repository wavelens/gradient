# Testing

Details in `docs/src/contributors/tests.md`.

## Running

- Backend tests: `nix build .#checks.x86_64-linux.unittest`, nextest on a throwaway Postgres 18.
- Local `cargo test` and `cargo nextest` over the whole workspace exceed the memory of a typical workstation.
- Local checks before a commit: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`.

## Scope

- Tests check logic we wrote, not serde or SeaORM.
- Calls into nix, git, the filesystem or the network go through a trait with an in-memory fake.
- Tests without effect on our confidence stay out.

## Shape Tests

The graph writer and the scheduler have tests on a real Postgres 18 database.

| Part | Location | Purpose |
|---|---|---|
| Test database | `gradient-fixtures/src/database.rs` | A fresh `test_<uuid>` database per test, cloned from the migrated `gradient_template` |
| Shape builder | `gradient-fixtures/src/shape/` | The rows of a test as a chain of setters |
| Graph case layer | `gradient-graph/src/cases/` | Graph writer on a shape, zero drift after the seed and at the end |
| Scheduler case layer | `gradient-scheduler/src/cases/` | Scheduler core, graph writer and worker sessions on a shape, with invariant checks after each step |

- Server settings: `PGHOST`, `PGUSER` and `PGDATABASE`, with a role allowed to create databases (`CREATEDB`).
- Tests end with `finish()`. Tests without `finish()` panic on drop and failing tests leave the database behind.
- `SchedulerCase::check(after)` can wait for detached tasks, expect zero drift and assert the invariants listed in the tests page.

## Other Suites

- HTTP handler suites in `gradient-*/tests/` use a mocked database through `gradient_fixtures::mock`.
- `gradient-test-support`: fakes, `TestState`, the test server and `StoreFixture`.
