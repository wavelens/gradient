# Contributing

Contributions are welcome. Please read this guide before opening a pull request.

## Code of Conduct

All participants are expected to follow the [Code of Conduct](https://github.com/wavelens/gradient/blob/main/CODE_OF_CONDUCT.md).

## Licensing

Gradient is licensed under **AGPL-3.0**. By submitting a contribution you agree that your work will be released under the same license. All files must carry an SPDX header:

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

# Frontend
nix run .#frontend
> run_tests()

cd frontend
pnpm install
pnpm run serve
```

## Integration Tests

NixOS VM tests:

```sh
nix build .#checks.x86_64-linux.gradient-api     -L
nix build .#checks.x86_64-linux.gradient-state   -L
nix build .#checks.x86_64-linux.gradient-cache   -L
nix build .#checks.x86_64-linux.gradient-oidc    -L
nix build .#checks.x86_64-linux.gradient-remote  -L
```

## Workflow

1. Open an issue to discuss the change before significant effort.
2. Fork and create a feature branch from `main`.
3. Implement with tests where applicable.
4. Open a pull request against `main`.

## Code Conventions

### Rust

- Format with `cargo fmt` before committing.
- No `unwrap()` in production paths — use `?` or explicit error handling.
- New API endpoints go in `web/src/endpoints/` following the pattern: extract path/query params → check authorization → query DB → return response.
- New database tables require a migration in `migration/src/` and an entity module in `entity/src/`.
- Log with `tracing::{info, debug, warn, error}`, not `println!`. Add `#[instrument]` to significant async functions.

### Angular / TypeScript

- Standalone components with Angular signals (`signal()`, `computed()`).
- Feature-based structure under `frontend/src/app/features/`.
- PrimeNG for UI components; SCSS variables from `src/app/styles/_variables.scss` for colours and spacing.

### Nix

- All packages and modules live in `nix/`.
- New NixOS options go in `nix/modules/gradient.nix`.
- New modules need a NixOS VM test in `nix/tests/`.
