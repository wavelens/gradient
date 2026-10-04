#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

set -euo pipefail

usage() {
    echo "Usage: $0 <new-version>"
    echo "  Example: $0 1.2.3 or $0 2.0.0-rc.1"
    exit 1
}

[[ $# -ne 1 ]] && usage

VERSION="$1"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$ ]]; then
    echo "Error: version must be X.Y.Z or X.Y.Z-<pre-release>, e.g. 2.0.0-rc.1 (got '$VERSION')"
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

# ── TOML manifests ────────────────────────────────────────────────────────────
# Only files that define a literal `version = "X.Y.Z"` are listed. Backend
# sub-crates inherit `version.workspace = true` from backend/Cargo.toml, except
# gradient-eval, which is self-contained so the separate cli workspace can path-
# depend on it without the backend workspace root.

TOML_FILES=(
    backend/Cargo.toml
    backend/gradient-eval/Cargo.toml
    cli/Cargo.toml
    nix/tools/report-inspector/pyproject.toml
)

for f in "${TOML_FILES[@]}"; do
    path="$REPO_ROOT/$f"
    sed -i -E "0,/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/{s/^(version[[:space:]]*=[[:space:]]*)\"[^\"]*\"/\\1\"$VERSION\"/}" "$path"
    echo "updated $f"
done

# ── Cargo dependencies ───────────────────────────────────────────────────────

CARGO_WORKSPACES=(
    backend
    cli
)

for ws in "${CARGO_WORKSPACES[@]}"; do
    cargo update --manifest-path "$REPO_ROOT/$ws/Cargo.toml"
    echo "updated $ws/Cargo.lock"
done

# ── Flake inputs ─────────────────────────────────────────────────────────────

nix flake update --flake "$REPO_ROOT"
echo "updated flake.lock"

# ── frontend/package.json ─────────────────────────────────────────────────────

PACKAGE_JSON="$REPO_ROOT/frontend/package.json"
sed -i "0,/\"version\": \"[^\"]*\"/{s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/}" "$PACKAGE_JSON"
echo "updated frontend/package.json"

pnpm --dir "$REPO_ROOT/frontend" update --recursive
echo "updated frontend/pnpm-lock.yaml"

# ── Nix packages ─────────────────────────────────────────────────────────────

NIX_FILES=(
    nix/packages/gradient.nix
    nix/packages/gradient-frontend.nix
    nix/packages/gradient-cli.nix
    nix/tools/report-inspector/default.nix
)

for f in "${NIX_FILES[@]}"; do
    path="$REPO_ROOT/$f"
    sed -i "s/^  version = \"[^\"]*\";/  version = \"$VERSION\";/" "$path"
    echo "updated $f"
done

FRONTEND_NIX="$REPO_ROOT/nix/packages/gradient-frontend.nix"
sed -i 's/^    hash = "[^"]*";/    hash = "";/' "$FRONTEND_NIX"
FRONTEND_BUILD="$(nix build --no-link "$REPO_ROOT#gradient-frontend.pnpmDeps" 2>&1 || true)"
FRONTEND_HASH="$(grep -oP 'got:\s+\Ksha256-[A-Za-z0-9+/=]+' <<< "$FRONTEND_BUILD" || true)"
if [[ -z "$FRONTEND_HASH" ]]; then
    echo "$FRONTEND_BUILD" | tail -20
    echo "Error: the pnpm dependency build reported no hash"
    exit 1
fi
sed -i "s|^    hash = \"\";|    hash = \"$FRONTEND_HASH\";|" "$FRONTEND_NIX"
echo "updated the pnpm dependency hash in nix/packages/gradient-frontend.nix"

# ── OpenAPI spec ─────────────────────────────────────────────────────────────

OPENAPI_SPEC="$REPO_ROOT/docs/gradient-api.yaml"
OPENAPI_VERSION='^  version: [0-9]\+\.[0-9]\+\.[0-9]\+\(-[0-9A-Za-z.]\+\)\?$'
sed -i "0,/$OPENAPI_VERSION/{s/$OPENAPI_VERSION/  version: $VERSION/}" "$OPENAPI_SPEC"
echo "updated docs/gradient-api.yaml"

echo ""
echo "Version updated to $VERSION"
