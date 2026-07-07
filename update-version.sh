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
# Only files that define a literal `version = "X.Y.Z"` are listed. Backend
# sub-crates inherit `version.workspace = true` from backend/Cargo.toml, except
# gradient-eval, which is self-contained so the separate cli workspace can path-
# depend on it without the backend workspace root.

CARGO_FILES=(
    backend/Cargo.toml
    backend/gradient-eval/Cargo.toml
    cli/Cargo.toml
    cli/connector/Cargo.toml
)

for f in "${CARGO_FILES[@]}"; do
    path="$REPO_ROOT/$f"
    sed -i -E "0,/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/{s/^(version[[:space:]]*=[[:space:]]*)\"[^\"]*\"/\\1\"$VERSION\"/}" "$path"
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
    sed -i "s/^  version = \"[^\"]*\";/  version = \"$VERSION\";/" "$path"
    echo "updated $f"
done

# ── OpenAPI spec ─────────────────────────────────────────────────────────────

OPENAPI_SPEC="$REPO_ROOT/docs/gradient-api.yaml"
sed -i "0,/^  version: [0-9]\+\.[0-9]\+\.[0-9]\+$/{s/^  version: [0-9]\+\.[0-9]\+\.[0-9]\+$/  version: $VERSION/}" "$OPENAPI_SPEC"
echo "updated docs/gradient-api.yaml"

echo ""
echo "Version updated to $VERSION"
