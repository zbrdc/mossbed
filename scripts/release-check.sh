#!/usr/bin/env bash
# Everything a release must pass.
#
# Unlike CI on an ordinary push, this insists that the V8-backed suites actually
# run: without the tardigrade binary they skip, and skipped is not passed.
set -euo pipefail

cd "$(dirname "$0")/.."

: "${TARDIGRADE_BIN:=../tardigrade/target/release/tardigrade}"
export TARDIGRADE_BIN
if [ ! -x "$TARDIGRADE_BIN" ]; then
  echo "release-check: no tardigrade binary at $TARDIGRADE_BIN" >&2
  echo "  build the sibling checkout, or set TARDIGRADE_BIN to one, then retry." >&2
  exit 1
fi
export TARDIGRADE_REQUIRE_TESTS=1

echo "== fmt =="
cargo fmt --check
echo "== clippy =="
cargo clippy --all-targets --examples -- -D warnings
echo "== tests, skipping disabled =="
cargo test
echo "== tests on the declared MSRV =="
if rustup toolchain list | grep -q '^1\.85\.0'; then
  cargo +1.85.0 test
else
  echo "  1.85.0 is not installed here; CI covers the MSRV. Install it with:"
  echo "    rustup toolchain install 1.85.0 --profile minimal"
fi
echo "== package =="
cargo package --allow-dirty --no-verify
echo
echo "release-check: passed."
echo "Manual steps that remain:"
echo "  - bump version in Cargo.toml and add the CHANGELOG entry"
echo "  - git tag -a vX.Y.Z, push it, then: gh release create vX.Y.Z --notes-from-tag"

