#!/usr/bin/env bash
# Construit l'archive binaire de release : dotmon-<version>-x86_64-linux.tar.gz
# contenant l'arborescence /usr prête à installer (utilisée par le paquet AUR dotmon-bin).
#
# Prérequis : `cargo build --release --locked -p dotmon` déjà exécuté.
# Usage : packaging/release/make-binary-tarball.sh [dossier-de-sortie]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
out="$(realpath -m "${1:-dist}")"
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
name="dotmon-$version-x86_64-linux"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

[[ -x target/release/dotmon ]] || { echo "target/release/dotmon introuvable : lancez cargo build --release --locked." >&2; exit 1; }

dest="$stage/$name"
install -Dm755 target/release/dotmon -t "$dest/usr/bin/"
install -Dm644 packaging/dotmon.desktop -t "$dest/usr/share/applications/"
install -Dm644 packaging/icons/dotmon.svg "$dest/usr/share/icons/hicolor/scalable/apps/dotmon.svg"
for size in 16 24 32 48 64 128 256 512; do
    install -Dm644 "packaging/icons/png/dotmon-$size.png" \
        "$dest/usr/share/icons/hicolor/${size}x${size}/apps/dotmon.png"
done
install -Dm644 crates/dotmon-app/ui/fonts/OFL-Doto.txt "$dest/usr/share/licenses/dotmon/OFL-Doto.txt"
install -Dm644 crates/dotmon-app/ui/fonts/OFL-SpaceMono.txt "$dest/usr/share/licenses/dotmon/OFL-SpaceMono.txt"
install -Dm644 README.md -t "$dest/usr/share/doc/dotmon/"
install -Dm644 docs/*.md -t "$dest/usr/share/doc/dotmon/"

mkdir -p "$out"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='2026-01-01 00:00Z' \
    -C "$stage" -cf - "$name" | gzip -n > "$out/$name.tar.gz"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
cat "$out/$name.tar.gz.sha256"
