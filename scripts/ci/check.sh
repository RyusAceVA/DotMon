#!/usr/bin/env bash
# Vérifications Rust de la CI (lançables à l'identique en local) :
# formatage, clippy sans avertissement, tests en release.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

scripts/ci/check-version.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo test --workspace --release --locked
