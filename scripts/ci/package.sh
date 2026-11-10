#!/usr/bin/env bash
# Construction et contrôle du paquet Arch (CI ou conteneur Arch propre).
# À lancer en UTILISATEUR NORMAL (makepkg refuse root), dépendances de build installées :
#   base-devel rust namcap desktop-file-utils shellcheck
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

echo "== Scripts shell"
shellcheck -x packaging/arch/*.sh packaging/aur/finalize.sh packaging/release/make-binary-tarball.sh \
    scripts/ci/*.sh scripts/charge.sh scripts/compare.sh scripts/gpu-veille.sh

echo "== Fichier desktop"
desktop-file-validate packaging/dotmon.desktop

echo "== Paquet source local (packaging/arch)"
cd packaging/arch
./dist.sh
makepkg -f --noconfirm --nocheck   # les tests ont déjà tourné dans scripts/ci/check.sh
pkg=""
for f in dotmon-*.pkg.tar.zst; do pkg="$f"; break; done
[[ -n "$pkg" ]] || { echo "aucun paquet construit" >&2; exit 1; }

echo "== namcap (PKGBUILD)"
namcap PKGBUILD
echo "== namcap ($pkg)"
namcap "$pkg"

echo "== Contenu"
pacman -Qlp "$pkg"
echo "== Chemins personnels dans le paquet (doit être vide)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
bsdtar -xf "$pkg" -C "$tmp"
if grep -rIlE '/home/[a-z]' "$tmp" --exclude=.BUILDINFO --exclude=.PKGINFO; then
    echo "chemin absolu trouvé dans le paquet" >&2; exit 1
fi
# Binaire : seuls les vrais répertoires personnels comptent (« …/accessible/root/… » est une
# chaîne D-Bus AT-SPI sans rapport). Le remappage de rustc réécrit $HOME en /home/build.
leaks="$(grep -aoE '/home/[A-Za-z0-9_.-]+' "$tmp/usr/bin/dotmon" | sort -u | grep -vx '/home/build' || true)"
if [[ -n "$leaks" ]]; then
    echo "chemin personnel trouvé dans le binaire :" >&2; echo "$leaks" >&2; exit 1
fi
echo "OK : $pkg"
