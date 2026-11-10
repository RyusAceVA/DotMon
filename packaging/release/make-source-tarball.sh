#!/usr/bin/env bash
# Archive source de release : dotmon-<version>.tar.gz (dossier racine dotmon-<version>/),
# produite par `git archive` depuis l'étiquette vX.Y.Z (ou HEAD en l'absence d'étiquette),
# donc strictement identique au code de l'étiquette. Sert au paquet AUR « dotmon ».
#   packaging/release/make-source-tarball.sh [dossier-de-sortie]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
out="$(realpath -m "${1:-dist}")"
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
ref="HEAD"
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && ref="v$version"
name="dotmon-$version"
mkdir -p "$out"
git archive --format=tar --prefix="$name/" "$ref" | gzip -n > "$out/$name.tar.gz"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz  (depuis $ref, commit $(git rev-parse --short "$ref^{commit}"))"
cat "$out/$name.tar.gz.sha256"
