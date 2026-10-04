# Contributing

Development environment, checks and the steps to send a change. All contributors follow the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Workflow

1. Open an issue to discuss a significant change first.
2. Fork and branch from `main`.
3. Implement with tests, see [Tests](tests.md).
4. Open a pull request against `main`.

## Licensing

Gradient is **AGPL-3.0-only**. Contributions are released under the same license. All files carry the SPDX header below.

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

    1.  An interactive NixOS test driver. `run_tests()` will boot a VM with a packaged server, PostgreSQL 18 forwarded to the host port, a user `test` (password `password`) and a project. `cargo run` in `backend/` can then work against that database.

=== "Frontend"

    ```sh
    nix run .#frontend # (1)!
    cd frontend && pnpm install && pnpm start
    ```

    1.  A VM with a superuser `admin` (password `admin_password`), a project, a task and a worker from declarative state. Evaluations and build jobs can run end to end against `pnpm start`.

`backend/.cargo/config.toml` will cap parallel `rustc` jobs at 1 (`[build] jobs = 1`) to bound peak memory. `cargo build -j N` can override the cap.

## Checks

| Check | Command |
|---|---|
| Backend tests | `nix build .#checks.x86_64-linux.unittest -L` |
| CLI tests | `nix build .#checks.x86_64-linux.cli-unittest -L` |
| Clippy | `nix build .#checks.x86_64-linux.clippy -L`, `...cli-clippy -L`, `...cli-static-clippy -L` (the CLI without Nix support) |
| VM tests | `nix build .#checks.x86_64-linux.gradient-<name> -L` for `api`, `cluster`, `deploy`, `e2e`, `eval`, `local-worker`, `scheduler`, `standalone`, `standalone-docker` |
| Format | `cargo fmt --all --check`, in `backend/` and `cli/` |
| Licenses and advisories | `cargo deny check`, in `backend/` and `cli/`. GPL-family dependencies are banned |

CI (`.github/workflows/rust.yml`) will check fmt, the `#[allow]` grep check and cargo-deny over both workspaces. Clippy is part of the flake checks.

## Rust

- `cargo fmt` before committing. Toolchain versions come from the devShell (`flake.lock`), mirrored in `rust-toolchain.toml` for rustup users.
- The `style_edition = "2024"` setting is part of the `rustfmt.toml` file.
- Workspaces each carry their own `deny.toml`, `clippy.toml` and `rustfmt.toml`, kept in step.
- The `tokio::spawn` and raw `Statement` bans are part of the backend `clippy.toml` file.
- No `unwrap()` in production paths (`clippy::unwrap_used = "deny"`). The alternatives are `?`, an explicit error branch, or `.expect("<the invariant>")` where the call cannot fail by construction.
- Shared state must use `gradient_util::sync::Mutex` instead of `std::sync::Mutex`. Poisoning is ignored, and one panicking critical section cannot break every later `lock()` call.
- Logging must go through `tracing` (`info`, `debug`, `warn`, `error`) and never through `println!` macros.
- `#[instrument]` on significant async functions.

| Change | Also update |
|---|---|
| New endpoint in `backend/gradient-web/src/endpoints/` | `docs/gradient-api.yaml`. Handlers extract parameters, check authorization, query and respond |
| New table | A migration in `backend/gradient-migration/src/`, an entity in `backend/gradient-entity/src/`, see [Migrations](migrations.md) |
| Configuration option | `nix/modules/` and the [Configuration](../reference/configuration.md) reference |

### `#[allow]` Policy

- `#[allow(unused_imports)]`, `#[allow(unused)]` and `#[allow(dead_code)]` are forbidden (CI grep check). Contributors fix the warning instead.
- Every other `#[allow(...)]` must carry `reason = "..."` (`clippy::allow_attributes_without_reason`).
- Uses of `#[allow(clippy::too_many_arguments)]` are temporary, tracked in #503.
- `allow-unwrap-in-tests` can only reach code inside a `#[test]` function.
- Integration tests and `gradient-test-support` use a crate-level `#![expect(clippy::unwrap_used, reason = "...")]` attribute.
- The `expect` attribute will warn once the last `unwrap()` call is gone.

## Angular and TypeScript

- Standalone components with signals (`signal()`, `computed()`), feature folders under `frontend/src/app/features/` in the app.
- UI components from `gr-ui` (`@gradient/ui/ui`, on `@angular/cdk`), charts through `<gr-metric-chart>` (Apache ECharts), colours and spacing from the `@gradient/ui/styles/variables` module. See the [Frontend Style Guide](frontend-style-guide.md).
- No UI or chart dependency with a field-of-use restriction. The bundle is distributed under AGPL-3.0. A dependency licensed beyond MIT, BSD or Apache-2.0 cannot be passed on.
- A refreshed `pnpm-lock.yaml` will change the `pnpmDeps` hash in `nix/packages/gradient-frontend.nix`. The new hash is obtainable by setting `lib.fakeHash`, running `nix build .#gradient-frontend.pnpmDeps` and taking the reported hash.
- `minimumReleaseAge` in `pnpm-workspace.yaml` will refuse packages younger than 24 hours. A local `pnpm update` will then resolve the same versions the Nix build would accept.

## Nix

- Packages and modules live in `nix/`: server options in `nix/modules/gradient.nix`, worker options in `nix/modules/gradient-worker.nix`, declarative state in `nix/modules/gradient-state.nix`.
- New modules need a NixOS VM test under `nix/tests/gradient/`.
