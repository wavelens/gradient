# Testing Guide for Gradient Backend

This guide explains how to run tests for the Gradient backend workspace.

## Quick Start

### Run All Tests
```bash
# Using the test script (recommended)
./test.sh

# Or using cargo alias
cargo test-script
```

### Run Individual Package Tests
```bash
# Core functionality tests
cargo test -p core

# Entity/database tests  
cargo test -p entity

# Web API tests
cargo test -p web

# Migration tests
cargo test -p migration

# Builder tests
cargo test -p builder

# Cache tests
cargo test -p cache
```

## Available Test Aliases

The `.cargo/config.toml` file defines several useful aliases:

- `cargo test-all` - Test all packages with all features
- `cargo test-unit` - Run only unit tests across workspace
- `cargo test-doc` - Run documentation tests
- `cargo test-verbose` - Run tests with verbose output
- `cargo check-all` - Check all packages without running tests
- `cargo clippy-all` - Run clippy linter on all packages

## Individual Package Tests

### Core Package
Tests core functionality including:
- Git operations (ls-remote, commit info fetching)
- SSH key management
- Utility functions
- Source management

```bash
cargo test -p core
```

### Entity Package  
Tests database entity models and relationships:
```bash
cargo test -p entity
```

### Web Package
Tests HTTP API endpoints and web functionality:
```bash
cargo test -p web
```

### Builder Package
Tests build scheduling and execution:
```bash
cargo test -p builder
```

### Cache Package
Tests Nix cache functionality:
```bash
cargo test -p cache
```

### Migration Package
Tests database migrations:
```bash
cargo test -p migration
```

## Test Organization

- **Unit tests**: Located in `src/` files as `#[cfg(test)]` modules
- **Integration tests**: Located in `tests/` directories
- **Test utilities**: Common test helpers in `src/tests.rs` files

## Mock Testing

The tests use SeaORM's MockDatabase for database operations and custom mock scripts for git operations to ensure tests are isolated and reproducible.

## Continuous Integration

For CI environments, use:
```bash
cargo test --workspace --lib --all-features
```

Note: Some tests may have conflicts when run in parallel due to shared resources. Use the individual package testing approach for reliable results.