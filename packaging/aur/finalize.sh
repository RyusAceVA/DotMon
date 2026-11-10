#!/usr/bin/env bash
# Prépare les dépôts AUR (dotmon et dotmon-bin) : calcule les SHA-256 des fichiers
# RÉELLEMENT téléchargés depuis la release GitHub, écrit PKGBUILD + dotmon.install + .SRCINFO
# dans packaging/aur/out/<paquet>/, prêts à être poussés vers l'AUR.
#
#   packaging/aur/finalize.sh
#       à lancer APRÈS la publication de la release GitHub vX.Y.Z (les archives doivent exister).
#
#   packaging/aur/finalize.sh --local <dossier-assets>
#       test hors ligne : les « téléchargements » sont lus dans <dossier-assets>
#       (résultat dans out-local/, JAMAIS à publier).
set -euo pipefail

owner="RyusAceVA"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' "$root/Cargo.toml")"

local_dir=""
out="$here/out"
if [[ "${1:-}" == "--local" ]]; then
    local_dir="$(realpath "${2:?dossier des assets}")"
    out="$here/out-local"
fi
base="https://github.com/$owner/dotmon"
src_name="dotmon-$version.tar.gz"
bin_name="dotmon-$version-x86_64-linux.tar.gz"   # (le PKGBUILD le désigne via $CARCH)

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

fetch() { # url nom-local → chemin du fichier
    local url="$1" name="$2" dest="$tmp/$2"
    if [[ -n "$local_dir" ]]; then
        cp "$local_dir/$name" "$dest"
    else
        curl --fail --location --silent --show-error --retry 3 -o "$dest" "$url"
    fi
    echo "$dest"
}

src_file="$(fetch "$base/releases/download/v$version/$src_name" "$src_name")"
bin_file="$(fetch "$base/releases/download/v$version/$bin_name" "$bin_name")"
sha_src="$(sha256sum "$src_file" | cut -d' ' -f1)"
sha_bin="$(sha256sum "$bin_file" | cut -d' ' -f1)"

# La CI publie un .sha256 à côté de l'archive binaire : il doit concorder avec ce qui a été téléchargé.
if [[ -z "$local_dir" ]]; then
    published="$(curl --fail --silent --location "$base/releases/download/v$version/$bin_name.sha256" | cut -d' ' -f1 || true)"
    [[ -n "$published" ]] || { echo "fichier .sha256 de la release introuvable" >&2; exit 1; }
    [[ "$published" == "$sha_bin" ]] || { echo "SHA-256 publié ≠ SHA-256 téléchargé pour $bin_name" >&2; exit 1; }
    published="$(curl --fail --silent --location "$base/releases/download/v$version/$src_name.sha256" | cut -d' ' -f1 || true)"
    [[ "$published" == "$sha_src" ]] || { echo "SHA-256 publié ≠ SHA-256 téléchargé pour $src_name" >&2; exit 1; }
fi

rm -rf "$out"
if [[ -n "$local_dir" ]]; then
    mkdir -p "$out/mirror/releases/download/v$version"
    cp "$src_file" "$out/mirror/releases/download/v$version/$src_name"
    cp "$bin_file" "$out/mirror/releases/download/v$version/$bin_name"
fi
for pkg in dotmon dotmon-bin; do
    mkdir -p "$out/$pkg"
    sed -e "s|@SHA256_SOURCE@|$sha_src|g" -e "s|@SHA256_BIN@|$sha_bin|g" \
        "$here/$pkg/PKGBUILD.in" > "$out/$pkg/PKGBUILD"
    cp "$root/packaging/arch/dotmon.install" "$out/$pkg/dotmon.install"
    if [[ -n "$local_dir" ]]; then
        # Test hors ligne : « url » pointe vers une arborescence locale imitant GitHub.
        sed -i "s|^url=.*|url='file://$out/mirror'|" "$out/$pkg/PKGBUILD"
    fi
    if grep -q '@[A-Z0-9_]*@' "$out/$pkg/PKGBUILD"; then
        echo "marqueur non remplacé dans $pkg/PKGBUILD" >&2; exit 1
    fi
    (cd "$out/$pkg" && makepkg --printsrcinfo > .SRCINFO)
done
echo "SHA-256 source  : $sha_src"
echo "SHA-256 binaire : $sha_bin"
echo "Prêt dans : $out/{dotmon,dotmon-bin}/  (PKGBUILD, dotmon.install, .SRCINFO)"
