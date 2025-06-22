#!/usr/bin/env bash
# Test script for Gradient backend workspace
# This script runs tests for all packages in the workspace

set -e

echo "🧪 Running Gradient Backend Tests..."
echo "=================================="

# Test each package individually to avoid conflicts
echo "Testing core package..."
cargo test -p core --lib

echo ""
echo "Testing entity package..."
cargo test -p entity --lib

echo ""
echo "Testing migration package..."
cargo test -p migration --lib

echo ""
echo "Testing web package..."
cargo test -p web --lib

echo ""
echo "Testing builder package (unit tests only)..."
cargo test -p builder --lib

echo ""
echo "Testing cache package (unit tests only)..."
cargo test -p cache --lib

echo ""
echo "✅ All tests completed!"
echo ""
echo "To run specific package tests:"
echo "  cargo test -p core"
echo "  cargo test -p web"
echo "  cargo test -p entity"
echo "  cargo test -p migration"
echo ""
echo "To run all workspace tests (may have conflicts):"
echo "  cargo test --workspace --lib"
