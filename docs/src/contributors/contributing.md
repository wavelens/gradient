# Contributing

How to set up a development environment, run the checks and send a change. Everyone follows the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Workflow

1. Open an issue to discuss a significant change first.
2. Fork and branch from `main`.
3. Implement with tests, see [Tests](tests.md).
4. Open a pull request against `main`.

## Licensing

Gradient is **AGPL-3.0-only**; a contribution is released under the same license. Every file carries an SPDX header:

```rust
// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
//
// SPDX-License-Identifier: AGPL-3.0-only
```

## Development Setup

**Requirements:** Nix with flakes.

=== "Backend"

    ```sh
    nix run .#backend # (1)!
    cd backend && cargo run
    ```

    1.  An interactive NixOS test driver; `run_tests()` boots a VM with a packaged server, PostgreSQL 18 forwarded to the host port, a user `test` (password `password`) and a project. `cargo run` in `backend/` then works against that database.

=== "Frontend"

    ```sh
    nix run .#frontend # (1)!
    cd frontend && pnpm install && pnpm start
    ```

    1.  A VM with a superuser `admin` (password `admin_password`), a project, a task and a worker from declarative state; evaluations and builds run end to end against `pnpm start`.

`backend/.cargo/config.toml` caps parallel `rustc` jobs at 1 (`[build] jobs = 1`) to bound peak memory; `cargo build -j N` overrides the cap.

## Checks

| Check | Command |
|---|---|
| Backend tests | `nix build .#checks.x86_64-linux.unittest -L` |
| CLI tests | `nix build .#checks.x86_64-linux.cli-unittest -L` |
| Clippy | `nix build .#checks.x86_64-linux.clippy -L`, `...cli-clippy -L`, `...cli-static-clippy -L` (the CLI without Nix support) |
| VM tests | `nix build .#checks.x86_64-linux.gradient-<name> -L` for `api`, `deploy`, `e2e`, `eval`, `local-worker`, `scheduler` |
| Format | `cargo fmt --all --check`, in `backend/` and `cli/` |
| Licenses and advisories | `cargo deny check`, in `backend/` and `cli/`; GPL-family dependencies are banned |

CI (`.github/workflows/rust.yml`) executes fmt, the `#[allow]` grep gate and cargo-deny over both workspaces; clippy executes as the flake checks.

## Rust

- `cargo fmt` before committing. The toolchain comes from the devShell (`flake.lock`); `rust-toolchain.toml` mirrors it for rustup users; `rustfmt.toml` sets `style_edition = "2024"`.
- Each workspace carries its own `deny.toml`, `clippy.toml` and `rustfmt.toml`, kept in step; the backend `clippy.toml` adds the `tokio::spawn` and raw `Statement` bans.
- No `unwrap()` in production paths (`clippy::unwrap_used = "deny"`): use `?`, an explicit error branch, or `.expect("<the invariant>")` where the call cannot fail by construction.
- Shared state uses `gradient_util::sync::Mutex`, not `std::sync::Mutex`: poisoning is ignored and one panicking critical section does not break every later `lock()`.
- Log with `tracing` (`info`, `debug`, `warn`, `error`), never `println!`; `#[instrument]` on significant async functions.

| Change | Also update |
|---|---|
| New endpoint in `backend/gradient-web/src/endpoints/` | `docs/gradient-api.yaml`; the handler extracts parameters, checks authorization, queries, responds |
| New table | A migration in `backend/gradient-migration/src/`, an entity in `backend/gradient-entity/src/`, see [Migrations](migrations.md) |
| Configuration option | `nix/modules/` and the [Configuration](../reference/configuration.md) reference |

**`#[allow]` policy:**

- `#[allow(unused_imports)]`, `#[allow(unused)]` and `#[allow(dead_code)]` are forbidden (CI grep gate): fix the warning instead.
- Every other `#[allow(...)]` carries `reason = "..."` (`clippy::allow_attributes_without_reason`). `clippy::too_many_arguments` allows are temporary, tracked in #503.
- `allow-unwrap-in-tests` only reaches code inside a `#[test]` function. Integration tests and `gradient-test-support` use a crate-level `#![expect(clippy::unwrap_used, reason = "...")]`: `expect` warns once the last `unwrap()` is gone.

## Angular and TypeScript

- Standalone components with signals (`signal()`, `computed()`), feature folders under `frontend/src/app/features/`.
- UI components from `gr-ui` (`src/app/shared/ui/`, on `@angular/cdk`), charts through `<gr-metric-chart>` (Apache ECharts), colours and spacing from `src/app/styles/_variables.scss`. See the [Frontend Style Guide](frontend-style-guide.md).
- No UI or chart dependency with a field-of-use restriction: the bundle is distributed under AGPL-3.0, and anything beyond MIT, BSD or Apache-2.0 cannot be passed on.
- A refreshed `pnpm-lock.yaml` changes the `pnpmDeps` hash in `nix/packages/gradient-frontend.nix`: set `lib.fakeHash`, run `nix build .#gradient-frontend.pnpmDeps`, take the reported hash.
- `minimumReleaseAge` in `pnpm-workspace.yaml` refuses packages younger than 24 hours; a local `pnpm update` then resolves the same versions the Nix build accepts.

## Nix

- Packages and modules live in `nix/`: server options in `nix/modules/gradient.nix`, worker options in `nix/modules/gradient-worker.nix`, declarative state in `nix/modules/gradient-state.nix`.
- A new module needs a NixOS VM test under `nix/tests/gradient/`.
