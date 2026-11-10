#!/usr/bin/env bash
# Reconstruit Dotmon depuis les sources du dépôt et (ré)installe le paquet.
# À lancer en UTILISATEUR NORMAL : makepkg demandera lui-même le mot de passe
# à pacman pour l'installation (jamais de sudo devant ce script).
#
#   packaging/arch/update.sh            construit, teste, installe
#   packaging/arch/update.sh --nocheck  idem sans lancer les tests
set -euo pipefail

if [[ $EUID -eq 0 ]]; then
    echo "update.sh : ne pas lancer en root (makepkg le refuse)." >&2
    exit 1
fi
cd "$(dirname "${BASH_SOURCE[0]}")"
./dist.sh
exec makepkg -fsi "$@"
