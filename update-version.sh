#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

set -euo pipefail

usage() {
    echo "Usage: $0 <new-version>"
    echo "  Example: $0 1.2.3"
    exit 1
}

[[ $# -ne 1 ]] && usage

VERSION="$1"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: version must be in X.Y.Z format (got '$VERSION')"
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

# ── Cargo crates ──────────────────────────────────────────────────────────────
# All gradient crates share the same version. The legacy `backend/builder` and
# `backend/evaluator` crates are intentionally excluded — they live on disk
# but are not in the workspace anymore.

CARGO_FILES=(
    backend/Cargo.toml
    backend/cache/Cargo.toml
    backend/core/Cargo.toml
    backend/entity/Cargo.toml
    backend/migration/Cargo.toml
    backend/proto/Cargo.toml
    backend/scheduler/Cargo.toml
    backend/test-support/Cargo.toml
    backend/web/Cargo.toml
    backend/worker/Cargo.toml
    cli/Cargo.toml
    cli/connector/Cargo.toml
)

for f in "${CARGO_FILES[@]}"; do
    path="$REPO_ROOT/$f"
    sed -i "0,/^version = \"[^\"]*\"/{s/^version = \"[^\"]*\"/version = \"$VERSION\"/}" "$path"
    echo "updated $f"
done

# ── frontend/package.json ─────────────────────────────────────────────────────

PACKAGE_JSON="$REPO_ROOT/frontend/package.json"
sed -i "0,/\"version\": \"[^\"]*\"/{s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/}" "$PACKAGE_JSON"
echo "updated frontend/package.json"

# ── Nix packages ─────────────────────────────────────────────────────────────

NIX_FILES=(
    nix/packages/gradient.nix
    nix/packages/gradient-frontend.nix
    nix/packages/gradient-cli.nix
)

for f in "${NIX_FILES[@]}"; do
    path="$REPO_ROOT/$f"
    sed -i "s/version = \"[^\"]*\";/version = \"$VERSION\";/" "$path"
    echo "updated $f"
done

echo ""
echo "Version updated to $VERSION"
