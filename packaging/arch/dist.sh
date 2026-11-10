#!/usr/bin/env bash
# Prépare l'archive locale des sources pour le PKGBUILD (empreinte SHA-256 affichée seulement :
# le PKGBUILD local utilise SKIP, pour ne pas être modifié à chaque construction).
# À lancer en utilisateur normal depuis packaging/arch/ (ou n'importe où).
# L'archive est déterministe : mêmes sources → même empreinte.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root"

version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
pkgver="$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")"
# pacman interdit « - » dans pkgver : 0.4.0_beta.1 ↔ 0.4.0-beta.1.
if [[ -z "$version" || "$version" != "${pkgver//_/-}" ]]; then
    echo "dist.sh : version Cargo ($version) ≠ pkgver du PKGBUILD ($pkgver)." >&2
    echo "Mettez les deux à la même valeur (et pkgrel=1 pour une nouvelle version)." >&2
    exit 1
fi

archive="$here/dotmon-$pkgver.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime='2026-01-01 00:00Z' \
    --transform "s,^,dotmon-$pkgver/," \
    -cf - Cargo.toml Cargo.lock rustfmt.toml README.md crates docs scripts packaging/dotmon.desktop packaging/icons \
    | gzip -n > "$archive"

sum="$(sha256sum "$archive" | cut -d' ' -f1)"
# Force makepkg à ré-extraire les sources (sinon il réutilise un src/ périmé).
rm -rf "$here/src" "$here/pkg"

echo "Archive : $archive"
echo "SHA-256 : $sum"
