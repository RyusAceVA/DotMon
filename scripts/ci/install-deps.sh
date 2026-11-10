#!/usr/bin/env bash
# Installe (EN ROOT, conteneur/chroot Arch jetable uniquement) les dépendances de
# construction et d'exécution déclarées dans le PKGBUILD : une seule source de vérité.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# shellcheck source=/dev/null
source packaging/arch/PKGBUILD
pacman -Syu --noconfirm --needed base-devel git namcap desktop-file-utils shellcheck
# depends, makedepends (et éventuellement checkdepends) sont définis par le PKGBUILD.
# shellcheck disable=SC2154
pacman -S --noconfirm --needed "${depends[@]}" "${makedepends[@]}" ${checkdepends[@]+"${checkdepends[@]}"}
