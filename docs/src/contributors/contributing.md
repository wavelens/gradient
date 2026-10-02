# Contributing

How to set up a development environment, run the checks and send a change. Every contributor is following the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Workflow

1. Open an issue to discuss a significant change first.
2. Fork and branch from `main`.
3. Implement with tests, see [Tests](tests.md).
4. Open a pull request against `main`.

## Licensing

Gradient is **AGPL-3.0-only**. Contributions are released under the same license. Every file is carrying the SPDX header below.

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

    1.  An interactive NixOS test driver. `run_tests()` is booting a VM with a packaged server, PostgreSQL 18 forwarded to the host port, a user `test` (password `password`) and a project. `cargo run` in `backend/` is then working against that database.

=== "Frontend"

    ```sh
    nix run .#frontend # (1)!
    cd frontend && pnpm install && pnpm start
    ```

    1.  A VM with a superuser `admin` (password `admin_password`), a project, a task and a worker from declarative state. Evaluations and builds are running end to end against `pnpm start`.

`backend/.cargo/config.toml` is capping parallel `rustc` jobs at 1 (`[build] jobs = 1`) to bound peak memory. `cargo build -j N` is overriding the cap.

## Checks

| Check | Command |
|---|---|
| Backend tests | `nix build .#checks.x86_64-linux.unittest -L` |
| CLI tests | `nix build .#checks.x86_64-linux.cli-unittest -L` |
| Clippy | `nix build .#checks.x86_64-linux.clippy -L`, `...cli-clippy -L`, `...cli-static-clippy -L` (the CLI without Nix support) |
| VM tests | `nix build .#checks.x86_64-linux.gradient-<name> -L` for `api`, `cluster`, `deploy`, `e2e`, `eval`, `local-worker`, `scheduler`, `standalone`, `standalone-docker` |
| Format | `cargo fmt --all --check`, in `backend/` and `cli/` |
| Licenses and advisories | `cargo deny check`, in `backend/` and `cli/`. GPL-family dependencies are banned |

CI (`.github/workflows/rust.yml`) is checking fmt, the `#[allow]` grep gate and cargo-deny over both workspaces. Clippy is part of the flake checks.

## Rust

- `cargo fmt` before committing. The toolchain is coming from the devShell (`flake.lock`). `rust-toolchain.toml` is mirroring the toolchain for rustup users. `rustfmt.toml` is setting `style_edition = "2024"`.
- Each workspace is carrying its own `deny.toml`, `clippy.toml` and `rustfmt.toml`, kept in step. The backend `clippy.toml` is adding the `tokio::spawn` and raw `Statement` bans.
- No `unwrap()` in production paths (`clippy::unwrap_used = "deny"`). The alternatives are `?`, an explicit error branch, or `.expect("<the invariant>")` where the call cannot fail by construction.
- Shared state is using `gradient_util::sync::Mutex`, not `std::sync::Mutex`. Poisoning is ignored, and one panicking critical section is not breaking every later `lock()`.
- Logging is going through `tracing` (`info`, `debug`, `warn`, `error`), never `println!`.
- `#[instrument]` on significant async functions.

| Change | Also update |
|---|---|
| New endpoint in `backend/gradient-web/src/endpoints/` | `docs/gradient-api.yaml`. The handler is extracting parameters, checking authorization, querying and responding |
| New table | A migration in `backend/gradient-migration/src/`, an entity in `backend/gradient-entity/src/`, see [Migrations](migrations.md) |
| Configuration option | `nix/modules/` and the [Configuration](../reference/configuration.md) reference |

### `#[allow]` Policy

- `#[allow(unused_imports)]`, `#[allow(unused)]` and `#[allow(dead_code)]` are forbidden (CI grep gate). Contributors fix the warning instead.
- Every other `#[allow(...)]` is carrying `reason = "..."` (`clippy::allow_attributes_without_reason`). `clippy::too_many_arguments` allows are temporary, tracked in #503.
- `allow-unwrap-in-tests` is only reaching code inside a `#[test]` function.
- Integration tests and `gradient-test-support` use a crate-level `#![expect(clippy::unwrap_used, reason = "...")]`. `expect` is warning once the last `unwrap()` is gone.

## Angular and TypeScript

- Standalone components with signals (`signal()`, `computed()`), feature folders under `frontend/src/app/features/`.
- UI components from `gr-ui` (`@gradient/ui/ui`, on `@angular/cdk`), charts through `<gr-metric-chart>` (Apache ECharts), colours and spacing from `@gradient/ui/styles/variables`. See the [Frontend Style Guide](frontend-style-guide.md).
- No UI or chart dependency with a field-of-use restriction. The bundle is distributed under AGPL-3.0. A dependency licensed beyond MIT, BSD or Apache-2.0 cannot be passed on.
- A refreshed `pnpm-lock.yaml` is changing the `pnpmDeps` hash in `nix/packages/gradient-frontend.nix`. The new hash is obtainable by setting `lib.fakeHash`, running `nix build .#gradient-frontend.pnpmDeps` and taking the reported hash.
- `minimumReleaseAge` in `pnpm-workspace.yaml` is refusing packages younger than 24 hours. A local `pnpm update` is then resolving the same versions the Nix build is accepting.

## Nix

- Packages and modules live in `nix/`: server options in `nix/modules/gradient.nix`, worker options in `nix/modules/gradient-worker.nix`, declarative state in `nix/modules/gradient-state.nix`.
- New modules need a NixOS VM test under `nix/tests/gradient/`.
