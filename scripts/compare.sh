#!/usr/bin/env bash
# Compare le backend Linux de Dotmon aux outils de CachyOS (top, free, ps,
# df, sensors, compteurs sysfs) sur les mêmes fenêtres d'une seconde, avec un
# verdict par mesure et une synthèse (CONFIRMÉ = écart répété). Lecture seule.
#
#   scripts/compare.sh [FENÊTRES] [--nvidia-smi]
#
# Ne réveille jamais la RTX : son état est lu dans sysfs ; nvidia-smi n'est
# lancé qu'avec --nvidia-smi ET si la carte est déjà allumée.
# Code de sortie : 0 = aucun écart confirmé, 2 = au moins un écart confirmé.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
cargo build --release -q -p dotmon-sys --example validate || exit 1
mkdir -p target/validation
out="target/validation/rapport-$(date +%Y%m%d-%H%M%S).txt"
target/release/examples/validate "$@" | tee "$out"
code=${PIPESTATUS[0]}
echo; echo "Rapport enregistré : $out"
exit "$code"
