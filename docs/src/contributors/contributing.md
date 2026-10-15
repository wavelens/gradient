# Contributing

Set up a development environment, run the checks and submit changes. All contributors follow the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Workflow

1. Discuss large changes in an issue before writing code.
2. Create a fork, then a branch based on `main`.
3. Write the change together with tests. See [Tests](tests.md).
4. Send a pull request that targets `main`.

## Licensing

Gradient uses the **AGPL-3.0-only** license. Contributions fall under the same license. Every file must include this SPDX header:

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

    1.  Start an interactive NixOS test driver. `run_tests()` boots a VM with a packaged server, PostgreSQL 18 forwarded to a host port, a user `test` with password `password`, and a project. Run `cargo run` in `backend/` to work against that database.

=== "Frontend"

    ```sh
    nix run .#frontend # (1)!
    cd frontend && pnpm install && pnpm start
    ```

    1.  Start a VM built from declarative state. It contains a superuser `admin` with password `admin_password`, a team, a project, a task, and a worker. With `pnpm start` running, evaluations and build jobs can run end to end.

`backend/.cargo/config.toml` caps parallel `rustc` jobs at 1 with `[build] jobs = 1` to bound peak memory. Override the cap with `cargo build -j N`.

## Checks

| Check | Command |
|---|---|
| Backend tests | `nix build .#checks.x86_64-linux.unittest -L` |
| CLI tests | `nix build .#checks.x86_64-linux.cli-unittest -L` |
| Clippy | `nix build .#checks.x86_64-linux.clippy -L`, `...cli-clippy -L`, `...cli-static-clippy -L` (the CLI without Nix support) |
| VM tests | `nix build .#checks.x86_64-linux.gradient-<name> -L` for `api`, `cluster`, `deploy`, `e2e`, `eval`, `local-worker`, `scheduler`, `standalone`, `standalone-docker` |
| Format | `cargo fmt --all --check`, in `backend/` and `cli/` |
| Licenses and advisories | `cargo deny check`, in `backend/` and `cli/`. GPL-family dependencies are banned |

CI (`.github/workflows/rust.yml`) runs fmt, the `#[allow]` grep check and cargo-deny across both workspaces. The flake checks include Clippy.

## Rust

- Run `cargo fmt` before each commit. The devShell in `flake.lock` pins the toolchain version, and `rust-toolchain.toml` mirrors that pin for rustup users.
- `rustfmt.toml` sets `style_edition = "2024"`.
- Every workspace has its own `deny.toml`, `clippy.toml` and `rustfmt.toml`. Keep these files in sync across workspaces.
- The backend `clippy.toml` bans `tokio::spawn` and raw `Statement`.
- Production code must not call `unwrap()` (`clippy::unwrap_used = "deny"`). Use `?`, an explicit error branch, or `.expect("<the invariant>")` where the call cannot fail by construction.
- For shared state, use `gradient_util::sync::Mutex` rather than `std::sync::Mutex`. Poisoning gets ignored, so a panic in one critical section cannot break every later `lock()` call.
- Log through `tracing` (`info`, `debug`, `warn`, `error`). Never use `println!` macros for logging.
- Add `#[instrument]` to significant async functions.

| Change | Also update |
|---|---|
| New endpoint in `backend/gradient-web/src/endpoints/` | `#[utoipa::path]` on the handler and its route in `backend/gradient-web/src/routes.rs`. Handlers extract parameters, check authorization, query and respond |
| New table | A migration in `backend/gradient-migration/src/`, an entity in `backend/gradient-entity/src/`, see [Migrations](migrations.md) |
| Configuration option | A field in `backend/gradient-types/src/cli/`, or in `backend/gradient-worker/src/config/options.rs` for the worker, then the generated files below |

The handler annotations generate `docs/gradient-api.yaml` and the frontend types. Regenerate both files with these commands.

```sh
cargo run --manifest-path backend/Cargo.toml --features api-spec --bin gradient-api-spec > docs/gradient-api.yaml
cargo run --manifest-path backend/Cargo.toml --features api-spec --bin gradient-api-spec -- typescript > frontend/src/app/core/models/api.generated.ts
```

Flag, environment variable, NixOS option and the row in the [Configuration](../reference/configuration.md) reference derive from the field and its doc comment. Three commands refresh the generated files.

```sh
cargo run --manifest-path backend/Cargo.toml -p gradient-types --example options > backend/gradient-types/options.json
cargo run --manifest-path backend/Cargo.toml -p gradient-worker --example options > backend/gradient-worker/options.json
nix eval --json --file docs/tools/options.nix | python3 docs/tools/options.py > docs/generated/configuration-options.md
```

### `#[allow]` Policy

- `#[allow(unused_imports)]`, `#[allow(unused)]` and `#[allow(dead_code)]` are forbidden (CI grep check). Contributors fix the warning instead.
- Every other `#[allow(...)]` must carry `reason = "..."` (`clippy::allow_attributes_without_reason`).
- Uses of `#[allow(clippy::too_many_arguments)]` are temporary, tracked in #503.
- `allow-unwrap-in-tests` can only reach code inside a `#[test]` function.
- Integration tests and `gradient-test-support` use a crate-level `#![expect(clippy::unwrap_used, reason = "...")]` attribute.
- The `expect` attribute will warn once the last `unwrap()` call is gone.

## Angular and TypeScript

- Build standalone components with signals (`signal()`, `computed()`). Keep feature code in feature folders under `frontend/src/app/features/`.
- Take UI components from `gr-ui` (`@gradient/ui/ui`, built on `@angular/cdk`). Render charts through `<gr-metric-chart>` (Apache ECharts). Get colours and spacing from the `@gradient/ui/styles/variables` module. See the [Frontend Style Guide](frontend-style-guide.md).
- Avoid UI or chart dependencies with a field-of-use restriction. The bundle is distributed under AGPL-3.0, and we cannot pass on a dependency licensed beyond MIT, BSD or Apache-2.0.
- A refreshed `pnpm-lock.yaml` will change the `pnpmDeps` hash in `nix/packages/gradient-frontend.nix`. To get the new hash, set `lib.fakeHash`, run `nix build .#gradient-frontend.pnpmDeps` and take the reported hash.
- `minimumReleaseAge` in `pnpm-workspace.yaml` will refuse packages younger than 24 hours. Run a local `pnpm update` to pin the same versions the Nix build would take.

## Nix

- Packages and modules live in `nix/`: server options in `nix/modules/gradient.nix`, worker options in `nix/modules/gradient-worker.nix`, declarative state in `nix/modules/gradient-state.nix`.
- New modules need a NixOS VM test under `nix/tests/gradient/`.
