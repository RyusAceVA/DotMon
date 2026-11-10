#!/usr/bin/env bash
# Suit l'état d'alimentation des GPU à gestion d'énergie dynamique sans
# jamais les réveiller : ne lit que /sys/.../power/ (pas de NVML, pas de
# nvidia-smi, pas de lspci -v). Une ligne toutes les 1 à 2 s, avec les
# processus qui ont un /dev/nvidia* ouvert : simple indice pour trouver qui
# réveille la carte (Hyprland en garde un ouvert sans l'empêcher de dormir).
#
#   scripts/gpu-veille.sh [DURÉE]     # 120 s par défaut, Ctrl+C pour arrêter
#
# Lecture : « active_ms » n'augmente que quand la carte est allumée. S'il ne
# bouge pas, la carte n'a pas été réveillée, même brièvement.
set -euo pipefail
t=${1:-120}

gpus=()
for d in /sys/bus/pci/devices/*; do
    [ "$(cat "$d/class" 2>/dev/null | cut -c1-4)" = 0x03 ] || continue
    [ "$(cat "$d/power/control" 2>/dev/null)" = auto ] && gpus+=("$d")
done
[ ${#gpus[@]} -gt 0 ] || { echo "Aucun GPU à veille dynamique (power/control = auto)."; exit 0; }

holders() {
    for p in /proc/[0-9]*; do
        # readlink lit seulement la cible du lien : le périphérique n'est jamais ouvert.
        for fd in "$p"/fd/*; do
            case "$(readlink "$fd" 2>/dev/null)" in
                /dev/nvidia*)
                    printf '%s:%s ' "${p#/proc/}" "$(cat "$p/comm" 2>/dev/null)"
                    break ;;
            esac
        done
    done
}

declare -A first last
for d in "${gpus[@]}"; do first[$d]=$(cat "$d/power/runtime_active_time"); done
trap 'echo; for d in "${gpus[@]}"; do
        echo "$(basename "$d") : actif $(( $(cat "$d/power/runtime_active_time") - ${first[$d]} )) ms pendant la surveillance"
      done' EXIT
echo "heure     GPU           état        active_ms   Δ    processus /dev/nvidia*"
for _ in $(seq "$t"); do
    for d in "${gpus[@]}"; do
        a=$(cat "$d/power/runtime_active_time")
        printf '%s  %s  %-10s %10s %5s  %s\n' "$(date +%T)" "$(basename "$d")" \
            "$(cat "$d/power/runtime_status")" "$a" "+$(( a - ${last[$d]:-$a} ))" "$(holders)"
        last[$d]=$a
    done
    sleep 1
done
