#!/usr/bin/env bash
# Charges contrôlées pour vérifier Dotmon contre une valeur ATTENDUE, connue
# à l'avance. Sans root ; ne modifie aucune configuration système.
# Tout s'arrête seul après DURÉE secondes (30 par défaut) ou avec Ctrl+C.
#
#   scripts/charge.sh cpu N [DURÉE]              N cœurs à 100 % (yes > /dev/null)
#   scripts/charge.sh ram GIO [DURÉE]            alloue et remplit GIO Gio
#   scripts/charge.sh ecriture MIO_S [DURÉE]     écriture disque directe à MIO_S Mio/s
#   scripts/charge.sh lecture MIO_S [DURÉE]      lecture disque directe à MIO_S Mio/s
#   scripts/charge.sh reseau URL MIO_S [DURÉE]   téléchargement limité à MIO_S Mio/s
#   scripts/charge.sh gpu [DURÉE]                RÉVEILLE la RTX (prime-run glxgears)
#   scripts/charge.sh nettoyer                   supprime le fichier de test disque
set -euo pipefail

dir="${XDG_CACHE_HOME:-$HOME/.cache}/dotmon-validation"
file="$dir/test.bin"
ncpu=$(nproc)

usage() { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
need() { command -v "$1" >/dev/null || { echo "Outil manquant : $1" >&2; exit 1; }; }
expect() { printf '\n  ATTENDU DANS DOTMON : %s\n\n' "$*"; }

[ $# -ge 1 ] || usage
cmd=$1; shift
case "$cmd" in
cpu)
    n=${1:?nombre de cœurs}; t=${2:-30}
    expect "CPU global ≈ valeur au repos + $(( n * 100 / ncpu )) % ($n/$ncpu) ;" \
        "$n processus « yes » à ≈ $(awk "BEGIN{printf \"%.1f\", 100/$ncpu}") % chacun ;" \
        "$n barres de cœurs pleines (pas forcément les mêmes d'une seconde à l'autre)"
    pids=()
    trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT INT TERM
    for _ in $(seq "$n"); do timeout "$t" yes >/dev/null & pids+=($!); done
    echo "Charge CPU pendant $t s…"; wait
    ;;
ram)
    g=${1:?Gio}; t=${2:-30}; need python3
    expect "RAM UTILISÉE ≈ valeur au repos + $g.0 Go (Dotmon : Go = Gio) ;" \
        "processus « python3 » à ≈ $g.0 Go ; « disponible » baisse d'autant"
    echo "Allocation de $g Gio pendant $t s…"
    python3 -c "
import time, sys
b = b'\x01' * ($g << 30)   # rempli : les pages sont réellement occupées
sys.stdout.write('alloué\n'); sys.stdout.flush()
time.sleep($t)"
    ;;
ecriture)
    r=${1:?Mio/s}; t=${2:-30}; need pv; mkdir -p "$dir"
    dev=$(findmnt -n -o SOURCE -T "$dir" | sed 's/\[.*//')
    expect "ÉCRITURE ≈ $r.0 Mo/s (ligne de $(basename "$dev") et total disques) ;" \
        "LECTURE ≈ 0 ; le fichier $file grossit de $(( r * t )) Mio"
    echo "Écriture directe (O_DIRECT, sans cache) à $r Mio/s pendant $t s…"
    timeout "$t" sh -c "pv -q -L ${r}m /dev/urandom 2>/dev/null \
        | dd of='$file' bs=1M iflag=fullblock oflag=direct status=none" || true
    echo "Fichier : $(du -h "$file" | cut -f1). « $0 nettoyer » pour le supprimer."
    ;;
lecture)
    r=${1:?Mio/s}; t=${2:-30}; need pv
    [ -s "$file" ] || { echo "Créez d'abord le fichier : $0 ecriture 50 20" >&2; exit 1; }
    expect "LECTURE ≈ $r.0 Mo/s tant que le fichier n'est pas fini" \
        "($(du -h "$file" | cut -f1) → $(( $(stat -c %s "$file") / 1048576 / r )) s max) ; ÉCRITURE ≈ 0"
    echo "Lecture directe (O_DIRECT, sans cache) à $r Mio/s pendant $t s…"
    timeout "$t" sh -c "dd if='$file' bs=1M iflag=direct status=none \
        | pv -q -L ${r}m 2>/dev/null >/dev/null" || true
    ;;
reseau)
    url=${1:?URL}; r=${2:?Mio/s}; t=${3:-30}; need curl
    expect "RÉCEPTION ≈ $r.0 Mo/s au plus (limite) ; si la connexion est plus lente," \
        "comparez au débit moyen affiché par curl à la fin. Interface : celle de la route par défaut"
    echo "Téléchargement limité à $r Mio/s pendant $t s (rien n'est écrit sur le disque)…"
    # --max-time interrompt volontairement le transfert (code 28) : pas une erreur.
    res=$(curl -sS -o /dev/null --max-time "$t" --limit-rate "${r}M" \
        -w '%{http_code} %{speed_download}' "$url" 2>/dev/null || true)
    read -r code speed <<<"$res"
    case "$code" in
    2*) echo "Débit moyen mesuré par curl : $(awk "BEGIN{printf \"%.2f\", $speed/1048576}") Mio/s" ;;
    *) echo "ÉCHEC : le serveur a répondu HTTP ${code:-?}. Essayez une autre URL de gros fichier." >&2; exit 1 ;;
    esac
    ;;
gpu)
    t=${1:-30}; need prime-run; need glxgears
    echo "ATTENTION : ce test RÉVEILLE volontairement la RTX (consommation accrue)."
    expect "GPU NVIDIA : pourcentage et VRAM affichés tant que glxgears tourne ;" \
        "après l'arrêt : « REPOS » (jusqu'à 30 s), puis « VEILLE » une fois la carte éteinte"
    timeout "$t" prime-run glxgears >/dev/null 2>&1 || true
    echo "glxgears arrêté. Surveillez le retour en veille : scripts/gpu-veille.sh"
    ;;
nettoyer)
    rm -fv "$file"; rmdir "$dir" 2>/dev/null || true
    ;;
*) usage ;;
esac
