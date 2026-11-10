#!/usr/bin/env bash
# Construit le binaire de release et l'archive publiable, sans chemin de la machine de
# construction dans le binaire, puis vérifie qu'aucun chemin personnel n'y figure.
#   scripts/ci/build-release.sh [dossier-de-sortie]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
root="$PWD"
out="${1:-dist}"

# Même remappage que le PKGBUILD : du plus général au plus précis (la dernière règle gagne).
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/home/build --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$root=/build/dotmon"
cargo build --release --locked -p dotmon

leaks="$(grep -aoE '/home/[A-Za-z0-9_.-]+' target/release/dotmon | sort -u | grep -vx '/home/build' || true)"
if [[ -n "$leaks" ]]; then
    echo "chemin personnel trouvé dans le binaire :" >&2; echo "$leaks" >&2; exit 1
fi
packaging/release/make-binary-tarball.sh "$out"
packaging/release/make-source-tarball.sh "$out"
