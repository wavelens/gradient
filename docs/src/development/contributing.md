# Contributing

Contributions are welcome. Please read this guide before opening a pull request.

## Code of Conduct

All participants are expected to follow the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Licensing

Gradient is licensed under **AGPL-3.0-only**. By submitting a contribution you agree that your work will be released under the same license. All files must carry an SPDX header:

```rust
// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
//
// SPDX-License-Identifier: AGPL-3.0-only
```

## Development Setup

**Prerequisites:** Nix with flakes enabled.

```sh
# Backend
nix run .#backend
> run_tests()

cd backend
cargo run

# Tip: parallel `rustc` jobs are capped at 2 in `backend/.cargo/config.toml`
# (`[build] jobs = 2`) to keep peak memory bounded on dev machines. Override
# with `cargo build -j N` if you have more headroom.

# Frontend
nix run .#frontend
> run_tests()

cd frontend
pnpm install
pnpm run serve
```

The frontend VM provisions a superuser `admin` (password `admin_password`),
a project, a task, and an in-VM worker via declarative state, so
evaluations and builds run end-to-end against `pnpm run serve`.

## Integration Tests

NixOS VM tests:

```sh
nix build .#checks.x86_64-linux.gradient-api       -L
nix build .#checks.x86_64-linux.gradient-state     -L
nix build .#checks.x86_64-linux.gradient-cache     -L
nix build .#checks.x86_64-linux.gradient-building  -L
nix build .#checks.x86_64-linux.gradient-oidc      -L
nix build .#checks.x86_64-linux.gradient-remote    -L
```

## Workflow

1. Open an issue to discuss the change before significant effort.
2. Fork and create a feature branch from `main`.
3. Implement with tests where applicable.
4. Open a pull request against `main`.

## Code Conventions

### Rust

- Format with `cargo fmt` before committing.
- No `unwrap()` in production paths - enforced by `clippy::unwrap_used = "deny"`. Use `?`, an
  explicit error branch, or `.expect("<the invariant>")` where the call is infallible by
  construction and the message says why.
- Shared state uses `gradient_util::sync::Mutex`, not `std::sync::Mutex`: it ignores poisoning,
  so a panic in one critical section does not become a panic at every later `lock()`.
- New API endpoints go in `web/src/endpoints/` following the pattern: extract path/query params → check authorization → query DB → return response.
- New database tables require a migration in `migration/src/` and an entity module in `entity/src/`.
- Log with `tracing::{info, debug, warn, error}`, not `println!`. Add `#[instrument]` to significant async functions.
- Update `docs/gradient-api.yaml` whenever an API endpoint is added or changed.
- Update environment variable documentation and the corresponding `nix/modules/` files when configuration options change.

#### Toolchain, formatting and lints

The toolchain is pinned in `rust-toolchain.toml` (rustup) and mirrored by the nix devShell
(`flake.lock`), which stays the source of truth. Formatting is pinned via `backend/rustfmt.toml`
(`style_edition = "2024"`), so `cargo fmt` is reproducible across rustfmt versions.

Run before pushing (matches CI):

```sh
cd backend
cargo fmt --all --check
cargo deny check                            # license/advisory policy - GPL-family deps are banned
nix build .#checks.x86_64-linux.clippy -L   # cargo clippy --workspace --all-targets -- -D warnings
```

`#[allow]` policy:

- `#[allow(unused_imports)]`, `#[allow(unused)]` and `#[allow(dead_code)]` are **forbidden**
  (CI grep-gate) - fix the underlying warning instead of silencing it.
- Every other `#[allow(...)]` must carry a `reason = "..."` (`clippy::allow_attributes_without_reason`).
  `clippy::too_many_arguments` allows are temporary and tracked in #503.
- `clippy.toml` sets `allow-unwrap-in-tests`, which only reaches code lexically inside a `#[test]`
  function. Integration tests and `gradient-test-support` keep their fixture helpers outside one, so
  those files carry a crate-level `#![expect(clippy::unwrap_used, reason = "...")]` - `expect` rather
  than `allow` so the attribute itself warns once the last `unwrap()` in the file is gone.

CI (`.github/workflows/rust.yml`) runs fmt, the grep-gate and cargo-deny; clippy runs as the
`checks.clippy` flake check.

### Angular / TypeScript

- Standalone components with Angular signals (`signal()`, `computed()`).
- Feature-based structure under `frontend/src/app/features/`.
- `gr-ui` (`src/app/shared/ui/`, built on `@angular/cdk`) for UI components; Apache ECharts (via `<app-metric-chart>`) for every chart; SCSS variables from `src/app/styles/_variables.scss` for colours and spacing.
- No third-party UI or charting dependency may impose a field-of-use restriction: the bundle ships under AGPL-3.0, so anything beyond MIT / BSD / Apache-2.0 cannot be conveyed downstream.
- Refreshing `pnpm-lock.yaml` changes the `pnpmDeps` hash in `nix/packages/gradient-frontend.nix`. Set it to `lib.fakeHash`, run `nix build .#gradient-frontend.pnpmDeps`, and take the hash the mismatch reports.
- `minimumReleaseAge` in `pnpm-workspace.yaml` refuses a package published in the last 24 hours, so a compromised publish is not consumed the day it lands. The Nix build enforces it either way; the setting is in the repo so a local `pnpm update` resolves to the same versions instead of producing a lockfile CI rejects.

### Nix

- All packages and modules live in `nix/`.
- Server options go in `nix/modules/gradient.nix`; worker options go in `nix/modules/gradient-worker.nix`.
- New modules need a NixOS VM test in `nix/tests/`.
